//! The agent: the library a target loads.
//!
//! It runs before the program's `main`, joins or creates a session, and answers the clock from it.
//!
//! What the shared object exports is exactly the `no_mangle` items below and in `platform`:
//! rustc's own version script hides the rest, and a second one of ours cannot be added because a
//! linker takes only one anonymous version tag.

mod interpose;
mod platform;
mod setter;
mod state;

// The waits are Linux's own. `epoll`, the timed message queues, the C11 threads calls and the
// `clockwait` pair have no shape the three platforms share, and the other two answer a wait with
// `kevent` and `mach_wait_until` besides, so each brings its own with its arm.
cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        mod genuine;
        mod wait;
    }
}

use std::fmt::Display;
use std::path::{Path, PathBuf};

use common::config::{self, BadValue, ENV_SHARED_FILE, EnvLookup, ProcessEnv};
use common::log::{self, Level};
use common::platform::genuine_clock_gettime;
use common::session::layout::{Fault, FaultCode, Header};
use common::session::{self, SessionError, Setup, naming};

/// What a process that fails closed exits with: the `EX_CONFIG` of `sysexits.h`.
const EX_CONFIG: libc::c_int = 78;

/// The genuine reading of `clock`, for a hook or a controller loaded into a faked process.
///
/// Everything else in this library that names a clock is faked, so this is the only way back to
/// what the operating system actually says.
///
/// # Safety
/// `ts` must be valid for writes of a `timespec`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libtimewarp_genuine_clock_gettime(
    clock: libc::clockid_t,
    ts: *mut libc::timespec,
) -> libc::c_int {
    // Safety: the caller guarantees `ts`, which is exactly what the seam asks for.
    unsafe { genuine_clock_gettime(clock, ts) }
}

#[ctor::ctor]
fn constructor() {
    // The genuine side of every wait, found before the program can call one. A lookup takes the
    // loader's own lock, and this is the one moment where nothing of the program's is holding it.
    #[cfg(target_os = "linux")]
    genuine::prime();
    // We are inside somebody else's program before its `main`. A panic here would abort a process
    // that has done nothing wrong, so the worst case is that the target runs on genuine time.
    drop(std::panic::catch_unwind(attach_or_create));
}

fn attach_or_create() {
    // Before anything that can fail, and read afresh at every exec: the logging variables say
    // where this process writes, not how the session behaves, so a process joining somebody else's
    // session reads them too.
    log::init(&ProcessEnv);

    // The decision is per process, made before the session is touched at all, attaching or
    // creating alike, so an excluded command never attaches, never creates, and never faults.
    match config::excluded(&ProcessEnv, own_command().as_deref()) {
        Ok(true) => return,
        Ok(false) => {}
        Err(bad) => return report_failure(&bad),
    }

    if let Some(inherited) = ProcessEnv.nonempty(ENV_SHARED_FILE) {
        // The inherited path always wins, whatever else the environment says: a session built from
        // this environment instead would fake something nobody asked for.
        match session::attach(Path::new(&inherited)) {
            Ok(session) => join(session),
            // The session the parent named is gone or unusable, so there is none to record the
            // failure in either.
            Err(reason) => report_failure(&reason),
        }
        return;
    }

    let dir = naming::session_dir_from_env();
    let created = Setup::from_env(&ProcessEnv, dir.clone())
        .map_err(SessionError::from)
        .and_then(|setup| session::create(&setup));
    match created {
        Ok(session) => publish(session),
        Err(SessionError::BadValue(bad)) => fall_back(dir, &bad),
        // An unusable directory leaves no place for a session file, so nothing can be recorded.
        Err(reason) => report_failure(&reason),
    }
}

/// Installs the session and names it for whatever this process execs, so its children attach to it
/// rather than making one of their own.
fn publish(session: session::Session) {
    // Safety: a constructor runs before `main`, so nothing in the program is reading `environ` yet
    // to race this write.
    unsafe { std::env::set_var(ENV_SHARED_FILE, session.path()) };
    install(session);
}

/// Hands the session to the process for good: the mapped header, the hook in front of its keeper,
/// and the lock file a setter takes the write lock on.
fn install(session: session::Session) {
    let lock_file = naming::lock_path(session.path());
    let (header, hook) = session.leak();
    state::install(header, hook, lock_file);
}

/// Joins the session this process inherited, loading the hook it names.
///
/// A hook that will not load here is the first thing a process can fail at with a session already in
/// hand, so the fault goes into that session before this process runs on genuine time or leaves. The
/// session outlives us, and a controller should still be able to read why we gave up on it.
fn join(mut session: session::Session) {
    match session.load_hook() {
        Ok(()) => install(session),
        // Genuine, never half-faked: a hook that failed while the session's settings applied would
        // fake the wrong thing silently. The session stays as it is for whoever else attaches.
        Err(bad) => report_failure_in(session.header(), &bad),
    }
}

/// A creator that cannot set up what the environment asked for.
///
/// The process runs on, because we live inside somebody else's program and its exit is not ours to
/// cause, and it runs genuine rather than half-faked: the null keeper, with no hook. The session is
/// published all the same, carrying the fault, so children attach quietly and a controller can read
/// what went wrong.
fn fall_back(dir: PathBuf, bad: &BadValue) {
    // Nothing is on disk yet, so a creator that leaves here leaves the directory clean.
    report_failure(bad);

    let setup = Setup::fallback(&ProcessEnv, dir);
    match session::create_faulted(&setup, Fault::new(FaultCode::BadValue, std::process::id())) {
        Ok(session) => publish(session),
        // No place for a file, so the bad value stays in the log alone.
        Err(reason) => common::log!(Level::Error, "{reason}"),
    }
}

/// Reports a failure in a session that outlives this process: the fault slot first, then the `error`
/// line, then the exit when the session fails closed.
///
/// First fault wins, so a later process joining the same broken session adds nothing but a line of
/// its own. The session's own flag decides whether this process runs on, because an attacher reads
/// none of the setup variables.
fn report_failure_in(header: &Header, reason: &dyn Display) {
    header.record_fault(Fault::new(FaultCode::BadValue, std::process::id()));
    common::log!(Level::Error, "{reason}");
    if session::fails_closed(Some(header), &ProcessEnv) {
        fail_closed(reason);
    }
}

/// Reports a failure that has no session to be recorded in: the `error` line, and the exit when
/// this process fails closed.
///
/// With no header to follow, the variable in the environment decides on its own whether the
/// process runs on, and a caller this returns to still has whatever it can publish to publish.
fn report_failure(reason: &dyn Display) {
    common::log!(Level::Error, "{reason}");
    if session::fails_closed(None, &ProcessEnv) {
        fail_closed(reason);
    }
}

/// One line to the program's own stderr, then `_exit`.
///
/// `_exit`, because we are in the middle of starting somebody's program and its `atexit` handlers
/// are theirs to run.
fn fail_closed(reason: &dyn Display) -> ! {
    log::Sink::stderr().write(Level::Error, format_args!("{reason}"));
    // Safety: `_exit` ends the process, so nothing here can be reached afterwards.
    unsafe { libc::_exit(EX_CONFIG) }
}

/// This process's own command: the basename of the file it was executed from, or `None` when it
/// cannot be learned.
///
/// `std::env::current_exe` already resolves this the same way on Linux, FreeBSD, and macOS alike,
/// through `/proc/self/exe`, a `sysctl`, and `_NSGetExecutablePath` respectively, so there is
/// nothing for the platform seam to add.
fn own_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.file_name()?.to_string_lossy().into_owned())
}
