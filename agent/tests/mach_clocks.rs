//! The Mach clocks: every counter, the two clock ports, the nanosecond read and the sleep measured
//! in ticks, each against the `clock_gettime` id that reads the same thing.
//!
//! macOS's alone. The target reads the pair inside one process, so a case here compares our round
//! trip through `mach_timebase_info` against the conversion the program itself makes, at whichever
//! ratio the machine running the suite has: 1:1 on Intel, 125:3 on Apple silicon.
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

use std::path::Path;

use harness::{HOUR, Report, Run, Seen, calls_seen, only_session};

/// Each Mach reading and the `clock_gettime` id beside it that reads the same counter.
const PAIRS: &[(&str, &str)] = &[
    ("absolute", "uptime_raw"),
    ("absolute", "uptime_raw_nsec"),
    ("approximate", "uptime_raw_approx"),
    ("continuous", "monotonic_raw"),
    ("continuous", "monotonic_raw_nsec"),
    ("continuous_approximate", "monotonic_raw_approx"),
    ("calendar", "realtime"),
    ("realtime_nsec", "realtime"),
    ("system", "uptime_raw"),
];

/// How far apart two readings of one counter may be and still be the same reading.
///
/// The two calls of a pair are adjacent, so what separates them is a `printf` and the rate the
/// family runs at. A tenth of a second covers both, and is far below the shifts and the advances
/// the cases below are about.
const SLACK: i64 = 100_000_000;

/// One run of `mach_clocks`, preloaded, which is how a program meets the library here.
fn mach_clocks(dir: &Path, env: &[(&str, &str)], wait_millis: Option<i64>) -> Report {
    let mut run = Run::prog("mach_clocks").session_dir(dir);
    if let Some(millis) = wait_millis {
        run = run.arg(millis.to_string());
    }
    for (name, value) in env {
        run = run.env(name, value);
    }
    let report = run.run();
    assert_eq!(
        report.nanos("genuine.available"),
        1,
        "the target could not reach the library's genuine clock"
    );
    assert!(
        report.nanos("timebase.numer") > 0 && report.nanos("timebase.denom") > 0,
        "the kernel answered no timebase"
    );
    for port in ["calendar", "system"] {
        assert_eq!(
            report.nanos(&format!("{port}.rc")),
            0,
            "the host handed out no {port} clock port"
        );
    }
    report
}

/// Asserts every Mach reading of `round` is the reading of the `clock_gettime` id beside it.
#[track_caller]
fn assert_pairs_agree(report: &Report, round: &str, slack: i64) {
    for (mach, beside) in PAIRS {
        let (ours, theirs) = (
            report.nanos(&format!("{round}.{mach}")),
            report.nanos(&format!("{round}.{beside}")),
        );
        assert!(ours > 0, "{round}.{mach} read {ours}");
        assert!(
            (ours - theirs).abs() <= slack,
            "{round}.{mach} read {ours} and {beside} read {theirs}, {} ms apart",
            (ours - theirs).abs() / 1_000_000
        );
    }
}

/// How far a reading moved between the two rounds.
fn advance(report: &Report, key: &str) -> i64 {
    report.nanos(&format!("b.{key}")) - report.nanos(&format!("a.{key}"))
}

/// Asserts a reading advanced `rate` times as far as the genuine counter under it did.
#[track_caller]
fn assert_rate(report: &Report, key: &str, rate: i64) {
    let genuine = advance(report, "uptime_raw.genuine");
    assert!(genuine > 0, "no genuine time passed between the two rounds");
    let wanted = genuine * rate;
    let slack = 50_000_000 + wanted.abs() / 5;
    let moved = advance(report, key);
    assert!(
        (moved - wanted).abs() <= slack,
        "{key} moved {moved} ns while the genuine counter moved {genuine} ns, wanted {wanted} ns"
    );
}

#[test]
fn a_shift_moves_every_mach_clock_with_the_id_beside_it() {
    let dir = tempfile::tempdir().unwrap();
    // One shift per family, because a bare shift is the wall clock's alone and the counters sit in
    // the other two.
    let report = mach_clocks(
        dir.path(),
        &[
            ("LIBTIMEWARP_SHIFT", "-2h"),
            ("LIBTIMEWARP_MONOTONIC_SHIFT", "1h"),
            ("LIBTIMEWARP_BOOTTIME_SHIFT", "2h"),
        ],
        None,
    );

    assert_pairs_agree(&report, "a", SLACK);
    assert_pairs_agree(&report, "b", SLACK);
    // And each family moved by the shift it was given, which is what leaves the pairs somewhere
    // the genuine counter is not.
    for (key, shift) in [
        ("a.realtime", -2 * HOUR),
        ("a.uptime_raw", HOUR),
        ("a.monotonic_raw", 2 * HOUR),
    ] {
        let genuine = report.nanos(&format!("{key}.genuine"));
        let moved = report.nanos(key) - genuine;
        assert!(
            (moved - shift).abs() <= SLACK,
            "{key} moved {moved} ns, wanted {shift} ns"
        );
    }
}

#[test]
fn a_rate_carries_every_mach_clock_with_the_id_beside_it() {
    let dir = tempfile::tempdir().unwrap();
    let report = mach_clocks(dir.path(), &[("LIBTIMEWARP_RATE", "10")], None);

    assert_pairs_agree(&report, "a", SLACK);
    assert_pairs_agree(&report, "b", SLACK);
    // Ten times the genuine counter, read through the ticks and through the id alike.
    for key in ["absolute", "uptime_raw", "continuous", "monotonic_raw"] {
        assert_rate(&report, key, 10);
    }
}

#[test]
fn a_frozen_family_freezes_the_mach_clocks_in_it() {
    let dir = tempfile::tempdir().unwrap();
    // No wait is asked for: a frozen family owes one the longest duration there is.
    let report = mach_clocks(dir.path(), &[("LIBTIMEWARP_RATE", "0")], None);

    assert_pairs_agree(&report, "a", SLACK);
    assert_pairs_agree(&report, "b", SLACK);
    for key in [
        "absolute",
        "uptime_raw",
        "continuous",
        "monotonic_raw",
        "realtime",
        "calendar",
    ] {
        let moved = advance(&report, key);
        assert_eq!(moved, 0, "{key} moved {moved} ns under a frozen clock");
    }
    // Genuine time did pass under it, so the freeze is ours and not the machine's.
    assert!(advance(&report, "uptime_raw.genuine") > 0);
}

#[test]
fn the_counter_that_stops_across_sleep_follows_monotonic_and_the_other_follows_boottime() {
    let dir = tempfile::tempdir().unwrap();
    // The two elapsed families at rates of their own, which is the one arrangement that tells
    // which family a counter is in rather than which counter it reads.
    let report = mach_clocks(
        dir.path(),
        &[
            ("LIBTIMEWARP_MONOTONIC_RATE", "10"),
            ("LIBTIMEWARP_BOOTTIME_RATE", "2"),
        ],
        None,
    );

    // `mach_absolute_time` and its approximate form stop while the machine sleeps, so they are the
    // Monotonic family's, with `CLOCK_UPTIME_RAW` beside them.
    for key in [
        "absolute",
        "approximate",
        "uptime_raw",
        "uptime_raw_nsec",
        "system",
    ] {
        assert_rate(&report, key, 10);
    }
    // `mach_continuous_time` and its approximate form count on, so they are Boottime's, with
    // `CLOCK_MONOTONIC_RAW` beside them.
    for key in [
        "continuous",
        "continuous_approximate",
        "monotonic_raw",
        "monotonic_raw_nsec",
    ] {
        assert_rate(&report, key, 2);
    }
}

#[test]
fn without_a_session_every_mach_clock_reads_the_genuine_counter() {
    let dir = tempfile::tempdir().unwrap();
    let report = mach_clocks(dir.path(), &[("LIBTIMEWARP_KEEPER", "null")], Some(50));

    // The approximate counters are read from the last tick either side, so the window is wider.
    assert_pairs_agree(&report, "a", 2 * SLACK);
    for key in ["absolute", "continuous", "system"] {
        let ours = report.nanos(&format!("a.{key}"));
        let genuine = report.nanos(if key == "continuous" {
            "a.monotonic_raw.genuine"
        } else {
            "a.uptime_raw.genuine"
        });
        assert!(
            (ours - genuine).abs() <= 2 * SLACK,
            "{key} read {ours} against a genuine {genuine}"
        );
    }
    assert!(
        (report.nanos("a.calendar") - report.nanos("a.realtime.genuine")).abs() <= 2 * SLACK,
        "the calendar clock was faked without a session"
    );
    // And the sleep in ticks blocked for exactly what it asked, which is the pass-through path.
    assert_eq!(report.nanos("wait.rc"), 0);
    let blocked = report.nanos("wait.genuine");
    assert!(
        (30_000_000..400_000_000).contains(&blocked),
        "the wait blocked {} ms for the 50 ms it asked",
        blocked / 1_000_000
    );
}

#[test]
fn a_sleep_in_ticks_wakes_when_the_clock_has_advanced_as_far_as_it_asked() {
    let dir = tempfile::tempdir().unwrap();
    // Half a fake second at ten times speed is a twentieth of a genuine one, which is a window no
    // unconverted wait could land in.
    let report = mach_clocks(dir.path(), &[("LIBTIMEWARP_RATE", "10")], Some(500));

    assert_eq!(report.nanos("wait.rc"), 0, "mach_wait_until refused");
    assert_eq!(report.nanos("wait.fake"), 500_000_000);
    let blocked = report.nanos("wait.genuine");
    assert!(
        (25_000_000..250_000_000).contains(&blocked),
        "the wait blocked {} ms, which is not the 50 ms a fake half-second comes to",
        blocked / 1_000_000
    );
}

#[test]
fn a_slow_clock_stretches_a_sleep_in_ticks() {
    let dir = tempfile::tempdir().unwrap();
    let report = mach_clocks(dir.path(), &[("LIBTIMEWARP_RATE", "0.5")], Some(50));

    assert_eq!(report.nanos("wait.rc"), 0);
    let blocked = report.nanos("wait.genuine");
    assert!(
        (60_000_000..500_000_000).contains(&blocked),
        "the wait blocked {} ms, which is not the 100 ms a half-speed 50 ms comes to",
        blocked / 1_000_000
    );
}

#[test]
fn every_mach_clock_reaches_the_hook_with_its_own_func() {
    let dir = tempfile::tempdir().unwrap();
    let hook = harness::hook_library("clock_group");
    let report = mach_clocks(
        dir.path(),
        &[("LIBTIMEWARP_HOOK", &format!("{}@4096", hook.display()))],
        Some(20),
    );
    assert_ne!(report.get("shared"), "");

    let seen = calls_seen(&only_session(dir.path()));
    assert!(!seen.is_empty(), "the hook was never reached");

    let realtime = i64::from(libc::CLOCK_REALTIME);
    let uptime_raw = i64::from(libc::CLOCK_UPTIME_RAW);
    let uptime_raw_approx = i64::from(libc::CLOCK_UPTIME_RAW_APPROX);
    let monotonic_raw = i64::from(libc::CLOCK_MONOTONIC_RAW);
    let monotonic_raw_approx = i64::from(libc::CLOCK_MONOTONIC_RAW_APPROX);
    for (what, func, kind, clock) in [
        ("mach_absolute_time", 9, 1, uptime_raw),
        ("mach_continuous_time", 10, 1, monotonic_raw),
        ("mach_approximate_time", 11, 1, uptime_raw_approx),
        (
            "mach_continuous_approximate_time",
            12,
            1,
            monotonic_raw_approx,
        ),
        ("clock_gettime_nsec_np on the wall clock", 13, 1, realtime),
        ("clock_gettime_nsec_np on the counter", 13, 1, uptime_raw),
        (
            "clock_gettime_nsec_np on the continuous counter",
            13,
            1,
            monotonic_raw,
        ),
        ("clock_get_time on the calendar port", 14, 1, realtime),
        ("clock_get_time on the system port", 14, 1, uptime_raw),
        // A deadline reduces to a duration, so the wait reaches the hook twice: once for the fake
        // clock the deadline is measured from, and once for the duration still to go.
        ("mach_wait_until", 105, 1, uptime_raw),
        ("mach_wait_until", 105, 3, uptime_raw),
    ] {
        assert!(
            seen.contains(&Seen { func, kind, clock }),
            "{what} did not reach the hook as function {func} of kind {kind} on clock {clock}: \
             {seen:?}"
        );
    }

    // Nothing else reached the hook. The one other function this target calls is `clock_gettime`
    // itself, and `host_get_clock_service` reached the hook as nothing at all: what it hands out is
    // a name, and the read on that name is the call above that converts.
    let named: &[i64] = &[1, 9, 10, 11, 12, 13, 14, 105];
    for call in &seen {
        assert!(
            named.contains(&call.func),
            "{call:?} is a function this target never calls"
        );
    }
}
