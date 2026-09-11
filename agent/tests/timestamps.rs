//! File timestamps: what a `stat` shows for a file written just now, where a `touch` with an
//! explicit time lands, and that only the Realtime family has any say in either.
//!
//! The target preloads the library, the way a program under it ordinarily does, and reports the
//! faked wall clock either side of its readings so a case can ask whether the timestamps agree
//! with the clock without knowing what the clock was set to.
//!
//! The lists below are this platform's own: the `64` spellings and `statx` are Linux's, and the
//! birth time is inside the `stat` buffer wherever there is room for it.
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

use harness::{HOUR, Report, Run, Seen, calls_seen, only_session};

/// A second, the slack a comparison against the clock allows: a run takes milliseconds, and a rate
/// of ten makes milliseconds into tens of them.
const SECOND: i64 = 1_000_000_000;

/// The whole second, in the faked wall clock, every setter in the target asks for: the twelfth of
/// January 1970, far enough from any clock a case sets that a round trip landing there is no
/// coincidence.
const TARGET_SECONDS: i64 = 1_000_000;

/// The instant that is, in nanoseconds.
const TARGET: i64 = TARGET_SECONDS * SECOND;

/// Every reader of the group, by the key the target reports it under. `statx` is reported
/// separately, because it is the one with a birth time and the one a system may not have.
#[cfg(target_os = "linux")]
const READERS: &[&str] = &[
    "stat",
    "fstat",
    "lstat",
    "fstatat",
    "stat64",
    "fstat64",
    "lstat64",
    "fstatat64",
];

/// The same for FreeBSD, whose libc has no `64` spellings: its `off_t` has always been 64 bits.
#[cfg(target_os = "freebsd")]
const READERS: &[&str] = &["stat", "fstat", "lstat", "fstatat"];

/// The three timestamps every `stat` buffer carries.
const STAMPS: &[&str] = &["atime", "mtime", "ctime"];

/// Every setter of the group, by the key the target reports it under. `utime` is among them on both
/// platforms: FreeBSD reaches the conversion through the `utimes` its own libc calls.
const SETTERS: &[&str] = &[
    "utime",
    "utimes",
    "futimes",
    "lutimes",
    "utimensat",
    "futimens",
];

/// One run of the target in `dir`, under the settings a case wants.
fn stamps(dir: &Path, env: &[(&str, &str)]) -> Report {
    let mut run = Run::prog("stamps")
        .session_dir(dir)
        .arg(dir.to_string_lossy())
        .arg(TARGET_SECONDS.to_string());
    for (name, value) in env {
        run = run.env(name, value);
    }
    run.run()
}

/// Asserts that `value` lies in the window the target's own two clock readings bracket, widened by
/// a second at each end.
#[track_caller]
fn assert_agrees_with_the_clock(report: &Report, key: &str) {
    let low = report.nanos("now.before") - SECOND;
    let high = report.nanos("now.after") + SECOND;
    let value = report.nanos(key);
    assert!(
        (low..=high).contains(&value),
        "{key} = {value} is not in {low}..={high} (off by {} ms)",
        (value - low).min(high - value) / 1_000_000,
    );
}

/// Asserts that `value` is `wanted` to within `slack`.
#[track_caller]
fn assert_near(what: &str, value: i64, wanted: i64, slack: i64) {
    assert!(
        (value - wanted).abs() <= slack,
        "{what} = {value} is not {wanted} (off by {})",
        value - wanted
    );
}

/// Every timestamp of every reader, in one list, so a case can walk the whole group.
fn every_reading(report: &Report) -> Vec<String> {
    let mut keys: Vec<String> = READERS
        .iter()
        .flat_map(|reader| STAMPS.iter().map(move |stamp| format!("{reader}.{stamp}")))
        .collect();
    if birth_in_stat(report) {
        keys.extend(READERS.iter().map(|reader| format!("{reader}.btime")));
    }
    if report.nanos("statx.available") == 1 {
        keys.extend(STAMPS.iter().map(|stamp| format!("statx.{stamp}")));
        if report.nanos("statx.has_btime") == 1 {
            keys.push("statx.btime".to_owned());
        }
    }
    keys
}

/// Whether every `stat` buffer of this run carried a birth time, which the target reports because
/// it is the one field whose place differs between the platforms.
fn birth_in_stat(report: &Report) -> bool {
    report.nanos("birth.in_stat") == 1
}

#[test]
fn a_file_written_just_now_shows_a_modification_time_of_about_now() {
    for env in [
        vec![("LIBTIMEWARP_SHIFT", "-2h")],
        vec![("LIBTIMEWARP_SHIFT", "8760h")],
        vec![("LIBTIMEWARP_RATE", "10")],
        vec![("LIBTIMEWARP_RATE", "0")],
        vec![("LIBTIMEWARP_TIME", "2001-09-09T01:46:40Z")],
    ] {
        let dir = tempfile::tempdir().unwrap();
        let report = stamps(dir.path(), &env);
        for key in every_reading(&report) {
            assert_agrees_with_the_clock(&report, &key);
        }
    }
}

#[test]
fn the_timestamps_move_with_the_wall_clock_and_not_with_the_genuine_one() {
    let dir = tempfile::tempdir().unwrap();
    let report = stamps(dir.path(), &[("LIBTIMEWARP_SHIFT", "-2h")]);

    // Two hours back is far outside the genuine window the run happened in, so a timestamp that
    // had been left alone would land in it.
    for key in every_reading(&report) {
        let value = report.nanos(&key);
        assert!(
            value < report.before - HOUR,
            "{key} = {value} was not moved with the clock"
        );
        assert_near(&key, value, report.before - 2 * HOUR, 2 * SECOND);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn statx_carries_a_birth_time_faked_with_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let report = stamps(dir.path(), &[("LIBTIMEWARP_SHIFT", "-2h")]);

    assert_eq!(report.nanos("statx.available"), 1, "this system has statx");
    assert_eq!(
        report.nanos("statx.has_btime"),
        1,
        "this filesystem keeps a birth time"
    );
    assert_near(
        "statx.btime",
        report.nanos("statx.btime"),
        report.before - 2 * HOUR,
        2 * SECOND,
    );
}

/// The same where the birth time sits in the `stat` buffer itself, so every reader carries one and
/// there is no `statx` to ask.
#[cfg(target_os = "freebsd")]
#[test]
fn a_stat_buffer_carries_a_birth_time_faked_with_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let report = stamps(dir.path(), &[("LIBTIMEWARP_SHIFT", "-2h")]);

    assert!(birth_in_stat(&report), "this stat buffer has a birth time");
    for reader in READERS {
        let key = format!("{reader}.btime");
        assert_near(
            &key,
            report.nanos(&key),
            report.before - 2 * HOUR,
            2 * SECOND,
        );
    }
}

#[test]
fn a_touch_with_an_explicit_time_lands_where_the_program_meant_it() {
    // The slack is a second of the clock the case runs, because `utime` names whole seconds and
    // the second it truncates is a genuine one: a clock at ten times real speed loses ten of its
    // own for it. Every other setter names microseconds or better and lands far inside this.
    for (env, slack) in [
        (vec![("LIBTIMEWARP_SHIFT", "-2h")], SECOND),
        (vec![("LIBTIMEWARP_RATE", "10")], 10 * SECOND),
        (vec![("LIBTIMEWARP_RATE", "0.25")], SECOND),
        (vec![("LIBTIMEWARP_TIME", "2001-09-09T01:46:40Z")], SECOND),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let report = stamps(dir.path(), &env);
        assert_eq!(report.nanos("target"), TARGET);
        for setter in SETTERS {
            assert_eq!(report.nanos(&format!("{setter}.rc")), 0, "{setter} failed");
            for stamp in ["atime", "mtime"] {
                let key = format!("{setter}.{stamp}");
                assert_near(&key, report.nanos(&key), TARGET, slack);
            }
        }
    }
}

#[test]
fn a_frozen_clock_stores_the_genuine_now_and_reads_its_own_instant_back() {
    let dir = tempfile::tempdir().unwrap();
    let report = stamps(dir.path(), &[("LIBTIMEWARP_RATE", "0")]);

    // A clock that never advances puts every instant the program could name infinitely far away,
    // so a set lands at the genuine now and reads back as the one instant the clock shows.
    for setter in SETTERS {
        for stamp in ["atime", "mtime"] {
            let key = format!("{setter}.{stamp}");
            assert_agrees_with_the_clock(&report, &key);
        }
    }
}

#[test]
fn the_flags_that_name_no_instant_pass_through() {
    let dir = tempfile::tempdir().unwrap();
    let report = stamps(dir.path(), &[("LIBTIMEWARP_SHIFT", "-2h")]);

    assert_eq!(report.nanos("flags.rc"), 0);
    // UTIME_OMIT asked for no change, so the modification time is still the round trip before it.
    assert_near("flags.mtime", report.nanos("flags.mtime"), TARGET, SECOND);
    // UTIME_NOW asked for the current time, which is the clock's own now and not the instant the
    // file was carrying.
    let low = report.nanos("flags.before") - SECOND;
    let high = report.nanos("flags.after") + SECOND;
    let atime = report.nanos("flags.atime");
    assert!(
        (low..=high).contains(&atime),
        "flags.atime = {atime} is not in {low}..={high}"
    );
}

#[test]
fn a_clock_far_in_the_past_clamps_a_timestamp_at_zero() {
    let dir = tempfile::tempdir().unwrap();
    let report = stamps(dir.path(), &[("LIBTIMEWARP_TIME", "@0")]);

    // The wall clock is at the epoch and the file is older than it in the clock's own terms, which
    // would put every timestamp before zero. None of them reads below it.
    for key in every_reading(&report) {
        assert_eq!(report.nanos(&key), 0, "{key} was not clamped at zero");
    }
}

#[test]
fn only_the_realtime_family_reaches_a_file_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let report = stamps(
        dir.path(),
        &[
            ("LIBTIMEWARP_MONOTONIC_RATE", "10"),
            ("LIBTIMEWARP_BOOTTIME_RATE", "0"),
            ("LIBTIMEWARP_MONOTONIC_SHIFT", "1h"),
            ("LIBTIMEWARP_CPU_RATE", "8"),
        ],
    );

    // Every other family is moved and the wall clock is not, so every timestamp reads genuine.
    for key in every_reading(&report) {
        let value = report.nanos(&key);
        assert!(
            (report.before - SECOND..=report.after + SECOND).contains(&value),
            "{key} = {value} did not read genuine"
        );
    }
    // And a set lands where the program meant it, undisturbed by the other three.
    for setter in SETTERS {
        assert_near(
            &format!("{setter}.mtime"),
            report.nanos(&format!("{setter}.mtime")),
            TARGET,
            SECOND,
        );
    }
}

#[test]
fn a_null_session_leaves_every_timestamp_as_the_filesystem_wrote_it() {
    let dir = tempfile::tempdir().unwrap();
    let report = stamps(dir.path(), &[("LIBTIMEWARP_KEEPER", "null")]);

    for key in every_reading(&report) {
        let value = report.nanos(&key);
        assert!(
            (report.before - SECOND..=report.after + SECOND).contains(&value),
            "{key} = {value} did not read genuine"
        );
    }
    for setter in SETTERS {
        assert_near(
            &format!("{setter}.mtime"),
            report.nanos(&format!("{setter}.mtime")),
            TARGET,
            SECOND,
        );
    }
}

#[test]
fn both_instant_kinds_reach_the_hook_with_their_own_func() {
    let dir = tempfile::tempdir().unwrap();
    let hook = harness::hook_library("clock_group");
    let report = stamps(
        dir.path(),
        &[("LIBTIMEWARP_HOOK", &format!("{}@8192", hook.display()))],
    );

    let session = only_session(dir.path());
    let seen = calls_seen(&session);
    assert!(!seen.is_empty(), "the hook was never reached");

    // Every reader is a genuine instant becoming a fake one, and every setter is the reverse. The
    // `64` names report the function they are the large-file spelling of.
    let realtime = i64::from(libc::CLOCK_REALTIME);
    let mut expected = vec![
        ("stat", 400, 1),
        ("fstat", 401, 1),
        ("lstat", 402, 1),
        ("fstatat", 403, 1),
        ("utimes", 406, 2),
        ("futimes", 407, 2),
        ("lutimes", 408, 2),
        ("utimensat", 409, 2),
        ("futimens", 410, 2),
    ];
    if cfg!(target_os = "linux") {
        // Linux alone carries a birth time in a buffer of its own, and its libc reaches `utime`
        // over an internal name; FreeBSD's builds it out of the `utimes` above.
        assert_eq!(report.nanos("statx.available"), 1);
        expected.extend([("statx", 404, 1), ("utime", 405, 2)]);
    }
    for (what, func, kind) in expected {
        assert!(
            seen.contains(&Seen {
                func,
                kind,
                clock: realtime,
            }),
            "{what} did not reach the hook as function {func} in kind {kind}: {seen:?}"
        );
    }

    // A file timestamp is a wall-clock instant, so no call of the group names another clock.
    for call in &seen {
        if (400..=410).contains(&call.func) {
            assert_eq!(call.clock, realtime, "{call:?} names another clock");
        }
    }
}
