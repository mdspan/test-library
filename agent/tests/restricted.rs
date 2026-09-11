//! The limit System Integrity Protection puts on us, pinned so that it is documented rather than
//! surprising.
//!
//! Every system binary is a *restricted* process. dyld ignores `DYLD_INSERT_LIBRARIES` for one, so
//! the library never loads, and it deletes every `DYLD_` variable from that process's environment
//! before `main` runs, so nothing below it gets the library either. `/bin/sh` is restricted, which
//! means `system()`, `popen()` and any `/bin/sh -c` in a faked program lose the session for that
//! whole subtree. The two ways around it are the ones libfaketime has: run targets directly, or
//! copy the shell and the tools a target spawns somewhere unprotected and point the target at the
//! copies.
#![cfg(target_os = "macos")]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "test support: a failed assumption should fail the test loudly, and not every helper \
              is used by every case"
)]

mod harness;

use harness::{HOUR, Run, session_files};

/// The wrapper that loses the session: the shell, which is protected and so restricted.
fn shell() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned()]
}

#[test]
fn a_target_below_bin_sh_reads_the_genuine_clock() {
    let dir = tempfile::tempdir().unwrap();
    // Run directly, the same target under the same settings is faked.
    let direct = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();
    direct.assert_wall_clock("probe", -2 * HOUR);

    let through = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .through(&shell())
        .run();

    through.assert_wall_clock("probe", 0);
    assert_eq!(
        through.get("probe.shared"),
        "",
        "the shell was faked after all, so this limit has gone away"
    );
    // The shell made no session and neither did the target below it, so the one file in the
    // directory is the direct run's.
    assert_eq!(session_files(dir.path()).len(), 1);
}

#[test]
fn the_session_a_parent_named_survives_and_nothing_below_reads_it() {
    let dir = tempfile::tempdir().unwrap();
    let direct = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();
    let session = direct.get("probe.shared").to_owned();
    assert!(!session.is_empty());

    let through = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP__SHARED_FILE", &session)
        .through(&shell())
        .run();

    // Only `DYLD_` variables are pruned, so the target still sees the path of the session it was
    // meant to join; without the library loaded nothing reads it.
    assert_eq!(through.get("probe.shared"), session);
    through.assert_wall_clock("probe", 0);
    assert_eq!(session_files(dir.path()).len(), 1);
}
