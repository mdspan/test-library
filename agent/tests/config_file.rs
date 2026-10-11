//! The configuration file supplies whatever the environment leaves unset, for a creator and for
//! every process that reads its own logging and exclusion variables.
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

use harness::{HOUR, Run};

/// The `EX_CONFIG` of `sysexits.h`, which is what a process that fails closed leaves behind.
const EX_CONFIG: i32 = 78;

fn write_file(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn session_count(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("libtimewarp")).map_or(0, |entries| {
        entries
            .filter(|entry| harness::is_session_file(&entry.as_ref().unwrap().file_name()))
            .count()
    })
}

#[test]
fn the_named_file_sets_the_clock() {
    let dir = tempfile::tempdir().unwrap();
    let rc = write_file(
        dir.path(),
        "rc",
        "# two hours back\nLIBTIMEWARP_SHIFT=-2h\n",
    );
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_CONFIG", rc.to_string_lossy())
        .run();

    report.assert_wall_clock("probe", -2 * HOUR);
}

#[test]
fn the_environment_wins_over_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let rc = write_file(dir.path(), "rc", "LIBTIMEWARP_SHIFT=-2h\n");
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_CONFIG", rc.to_string_lossy())
        .env("LIBTIMEWARP_SHIFT", "1h")
        .run();

    report.assert_wall_clock("probe", HOUR);
}

#[test]
fn the_user_file_is_read_from_home() {
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), ".libtimewarprc", "LIBTIMEWARP_SHIFT=-3h\n");
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("HOME", dir.path().to_string_lossy())
        .run();

    report.assert_wall_clock("probe", -3 * HOUR);
}

#[test]
fn the_file_can_place_the_session_directory() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    std::fs::create_dir(&sessions).unwrap();
    let rc = write_file(
        dir.path(),
        "rc",
        &format!("LIBTIMEWARP_SESSION_DIR={}\n", sessions.display()),
    );
    let report = Run::prog("probe")
        .env("LIBTIMEWARP_CONFIG", rc.to_string_lossy())
        .run();

    assert!(
        report
            .get("probe.shared")
            .starts_with(&*sessions.to_string_lossy()),
        "{}",
        report.get("probe.shared")
    );
    assert_eq!(session_count(&sessions), 1);
}

#[test]
fn a_command_skipped_by_the_file_runs_genuine_and_attaches_to_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let rc = write_file(
        dir.path(),
        "rc",
        "LIBTIMEWARP_SHIFT=-2h\nLIBTIMEWARP_SKIP_CMDS=probe\n",
    );
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_CONFIG", rc.to_string_lossy())
        .run();

    report.assert_wall_clock("probe", 0);
    assert_eq!(report.get("probe.shared"), "", "it attached to a session");
    assert_eq!(session_count(dir.path()), 0);
}

#[test]
fn a_bad_line_runs_the_process_genuine_and_leaves_no_session() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("timewarp.log");
    let rc = write_file(
        dir.path(),
        "rc",
        "LIBTIMEWARP_SHIFT=-2h\nLIBTIMEWARP_SHFIT=1h\n",
    );
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_CONFIG", rc.to_string_lossy())
        .env("LIBTIMEWARP_LOG", log.to_string_lossy())
        .run();

    report.assert_wall_clock("probe", 0);
    assert_eq!(report.get("probe.shared"), "", "a session was published");
    assert_eq!(session_count(dir.path()), 0);

    let written = harness::log_of(&log);
    assert_eq!(written.lines().count(), 1, "{written}");
    assert!(
        written.contains("] error: LIBTIMEWARP_CONFIG: line 2 is not a setting"),
        "{written}"
    );
}

#[test]
fn a_bad_value_in_the_file_is_recorded_in_the_session_like_one_in_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("timewarp.log");
    let rc = write_file(
        dir.path(),
        "rc",
        &format!("LIBTIMEWARP_RATE=-1\nLIBTIMEWARP_LOG={}\n", log.display()),
    );
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_CONFIG", rc.to_string_lossy())
        .run();

    report.assert_wall_clock("probe", 0);
    assert_eq!(session_count(dir.path()), 1);
    let written = harness::log_of(&log);
    assert!(
        written.contains("LIBTIMEWARP_RATE: not a valid value"),
        "{written}"
    );
}

#[test]
fn failing_closed_on_a_missing_named_file_exits_before_main() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_FAIL_CLOSED", "1")
        .env(
            "LIBTIMEWARP_CONFIG",
            dir.path().join("absent").to_string_lossy(),
        )
        .run_unchecked();

    assert_eq!(report.status.code(), Some(EX_CONFIG));
    assert_eq!(report.stdout, "", "the program ran anyway");
    assert!(
        report.stderr.contains("LIBTIMEWARP_CONFIG: cannot be read"),
        "{}",
        report.stderr
    );
    assert_eq!(session_count(dir.path()), 0);
}
