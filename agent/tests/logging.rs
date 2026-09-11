//! The log a user names a destination for, seen from outside the process that wrote it.
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

use std::path::Path;

use harness::Run;

/// A bad value, so the constructor has an `error` to report. Any of the Configuration chapter's
/// would do; this one is the shortest to write.
const A_BAD_VALUE: (&str, &str) = ("LIBTIMEWARP_RATE", "-1");

fn log_of(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn a_named_path_gets_one_line_per_event() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("timewarp.log");
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_LOG", log.to_string_lossy())
        .env(A_BAD_VALUE.0, A_BAD_VALUE.1)
        .run();

    let written = log_of(&log);
    assert_eq!(written.lines().count(), 1, "{written}");
    assert!(
        written.contains(&format!("[{}] error: ", report.get("probe.pid"))),
        "{written}"
    );
    assert!(
        written.contains("LIBTIMEWARP_RATE: not a valid value"),
        "{written}"
    );
}

#[test]
fn the_program_we_are_faking_sees_nothing_on_its_own_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("timewarp.log");
    for named in [true, false] {
        let mut run = Run::prog("probe")
            .session_dir(dir.path())
            .env(A_BAD_VALUE.0, A_BAD_VALUE.1);
        if named {
            run = run.env("LIBTIMEWARP_LOG", log.to_string_lossy());
        }
        let report = run.run();
        assert_eq!(report.stderr, "", "with a destination named: {named}");
    }
}

#[test]
fn naming_no_destination_writes_nothing_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env(A_BAD_VALUE.0, A_BAD_VALUE.1)
        .run();

    assert_eq!(report.stderr, "");
    for line in report.stdout.lines() {
        assert!(!line.contains("libtimewarp["), "{line}");
    }
}

#[test]
fn a_descriptor_wins_over_a_path() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("timewarp.log");
    // The target's own stdout, which is the one descriptor a test can name without opening it.
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_LOG_FD", "1")
        .env("LIBTIMEWARP_LOG", log.to_string_lossy())
        .env(A_BAD_VALUE.0, A_BAD_VALUE.1)
        .run();

    assert!(
        report
            .stdout
            .contains("error: LIBTIMEWARP_RATE: not a valid value"),
        "{}",
        report.stdout
    );
    assert!(!log.exists(), "the path was opened as well");
}

#[test]
fn a_process_that_execs_reads_the_variables_again() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("timewarp.log");

    let by_path =
        execs_under_a_destination(dir.path(), &[("LIBTIMEWARP_LOG", &log.to_string_lossy())]);
    assert_two_lines(&log_of(&log), &by_path);

    // The same through a descriptor. Our private copy is `O_CLOEXEC`, so the exec'd process has a
    // logger at all only by reading the number again and duplicating it for itself.
    let by_descriptor = execs_under_a_destination(dir.path(), &[("LIBTIMEWARP_LOG_FD", "1")]);
    assert_two_lines(&by_descriptor.stdout, &by_descriptor);
}

/// One run of `execer`, with the session it inherits unusable, so each of the two processes has an
/// `error` of its own to report.
fn execs_under_a_destination(dir: &Path, vars: &[(&str, &str)]) -> harness::Report {
    let probe = harness::program("probe");
    let mut run = Run::prog("execer")
        .arg(probe.to_string_lossy())
        .session_dir(dir)
        .env(
            "LIBTIMEWARP__SHARED_FILE",
            dir.join("gone.session").to_string_lossy(),
        );
    for (name, value) in vars {
        run = run.env(name, value);
    }
    run.run()
}

fn assert_two_lines(written: &str, report: &harness::Report) {
    let lines: Vec<&str> = written
        .lines()
        .filter(|line| line.starts_with("libtimewarp["))
        .collect();
    assert_eq!(lines.len(), 2, "{written}");
    for (line, pid) in lines.iter().zip(["before.pid", "after.pid"]) {
        assert!(
            line.starts_with(&format!("libtimewarp[{}] error: ", report.get(pid))),
            "{line}"
        );
    }
}

#[test]
fn a_level_names_how_much_of_the_log_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    // `error` is the most severe level, so every setting keeps it, and one nobody can read falls
    // back to the default, which keeps it too.
    for level in ["error", "trace", "chatty"] {
        let log = dir.path().join(format!("{level}.log"));
        Run::prog("probe")
            .session_dir(dir.path())
            .env("LIBTIMEWARP_LOG", log.to_string_lossy())
            .env("LIBTIMEWARP_LOG_LEVEL", level)
            .env(A_BAD_VALUE.0, A_BAD_VALUE.1)
            .run();

        assert_eq!(log_of(&log).lines().count(), 1, "at level {level}");
    }
}
