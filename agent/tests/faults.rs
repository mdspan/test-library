//! What a target does when the library cannot set up what the environment asked for: genuine time
//! on the null keeper, a fault a controller can read, or an exit.
#![cfg(any(target_os = "linux", target_os = "freebsd"))]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "test support: a failed assumption should fail the test loudly, and not every helper \
              is used by every case"
)]

mod harness;

use std::path::{Path, PathBuf};

use common::session::layout::{Fault, FaultCode, Keeper};
use harness::{HOUR, Run, log_of, only_session, session_files};

/// One of the values the library refuses, and the shortest of them to write.
const A_BAD_VALUE: (&str, &str) = ("LIBTIMEWARP_RATE", "-1");

/// The `EX_CONFIG` of `sysexits.h`, which is what a process that fails closed leaves behind.
const EX_CONFIG: i32 = 78;

#[test]
fn the_null_keeper_leaves_every_clock_genuine() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_KEEPER", "null")
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    report.assert_wall_clock("probe", 0);

    // A session all the same, so the tree below it joins this one rather than building its own.
    let session = only_session(dir.path());
    assert_eq!(session.header().keeper, Keeper::Null as u32);
    assert_eq!(session.header().fault(), None, "nothing went wrong here");
    assert_eq!(session.path(), Path::new(report.get("probe.shared")));
}

#[test]
fn a_creator_that_cannot_set_up_what_it_was_asked_for_publishes_a_faulted_session() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env(A_BAD_VALUE.0, A_BAD_VALUE.1)
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    report.assert_wall_clock("probe", 0);

    let session = only_session(dir.path());
    let header = session.header();
    // Never a hook without its settings, and never the settings without the hook.
    assert_eq!(header.keeper, Keeper::Null as u32);
    assert_eq!(header.hook_path(), b"");
    assert_eq!(
        header.fault(),
        Some(Fault::new(
            FaultCode::BadValue,
            report.get("probe.pid").parse().unwrap()
        ))
    );
    assert_eq!(session.path(), Path::new(report.get("probe.shared")));
}

#[test]
fn every_value_the_library_refuses_drops_the_process_to_the_null_keeper() {
    for vars in [
        vec![("LIBTIMEWARP_KEEPER", "genuine")],
        vec![("LIBTIMEWARP_RATE", "-1")],
        vec![("LIBTIMEWARP_TIME", "yesterday")],
        vec![("LIBTIMEWARP_TIME", "2300-01-01T00:00:00Z")],
        vec![("LIBTIMEWARP_MONOTONIC_SHIFT", "-100000d")],
        vec![("LIBTIMEWARP_REALTIME_TIMEBASE", "CLOCK_YESTERDAY")],
        vec![("LIBTIMEWARP_MONOTONIC_TIMEBASE", "CLOCK_REALTIME")],
        vec![("LIBTIMEWARP_TIMEKEEP_INTERVAL", "often")],
        vec![
            ("LIBTIMEWARP_TIME", "2020-01-01T00:00:00Z"),
            ("LIBTIMEWARP_SHIFT", "-2h"),
        ],
    ] {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("timewarp.log");
        let mut run = Run::prog("probe")
            .session_dir(dir.path())
            .env("LIBTIMEWARP_LOG", log.to_string_lossy());
        for (name, value) in &vars {
            run = run.env(name, value);
        }
        let report = run.run();

        report.assert_wall_clock("probe", 0);
        let session = only_session(dir.path());
        assert_eq!(session.header().keeper, Keeper::Null as u32, "{vars:?}");
        assert!(session.header().fault().is_some(), "{vars:?}");

        let written = log_of(&log);
        assert_eq!(written.lines().count(), 1, "{vars:?}: {written}");
        assert!(written.contains("] error: "), "{vars:?}: {written}");
    }
}

#[test]
fn children_of_a_faulted_creator_attach_with_no_line_of_their_own() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("timewarp.log");
    let probe = harness::program("probe");
    let report = Run::prog("execer")
        .arg(probe.to_string_lossy())
        .session_dir(dir.path())
        .env("LIBTIMEWARP_LOG", log.to_string_lossy())
        .env(A_BAD_VALUE.0, A_BAD_VALUE.1)
        .run();

    report.assert_wall_clock("before", 0);
    report.assert_wall_clock("after", 0);
    assert_eq!(report.get("before.shared"), report.get("after.shared"));
    assert_eq!(
        session_files(dir.path()).len(),
        1,
        "the child built a session of its own"
    );

    // The creator's line and nobody else's: the child took the session as it found it.
    let written = log_of(&log);
    assert_eq!(written.lines().count(), 1, "{written}");
    assert!(
        written.contains(&format!("[{}] error: ", report.get("before.pid"))),
        "{written}"
    );
}

#[test]
fn failing_closed_exits_on_a_bad_value_and_publishes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_FAIL_CLOSED", "1")
        .env(A_BAD_VALUE.0, A_BAD_VALUE.1)
        .run_unchecked();

    assert_eq!(report.status.code(), Some(EX_CONFIG));
    assert_eq!(report.stdout, "", "the program ran anyway");
    // One line, on the stderr we otherwise never touch, naming what failed.
    assert_eq!(report.stderr.lines().count(), 1, "{}", report.stderr);
    assert!(
        report
            .stderr
            .contains("LIBTIMEWARP_RATE: not a valid value"),
        "{}",
        report.stderr
    );
    assert!(
        session_files(dir.path()).is_empty(),
        "the directory was left for the user to clean up"
    );
}

#[test]
fn failing_closed_leaves_a_session_it_could_set_up_alone() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_FAIL_CLOSED", "1")
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    report.assert_wall_clock("probe", -2 * HOUR);
    assert_eq!(report.stderr, "");
    // The choice is the session's, so it is recorded for every process that attaches later.
    assert_eq!(only_session(dir.path()).header().fail_closed, 1);
}

/// A session directory that cannot be made, because a file sits where its parent would be. It is
/// the one failure here that leaves no place for a session file at all.
fn no_place_for_a_session(dir: &Path) -> PathBuf {
    let blocked = dir.join("file");
    std::fs::write(&blocked, b"").unwrap();
    blocked
}

#[test]
fn a_failure_before_any_session_exists_stays_in_the_log_alone() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("timewarp.log");
    let blocked = no_place_for_a_session(dir.path());

    let report = Run::prog("probe")
        .session_dir(&blocked)
        .env("LIBTIMEWARP_LOG", log.to_string_lossy())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    report.assert_wall_clock("probe", 0);
    assert_eq!(report.get("probe.shared"), "", "a session was published");
    assert_eq!(log_of(&log).lines().count(), 1, "{}", log_of(&log));
}

#[test]
fn the_variable_decides_on_its_own_when_there_is_no_session_to_follow() {
    let dir = tempfile::tempdir().unwrap();
    let blocked = no_place_for_a_session(dir.path());

    let report = Run::prog("probe")
        .session_dir(&blocked)
        .env("LIBTIMEWARP_FAIL_CLOSED", "1")
        .run_unchecked();

    assert_eq!(report.status.code(), Some(EX_CONFIG));
    assert_eq!(report.stdout, "");
}

#[test]
fn an_attacher_with_no_session_to_join_follows_its_own_environment() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env(
            "LIBTIMEWARP__SHARED_FILE",
            dir.path().join("gone.session").to_string_lossy(),
        )
        .env("LIBTIMEWARP_FAIL_CLOSED", "1")
        .run_unchecked();

    assert_eq!(report.status.code(), Some(EX_CONFIG));
    assert!(
        session_files(dir.path()).is_empty(),
        "a fresh session would fake what nobody asked for"
    );
}
