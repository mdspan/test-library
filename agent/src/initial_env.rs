//! The environment the program started with: the array of `name=value` strings the kernel laid
//! out on the initial stack.
//!
//! libc reads the environment through a pointer of its own, which `setenv` moves to a copy as soon
//! as it adds a variable, and on FreeBSD whatever it changes. Some readers keep to the array the
//! kernel laid out all the same: the Go runtime copies its environment from there when it starts,
//! after every constructor has run, and `main`'s third argument is that array on musl, FreeBSD,
//! and macOS. A session named through `setenv` alone never reaches the programs those start.
//!
//! The array has no spare slot, and what follows it is not ours to move: the auxiliary vector on
//! Linux and FreeBSD, which libc and this library keep pointers into, and the strings dyld hands
//! `main` after it on macOS. So the session's name goes in by taking the place of an entry: the
//! session's own variable wherever the array has one, and otherwise the first variable that only
//! a process creating a session reads.

use std::ffi::{CStr, CString, c_char};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use common::config::{
    ENV_BOOTTIME_RATE, ENV_BOOTTIME_SHIFT, ENV_BOOTTIME_TIMEBASE, ENV_CPU_RATE, ENV_EMULATE_VDSO,
    ENV_HOOK, ENV_KEEPER, ENV_MONOTONIC_RATE, ENV_MONOTONIC_SHIFT, ENV_MONOTONIC_TIMEBASE,
    ENV_RATE, ENV_REALTIME_RATE, ENV_REALTIME_SHIFT, ENV_REALTIME_TIMEBASE, ENV_SHARED_FILE,
    ENV_SHIFT, ENV_TIME,
};
use common::log::Level;

/// The setup variables only a process creating a session reads: every one but the session
/// directory and the fail-closed switch. A process joining a session takes it as its header says
/// and reads none of these, so a child loses nothing by missing one.
const CREATOR_ONLY: [&str; 16] = [
    ENV_TIME,
    ENV_SHIFT,
    ENV_RATE,
    ENV_CPU_RATE,
    ENV_REALTIME_SHIFT,
    ENV_REALTIME_RATE,
    ENV_REALTIME_TIMEBASE,
    ENV_MONOTONIC_SHIFT,
    ENV_MONOTONIC_RATE,
    ENV_MONOTONIC_TIMEBASE,
    ENV_BOOTTIME_SHIFT,
    ENV_BOOTTIME_RATE,
    ENV_BOOTTIME_TIMEBASE,
    ENV_KEEPER,
    ENV_HOOK,
    ENV_EMULATE_VDSO,
];

/// The array, or null when the constructor could not tell which array it is.
static ARRAY: AtomicPtr<*mut c_char> = AtomicPtr::new(ptr::null_mut());

/// Finds the array, before anything of ours or of the program's can move libc's pointer off it.
///
/// Called once, from the constructor, ahead of everything that touches the environment. Relaxed,
/// because the constructor is the whole of this process at the time.
pub(crate) fn prime() {
    ARRAY.store(find(), Ordering::Relaxed);
}

/// libc's own pointer, which points at the array when the constructor runs unless an earlier
/// constructor's `setenv` has moved it. Only the array the kernel laid out has the auxiliary vector
/// behind it.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn find() -> *mut *mut c_char {
    unsafe extern "C" {
        /// The environment as libc keeps it.
        static mut environ: *mut *mut c_char;
    }
    // A raw pointer, because libc may replace what it points at.
    let held = &raw const environ;
    // Safety: `environ` is libc's own pointer to the environment, which it keeps for the life of
    // the process.
    let strings = unsafe { ptr::read(held) };
    if strings.is_null() {
        return ptr::null_mut();
    }
    // Safety: libc keeps a null-terminated array there. Past the terminator of the kernel's lies
    // the vector, and past a copy's the allocator's own words, which the walk refuses at the first.
    if unsafe { crate::auxv::prime(strings) } {
        strings
    } else {
        ptr::null_mut()
    }
}

/// What follows the arguments: dyld hands `main` the arguments as the kernel laid them out, a null,
/// and then the environment, and keeps the same `argv` behind `_NSGetArgv`.
#[cfg(target_os = "macos")]
fn find() -> *mut *mut c_char {
    // Safety: `_NSGetArgv` takes nothing and answers the address of a process-wide word, which
    // dyld set before any initializer of ours ran.
    let arguments = unsafe { libc::_NSGetArgv() };
    // Safety: as above.
    let arguments = unsafe { *arguments };
    // Safety: the same, for the count.
    let count = unsafe { libc::_NSGetArgc() };
    // Safety: as above.
    let count = unsafe { *count };
    let Ok(count) = usize::try_from(count) else {
        return ptr::null_mut();
    };
    if arguments.is_null() {
        return ptr::null_mut();
    }
    // Safety: the array holds `count` arguments and the null after them.
    let end = unsafe { arguments.add(count) };
    // Safety: as above.
    if !unsafe { *end }.is_null() {
        return ptr::null_mut();
    }
    // Safety: the environment starts past that null.
    unsafe { end.add(1) }
}

/// Names the session at `path` in the array, so a child started with it attaches to the session.
///
/// Called once `setenv` has named the session in libc's own environment. When the array had no
/// entry for it, that `setenv` moved libc to a copy, so libc and everything reading the environment
/// through it keep every variable this takes the place of.
pub(crate) fn name_session(path: &Path) {
    let array = ARRAY.load(Ordering::Relaxed);
    if array.is_null() {
        common::log!(
            Level::Warn,
            "the environment this program started with could not be found, so a child given \
             that environment, as every child of a Go program is, starts a session of its own"
        );
        return;
    }
    let mut named = ENV_SHARED_FILE.as_bytes().to_vec();
    named.push(b'=');
    named.extend_from_slice(path.as_os_str().as_bytes());
    let Ok(named) = CString::new(named) else {
        return;
    };
    // Safety: the array is the one the kernel laid out, on the initial stack, which lasts as long
    // as the process; and the constructor is the whole of this process at the time.
    if unsafe { take_place(array, named) } == Placed::Nowhere {
        common::log!(
            Level::Warn,
            "nothing in the environment this program started with can name the session, neither \
             {ENV_SHARED_FILE} nor a variable only a new session reads, so a child given that \
             environment, as every child of a Go program is, starts a session of its own; start \
             the program with {ENV_SHARED_FILE}= set to empty to avoid that"
        );
    }
}

/// Where [`take_place`] put the session's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placed {
    /// In every entry of the session's own variable.
    Own,
    /// In the entry of the first of [`CREATOR_ONLY`].
    CreatorOnly,
    /// Nowhere: the array has neither.
    Nowhere,
}

/// Points every entry of the session's own variable in `array` at `named`, or with none, the entry
/// of the first of [`CREATOR_ONLY`]. An entry that already reads `named` is left alone.
///
/// `named` is leaked once an entry takes it: a Go runtime keeps reading the bytes it copied the
/// pointer of for as long as it runs.
///
/// # Safety
/// `array` must be a writable null-terminated array of pointers to NUL-terminated strings that
/// outlives the process, and nothing else may read or write it while this runs.
unsafe fn take_place(array: *mut *mut c_char, named: CString) -> Placed {
    // Each entry of the session's own variable, and whether it already reads `named`.
    let mut own = Vec::new();
    let mut creator_only = None;
    let mut slot = array;
    loop {
        // Safety: the caller guarantees a null-terminated array, and the walk stops at the null.
        let entry = unsafe { *slot };
        if entry.is_null() {
            break;
        }
        // Safety: the caller guarantees each entry is a NUL-terminated string.
        let text = unsafe { CStr::from_ptr(entry) }.to_bytes();
        if is_named(text, ENV_SHARED_FILE) {
            own.push((slot, text == named.as_bytes()));
        } else if creator_only.is_none() && CREATOR_ONLY.iter().any(|name| is_named(text, name)) {
            creator_only = Some(slot);
        }
        // Safety: the null has not been reached, so there is another element.
        slot = unsafe { slot.add(1) };
    }
    let (placed, stale): (_, Vec<_>) = if own.is_empty() {
        let Some(first) = creator_only else {
            return Placed::Nowhere;
        };
        (Placed::CreatorOnly, vec![first])
    } else {
        let stale = own
            .into_iter()
            .filter_map(|(slot, current)| (!current).then_some(slot));
        (Placed::Own, stale.collect())
    };
    if stale.is_empty() {
        return placed;
    }
    let leaked = named.into_raw();
    for slot in stale {
        // Safety: the slot is an element of the array, which the caller guarantees is aligned,
        // writable, and touched by nothing else.
        let entry = unsafe { AtomicPtr::from_ptr(slot) };
        // Release: a thread that finds the pointer finds the whole string behind it.
        entry.store(leaked, Ordering::Release);
    }
    placed
}

/// Whether `entry`, a `name=value` string, is a value of the variable `name`.
fn is_named(entry: &[u8], name: &str) -> bool {
    entry
        .strip_prefix(name.as_bytes())
        .is_some_and(|rest| rest.first() == Some(&b'='))
}

#[cfg(test)]
mod tests {
    use common::config::{self, ENV_FAIL_CLOSED, ENV_LOG, ENV_ONLY_CMDS, ENV_PRELOAD};
    use common::config::{ENV_CONFIG, ENV_SESSION_DIR, ENV_SKIP_CMDS};
    use common::time::Family;

    use super::*;

    const NAMED: &str = "LIBTIMEWARP__SHARED_FILE=/run/s.session";

    /// An environment of `entries`, and what [`take_place`] made of it.
    fn placed(entries: &[&str]) -> (Placed, Vec<String>, Vec<bool>) {
        let strings: Vec<CString> = entries.iter().map(|e| CString::new(*e).unwrap()).collect();
        let mut array: Vec<*mut c_char> = strings.iter().map(|s| s.as_ptr().cast_mut()).collect();
        array.push(ptr::null_mut());
        let before = array.clone();
        // Safety: the array is null-terminated, holds live strings, and is this case's alone.
        let placed = unsafe { take_place(array.as_mut_ptr(), CString::new(NAMED).unwrap()) };
        let after = array
            .iter()
            .take(entries.len())
            .map(|&entry| {
                // Safety: every entry is a live string, the planted ones or the leaked name.
                let entry = unsafe { CStr::from_ptr(entry) };
                entry.to_string_lossy().into_owned()
            })
            .collect();
        let moved = array.iter().zip(&before).map(|(a, b)| a != b).collect();
        (placed, after, moved)
    }

    #[test]
    fn every_entry_of_the_session_variable_takes_the_name() {
        let (placed, after, _) = placed(&[
            "PATH=/bin",
            "LIBTIMEWARP__SHARED_FILE=",
            "LIBTIMEWARP_TIME=2020-01-01T00:00:00Z",
            "LIBTIMEWARP__SHARED_FILE=pid-1-x.session",
        ]);
        assert_eq!(placed, Placed::Own);
        assert_eq!(
            after,
            [
                "PATH=/bin",
                NAMED,
                "LIBTIMEWARP_TIME=2020-01-01T00:00:00Z",
                NAMED
            ]
        );
    }

    #[test]
    fn an_entry_that_already_names_the_session_is_left_where_it_points() {
        let (placed, _, moved) = placed(&["PATH=/bin", NAMED, "LIBTIMEWARP_RATE=0"]);
        assert_eq!(placed, Placed::Own);
        assert_eq!(moved, [false, false, false, false]);
    }

    #[test]
    fn with_no_session_variable_the_first_variable_only_a_creator_reads_gives_way() {
        let (placed, after, _) = placed(&[
            &format!("{ENV_PRELOAD}=/lib/libtimewarp.so"),
            "LIBTIMEWARP_LOG=/tmp/log",
            "LIBTIMEWARP_LOG_LEVEL=debug",
            "LIBTIMEWARP_FAIL_CLOSED=1",
            "LIBTIMEWARP_SESSION_DIR=/run",
            "XDG_RUNTIME_DIR=/run/user/1",
            "LIBTIMEWARP_CONFIG=/etc/c.toml",
            "LIBTIMEWARP_SKIP_CMDS=curl",
            "LIBTIMEWARP_ONLY_CMDS=date",
            "LIBTIMEWARP_TIMEX=1",
            "LIBTIMEWARP_RATE",
            "LIBTIMEWARP_RATE=0",
            "LIBTIMEWARP_TIME=@0",
        ]);
        assert_eq!(placed, Placed::CreatorOnly);
        let rate = after.iter().position(|entry| entry == NAMED);
        assert_eq!(rate, Some(11), "{after:#?}");
        assert_eq!(after.iter().filter(|entry| *entry == NAMED).count(), 1);
        assert_eq!(
            after.last().map(String::as_str),
            Some("LIBTIMEWARP_TIME=@0")
        );
    }

    #[test]
    fn with_neither_the_environment_is_left_as_it_started() {
        let (placed, _, moved) = placed(&[
            "PATH=/bin",
            &format!("{ENV_PRELOAD}=/lib/libtimewarp.so"),
            "LIBTIMEWARP_LOG=/tmp/log",
        ]);
        assert_eq!(placed, Placed::Nowhere);
        assert!(moved.iter().all(|moved| !moved));
    }

    /// Every variable that configures a family is read at creation alone, and none that a joining
    /// process or the tool reads is taken for one.
    #[test]
    fn the_variables_given_way_are_the_ones_only_a_creator_reads() {
        for family in [
            Family::Realtime,
            Family::Monotonic,
            Family::Boottime,
            Family::Cpu,
        ] {
            let vars = config::family_vars(family);
            for name in [vars.shift, Some(vars.rate), vars.timebase]
                .into_iter()
                .flatten()
            {
                assert!(CREATOR_ONLY.contains(&name), "{name}");
            }
        }
        for name in [
            ENV_SHARED_FILE,
            ENV_PRELOAD,
            ENV_FAIL_CLOSED,
            ENV_LOG,
            ENV_SKIP_CMDS,
            ENV_ONLY_CMDS,
            ENV_SESSION_DIR,
            ENV_CONFIG,
        ] {
            assert!(!CREATOR_ONLY.contains(&name), "{name}");
        }
    }

    /// What the constructor found, before it named the session in the environment.
    #[test]
    fn the_environment_the_kernel_laid_out_was_found() {
        assert!(!ARRAY.load(Ordering::Relaxed).is_null());
    }
}
