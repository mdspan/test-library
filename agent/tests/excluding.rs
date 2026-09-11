//! `curl` and `git` see genuine time inside a faked tree: a command on the skip list, or off the
//! only list, runs untouched, and its own children still attach to the session as usual.
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

use harness::{HOUR, Run};

fn session_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir.join("libtimewarp")) else {
        return Vec::new();
    };
    entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.file_name().is_some_and(harness::is_session_file))
        .collect()
}

fn log_of(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// The `EX_CONFIG` of `sysexits.h`, which is what a process that fails closed leaves behind.
const EX_CONFIG: i32 = 78;

#[test]
fn a_command_on_the_skip_list_runs_genuine_and_its_own_child_is_faked_again() {
    let dir = tempfile::tempdir().unwrap();
    let relay = harness::program("relay");
    let excluded = harness::copy_named(&relay, "banned-cmd");

    let report = Run::new(relay.clone())
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .env("LIBTIMEWARP_SKIP_CMDS", "banned-cmd")
        .arg("shell")
        .arg(excluded.to_string_lossy())
        .arg("excluded")
        .arg(relay.to_string_lossy())
        .arg("child")
        .run();

    // The shell is faked, the excluded command in the middle sees genuine time, and its own
    // child, another `relay`, is faked again on the same session.
    report.assert_wall_clock("shell", -2 * HOUR);
    report.assert_wall_clock("excluded", 0);
    report.assert_wall_clock("child", -2 * HOUR);

    assert_eq!(report.get("shell.shared"), report.get("child.shared"));
    assert_eq!(
        report.get("excluded.shared"),
        report.get("shell.shared"),
        "the excluded command left LIBTIMEWARP__SHARED_FILE as it found it"
    );
    assert_eq!(
        session_files(dir.path()).len(),
        1,
        "the excluded command or its child built a session of its own"
    );
}

#[test]
fn a_command_on_the_skip_list_alone_runs_genuine_and_attaches_to_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .env("LIBTIMEWARP_SKIP_CMDS", "probe")
        .run();

    report.assert_wall_clock("probe", 0);
    assert_eq!(report.get("probe.shared"), "", "it attached to a session");
    assert!(
        session_files(dir.path()).is_empty(),
        "an excluded creator built a session anyway"
    );
}

#[test]
fn a_command_off_the_only_list_runs_genuine_and_attaches_to_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .env("LIBTIMEWARP_ONLY_CMDS", "something-else")
        .run();

    report.assert_wall_clock("probe", 0);
    assert_eq!(report.get("probe.shared"), "", "it attached to a session");
    assert!(session_files(dir.path()).is_empty());
}

#[test]
fn a_command_on_the_only_list_is_faked_as_usual() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .env("LIBTIMEWARP_ONLY_CMDS", "something-else,probe")
        .run();

    report.assert_wall_clock("probe", -2 * HOUR);
    assert_eq!(session_files(dir.path()).len(), 1);
}

#[test]
fn setting_both_lists_is_a_bad_value_that_lands_before_any_session_exists() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("timewarp.log");
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_LOG", log.to_string_lossy())
        .env("LIBTIMEWARP_SKIP_CMDS", "curl")
        .env("LIBTIMEWARP_ONLY_CMDS", "git")
        .run();

    report.assert_wall_clock("probe", 0);
    assert_eq!(report.get("probe.shared"), "", "a session was published");
    assert!(
        session_files(dir.path()).is_empty(),
        "the one bad value with nowhere to record itself left a session behind"
    );

    let written = log_of(&log);
    assert_eq!(written.lines().count(), 1, "{written}");
    assert!(written.contains("] error: "), "{written}");
    assert!(written.contains("LIBTIMEWARP_SKIP_CMDS"), "{written}");
}

#[test]
fn failing_closed_on_both_lists_set_exits_before_any_session_exists() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_FAIL_CLOSED", "1")
        .env("LIBTIMEWARP_SKIP_CMDS", "curl")
        .env("LIBTIMEWARP_ONLY_CMDS", "git")
        .run_unchecked();

    assert_eq!(report.status.code(), Some(EX_CONFIG));
    assert_eq!(report.stdout, "", "the program ran anyway");
    assert!(
        session_files(dir.path()).is_empty(),
        "a failing creator published a session"
    );
}
