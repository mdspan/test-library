//! Sleeps and timeouts: every wait the library takes over on Linux, measured against the genuine
//! time it actually blocked, the family each one follows, what a wait cut short still owes, and
//! what a change landing mid-wait reaches, which is nothing.
//!
//! The target is linked against the library rather than preloading it, which is how it reaches the
//! genuine-clock symbol and measures itself without the clock under test.
#![cfg(target_os = "linux")]
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
use harness::{Report, Run, arena_words, only_session};

const MILLI: i64 = 1_000_000;

/// Every wait measured against the Monotonic family: the relative timeouts the kernel counts on
/// the monotonic clock, and the calls that were handed a monotonic clock of their own.
const MONOTONIC_WAITS: &[&str] = &[
    "nanosleep",
    "clock_nanosleep_relative",
    "clock_nanosleep_absolute",
    "sys_nanosleep",
    "sys_clock_nanosleep",
    "sleep",
    "usleep",
    "thrd_sleep",
    "poll",
    "ppoll",
    "select",
    "pselect",
    "epoll_wait",
    "epoll_pwait",
    "epoll_pwait2",
    "sigtimedwait",
    "aio_suspend",
    "recvmmsg",
    "pthread_cond_timedwait_monotonic",
    "pthread_cond_clockwait",
    "sem_clockwait",
];

/// Every wait measured against the Realtime family: an absolute deadline on the wall clock,
/// whether the call names that clock itself or takes it as the only one it has.
const REALTIME_WAITS: &[&str] = &[
    "clock_nanosleep_wall",
    "pthread_mutex_timedlock",
    "pthread_rwlock_timedrdlock",
    "pthread_rwlock_timedwrlock",
    "pthread_timedjoin_np",
    "mtx_timedlock",
    "sem_timedwait",
    "pthread_cond_timedwait",
    "pthread_cond_timedwait_after_destroy",
    "cnd_timedwait",
    "mq_timedreceive",
    "mq_timedsend",
];

/// The program that asks every wait for one fake duration and reports the genuine time each
/// blocked. Linked with the libraries the timed message queues and asynchronous reads need on a
/// system that has not folded them into libc.
fn waits_program() -> PathBuf {
    harness::program_linked_with("waits", &["-pthread", "-lrt"])
}

/// One run of it, asking every wait for `base_millis` of faketime.
fn waits(dir: &Path, base_millis: i64, env: &[(&str, &str)]) -> Report {
    let mut run = Run::new(waits_program())
        .without_library()
        .session_dir(dir)
        .arg(base_millis.to_string());
    for (name, value) in env {
        run = run.env(name, value);
    }
    run.run()
}

/// Asserts that the wait under `key` blocked for about `expected` genuine nanoseconds, and says
/// whether it ran at all: a wait this system does not have reports nothing to measure.
///
/// The window is wide below and wider above, because a slow machine can only ever make a wait
/// longer. It still has to exclude the duration the wait asked for, which is what the cases below
/// choose their rates to keep clear of.
#[track_caller]
fn assert_blocked(report: &Report, key: &str, expected: i64) -> bool {
    if report.nanos(&format!("{key}.available")) == 0 {
        return false;
    }
    let genuine = report.nanos(&format!("{key}.genuine"));
    let low = expected * 3 / 5;
    let high = expected * 2 + 60 * MILLI;
    assert!(
        (low..=high).contains(&genuine),
        "{key} blocked {} ms, which is not the {} ms it was owed ({}..={} ms)",
        genuine / MILLI,
        expected / MILLI,
        low / MILLI,
        high / MILLI,
    );
    true
}

/// The genuine duration a fake one comes to at a rate of `numerator / denominator`.
fn owed(report: &Report, key: &str, numerator: i64, denominator: i64) -> i64 {
    report.nanos(&format!("{key}.fake")) * denominator / numerator
}

/// Asserts every wait of the run blocked for what its own fake duration comes to at the rate the
/// family it follows is running, and that enough of them were there to measure.
#[track_caller]
fn assert_every_wait(report: &Report, monotonic: (i64, i64), realtime: (i64, i64)) {
    let mut measured = 0;
    for (keys, (numerator, denominator)) in
        [(MONOTONIC_WAITS, monotonic), (REALTIME_WAITS, realtime)]
    {
        for key in keys {
            if assert_blocked(report, key, owed(report, key, numerator, denominator)) {
                measured += 1;
            }
        }
    }
    // Only `epoll_pwait2`, `sem_clockwait` and `pthread_cond_clockwait` may be missing, and only
    // on a kernel or a libc older than the suite runs on.
    let least = MONOTONIC_WAITS.len() + REALTIME_WAITS.len() - 3;
    assert!(measured >= least, "only {measured} waits ran of {least}");
}

#[test]
fn every_wait_wakes_when_a_ten_times_clock_has_advanced_as_far_as_it_asked() {
    let dir = tempfile::tempdir().unwrap();
    let report = waits(dir.path(), 500, &[("LIBTIMEWARP_RATE", "10")]);
    assert_every_wait(&report, (10, 1), (10, 1));
}

#[test]
fn every_wait_stretches_when_the_clock_runs_slow() {
    let dir = tempfile::tempdir().unwrap();
    let report = waits(dir.path(), 50, &[("LIBTIMEWARP_RATE", "0.5")]);
    assert_every_wait(&report, (1, 2), (1, 2));
}

#[test]
fn without_a_session_every_wait_blocks_for_exactly_what_it_asked() {
    let dir = tempfile::tempdir().unwrap();
    let report = waits(dir.path(), 50, &[("LIBTIMEWARP_KEEPER", "null")]);
    assert_every_wait(&report, (1, 1), (1, 1));
}

#[test]
fn a_monotonic_rate_reaches_the_monotonic_waits_alone() {
    let dir = tempfile::tempdir().unwrap();
    let report = waits(dir.path(), 200, &[("LIBTIMEWARP_MONOTONIC_RATE", "20")]);
    assert_every_wait(&report, (20, 1), (1, 1));
}

#[test]
fn a_wall_clock_rate_reaches_the_waits_that_name_a_wall_clock_deadline_alone() {
    let dir = tempfile::tempdir().unwrap();
    let report = waits(dir.path(), 100, &[("LIBTIMEWARP_REALTIME_RATE", "20")]);
    assert_every_wait(&report, (1, 1), (20, 1));
}

#[test]
fn a_sleep_cut_short_reports_the_faketime_it_still_owes() {
    let dir = tempfile::tempdir().unwrap();
    let report = waits(dir.path(), 1_000, &[("LIBTIMEWARP_RATE", "10")]);

    assert_eq!(report.nanos("nanosleep_interrupted.available"), 1);
    assert_eq!(
        report.nanos("nanosleep_interrupted.rc"),
        -1,
        "the signal did not cut the sleep short"
    );
    assert_eq!(
        report.nanos("nanosleep_interrupted.errno"),
        i64::from(libc::EINTR)
    );

    // A fake second at ten times speed is a tenth of a genuine one, and the signal landed about
    // 20 ms into that. So the genuine remainder is around 80 ms, and the fake one the program is
    // owed is ten times that: a number no genuine remainder of this sleep could ever be.
    let remaining = report.nanos("nanosleep_interrupted.remaining");
    assert!(
        (300 * MILLI..=950 * MILLI).contains(&remaining),
        "the sleep reported {} ms left, which is not the faketime it owes",
        remaining / MILLI
    );
}

/// Starts `sleeper` on a fake duration and waits until it says the conversion is behind it.
fn sleeping(
    dir: &Path,
    fake_millis: i64,
    env: &[(&str, &str)],
) -> (Child, BufReader<std::process::ChildStdout>) {
    let mut command = Command::new(harness::program_linked_to_library("sleeper"));
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
    let mut line = String::new();
    while !line.starts_with("sleeping=") {
        line.clear();
        assert!(
            printing.read_line(&mut line).unwrap() > 0,
            "the sleeper left before it slept"
        );
    }
    (child, printing)
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

/// What the sleeper reported when it woke, in genuine nanoseconds.
fn slept_for(child: &mut Child, printing: &mut BufReader<std::process::ChildStdout>) -> i64 {
    let mut line = String::new();
    while !line.starts_with("genuine=") {
        line.clear();
        assert!(
            printing.read_line(&mut line).unwrap() > 0,
            "the sleeper left without reporting"
        );
    }
    assert!(child.wait().unwrap().success());
    line.trim_start_matches("genuine=").trim().parse().unwrap()
}

#[test]
fn a_rate_change_landing_mid_sleep_does_not_reach_the_sleep() {
    let dir = tempfile::tempdir().unwrap();
    // One genuine second, at the rate the sleep converted with.
    let (mut child, mut printing) = sleeping(dir.path(), 1_000, &[]);
    let controller = attach_to_session(dir.path());

    let mut transaction = controller.begin().unwrap();
    transaction.set_rate(Family::Monotonic, 100.0).unwrap();
    transaction.commit().unwrap();

    let genuine = slept_for(&mut child, &mut printing);
    assert!(
        (700 * MILLI..=2_500 * MILLI).contains(&genuine),
        "the sleep blocked {} ms, so the rate change reached it",
        genuine / MILLI
    );
}

#[test]
fn a_frozen_clock_holds_a_sleeper_and_thawing_does_not_release_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, _printing) = sleeping(dir.path(), 50, &[("LIBTIMEWARP_MONOTONIC_RATE", "0")]);
    let controller = attach_to_session(dir.path());

    // A frozen clock never advances, so the sleep is owed the longest genuine duration there is.
    // Fifty milliseconds of faketime is what it asked for, and this is many times that.
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert_eq!(
        child.try_wait().unwrap(),
        None,
        "the frozen sleeper woke up"
    );

    // And thawing does not reach it either: nothing re-plans a wait already under way. This is the
    // documented behaviour, so the case pins it rather than working around it.
    let mut transaction = controller.begin().unwrap();
    transaction.set_rate(Family::Monotonic, 1.0).unwrap();
    transaction.commit().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert_eq!(
        child.try_wait().unwrap(),
        None,
        "thawing released a sleeper the freeze had caught"
    );

    // Killing the process is the only exit, which is what the design says to expect.
    child.kill().unwrap();
    drop(child.wait());
}

/// What the hook recorded: one triple per call, in the order they arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    func: i64,
    kind: i64,
    clock: i64,
}

/// The log `clock_group.c` left in the arena.
fn calls_seen(session: &common::session::Session) -> Vec<Seen> {
    let count = arena_words(session, 1).first().copied().unwrap_or(0);
    let words = arena_words(session, 1 + usize::try_from(count).unwrap() * 3);
    words
        .get(1..)
        .unwrap_or_default()
        .chunks_exact(3)
        .filter_map(|triple| match *triple {
            [func, kind, clock] => Some(Seen { func, kind, clock }),
            _ => None,
        })
        .collect()
}

/// Every function of the sleeps and timeouts rows, the number the ABI gives it, and the clock of
/// the family it follows.
fn wait_funcs() -> Vec<(&'static str, i64, i64)> {
    let monotonic = i64::from(libc::CLOCK_MONOTONIC);
    let realtime = i64::from(libc::CLOCK_REALTIME);
    vec![
        ("nanosleep", 100, monotonic),
        ("sleep", 101, monotonic),
        ("usleep", 102, monotonic),
        ("thrd_sleep", 103, monotonic),
        ("clock_nanosleep on the monotonic clock", 104, monotonic),
        ("clock_nanosleep on the wall clock", 104, realtime),
        ("poll", 200, monotonic),
        ("ppoll", 201, monotonic),
        ("select", 202, monotonic),
        ("pselect", 203, monotonic),
        ("epoll_wait", 204, monotonic),
        ("epoll_pwait", 205, monotonic),
        ("pthread_cond_timedwait", 208, realtime),
        (
            "pthread_cond_timedwait on a monotonic variable",
            208,
            monotonic,
        ),
        ("pthread_mutex_timedlock", 210, realtime),
        ("pthread_rwlock_timedrdlock", 211, realtime),
        ("pthread_rwlock_timedwrlock", 212, realtime),
        ("pthread_timedjoin_np", 213, realtime),
        ("sem_timedwait", 214, realtime),
        ("sigtimedwait", 216, monotonic),
        ("aio_suspend", 217, monotonic),
        ("recvmmsg", 218, monotonic),
        ("cnd_timedwait", 219, realtime),
        ("mtx_timedlock", 220, realtime),
        ("mq_timedsend", 221, realtime),
        ("mq_timedreceive", 222, realtime),
    ]
}

/// The three a libc or a kernel older than this one may not have. Each is named by the key `waits`
/// reports it under, so a run that skipped one is not held to it.
fn optional_wait_funcs() -> Vec<(&'static str, i64, i64)> {
    let monotonic = i64::from(libc::CLOCK_MONOTONIC);
    vec![
        ("epoll_pwait2", 206, monotonic),
        ("pthread_cond_clockwait", 209, monotonic),
        ("sem_clockwait", 215, monotonic),
    ]
}

#[test]
fn every_wait_reaches_the_hook_as_a_fake_duration_with_its_familys_clock() {
    let dir = tempfile::tempdir().unwrap();
    let hook = harness::hook_library("clock_group");
    let report = waits(
        dir.path(),
        20,
        &[("LIBTIMEWARP_HOOK", &format!("{}@65536", hook.display()))],
    );
    assert_ne!(report.get("shared"), "");

    let session = only_session(dir.path());
    let seen = calls_seen(&session);
    assert!(!seen.is_empty(), "the hook was never reached");

    // The conversion is the same one for every wait: a fake duration becoming a genuine one, on
    // the clock of the family the wait follows.
    let optional = optional_wait_funcs();
    let asked = optional
        .iter()
        .filter(|&&(key, _, _)| report.nanos(&format!("{key}.available")) == 1);
    for &(what, func, clock) in wait_funcs().iter().chain(asked) {
        assert!(
            seen.contains(&Seen {
                func,
                kind: 3,
                clock
            }),
            "{what} did not reach the hook as function {func} on clock {clock}"
        );
    }

    // And nothing reached it in any other shape, or under any other name. The clock reads beside
    // the waits are the deadline conversions asking what the clock shows now. `pthread_cond_init`
    // and `pthread_cond_destroy` read no time, so the ABI gives them no number and they can never
    // arrive here at all.
    let known: Vec<i64> = (1..=8)
        .chain(wait_funcs().iter().map(|&(_, func, _)| func))
        .chain(optional.iter().map(|&(_, func, _)| func))
        .collect();
    for call in &seen {
        assert!(
            call.kind == 1 || call.kind == 3,
            "{call:?} is neither a clock read nor a wait"
        );
        assert!(
            known.contains(&call.func),
            "{call:?} is a function nothing in this group calls"
        );
    }
}

#[test]
fn a_wait_the_family_table_leaves_alone_blocks_for_what_it_asked() {
    let dir = tempfile::tempdir().unwrap();
    // Every family ten times faster, so a wait that followed any of them would be a tenth as long.
    // `nanosleep` follows Monotonic, and this checks the other side of the same rule: the run
    // above with no session at all leaves every wait exactly as the program wrote it.
    let faked = waits(dir.path(), 200, &[("LIBTIMEWARP_RATE", "10")]);
    assert_blocked(&faked, "nanosleep", 20 * MILLI);

    let plain = waits(dir.path(), 200, &[("LIBTIMEWARP_KEEPER", "null")]);
    assert_blocked(&plain, "nanosleep", 200 * MILLI);
}

#[test]
fn a_wait_asked_for_nothing_answers_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let report = waits(dir.path(), 0, &[("LIBTIMEWARP_RATE", "10")]);

    for key in MONOTONIC_WAITS.iter().chain(REALTIME_WAITS) {
        if *key == "sleep" || report.nanos(&format!("{key}.available")) == 0 {
            continue;
        }
        let genuine = report.nanos(&format!("{key}.genuine"));
        assert!(
            genuine < 500 * MILLI,
            "{key} blocked {} ms for a wait of nothing",
            genuine / MILLI
        );
    }
}
