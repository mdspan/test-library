//! Sleeps and timeouts: every wait the library takes over, measured against the genuine time it
//! actually blocked, the family each one follows, what a wait cut short still owes, and what a
//! change landing mid-wait reaches, which is nothing.
//!
//! The lists below are this platform's own, and a wait the platform does not have is named on
//! neither of them rather than skipped quietly.
//!
//! The target is linked against the library rather than preloading it, which is how it reaches the
//! genuine-clock symbol and measures itself without the clock under test.
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

use common::hook::{Func, Kind};
use common::time::Family;
use controller::Controller;
use harness::{Report, Run, Seen, calls_seen, only_session};

const MILLI: i64 = 1_000_000;

/// Every wait measured against an elapsed-time family: the relative timeouts the kernel counts on
/// the monotonic clock, and the calls that were handed a monotonic clock of their own.
///
/// One family covers both, because `CLOCK_MONOTONIC` belongs to Monotonic through FreeBSD 14.x and
/// to Boottime from 15.0, so the cases below move the two together.
#[cfg(target_os = "linux")]
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

/// The same for FreeBSD, which has no `epoll`, no `clockwait` pair and no call by number, and
/// answers a set of events with `kevent` instead.
#[cfg(target_os = "freebsd")]
const MONOTONIC_WAITS: &[&str] = &[
    "nanosleep",
    "clock_nanosleep_relative",
    "clock_nanosleep_absolute",
    "sleep",
    "usleep",
    "thrd_sleep",
    "poll",
    "ppoll",
    "select",
    "pselect",
    "kevent",
    "sigtimedwait",
    "aio_suspend",
    "recvmmsg",
    "pthread_cond_timedwait_monotonic",
];

/// The same for macOS, which has none of `clock_nanosleep`, `ppoll`, `thrd_sleep`, `sigtimedwait`
/// or `recvmmsg` either, and carries a wider form of `kevent` beside the plain one.
#[cfg(target_os = "macos")]
const MONOTONIC_WAITS: &[&str] = &[
    "nanosleep",
    "sleep",
    "usleep",
    "poll",
    "select",
    "pselect",
    "kevent",
    "kevent64",
    "aio_suspend",
];

/// Every wait measured against the Realtime family: an absolute deadline on the wall clock,
/// whether the call names that clock itself or takes it as the only one it has.
#[cfg(not(target_os = "macos"))]
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

/// The same for macOS, whose one wait with a wall-clock deadline is the condition variable: it has
/// no timed lock, no timed semaphore, no message queue and no `clock_nanosleep`.
#[cfg(target_os = "macos")]
const REALTIME_WAITS: &[&str] = &["pthread_cond_timedwait"];

/// The libraries the target needs beyond libc: the timed message queues and the asynchronous reads
/// live in `librt` on a system that has not folded them into libc, and FreeBSD keeps the C11
/// threads calls in a library of their own.
#[cfg(target_os = "linux")]
const EXTRA_LIBRARIES: &[&str] = &["-pthread", "-lrt"];
#[cfg(target_os = "freebsd")]
const EXTRA_LIBRARIES: &[&str] = &["-pthread", "-lrt", "-lstdthreads"];
/// macOS has neither `librt` nor a C11 threads library, and its threads are in libSystem.
#[cfg(target_os = "macos")]
const EXTRA_LIBRARIES: &[&str] = &["-pthread"];

/// The program that asks every wait for one fake duration and reports the genuine time each
/// blocked.
fn waits_program() -> PathBuf {
    harness::program_linked_with("waits", EXTRA_LIBRARIES)
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

/// Asserts that the wait under `key` blocked for about `expected` genuine nanoseconds.
///
/// The window is wide below and wider above, because a slow machine can only ever make a wait
/// longer, and so can the kernel's leeway. It still has to exclude the duration the wait asked
/// for, which is what the cases below choose their rates and durations to keep clear of.
#[track_caller]
fn assert_blocked(report: &Report, key: &str, expected: i64) {
    let genuine = report.nanos(&format!("{key}.genuine"));
    let low = expected * 3 / 5;
    let high = expected * 2 + 60 * MILLI + harness::TIMER_LEEWAY;
    assert!(
        (low..=high).contains(&genuine),
        "{key} blocked {} ms, which is not the {} ms it was owed ({}..={} ms)",
        genuine / MILLI,
        expected / MILLI,
        low / MILLI,
        high / MILLI,
    );
}

/// How many of the waits above a run may find missing.
///
/// On Linux `epoll_pwait2`, `sem_clockwait` and `pthread_cond_clockwait`, each on a kernel or a
/// libc older than the suite runs on. On FreeBSD the two timed message queues, which need the
/// `mqueuefs` module loaded, and the asynchronous read, which a kernel may have turned off. On
/// macOS the asynchronous read alone, for the same reason.
#[cfg(not(target_os = "macos"))]
const MAY_BE_MISSING: usize = 3;
#[cfg(target_os = "macos")]
const MAY_BE_MISSING: usize = 1;

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
            if report.nanos(&format!("{key}.available")) == 0 {
                continue;
            }
            assert_blocked(report, key, owed(report, key, numerator, denominator));
            measured += 1;
        }
    }
    let least = MONOTONIC_WAITS.len() + REALTIME_WAITS.len() - MAY_BE_MISSING;
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
    // The rate goes on both elapsed families, because `CLOCK_MONOTONIC` belongs to one of them
    // through FreeBSD 14.x and to the other from 15.0, and the waits the kernel measures against
    // its own elapsed clock follow whichever one it left. The wall clock is untouched either way,
    // which is what this case is about.
    let report = waits(
        dir.path(),
        200,
        &[
            ("LIBTIMEWARP_MONOTONIC_RATE", "20"),
            ("LIBTIMEWARP_BOOTTIME_RATE", "20"),
        ],
    );
    assert_every_wait(&report, (20, 1), (1, 1));
}

#[test]
fn a_wall_clock_rate_reaches_the_waits_that_name_a_wall_clock_deadline_alone() {
    let dir = tempfile::tempdir().unwrap();
    // Two hundred fake milliseconds are ten genuine ones on the wall clock, whose window, the
    // kernel's leeway included, still ends short of the two hundred an unconverted wait blocks.
    let report = waits(dir.path(), 200, &[("LIBTIMEWARP_REALTIME_RATE", "20")]);
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

/// Blocks until the sleeper's `nanosleep` has reached the hook as a duration to convert.
///
/// `sleeping=1` goes out before the call, so it says nothing about whether the conversion has
/// happened yet; the hook sees the conversion itself. Without this wait a change can land on a
/// sleep that has not converted, which measures the new rate rather than the question being asked.
fn wait_for_the_conversion(dir: &Path) {
    let session = only_session(dir);
    for _ in 0..2_000 {
        if calls_seen(&session).iter().any(|seen| {
            seen.func == i64::from(Func::Nanosleep as u32)
                && seen.kind == i64::from(Kind::DurationToGenuine as u32)
        }) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("the sleeper never converted its sleep");
}

#[test]
fn a_rate_change_landing_mid_sleep_does_not_reach_the_sleep() {
    let dir = tempfile::tempdir().unwrap();
    let hook = harness::hook_library("clock_group");
    // One genuine second, at the rate the sleep converted with.
    let (mut child, mut printing) = sleeping(
        dir.path(),
        1_000,
        &[("LIBTIMEWARP_HOOK", &format!("{}@65536", hook.display()))],
    );
    let controller = attach_to_session(dir.path());
    wait_for_the_conversion(dir.path());

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

/// Every function of the sleeps and timeouts rows Linux and FreeBSD both have, the number the ABI
/// gives it, and the clock of the family it follows.
///
/// A wait the kernel measures against its own elapsed clock carries the clock the Monotonic family
/// tracks here; a wait handed a clock of its own carries the one the program named. The two are
/// `CLOCK_MONOTONIC` alike on Linux, and part on FreeBSD from 15.0.
#[cfg(not(target_os = "macos"))]
fn shared_wait_funcs() -> Vec<(&'static str, i64, i64)> {
    let elapsed = i64::from(common::platform::monotonic_timebase());
    let named = i64::from(libc::CLOCK_MONOTONIC);
    let realtime = i64::from(libc::CLOCK_REALTIME);
    vec![
        ("nanosleep", 100, elapsed),
        ("sleep", 101, elapsed),
        ("usleep", 102, elapsed),
        ("clock_nanosleep on the monotonic clock", 104, named),
        ("clock_nanosleep on the wall clock", 104, realtime),
        ("poll", 200, elapsed),
        ("ppoll", 201, elapsed),
        ("select", 202, elapsed),
        ("pselect", 203, elapsed),
        ("pthread_cond_timedwait", 208, realtime),
        ("pthread_cond_timedwait on a monotonic variable", 208, named),
        ("pthread_mutex_timedlock", 210, realtime),
        ("pthread_rwlock_timedrdlock", 211, realtime),
        ("pthread_rwlock_timedwrlock", 212, realtime),
        ("pthread_timedjoin_np", 213, realtime),
        ("sem_timedwait", 214, realtime),
        ("sigtimedwait", 216, elapsed),
    ]
}

/// The few of that set macOS has. It has no `clock_nanosleep`, no timed lock, no timed semaphore
/// and no message queue, and its condition variable reads every deadline on the wall clock.
#[cfg(target_os = "macos")]
fn shared_wait_funcs() -> Vec<(&'static str, i64, i64)> {
    let elapsed = i64::from(common::platform::monotonic_timebase());
    let realtime = i64::from(libc::CLOCK_REALTIME);
    vec![
        ("nanosleep", 100, elapsed),
        ("sleep", 101, elapsed),
        ("usleep", 102, elapsed),
        ("poll", 200, elapsed),
        ("select", 202, elapsed),
        ("pselect", 203, elapsed),
        ("pthread_cond_timedwait", 208, realtime),
    ]
}

/// The ones only Linux has, plus the four this platform reaches under their own names: glibc keeps
/// `thrd_sleep`, `cnd_timedwait`, `mtx_timedlock` and `recvmmsg` inside libc, over names no loader
/// can reach, so each arrives at the hook as itself.
#[cfg(target_os = "linux")]
fn own_wait_funcs() -> Vec<(&'static str, i64, i64)> {
    let monotonic = i64::from(libc::CLOCK_MONOTONIC);
    let realtime = i64::from(libc::CLOCK_REALTIME);
    vec![
        ("thrd_sleep", 103, monotonic),
        ("epoll_wait", 204, monotonic),
        ("epoll_pwait", 205, monotonic),
        ("aio_suspend", 217, monotonic),
        ("recvmmsg", 218, monotonic),
        ("cnd_timedwait", 219, realtime),
        ("mtx_timedlock", 220, realtime),
        ("mq_timedsend", 221, realtime),
        ("mq_timedreceive", 222, realtime),
    ]
}

/// The one only FreeBSD has. Its `thrd_sleep`, `cnd_timedwait`, `mtx_timedlock` and `recvmmsg` are
/// wrappers over waits already on the shared list, so each arrives at the hook under the name it
/// delegates to and none of them is here.
#[cfg(target_os = "freebsd")]
fn own_wait_funcs() -> Vec<(&'static str, i64, i64)> {
    vec![(
        "kevent",
        207,
        i64::from(common::platform::monotonic_timebase()),
    )]
}

/// The two only macOS has: the same wait on a set of events, and the wider form beside it. Its
/// `kevent_qos` names no deadline, so nothing of it ever reaches the hook.
#[cfg(target_os = "macos")]
fn own_wait_funcs() -> Vec<(&'static str, i64, i64)> {
    let elapsed = i64::from(common::platform::monotonic_timebase());
    vec![("kevent", 207, elapsed), ("kevent64", 223, elapsed)]
}

/// Every function of the two rows this platform has.
fn wait_funcs() -> Vec<(&'static str, i64, i64)> {
    let mut funcs = shared_wait_funcs();
    funcs.extend(own_wait_funcs());
    funcs
}

/// The ones a run may find missing. Each is named by the key `waits` reports it under, so a run
/// that skipped one is not held to it.
#[cfg(target_os = "linux")]
fn optional_wait_funcs() -> Vec<(&'static str, i64, i64)> {
    let monotonic = i64::from(libc::CLOCK_MONOTONIC);
    vec![
        ("epoll_pwait2", 206, monotonic),
        ("pthread_cond_clockwait", 209, monotonic),
        ("sem_clockwait", 215, monotonic),
    ]
}

/// The same here: the asynchronous read a kernel may have turned off, and the timed message queues,
/// which need the `mqueuefs` module loaded.
#[cfg(target_os = "freebsd")]
fn optional_wait_funcs() -> Vec<(&'static str, i64, i64)> {
    let realtime = i64::from(libc::CLOCK_REALTIME);
    vec![
        (
            "aio_suspend",
            217,
            i64::from(common::platform::monotonic_timebase()),
        ),
        ("mq_timedsend", 221, realtime),
        ("mq_timedreceive", 222, realtime),
    ]
}

/// The asynchronous read alone here, which this kernel may refuse on the descriptor the target
/// opens.
#[cfg(target_os = "macos")]
fn optional_wait_funcs() -> Vec<(&'static str, i64, i64)> {
    vec![(
        "aio_suspend",
        217,
        i64::from(common::platform::monotonic_timebase()),
    )]
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
    // Every family twenty times faster, so a wait that followed any of them would be a twentieth
    // as long. `nanosleep` follows Monotonic, and this checks the other side of the same rule: the
    // run above with no session at all leaves every wait exactly as the program wrote it.
    let faked = waits(dir.path(), 200, &[("LIBTIMEWARP_RATE", "20")]);
    assert_blocked(&faked, "nanosleep", 10 * MILLI);

    let plain = waits(dir.path(), 200, &[("LIBTIMEWARP_KEEPER", "null")]);
    assert_blocked(&plain, "nanosleep", 200 * MILLI);
}

/// Asserts every wait of a run asked for nothing answered at once.
#[track_caller]
fn assert_answered_at_once(report: &Report) {
    for key in MONOTONIC_WAITS.iter().chain(REALTIME_WAITS) {
        if report.nanos(&format!("{key}.available")) == 0 {
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

#[test]
fn a_wait_asked_for_nothing_answers_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let report = waits(dir.path(), 0, &[("LIBTIMEWARP_RATE", "10")]);
    assert_answered_at_once(&report);
}

#[test]
fn a_wait_asked_for_nothing_answers_at_once_on_a_frozen_clock() {
    let dir = tempfile::tempdir().unwrap();
    // Every family the waits follow is frozen. A zero timeout asks for no advance at all, and an
    // absolute deadline at what the clock shows is already reached, so neither may block.
    let report = Run::new(waits_program())
        .without_library()
        .session_dir(dir.path())
        .arg("0")
        .env("LIBTIMEWARP_RATE", "0")
        .within(std::time::Duration::from_secs(30))
        .run();
    assert_answered_at_once(&report);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_condition_variable_among_more_live_ones_than_the_library_keeps_times_out_on_its_own_clock() {
    // Below the table's size and past it, where a variable made last has no clock recorded, and
    // neither has one built by the static initialiser, which reads the wall clock.
    for count in [300, harness::PAST_THE_TABLE] {
        for (rate, env) in harness::object_settings() {
            let report = harness::clock_objects("cond_crowd", count, &env);
            harness::assert_on_time(&report, "cond_crowd", harness::OBJECT_MILLIS * MILLI / rate);
            harness::assert_on_time(&report, "cond_crowd_wall", harness::OBJECT_MILLIS * MILLI);
        }
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_condition_variable_given_its_clock_again_and_then_the_default_times_out_on_the_wall_clock() {
    // Most of the table, so the last variables made sit past slots that others gave up when they
    // went back to the wall clock, ahead of their own on their walks. Where the families run
    // apart, a wall-clock deadline read on the monotonic clock comes due at once.
    for (_, env) in harness::object_settings() {
        let report = harness::clock_objects("cond_reinit", 3_000, &env);
        let waits = report.nanos("cond_reinit.waits");
        assert!(waits > 0, "no wait was made\n{}", report.stdout);
        assert_eq!(
            report.nanos("cond_reinit.on_time"),
            waits,
            "a wall-clock deadline was read on another clock\n{}",
            report.stdout
        );
    }
}
