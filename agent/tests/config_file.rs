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

fn write_file(path: &Path, text: &str) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    path.to_path_buf()
}

fn session_count(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("libtimewarp")).map_or(0, |entries| {
        entries
            .filter(|entry| harness::is_session_file(&entry.as_ref().unwrap().file_name()))
            .count()
    })
}

/// A run of `probe` with the configuration file `text` named by `LIBTIMEWARP_CONFIG`.
fn configured(dir: &Path, text: &str) -> Run {
    let config = write_file(&dir.join("timewarp.toml"), text);
    Run::prog("probe")
        .session_dir(dir)
        .env("LIBTIMEWARP_CONFIG", config.to_string_lossy())
}

#[test]
fn the_named_file_sets_the_clock() {
    let dir = tempfile::tempdir().unwrap();
    let report = configured(dir.path(), "# two hours back\nshift = \"-2h\"\n").run();

    report.assert_wall_clock("probe", -2 * HOUR);
}

#[test]
fn the_environment_wins_over_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let report = configured(dir.path(), "shift = \"-2h\"\n")
        .env("LIBTIMEWARP_SHIFT", "1h")
        .run();

    report.assert_wall_clock("probe", HOUR);
}

#[test]
fn the_user_file_is_read_from_the_config_home() {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        &dir.path().join(".config/libtimewarp/config.toml"),
        "[realtime]\nshift = -10800\n",
    );
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
    let config = write_file(
        &dir.path().join("timewarp.toml"),
        &format!("session_dir = '{}'\n", sessions.display()),
    );
    let report = Run::prog("probe")
        .env("LIBTIMEWARP_CONFIG", config.to_string_lossy())
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
    let report = configured(dir.path(), "shift = \"-2h\"\nskip_cmds = [\"probe\"]\n").run();

    report.assert_wall_clock("probe", 0);
    assert_eq!(report.get("probe.shared"), "", "it attached to a session");
    assert_eq!(session_count(dir.path()), 0);
}

#[test]
fn an_unknown_key_runs_the_process_genuine_and_leaves_no_session() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("timewarp.log");
    let report = configured(dir.path(), "shift = \"-2h\"\nshfit = \"1h\"\n")
        .env("LIBTIMEWARP_LOG", log.to_string_lossy())
        .run();

    report.assert_wall_clock("probe", 0);
    assert_eq!(report.get("probe.shared"), "", "a session was published");
    assert_eq!(session_count(dir.path()), 0);

    let written = harness::log_of(&log);
    assert_eq!(written.lines().count(), 1, "{written}");
    assert!(
        written.contains("timewarp.toml: line 2: not a setting"),
        "{written}"
    );
}

#[test]
fn a_bad_value_in_the_file_is_recorded_in_the_session_like_one_in_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("timewarp.log");
    let report = configured(
        dir.path(),
        &format!("rate = -1\n\n[log]\nfile = '{}'\n", log.display()),
    )
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
            dir.path().join("absent.toml").to_string_lossy(),
        )
        .run_unchecked();

    assert_eq!(report.status.code(), Some(EX_CONFIG));
    assert_eq!(report.stdout, "", "the program ran anyway");
    assert!(
        report.stderr.contains("absent.toml: cannot be read"),
        "{}",
        report.stderr
    );
    assert_eq!(session_count(dir.path()), 0);
}
