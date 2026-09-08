//! The narrow complete path: a shifted wall clock, seen by a preloaded program.
//!
//! `LD_PRELOAD` needs a platform arm that exports the interposed names, and the target needs one
//! that can read a genuine clock, so these cases only apply where both exist.
#![cfg(target_os = "linux")]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test support: a failed assumption should fail the test loudly"
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

/// One run of `families`, which reports every clock faked and genuine, twice, with real time
/// passing in between.
fn families(env: &[(&str, &str)]) -> harness::Report {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let mut run = Run::new(harness::program_linked_to_library("families"))
        .without_library()
        .session_dir(dir.path());
    for (name, value) in env {
        run = run.env(name, value);
    }
    run.run()
}

/// How far a clock moved between the two rounds, faked and genuine.
fn advance(report: &harness::Report, clock: &str) -> (i64, i64) {
    (
        report.nanos(&format!("b.{clock}")) - report.nanos(&format!("a.{clock}")),
        report.nanos(&format!("b.{clock}.genuine")) - report.nanos(&format!("a.{clock}.genuine")),
    )
}

/// Asserts that `clock` ran at `rate` times its own genuine passage.
fn assert_rate(report: &harness::Report, clock: &str, rate: i64) {
    let (faked, genuine) = advance(report, clock);
    let expected = genuine * rate;
    let slack = 50_000_000 + expected.abs() / 10;
    assert!(
        (faked - expected).abs() <= slack,
        "{clock} moved {faked} ns while its timebase moved {genuine} ns, wanted {expected} ns"
    );
}

#[test]
fn each_family_carries_its_own_line() {
    // The wall clock frozen while the monotonic clocks run at ten times speed, which is the
    // combination a single line could not express.
    let report = families(&[
        ("LIBTIMEWARP_REALTIME_RATE", "0"),
        ("LIBTIMEWARP_MONOTONIC_RATE", "10"),
        ("LIBTIMEWARP_BOOTTIME_RATE", "2"),
        ("LIBTIMEWARP_CPU_RATE", "4"),
    ]);

    assert_rate(&report, "realtime", 0);
    assert_rate(&report, "monotonic", 10);
    assert_rate(&report, "boottime", 2);
    assert_rate(&report, "process_cpu", 4);
}

#[test]
fn a_bare_rate_covers_elapsed_time_and_leaves_consumption_alone() {
    let report = families(&[("LIBTIMEWARP_RATE", "10")]);

    assert_rate(&report, "realtime", 10);
    assert_rate(&report, "monotonic", 10);
    assert_rate(&report, "boottime", 10);
    assert_rate(&report, "process_cpu", 1);
}

#[test]
fn every_clock_of_a_family_reads_the_family_value() {
    let report = families(&[("LIBTIMEWARP_RATE", "5"), ("LIBTIMEWARP_SHIFT", "-2h")]);

    for group in [
        ["realtime", "realtime_coarse", "realtime_alarm"],
        ["monotonic", "monotonic_coarse", "monotonic_raw"],
        ["boottime", "boottime_alarm", "boottime"],
    ] {
        let readings: Vec<i64> = group
            .iter()
            .map(|clock| report.nanos(&format!("a.{clock}")))
            .collect();
        let spread = readings.iter().max().unwrap() - readings.iter().min().unwrap();
        assert!(spread < 100_000_000, "{group:?} spread by {spread} ns");
    }
}

#[test]
fn the_cpu_family_scales_each_clock_on_its_own() {
    let report = families(&[("LIBTIMEWARP_CPU_RATE", "8")]);

    for clock in ["process_cpu", "thread_cpu"] {
        let faked = report.nanos(&format!("a.{clock}"));
        let genuine = report.nanos(&format!("a.{clock}.genuine"));
        assert!(genuine > 10_000_000, "{clock} consumed only {genuine} ns");
        // The faked reading is taken first, so it trails eight times the genuine one by whatever
        // the process consumed between the two calls.
        let expected = 8 * genuine;
        assert!(
            (faked - expected).abs() < expected / 100,
            "{clock} read {faked} against a genuine {genuine}"
        );
    }
}

#[test]
fn the_tai_clock_keeps_its_offset_from_the_wall_clock() {
    let report = families(&[("LIBTIMEWARP_TIME", "1990-01-01T00:00:00Z")]);

    let genuine_offset =
        report.nanos("a.tai.genuine") - report.nanos("a.realtime.genuine") + HOUR / 3_600 / 2;
    let faked_offset = report.nanos("a.tai") - report.nanos("a.realtime");
    assert!(
        (faked_offset - genuine_offset).abs() < 1_000_000_000,
        "faked TAI leads by {faked_offset} ns, genuine by {genuine_offset} ns"
    );
    // Today's offset whatever era the wall clock is set to, and the era here is 1990.
    assert!(report.nanos("a.realtime") < 700_000_000 * 1_000_000_000);
}

#[test]
fn an_absolute_time_starts_the_wall_clock_where_it_says() {
    let report = families(&[("LIBTIMEWARP_TIME", "2020-01-01T00:00:00Z")]);
    let start = 1_577_836_800i64 * 1_000_000_000;

    let reading = report.nanos("a.realtime");
    assert!(
        (start..start + 60 * 1_000_000_000).contains(&reading),
        "the wall clock read {reading}, wanted 2020-01-01"
    );
    // And it runs from there, rather than standing still.
    assert_rate(&report, "realtime", 1);
}

#[test]
fn the_unix_seconds_spelling_agrees_with_the_timestamp_one() {
    let stamped = families(&[("LIBTIMEWARP_TIME", "2020-01-01T00:00:00Z")]);
    let seconds = families(&[("LIBTIMEWARP_TIME", "@1577836800")]);

    let drift = (stamped.nanos("a.realtime") - seconds.nanos("a.realtime")).abs();
    assert!(
        drift < 60 * 1_000_000_000,
        "the two spellings differ by {drift} ns"
    );
}

#[test]
fn a_timebase_override_changes_passage_and_leaves_the_zero_alone() {
    let report = families(&[("LIBTIMEWARP_REALTIME_TIMEBASE", "CLOCK_BOOTTIME")]);

    // The zero still comes from the default timebase, so the wall clock still reads the wall clock.
    let reading = report.nanos("a.realtime");
    let genuine = report.nanos("a.realtime.genuine");
    assert!(
        (reading - genuine).abs() < 1_000_000_000,
        "the wall clock read {reading} against a genuine {genuine}"
    );
    // And passage now comes from a clock NTP cannot step, at the same speed.
    assert_rate(&report, "realtime", 1);
}

#[test]
fn a_monotonic_family_can_be_moved_as_long_as_it_stays_above_zero() {
    let report = families(&[("LIBTIMEWARP_BOOTTIME_SHIFT", "1h")]);

    let moved = report.nanos("a.boottime") - report.nanos("a.boottime.genuine");
    assert!(
        (moved - HOUR).abs() < 1_000_000_000,
        "boottime moved {moved} ns"
    );
    // The bare shift never reaches it, so the monotonic clocks stay where they are.
    let untouched = report.nanos("a.monotonic") - report.nanos("a.monotonic.genuine");
    assert!(
        untouched.abs() < 1_000_000_000,
        "monotonic moved {untouched} ns"
    );
}

#[test]
fn the_clocks_the_family_table_leaves_alone_read_genuine() {
    let report = families(&[("LIBTIMEWARP_RATE", "10"), ("LIBTIMEWARP_CPU_RATE", "8")]);

    let faked = report.nanos("own_cpuclockid");
    let genuine = report.nanos("own_cpuclockid.genuine");
    assert!(
        (faked - genuine).abs() < 100_000_000,
        "an id from clock_getcpuclockid read {faked} against a genuine {genuine}"
    );
}

/// Every bad value this ticket can produce leaves the program on genuine time.
#[test]
fn a_setting_the_library_cannot_honour_leaves_every_family_genuine() {
    for env in [
        // A shift that would put a nondecreasing family below zero.
        vec![("LIBTIMEWARP_MONOTONIC_SHIFT", "-100000d")],
        // A timebase name this platform does not have.
        vec![("LIBTIMEWARP_REALTIME_TIMEBASE", "CLOCK_YESTERDAY")],
        // A timebase the family may not track: a step in it would break monotonicity.
        vec![("LIBTIMEWARP_MONOTONIC_TIMEBASE", "CLOCK_REALTIME")],
        // A negative rate, and one past the ceiling the fixed point holds.
        vec![("LIBTIMEWARP_RATE", "-1")],
        vec![("LIBTIMEWARP_CPU_RATE", "5e9")],
        // A timestamp that does not parse, and one outside the range an instant holds.
        vec![("LIBTIMEWARP_TIME", "yesterday")],
        vec![("LIBTIMEWARP_TIME", "2300-01-01T00:00:00Z")],
        // The absolute time and a Realtime shift say the same thing twice.
        vec![
            ("LIBTIMEWARP_TIME", "2020-01-01T00:00:00Z"),
            ("LIBTIMEWARP_SHIFT", "-2h"),
        ],
    ] {
        let report = families(&env);
        for clock in ["realtime", "monotonic", "boottime", "process_cpu"] {
            let faked = report.nanos(&format!("a.{clock}"));
            let genuine = report.nanos(&format!("a.{clock}.genuine"));
            assert!(
                (faked - genuine).abs() < 1_000_000_000,
                "{env:?} left {clock} at {faked} against a genuine {genuine}"
            );
        }
        assert_ne!(
            report.get("shared"),
            "",
            "{env:?} left its children no session to attach to"
        );
    }
}
