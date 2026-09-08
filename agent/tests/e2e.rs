//! The narrow complete path: a shifted wall clock, seen by a preloaded program.
//!
//! `LD_PRELOAD` needs a platform arm that exports the interposed names, and the target needs one
//! that can read a genuine clock, so these cases only apply where both exist.
#![cfg(target_os = "linux")]
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

use harness::{HOUR, Run};

fn session_files(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir.join("libtimewarp")) else {
        return Vec::new();
    };
    entries
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn a_shift_moves_the_wall_clock() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    report.assert_wall_clock("probe", -2 * HOUR);
}

#[test]
fn every_documented_duration_spelling_reaches_the_clock() {
    let dir = tempfile::tempdir().unwrap();
    for (spelling, nanos) in [
        ("-7200", -2 * HOUR),
        ("1.5h", 3 * HOUR / 2),
        ("500ms", 500_000_000),
        ("1d", 24 * HOUR),
    ] {
        let report = Run::prog("probe")
            .session_dir(dir.path())
            .env("LIBTIMEWARP_SHIFT", spelling)
            .run();
        report.assert_wall_clock("probe", nanos);
    }
}

#[test]
fn with_no_shift_the_clock_is_the_genuine_one() {
    let dir = tempfile::tempdir().unwrap();
    Run::prog("probe")
        .session_dir(dir.path())
        .run()
        .assert_wall_clock("probe", 0);
}

#[test]
fn without_the_library_the_clock_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .without_library()
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    report.assert_wall_clock("probe", 0);
    assert!(session_files(dir.path()).is_empty());
}

#[test]
fn a_bad_shift_leaves_the_program_on_genuine_time() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "two hours ago")
        .run();

    report.assert_wall_clock("probe", 0);
}

#[test]
fn only_the_realtime_family_moves() {
    let dir = tempfile::tempdir().unwrap();
    let faked = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();
    let genuine = Run::prog("probe")
        .without_library()
        .session_dir(dir.path())
        .run();

    let drift = (faked.nanos("probe.monotonic") - genuine.nanos("probe.monotonic")).abs();
    assert!(
        drift < 10_000_000_000,
        "the monotonic clock moved by {drift} ns"
    );
}

#[test]
fn a_run_leaves_one_named_session_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    let files = session_files(dir.path());
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(
        files[0],
        format!(
            "{}.session",
            Path::new(report.get("probe.shared"))
                .file_stem()
                .unwrap()
                .to_string_lossy()
        )
    );
    assert!(
        files[0].starts_with(&format!("pid-{}-", report.get("probe.pid"))),
        "{files:?}"
    );
    assert!(!files[0].contains(".tmp"), "{files:?}");
}

#[test]
fn the_constructor_names_the_session_for_the_processes_below_it() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe").session_dir(dir.path()).run();

    let shared = report.get("probe.shared");
    assert!(!shared.is_empty(), "LIBTIMEWARP__SHARED_FILE was not set");
    assert!(Path::new(shared).exists(), "{shared} does not exist");
}

#[test]
fn a_fork_inherits_the_mapping() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("forker")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    report.assert_wall_clock("child", -2 * HOUR);
    report.assert_wall_clock("parent", -2 * HOUR);
    assert_ne!(report.get("child.pid"), report.get("parent.pid"));
    assert_eq!(report.get("child.shared"), report.get("parent.shared"));
    assert_eq!(session_files(dir.path()).len(), 1);
}

#[test]
fn an_exec_reattaches_by_the_inherited_path() {
    let dir = tempfile::tempdir().unwrap();
    let probe = harness::program("probe");
    let report = Run::prog("execer")
        .arg(probe.to_string_lossy())
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    report.assert_wall_clock("before", -2 * HOUR);
    report.assert_wall_clock("after", -2 * HOUR);
    assert_eq!(report.get("before.shared"), report.get("after.shared"));
    assert_eq!(
        session_files(dir.path()).len(),
        1,
        "the exec made a session of its own"
    );
}

#[test]
fn the_inherited_path_wins_over_the_rest_of_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let first = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    let second = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP__SHARED_FILE", first.get("probe.shared"))
        .env("LIBTIMEWARP_SHIFT", "5h")
        .run();

    second.assert_wall_clock("probe", -2 * HOUR);
    assert_eq!(
        session_files(dir.path()).len(),
        1,
        "the joiner made a session of its own"
    );
}

#[test]
fn an_empty_inherited_path_counts_as_no_session_to_join() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP__SHARED_FILE", "")
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    report.assert_wall_clock("probe", -2 * HOUR);
    assert_eq!(session_files(dir.path()).len(), 1);
}

#[test]
fn an_unusable_inherited_path_leaves_the_program_on_genuine_time() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env(
            "LIBTIMEWARP__SHARED_FILE",
            dir.path().join("gone.session").to_string_lossy(),
        )
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    report.assert_wall_clock("probe", 0);
    assert!(
        session_files(dir.path()).is_empty(),
        "a fresh session would fake what nobody asked for"
    );
}

#[test]
fn the_session_directory_follows_xdg_runtime_dir_when_no_dir_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .env("XDG_RUNTIME_DIR", dir.path().to_string_lossy())
        .run();

    assert_eq!(
        Path::new(report.get("probe.shared")).parent().unwrap(),
        dir.path().join("libtimewarp")
    );
}

#[test]
fn the_library_exports_the_interposed_names_and_nothing_else() {
    let output = std::process::Command::new("nm")
        .args(["-D", "--defined-only"])
        .arg(harness::library())
        .output()
        .expect("nm");
    assert!(output.status.success());

    let mut exported: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(2))
        .map(str::to_string)
        .collect();
    exported.sort();

    assert_eq!(
        exported,
        vec![
            "clock_gettime".to_owned(),
            "gettimeofday".to_owned(),
            "libtimewarp_genuine_clock_gettime".to_owned(),
            "time".to_owned(),
        ]
    );
}

#[test]
fn the_genuine_clock_stays_reachable_through_our_own_symbol() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::new(harness::program_linked_to_library("genuine"))
        .without_library()
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    let faked = report.nanos("faked");
    let genuine = report.nanos("genuine");
    assert!(
        (genuine - faked - 2 * HOUR).abs() < 1_000_000_000,
        "faked {faked}, genuine {genuine}"
    );
    assert!(
        (genuine - report.before).abs() < 10_000_000_000,
        "the genuine symbol was faked"
    );
}
