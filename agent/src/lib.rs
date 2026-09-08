//! The agent: the library a target loads.
//!
//! It runs before the program's `main`, joins or creates a session, and answers the clock from it.

mod interpose;
mod platform;
mod state;

use std::path::Path;

use common::config::{ENV_SHARED_FILE, EnvLookup, ProcessEnv};
use common::platform::genuine_clock_gettime;
use common::session::{self, Setup, naming};

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
    // We are inside somebody else's program before its `main`. A panic here would abort a process
    // that has done nothing wrong, so the worst case is that the target runs on genuine time.
    drop(std::panic::catch_unwind(attach_or_create));
}

fn attach_or_create() {
    if let Some(inherited) = ProcessEnv.nonempty(ENV_SHARED_FILE) {
        // The inherited path always wins, whatever else the environment says: a session built from
        // this environment instead would fake something nobody asked for.
        match session::attach(Path::new(&inherited)) {
            Ok(session) => state::install(session.leak()),
            Err(reason) => give_up(&reason),
        }
        return;
    }

    let setup = match Setup::from_env(&ProcessEnv, naming::session_dir_from_env()) {
        Ok(setup) => setup,
        Err(reason) => return give_up(&reason),
    };
    match session::create(&setup) {
        Ok(session) => {
            // So whatever this process execs attaches to the session rather than making its own.
            // Safety: a constructor runs before `main`, so nothing in the program is reading
            // `environ` yet to race this write.
            unsafe { std::env::set_var(ENV_SHARED_FILE, session.path()) };
            state::install(session.leak());
        }
        Err(reason) => give_up(&reason),
    }
}

/// The one place setup can fail. The process runs on genuine time, because we live inside somebody
/// else's program and its exit is not ours to cause. Logging `reason` and recording the fault for
/// `LIBTIMEWARP_FAIL_CLOSED` come later.
fn give_up(reason: &dyn std::fmt::Display) {
    let _ = reason;
}
