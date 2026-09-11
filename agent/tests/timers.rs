//! Timers: every timer the library takes over, measured against the genuine time it took to fire,
//! the family each one follows, what a query reports while a timer is armed, and what a change
//! landing after the arm reaches, which is nothing.
//!
//! The lists below are this platform's own, and a timer the platform does not have is named on
//! none of them rather than skipped quietly.
//!
//! The target is linked against the library rather than preloading it, which is how it reaches the
//! genuine-clock symbol and measures itself without the clock under test.
#![cfg(any(target_os = "linux", target_os = "freebsd"))]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "test support: a failed assumption should fail the test loudly, and not every helper \
              is used by every case"
)]
#![expect(
    clippy::significant_drop_tightening,
    reason = "a transaction is consumed by `commit`, which is where its lock is released; the lint \
              does not see through a method taking `self` and suggests a drop that would not \
              compile"
)]

mod harness;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use common::time::Family;
use controller::Controller;
use harness::{Report, Run, Seen, calls_seen, only_session};

const MILLI: i64 = 1_000_000;

/// Every timer measured against an elapsed-time family: the ones created on a monotonic clock, and
/// the absolute expiries read on one.
///
/// One family covers both, because `CLOCK_MONOTONIC` belongs to Monotonic through FreeBSD 14.x and
/// to Boottime from 15.0, so the cases below move the two together.
#[cfg(target_os = "linux")]
const MONOTONIC_TIMERS: &[&str] = &[
    "timer_settime",
    "timer_absolute",
    "timerfd_relative",
    "timerfd_absolute",
    "timerfd_first",
    "timerfd_interval",
];

/// The same for FreeBSD, which has no timerfd.
#[cfg(target_os = "freebsd")]
const MONOTONIC_TIMERS: &[&str] = &["timer_settime", "timer_absolute"];

/// Every timer measured against the Realtime family: `ITIMER_REAL`, `alarm`, and the ones created
/// on the wall clock.
#[cfg(target_os = "linux")]
const REALTIME_TIMERS: &[&str] = &[
    "setitimer_real",
    "alarm",
    "timer_realtime",
    "timerfd_realtime",
];

/// The same for FreeBSD, without the timerfd.
#[cfg(target_os = "freebsd")]
const REALTIME_TIMERS: &[&str] = &["setitimer_real", "alarm", "timer_realtime"];

/// Every timer measured against the Cpu family, which counts consumption rather than elapsed time.
const CPU_TIMERS: &[&str] = &["itimer_virtual"];

/// Every query answered in the units of an elapsed-time family.
#[cfg(target_os = "linux")]
const MONOTONIC_QUERIES: &[&str] = &["timer_gettime", "timerfd_gettime"];

/// The same for FreeBSD, without the timerfd.
#[cfg(target_os = "freebsd")]
const MONOTONIC_QUERIES: &[&str] = &["timer_gettime"];

/// Every query answered in the units of the Realtime family, the old value an arm reports included.
const REALTIME_QUERIES: &[&str] = &["setitimer_old", "getitimer_real", "alarm_remaining"];

/// The program that arms every timer for one fake duration and reports the genuine time each took
/// to fire. Linked with the library a system that has not folded the POSIX timers into libc keeps
/// them in.
fn timers_program() -> PathBuf {
    harness::program_linked_with("timers", &["-lrt"])
}

/// The two elapsed families set together, which is what the cases below move rather than Monotonic
/// alone: `CLOCK_MONOTONIC` belongs to one of them through FreeBSD 14.x and to the other from 15.0,
/// and a timer created on it follows whichever one it is in. The wall clock is untouched either
/// way, which is what those cases are about.
fn elapsed_rate(rate: &str) -> Vec<(&str, &str)> {
    vec![
        ("LIBTIMEWARP_MONOTONIC_RATE", rate),
        ("LIBTIMEWARP_BOOTTIME_RATE", rate),
    ]
}

/// One run of it, arming every timer for `base_millis` of faketime.
///
/// The CPU timer is left out unless the case asks for it, because it fires only once the run has
/// burned the consumption it was armed for, and only the cases that measure that are worth it.
fn timers(dir: &Path, base_millis: i64, env: &[(&str, &str)]) -> Report {
    run_timers(dir, base_millis, false, env)
}

/// The same, with the CPU timer.
fn timers_with_cpu(dir: &Path, base_millis: i64, env: &[(&str, &str)]) -> Report {
    run_timers(dir, base_millis, true, env)
}

/// The run itself, which the two above choose the CPU timer for.
fn run_timers(dir: &Path, base_millis: i64, cpu: bool, env: &[(&str, &str)]) -> Report {
    let mut run = Run::new(timers_program())
        .without_library()
        .session_dir(dir)
        .arg(base_millis.to_string())
        .arg(if cpu { "cpu" } else { "nocpu" });
    for (name, value) in env {
        run = run.env(name, value);
    }
    run.run()
}

/// Asserts that the timer under `key` took about `expected` genuine nanoseconds to fire, and says
/// whether it ran at all: a timer this system does not have reports nothing to measure.
///
/// The window is wide below and wider above, because a slow machine can only ever make a timer
/// later. It still has to exclude the duration the timer was armed for, which is what the cases
/// below choose their rates to keep clear of.
#[track_caller]
fn assert_fired(report: &Report, key: &str, expected: i64) -> bool {
    if report.nanos(&format!("{key}.available")) == 0 {
        return false;
    }
    let genuine = report.nanos(&format!("{key}.genuine"));
    let low = expected * 3 / 5;
    let high = expected * 2 + 60 * MILLI;
    assert!(
        (low..=high).contains(&genuine),
        "{key} fired after {} ms, which is not the {} ms it was armed for ({}..={} ms)",
        genuine / MILLI,
        expected / MILLI,
        low / MILLI,
        high / MILLI,
    );
    true
}

/// Asserts that the query under `key` reported about the fake duration the timer was armed for.
///
/// A query converts the genuine remaining back the way the arm converted, so the answer is the
/// duration the program asked for, less the little of it that has already run out. The window
/// excludes the genuine remaining itself, which is what a query that never converted would report.
#[track_caller]
fn assert_reported(report: &Report, key: &str) -> bool {
    if report.nanos(&format!("{key}.available")) == 0 {
        return false;
    }
    let fake = report.nanos(&format!("{key}.fake"));
    let remaining = report.nanos(&format!("{key}.remaining"));
    let low = fake / 3;
    let high = fake + 10 * MILLI;
    assert!(
        (low..=high).contains(&remaining),
        "{key} reported {} ms left of the {} ms it was armed for ({}..={} ms)",
        remaining / MILLI,
        fake / MILLI,
        low / MILLI,
        high / MILLI,
    );
    true
}

/// The genuine duration a fake one comes to at a rate of `numerator / denominator`.
fn owed(report: &Report, key: &str, numerator: i64, denominator: i64) -> i64 {
    report.nanos(&format!("{key}.fake")) * denominator / numerator
}

/// Asserts every timer of the run fired after what its own fake duration comes to at the rate the
/// family it follows is running, that every query answered in the program's own units, and that
/// every one of them was there to measure.
#[track_caller]
fn assert_every_timer(
    report: &Report,
    monotonic: (i64, i64),
    realtime: (i64, i64),
    cpu: Option<(i64, i64)>,
) {
    let mut groups = vec![(MONOTONIC_TIMERS, monotonic), (REALTIME_TIMERS, realtime)];
    if let Some(cpu) = cpu {
        groups.push((CPU_TIMERS, cpu));
    }

    let mut measured = 0;
    for (keys, (numerator, denominator)) in groups {
        for key in keys {
            if assert_fired(report, key, owed(report, key, numerator, denominator)) {
                measured += 1;
            }
        }
    }
    for keys in [MONOTONIC_QUERIES, REALTIME_QUERIES] {
        for key in keys {
            if assert_reported(report, key) {
                measured += 1;
            }
        }
    }
    let all = MONOTONIC_TIMERS.len()
        + REALTIME_TIMERS.len()
        + usize::from(cpu.is_some()) * CPU_TIMERS.len()
        + MONOTONIC_QUERIES.len()
        + REALTIME_QUERIES.len();
    assert_eq!(measured, all, "only {measured} timers ran of {all}");

    // A disarmed timer has nothing left, whatever the family is doing.
    assert_eq!(report.nanos("disarmed_real.remaining"), 0);
    assert_eq!(report.nanos("itimer_prof.armed"), 0);
}

#[test]
fn every_timer_fires_when_a_ten_times_clock_has_advanced_as_far_as_it_was_armed_for() {
    let dir = tempfile::tempdir().unwrap();
    // A bare rate covers Realtime, Monotonic and Boottime and leaves Cpu at real speed, so the CPU
    // timer of this run is the other half of the same rule.
    let report = timers_with_cpu(dir.path(), 500, &[("LIBTIMEWARP_RATE", "10")]);
    assert_every_timer(&report, (10, 1), (10, 1), Some((1, 1)));
}

#[test]
fn every_timer_stretches_when_the_clock_runs_slow() {
    let dir = tempfile::tempdir().unwrap();
    let report = timers(dir.path(), 50, &[("LIBTIMEWARP_RATE", "0.5")]);
    assert_every_timer(&report, (1, 2), (1, 2), None);
}

#[test]
fn without_a_session_every_timer_fires_after_exactly_what_it_was_armed_for() {
    let dir = tempfile::tempdir().unwrap();
    let report = timers(dir.path(), 50, &[("LIBTIMEWARP_KEEPER", "null")]);
    assert_every_timer(&report, (1, 1), (1, 1), None);
}

#[test]
fn a_monotonic_rate_reaches_the_timers_created_on_a_monotonic_clock_alone() {
    let dir = tempfile::tempdir().unwrap();
    let report = timers(dir.path(), 200, &elapsed_rate("20"));
    assert_every_timer(&report, (20, 1), (1, 1), None);
}

#[test]
fn a_wall_clock_rate_reaches_the_timers_that_follow_the_wall_clock_alone() {
    let dir = tempfile::tempdir().unwrap();
    let report = timers(dir.path(), 100, &[("LIBTIMEWARP_REALTIME_RATE", "20")]);
    assert_every_timer(&report, (1, 1), (20, 1), None);
}

#[test]
fn a_cpu_rate_reaches_the_timers_that_count_consumption_alone() {
    let dir = tempfile::tempdir().unwrap();
    let report = timers_with_cpu(dir.path(), 100, &[("LIBTIMEWARP_CPU_RATE", "10")]);
    assert_every_timer(&report, (1, 1), (1, 1), Some((10, 1)));
}

#[test]
fn a_sleep_and_a_timer_armed_together_still_fire_together() {
    let dir = tempfile::tempdir().unwrap();
    let report = timers(dir.path(), 500, &[("LIBTIMEWARP_RATE", "10")]);

    assert_eq!(report.nanos("together.available"), 1);
    let expected = report.nanos("together.fake") / 10;
    let sleep = report.nanos("together.sleep");
    let timer = report.nanos("together.timer");
    assert!(
        (expected * 3 / 5..=expected * 2 + 60 * MILLI).contains(&sleep),
        "the sleep woke after {} ms, not the {} ms it asked for",
        sleep / MILLI,
        expected / MILLI,
    );
    assert!(
        (timer - sleep).abs() <= 40 * MILLI,
        "the sleep woke after {} ms and the timer fired after {} ms, so the two parted",
        sleep / MILLI,
        timer / MILLI,
    );
}

/// Starts `timer_arm` on a fake duration and waits until it says the arm is behind it.
fn arming(
    dir: &Path,
    fake_millis: i64,
    env: &[(&str, &str)],
) -> (Child, BufReader<std::process::ChildStdout>, i64) {
    // Linked with the library a system that has not folded the POSIX timers into libc keeps them
    // in, which is what the target arms where there is no timerfd.
    let mut command = Command::new(harness::program_linked_with("timer_arm", &["-lrt"]));
    command
        .arg(fake_millis.to_string())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LIBTIMEWARP_SESSION_DIR", dir)
        .stdout(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn().unwrap();
    let mut printing = BufReader::new(child.stdout.take().unwrap());
    let mut disarmed = -1;
    let mut line = String::new();
    while !line.starts_with("armed=") {
        line.clear();
        assert!(
            printing.read_line(&mut line).unwrap() > 0,
            "the target left before it armed anything"
        );
        if let Some(value) = line.strip_prefix("disarmed=") {
            disarmed = value.trim().parse().unwrap();
        }
    }
    (child, printing, disarmed)
}

/// The one session a run made, as soon as it is there to attach to.
fn attach_to_session(dir: &Path) -> Controller {
    for _ in 0..1_000 {
        if let [path] = harness::session_files(dir).as_slice()
            && let Ok(controller) = Controller::attach(path)
        {
            return controller;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("no session appeared in {}", dir.display());
}

/// What the target reported when its timer fired, in genuine nanoseconds.
fn fired_after(child: &mut Child, printing: &mut BufReader<std::process::ChildStdout>) -> i64 {
    let mut line = String::new();
    while !line.starts_with("genuine=") {
        line.clear();
        assert!(
            printing.read_line(&mut line).unwrap() > 0,
            "the target left without reporting"
        );
    }
    assert!(child.wait().unwrap().success());
    line.trim_start_matches("genuine=").trim().parse().unwrap()
}

#[test]
fn a_rate_change_landing_after_the_arm_does_not_reach_the_timer() {
    let dir = tempfile::tempdir().unwrap();
    // One genuine second, at the rate the arm converted with.
    let (mut child, mut printing, _) = arming(dir.path(), 1_000, &[]);
    let controller = attach_to_session(dir.path());

    let mut transaction = controller.begin().unwrap();
    for family in [Family::Monotonic, Family::Boottime] {
        transaction.set_rate(family, 100.0).unwrap();
    }
    transaction.commit().unwrap();

    let genuine = fired_after(&mut child, &mut printing);
    assert!(
        (700 * MILLI..=2_500 * MILLI).contains(&genuine),
        "the timer fired after {} ms, so the rate change reached it",
        genuine / MILLI,
    );
}

#[test]
fn a_frozen_clock_holds_a_timer_and_thawing_does_not_release_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, _printing, disarmed) = arming(dir.path(), 50, &elapsed_rate("0"));

    // A disarm is a disarm under a freeze too: the timer with nothing left reports nothing left
    // rather than the longest duration there is.
    assert_eq!(disarmed, 0);

    // A frozen clock never advances, so the timer is owed the longest genuine duration there is.
    // Fifty milliseconds of faketime is what it was armed for, and this is many times that.
    let controller = attach_to_session(dir.path());
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert_eq!(child.try_wait().unwrap(), None, "the frozen timer fired");

    // And thawing does not reach it either: nothing re-plans a timer already armed. This is the
    // documented behaviour, so the case pins it rather than working around it.
    let mut transaction = controller.begin().unwrap();
    for family in [Family::Monotonic, Family::Boottime] {
        transaction.set_rate(family, 1.0).unwrap();
    }
    transaction.commit().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert_eq!(
        child.try_wait().unwrap(),
        None,
        "thawing released a timer the freeze had caught"
    );

    // Killing the process is the only exit, which is what the design says to expect.
    child.kill().unwrap();
    drop(child.wait());
}

/// Every arm of the timers group, the number the ABI gives it, and the clock the timer was created
/// on, which is the one the program named.
fn arm_calls() -> Vec<(&'static str, i64, i64)> {
    let monotonic = i64::from(libc::CLOCK_MONOTONIC);
    let realtime = i64::from(libc::CLOCK_REALTIME);
    let cpu = i64::from(libc::CLOCK_PROCESS_CPUTIME_ID);
    let mut calls = vec![
        ("setitimer on ITIMER_REAL", 300, realtime),
        ("setitimer on a CPU timer", 300, cpu),
        ("alarm", 302, realtime),
        ("timer_settime on a monotonic timer", 303, monotonic),
        ("timer_settime on a wall-clock timer", 303, realtime),
    ];
    if cfg!(target_os = "linux") {
        calls.extend([
            ("timerfd_settime on a monotonic timerfd", 305, monotonic),
            ("timerfd_settime on a wall-clock timerfd", 305, realtime),
        ]);
    }
    calls
}

/// Every query of the group, the same way.
fn query_calls() -> Vec<(&'static str, i64, i64)> {
    let monotonic = i64::from(libc::CLOCK_MONOTONIC);
    let realtime = i64::from(libc::CLOCK_REALTIME);
    let mut calls = vec![
        ("the old value setitimer reports", 300, realtime),
        ("getitimer", 301, realtime),
        ("the old value alarm reports", 302, realtime),
        ("timer_gettime", 304, monotonic),
    ];
    if cfg!(target_os = "linux") {
        calls.push(("timerfd_gettime", 306, monotonic));
    }
    calls
}

#[test]
fn every_timer_reaches_the_hook_as_an_arm_or_a_query_with_its_familys_clock() {
    let dir = tempfile::tempdir().unwrap();
    let hook = harness::hook_library("clock_group");
    let report = timers(
        dir.path(),
        20,
        &[("LIBTIMEWARP_HOOK", &format!("{}@65536", hook.display()))],
    );
    assert_ne!(report.get("shared"), "");

    let session = only_session(dir.path());
    let seen = calls_seen(&session);
    assert!(!seen.is_empty(), "the hook was never reached");

    // An arm is a fake duration becoming a genuine one, on the clock of the family the timer
    // follows, and a query is the same clock in the other direction.
    for (what, func, clock) in arm_calls() {
        assert!(
            seen.contains(&Seen {
                func,
                kind: 3,
                clock
            }),
            "{what} did not reach the hook as function {func} on clock {clock}"
        );
    }
    for (what, func, clock) in query_calls() {
        assert!(
            seen.contains(&Seen {
                func,
                kind: 4,
                clock
            }),
            "{what} did not reach the hook as function {func} on clock {clock}"
        );
    }

    // And nothing reached it in any other shape, or under any other name. The clock reads beside
    // the arms are the absolute expiries asking what the timer's clock shows now, and the sleep is
    // the one a timer is armed beside. `timer_create` and `timerfd_create` read no time, so the
    // ABI gives them no number and they can never arrive here at all.
    let known: Vec<i64> = (1..=8)
        .chain([100])
        .chain(arm_calls().iter().map(|&(_, func, _)| func))
        .chain(query_calls().iter().map(|&(_, func, _)| func))
        .collect();
    for call in &seen {
        assert!(
            [1, 3, 4].contains(&call.kind),
            "{call:?} is neither a clock read, an arm, nor a query"
        );
        assert!(
            known.contains(&call.func),
            "{call:?} is a function nothing in this group calls"
        );
    }
}
