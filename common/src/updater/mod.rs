//! The updater: the process that keeps a session's timekeep page fresh.
//!
//! The page a FreeBSD reader computes its clock from goes stale on its own, because a difference of
//! two counter readings overflows the page's own mask and scale within a second or so. Something
//! must republish it at a fixed interval for the whole life of the session, even when nothing
//! changes. The page's lifetime is the session's, so that something can live in no one target, and
//! targets stay thread-free, so it is a process of its own.
//!
//! It is alive by lease: an exclusive lock on `<name>.lease`, which the kernel releases when the
//! holder dies. Everyone who attaches a session with the page on, and every controller operation on
//! one, probes that lease and starts an updater on finding it released. Two respawners racing both
//! exec one; the lease decides, and the one that fails to take it exits.
//!
//! It exits with the session: every few seconds it walks the processes of its uid and leaves when
//! none of them maps the session file. Descriptors play no part, so a daemon that closes everything
//! after starting keeps its updater, and a forked child is counted because it inherited the map.
//!
//! **The residual window is accepted, not fixed.** An updater that dies in a session where nothing
//! attaches and no controller acts leaves the page to go stale, silently, for exactly the readers
//! that never run our code. That is the cost of keeping targets thread-free.

mod lease;
mod spawn;

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::config::{ENV_PRELOAD, ENV_SHARED_FILE, ENV_UPDATER, EnvLookup, ProcessEnv};
use crate::log::Level;
use crate::platform::{self, clock};
use crate::session::layout::Header;
use crate::session::{naming, page_is_live, timekeep};
use crate::time::Nanos;

pub use spawn::UPDATER_FLAG;

/// How often the processes of this uid are walked to see whether anyone is still attached. Far
/// longer than the republish interval, because the walk costs a system call per process and nothing
/// goes wrong from noticing a moment late.
const LIVENESS_INTERVAL: Nanos = Nanos(5 * Nanos::SECOND);

/// Room for the process list of one uid, and for one process's mappings.
///
/// Fixed and taken once, because an updater allocates nothing after startup: it is mlocked, so
/// every page it holds is a page the machine cannot reclaim, and its OOM score has to stay near
/// zero. A list that does not fit leaves the walk unanswered, which the caller reads as "still
/// attached" rather than as a reason to leave.
const PROCESS_BUFFER: usize = 128 * 1024;
const MAPPING_BUFFER: usize = 256 * 1024;

/// Where the library that runs the updater lives, once somebody has said.
static LIBRARY: OnceLock<PathBuf> = OnceLock::new();

/// Names the shared object an updater is exec'd from.
///
/// Called by the agent's constructor, which is the one place that can answer with certainty: the
/// loader knows where it put us. A process without the library loaded falls back to the preload
/// variable, and a process with neither cannot start an updater at all.
pub fn install_library(path: PathBuf) {
    drop(LIBRARY.set(path));
}

/// The library an updater is exec'd from, or `None` when this process cannot name one.
#[must_use]
pub fn library() -> Option<PathBuf> {
    if let Some(known) = LIBRARY.get() {
        return Some(known.clone());
    }
    // A controller run inside a faked tree has the library named in its own environment, which is
    // the only other place the path is written down.
    let preloaded = ProcessEnv.nonempty(ENV_PRELOAD)?;
    preloaded
        .as_bytes()
        .split(|&byte| byte == b':' || byte == b' ')
        .map(|entry| PathBuf::from(OsStr::from_bytes(entry)))
        .find(|entry| {
            entry
                .file_name()
                .is_some_and(|name| name.as_bytes().starts_with(LIBRARY_PREFIX))
        })
}

/// What this library's file is called, up to the version and the suffix each platform adds.
const LIBRARY_PREFIX: &[u8] = b"libtimewarp";

/// Makes sure `session` has an updater, starting one when the lease says it has none.
///
/// Called wherever the design says to probe: at creation, at every attach, and at every controller
/// operation. A session with the page off, a build that cannot write one, and a platform that reads
/// the clock some other way all return here at once.
///
/// Nothing here can fail loudly. The caller is somebody's constructor or somebody's controller
/// call, and a session whose page goes stale is worse served by an error than by a line in the log.
pub fn ensure(session: &Path, header: &Header) {
    if !page_is_live(header) {
        return;
    }
    match lease::held(&naming::lease_path(session)) {
        // An updater is alive, or the kernel would not say; either way, starting a second one would
        // only make it exit again.
        Some(true) | None => {}
        Some(false) => {
            if let Err(why) = spawn::updater(session, &ProcessEnv) {
                crate::log!(Level::Warn, "the timekeep page has no updater: {why}");
            }
        }
    }
}

/// Whether this process was started to be an updater, and for which session.
///
/// The variable is the library's own, set by whoever spawned us. The session comes with it, because
/// a constructor cannot reach the arguments its process was started with and the fallback runs the
/// loop from one.
#[must_use]
pub fn requested(env: &impl EnvLookup) -> Option<PathBuf> {
    env.nonempty(ENV_UPDATER)?;
    Some(PathBuf::from(env.nonempty(ENV_SHARED_FILE)?))
}

/// The session named by an updater's own command line, or `None` when it names none.
///
/// What `main` reads, where the fallback reads [`requested`] instead.
#[must_use]
pub fn session_in_args(args: &[OsString]) -> Option<PathBuf> {
    let flag = args
        .iter()
        .position(|arg| arg.as_bytes() == UPDATER_FLAG.as_bytes())?;
    args.get(flag.checked_add(1)?).map(PathBuf::from)
}

/// Runs the updater for `session` until nothing maps it any more, answering the exit status the
/// process should leave with.
///
/// Zero for every ordinary end, including losing the race for the lease: two respawners both exec
/// one, and the loser leaving quietly is how the lease decides between them.
#[must_use]
pub fn run(session: &Path) -> libc::c_int {
    // The lease first, so a second updater started by a racing respawner leaves before it maps
    // anything, and so the attach below finds the lease held rather than starting a third.
    let Ok(held) = lease::Lease::take(&naming::lease_path(session)) else {
        return 0;
    };

    let Ok(mapped) = crate::session::attach(session) else {
        return 0;
    };
    let Some(page) = mapped.timekeep() else {
        return 0;
    };
    let Some(basis) = timekeep::counter::basis() else {
        crate::log!(
            Level::Warn,
            "no hardware counter this process can publish a timekeep page against"
        );
        return 0;
    };
    let header = mapped.header();
    let Ok(wanted) = c_path(session) else {
        return 0;
    };

    // Everything the loop below touches, taken before it starts. From here on the updater
    // allocates nothing: it is mlocked, and a page it takes later is a page the machine cannot
    // reclaim.
    let mut processes = vec![0u8; PROCESS_BUFFER];
    let mut mappings = vec![0u8; MAPPING_BUFFER];
    pin_in_memory();

    let interval = timekeep::interval(header);
    let mut checked = now();
    loop {
        timekeep::republish(header, page, basis);
        sleep(interval);
        let at = now();
        if at.saturating_sub(checked) >= LIVENESS_INTERVAL {
            checked = at;
            if !still_attached(&wanted, &mut processes, &mut mappings) {
                break;
            }
        }
    }
    drop(held);
    0
}

/// Whether any process of this uid still has the session file mapped.
///
/// A walk the kernel would not answer counts as attached, because an updater that left on a
/// question it could not ask would take the page with it.
#[cfg(target_os = "freebsd")]
fn still_attached(session: &CString, processes: &mut [u8], mappings: &mut [u8]) -> bool {
    platform::any_process_maps(session.as_bytes_with_nul(), processes, mappings).unwrap_or(true)
}

/// No other platform keeps an updater, so no other platform has a walk to make.
#[cfg(not(target_os = "freebsd"))]
const fn still_attached(_session: &CString, _processes: &mut [u8], _mappings: &mut [u8]) -> bool {
    true
}

/// Sleeps for a genuine duration, unscaled by anything this library fakes.
///
/// The system call rather than libc's, because the process running the loop may have this very
/// library preloaded and libc's own would come back to our interposer.
#[cfg(target_os = "freebsd")]
fn sleep(duration: Nanos) {
    let wanted = duration.to_timespec();
    // Safety: `wanted` is a live, correctly typed buffer the kernel reads and no more.
    unsafe { platform::genuine_nanosleep(&raw const wanted) };
}

/// The same, where no updater ever runs, so nothing here interposes anything.
#[cfg(not(target_os = "freebsd"))]
fn sleep(duration: Nanos) {
    let wanted = duration.to_timespec();
    // Safety: `wanted` is a live, correctly typed buffer.
    unsafe { libc::nanosleep(&raw const wanted, std::ptr::null_mut()) };
}

/// The genuine instant the loop measures its own cadence against.
fn now() -> Nanos {
    platform::genuine_nanos(clock::MONOTONIC).unwrap_or(Nanos::ZERO)
}

/// Keeps every page of the updater resident, so the OOM killer picks it last.
///
/// With no allocation after startup, a process this small and this pinned is worth nothing to
/// reclaim. The killer can still pick it, and the lease is what lets the next attach notice.
fn pin_in_memory() {
    // Safety: `mlockall` takes flags and touches nothing of ours. A refusal, which an unprivileged
    // process may well get, leaves the updater running unpinned.
    unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) };
}

/// A path as the system calls here take it.
fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::other("a path with a NUL in it names nothing"))
}

#[cfg(test)]
mod tests;
