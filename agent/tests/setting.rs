//! A target that sets the time: the clock moves for every process of the session, and a program
//! that probes for privilege by calling a setter sees exactly what it would have seen without us.
//!
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "test support: a failed assumption should fail the test loudly, and not every helper \
              is used by every case"
)]
#![expect(
    clippy::print_stderr,
    reason = "a case that cannot get the capability it wanted says so, rather than passing quietly"
)]

mod harness;

use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

use common::hook::{Func, Kind};
use common::platform::{clock, may_set_the_clock};
use common::session::Session;
use harness::{Report, Run, arena_words, only_session};

const SECOND: i64 = 1_000_000_000;
const DAY: i64 = 86_400 * SECOND;

/// The instant every case here asks the wall clock to show: 2020-01-01T00:00:00Z. Far enough from
/// today that a clock sitting at it cannot be one that never moved.
const TARGET: i64 = 1_577_836_800 * SECOND;

/// How far the one adjustment that is meant to land moves the clock, and how far ahead of `TARGET`
/// the last setter puts it. Both are the numbers `setters.c` uses, checked against it on every run.
const ADJUST_S: i64 = 600;
const LAST_S: i64 = 7_200;

/// How close a reading has to be to count as the same instant. A second, which is longer than any
/// of these runs.
const SLACK: i64 = SECOND;

/// The arena a hook logging every call is given, and how many words each of its entries takes.
const ARENA: u64 = 4096;
const SETTER_ENTRY: usize = 4;
const DECLINED_ENTRY: usize = 3;

/// A command that execs the target with the privilege the kernel asks for before it lets a process
/// set the clock, or `None` when this machine will not hand it out.
///
/// On Linux that is `CAP_SYS_TIME` rather than root, and an ordinary user holds every capability
/// inside a user namespace of their own; a machine with unprivileged user namespaces turned off,
/// running as a user who holds nothing, leaves the privileged half of these cases without a way in.
/// FreeBSD gates the call on effective uid 0, which no wrapper can grant, so there a run that is
/// not already root checks the refusal instead.
fn with_the_capability() -> Option<Vec<String>> {
    if may_set_the_clock() {
        return Some(Vec::new());
    }
    #[cfg(target_os = "linux")]
    {
        let works = Command::new("unshare")
            .args(["-U", "-r", "true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        works.then(|| ["unshare", "-U", "-r"].map(str::to_owned).to_vec())
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// The same, with a line saying what a machine that will not hand the capability out leaves
/// unexercised.
fn capability_or_note(what: &str) -> Vec<String> {
    with_the_capability().unwrap_or_else(|| {
        eprintln!(
            "this machine grants no CAP_SYS_TIME and no user namespace to take it in, so {what} \
             runs as an unprivileged process and checks the refusal instead"
        );
        Vec::new()
    })
}

/// Whether a run that reaches the platform's own setters may pass a null pointer to them.
///
/// Linux and FreeBSD make each of these a call into the kernel, which answers `EFAULT` for a
/// pointer it cannot read and takes a null delta as the question it is. macOS builds
/// `clock_settime` out of `settimeofday` inside libc and reads the pointer on the way, so a
/// genuine run there would fault where the other two are refused. Under the library the answer is
/// ours on all three, so every faked run makes them.
const GENUINE_NULLS: bool = !cfg!(target_os = "macos");

/// One run of the program that tries every setter, under whatever wrapper and environment a case
/// wants. The library answers the calls that pass a null pointer, so a faked run makes them.
fn setters(dir: &Path, wrapper: &[String], env: &[(&str, &str)]) -> Report {
    run_setters(dir, wrapper, env, true)
}

/// The run itself, which decides whether the calls that pass a null pointer are made at all.
fn run_setters(dir: &Path, wrapper: &[String], env: &[(&str, &str)], nulls: bool) -> Report {
    let mut run = Run::prog("setters")
        .session_dir(dir)
        .through(wrapper)
        .arg(TARGET.to_string())
        .arg(if nulls { "nulls" } else { "nonulls" });
    for (name, value) in env {
        run = run.env(name, value);
    }
    let report = run.run();
    // The two distances the assertions below use are the program's own, so neither side can drift.
    assert_eq!(report.nanos("adjust_s"), ADJUST_S);
    assert_eq!(report.nanos("last_s"), LAST_S);
    assert_eq!(report.nanos("target"), TARGET);
    report
}

/// Whether the run was allowed to set the clock, which the target's own privilege probe answers.
fn allowed(report: &Report) -> bool {
    report.nanos("probe.result") == 0
}

/// Asserts that a call was refused with `errno`.
fn assert_refused(report: &Report, tag: &str, errno: libc::c_int) {
    assert_eq!(
        report.nanos(&format!("{tag}.result")),
        -1,
        "{tag} succeeded"
    );
    assert_eq!(
        report.nanos(&format!("{tag}.errno")),
        i64::from(errno),
        "{tag} was refused with the wrong errno"
    );
}

fn assert_succeeded(report: &Report, tag: &str) {
    assert_eq!(
        report.nanos(&format!("{tag}.result")),
        0,
        "{tag} was refused with errno {}",
        report.get(&format!("{tag}.errno"))
    );
}

fn assert_near(what: &str, value: i64, wanted: i64) {
    assert!(
        (value - wanted).abs() <= SLACK,
        "{what} = {value} is not {wanted} (off by {} s)",
        (value - wanted) / SECOND,
    );
}

/// Asserts that the machine's own clock is where it was, which is what proves a setter never
/// reached the genuine function.
fn assert_the_machine_is_untouched(report: &Report) {
    assert!(
        report.after - report.before < 60 * SECOND,
        "the genuine clock moved by {} s across the run",
        (report.after - report.before) / SECOND
    );
    assert!(
        (report.after - TARGET).abs() > 365 * DAY,
        "the genuine clock is sitting where the target asked us to put the faked one"
    );
}

/// Whether the kernel asks for privilege before it looks at the value a set carries. Linux looks
/// at the value first; macOS and FreeBSD ask first, so a process that may not set the clock hears
/// `EPERM` for a malformed value.
const PRIVILEGE_BEFORE_VALUE: bool = !cfg!(target_os = "linux");

/// Whether it asks before it looks at the clock id too, which FreeBSD alone does: macOS's libc
/// refuses a clock it cannot set before it calls the kernel at all.
const PRIVILEGE_BEFORE_CLOCK: bool = cfg!(target_os = "freebsd");

/// What a malformed call is refused with in a run that may or may not set the clock: `EINVAL`, or
/// `EPERM` where the kernel asks for privilege before it finds what is wrong.
const fn malformed(privilege_first: bool, privileged: bool) -> libc::c_int {
    if privilege_first && !privileged {
        libc::EPERM
    } else {
        libc::EINVAL
    }
}

/// Every malformed call, with the errno it is refused with, which depends on the order the kernel
/// checks a set in and, through that, on whether this run may set the clock.
fn assert_the_malformed_calls_are_refused(report: &Report) {
    let privileged = allowed(report);
    // A clock the kernel keeps but will not set, and one it has never heard of. A platform with no
    // TAI clock has no such id to name, which the target says rather than passing quietly.
    for tag in settable_refusals(report) {
        assert_refused(report, tag, malformed(PRIVILEGE_BEFORE_CLOCK, privileged));
    }
    // A field outside what the call may carry: a fraction wider than a second, a date before the
    // epoch, and a delta wider than libc's own `adjtime` takes.
    for tag in ["bad_nanos", "before_epoch", "bad_micros", "bad_delta"] {
        assert_refused(report, tag, malformed(PRIVILEGE_BEFORE_VALUE, privileged));
    }
    // A pointer the call cannot read, and the question a null delta asks. Linux and macOS look at
    // the clock before the pointer and FreeBSD reads the pointer first; macOS asks for privilege
    // before it reads one. A null delta needs no privilege where it is a question, on Linux and
    // FreeBSD: an adjustment there lands at once rather than slewed, so there is never one still
    // under way to report. macOS reads the delta, after asking for privilege.
    if report.nanos("nulls.available") == 1 {
        let unread = if cfg!(target_os = "macos") && !privileged {
            libc::EPERM
        } else {
            libc::EFAULT
        };
        assert_refused(report, "null_realtime", unread);
        assert_refused(
            report,
            "null_monotonic",
            if cfg!(target_os = "freebsd") {
                libc::EFAULT
            } else {
                libc::EINVAL
            },
        );
        if cfg!(target_os = "macos") {
            assert_refused(report, "no_delta", unread);
        } else {
            assert_succeeded(report, "no_delta");
        }
    }
}

/// Every clock the target named that the kernel will not set, by the tag it reports it under.
fn settable_refusals(report: &Report) -> Vec<&'static str> {
    let mut tags = vec!["monotonic", "boottime", "bogus"];
    for (name, tag) in [("coarse.available", "coarse"), ("tai.available", "tai")] {
        if report.nanos(name) == 1 {
            tags.push(tag);
        }
    }
    tags
}

#[test]
fn a_clock_the_kernel_cannot_set_is_refused_the_way_the_kernel_refuses_it() {
    let dir = tempfile::tempdir().unwrap();
    let report = setters(dir.path(), &[], &[]);

    assert_the_malformed_calls_are_refused(&report);
}

#[test]
fn an_excluded_command_reaches_the_genuine_setters() {
    if may_set_the_clock() {
        eprintln!(
            "this process could genuinely set the machine's clock, so the case that lets a setter \
             through to the kernel is not run"
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // A command on the skip list never touches a session, so every setter it makes is the
    // platform's own and answers exactly what it would have without us.
    let report = run_setters(
        dir.path(),
        &[],
        &[("LIBTIMEWARP_SKIP_CMDS", "setters,curl")],
        GENUINE_NULLS,
    );

    assert!(
        harness::session_files(dir.path()).is_empty(),
        "an excluded command attached to something"
    );
    assert_the_malformed_calls_are_refused(&report);
    for tag in ["clock_settime", "adjtime", "settimeofday", "probe"] {
        assert_refused(&report, tag, libc::EPERM);
    }
    assert_the_machine_is_untouched(&report);
}

#[test]
fn a_setter_succeeds_exactly_where_the_kernel_would_have_let_this_process_set_the_clock() {
    let dir = tempfile::tempdir().unwrap();
    let report = setters(dir.path(), &[], &[]);

    assert_eq!(
        allowed(&report),
        may_set_the_clock(),
        "the target answered a privilege this process does not have, or refused one it does"
    );
    assert_the_malformed_calls_are_refused(&report);
    assert_the_machine_is_untouched(&report);

    if allowed(&report) {
        assert_the_clock_followed_every_setter(&report);
    } else {
        for tag in ["clock_settime", "adjtime", "settimeofday"] {
            assert_refused(&report, tag, libc::EPERM);
        }
        // And a refused setter moves nothing: every reading is the genuine clock.
        for key in ["before", "after_clock_settime", "after_adjtime"] {
            assert_near(key, report.nanos(key), report.before);
        }
    }
}

#[test]
fn a_target_with_the_capability_moves_the_clock_the_way_it_asked() {
    let wrapper = capability_or_note("the setters that land");
    let dir = tempfile::tempdir().unwrap();
    let report = setters(dir.path(), &wrapper, &[]);

    assert_the_machine_is_untouched(&report);
    if allowed(&report) {
        assert_the_clock_followed_every_setter(&report);
    } else {
        assert_refused(&report, "clock_settime", libc::EPERM);
    }
}

/// The three setters that land, each read back afterwards: an absolute set, a shift by a delta
/// applied at once rather than slewed, and another absolute set.
fn assert_the_clock_followed_every_setter(report: &Report) {
    assert_succeeded(report, "clock_settime");
    assert_near(
        "after_clock_settime",
        report.nanos("after_clock_settime"),
        TARGET,
    );

    assert_succeeded(report, "adjtime");
    assert_near(
        "after_adjtime",
        report.nanos("after_adjtime"),
        TARGET + ADJUST_S * SECOND,
    );
    // Nothing is ever left pending, because the delta lands at once.
    assert_eq!(report.nanos("adjtime.old_sec"), 0);
    assert_eq!(report.nanos("adjtime.old_usec"), 0);

    assert_succeeded(report, "settimeofday");
    assert_near(
        "after_settimeofday",
        report.nanos("after_settimeofday"),
        TARGET + LAST_S * SECOND,
    );
}

#[test]
fn two_processes_in_one_session_see_the_same_move() {
    let wrapper = capability_or_note("one process watching another set the clock");
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("settime_pair")
        .session_dir(dir.path())
        .through(&wrapper)
        .arg(TARGET.to_string())
        .run();

    assert_the_machine_is_untouched(&report);
    // Before the set, the watching process reads the genuine wall clock, whichever way this goes.
    assert_near(
        "observer.before",
        report.nanos("observer.before"),
        report.before,
    );

    if report.nanos("setter.result") == 0 {
        // The move reached the other process on its next read, and the two agree.
        assert_near("observer.after", report.nanos("observer.after"), TARGET);
        assert_near("setter.after", report.nanos("setter.after"), TARGET);
    } else {
        assert_eq!(report.nanos("setter.errno"), i64::from(libc::EPERM));
        assert_near(
            "observer.after",
            report.nanos("observer.after"),
            report.before,
        );
    }
}

#[test]
fn a_session_on_the_null_keeper_answers_as_an_unprivileged_process_would() {
    let wrapper = capability_or_note("a null session refusing a process that holds the capability");
    let dir = tempfile::tempdir().unwrap();
    let report = setters(dir.path(), &wrapper, &[("LIBTIMEWARP_KEEPER", "null")]);

    // EPERM for the clock that can be set, EINVAL for the ones that cannot, whatever this process
    // actually holds: a session that fakes nothing may not move the machine's clock either.
    assert_the_malformed_calls_are_refused(&report);
    for tag in ["clock_settime", "adjtime", "settimeofday", "probe"] {
        assert_refused(&report, tag, libc::EPERM);
    }
    assert_the_machine_is_untouched(&report);
    for key in ["before", "after_clock_settime", "after_settimeofday"] {
        assert_near(key, report.nanos(key), report.before);
    }
}

/// `LIBTIMEWARP_HOOK` for a hook in the suite, with the arena it should be given.
fn hook_value(name: &str) -> String {
    format!("{}@{ARENA}", harness::hook_library(name).display())
}

/// The entries a logging hook left in the arena, each as many words as that hook writes.
fn logged(session: &Session, entry: usize) -> Vec<Vec<i64>> {
    let words = arena_words(session, usize::try_from(ARENA).unwrap() / size_of::<i64>());
    let (count, entries) = words.split_first().expect("the log's own counter");
    entries
        .chunks_exact(entry)
        .take(usize::try_from(*count).unwrap())
        .map(<[i64]>::to_vec)
        .collect()
}

/// Every setter a hook was handed, each entry beginning with the function, the kind, and the clock.
fn setters_seen_by(session: &Session, entry: usize) -> Vec<Vec<i64>> {
    let setters = [Func::Settimeofday, Func::ClockSettime, Func::Adjtime]
        .map(|func| i64::from(func as u32))
        .to_vec();
    logged(session, entry)
        .into_iter()
        .filter(|seen| seen.first().is_some_and(|func| setters.contains(func)))
        .collect()
}

/// The entry a logging hook recorded for `func` naming `clock`. Both halves matter: one program
/// sets several clocks, so the function alone names more than one call.
fn entry_for(seen: &[Vec<i64>], func: Func, clock: libc::clockid_t) -> &[i64] {
    seen.iter()
        .map(Vec::as_slice)
        .find(|entry| {
            entry.first() == Some(&i64::from(func as u32))
                && entry.get(2) == Some(&i64::from(clock))
        })
        .unwrap_or_else(|| panic!("{func:?} on clock {clock} never reached the hook"))
}

#[test]
fn a_handled_setter_means_success_and_the_keeper_never_moves() {
    let dir = tempfile::tempdir().unwrap();
    let report = setters(
        dir.path(),
        &[],
        &[
            ("LIBTIMEWARP_HOOK", &hook_value("setter_log")),
            // A keeper with a setting of its own, so a value it moved would be recognisable.
            ("LIBTIMEWARP_SHIFT", "-2h"),
        ],
    );

    // Handled, so the program sees success whatever privilege it holds and whichever clock it
    // named. The number the hook wrote into `*out` is ignored, because a setter wants neither.
    for tag in ["clock_settime", "settimeofday", "adjtime"]
        .into_iter()
        .chain(
            settable_refusals(&report)
                .into_iter()
                .filter(|tag| *tag != "bogus"),
        )
    {
        assert_succeeded(&report, tag);
    }
    // A clock the kernel will not read never reaches the hook, and neither does a call with no
    // instant to hand it: a malformed one, or a bare probe for privilege. Each is refused as it
    // would be without a hook.
    let privileged = may_set_the_clock();
    assert_refused(
        &report,
        "bogus",
        malformed(PRIVILEGE_BEFORE_CLOCK, privileged),
    );
    assert_refused(
        &report,
        "bad_nanos",
        malformed(PRIVILEGE_BEFORE_VALUE, privileged),
    );
    if privileged {
        assert_succeeded(&report, "probe");
    } else {
        assert_refused(&report, "probe", libc::EPERM);
    }

    // The keeper never ran a set, so the wall clock is where the session's own shift put it.
    for key in ["before", "after_clock_settime", "after_settimeofday"] {
        assert_near(key, report.nanos(key), report.before - 2 * 3_600 * SECOND);
    }
    assert_the_machine_is_untouched(&report);

    // Each setter reached the hook in the fake-to-genuine instant shape, carrying the instant the
    // program asked for.
    let session = only_session(dir.path());
    let seen = setters_seen_by(&session, SETTER_ENTRY);
    for entry in &seen {
        assert_eq!(entry[1], i64::from(Kind::InstantToGenuine as u32));
    }
    assert_eq!(
        entry_for(&seen, Func::Settimeofday, clock::REALTIME)[3],
        TARGET + LAST_S * SECOND
    );
    assert_eq!(
        entry_for(&seen, Func::ClockSettime, clock::REALTIME)[3],
        TARGET
    );
    // A clock the keeper would refuse reaches the hook all the same, carrying the id the call
    // named, so a hook that wants it can have it.
    assert_eq!(
        entry_for(&seen, Func::ClockSettime, clock::MONOTONIC)[3],
        TARGET
    );
    // A move by a duration names an instant only together with what the family shows now, which is
    // the faked wall clock rather than the genuine one.
    assert_near(
        "the instant adjtime asked for",
        entry_for(&seen, Func::Adjtime, clock::REALTIME)[3],
        report.before - 2 * 3_600 * SECOND + ADJUST_S * SECOND,
    );
}

#[test]
fn a_declined_setter_goes_to_the_keeper_and_never_to_the_genuine_function() {
    let wrapper = capability_or_note("a declined setter that lands on the keeper");
    let dir = tempfile::tempdir().unwrap();
    // A hook that records every call it is handed and declines all of them.
    let report = setters(
        dir.path(),
        &wrapper,
        &[("LIBTIMEWARP_HOOK", &hook_value("clock_group"))],
    );

    // The hook saw all three setters, in the fake-to-genuine instant shape, and declined each one.
    let session = only_session(dir.path());
    let seen = setters_seen_by(&session, DECLINED_ENTRY);
    for func in [Func::Settimeofday, Func::ClockSettime, Func::Adjtime] {
        let entry = entry_for(&seen, func, clock::REALTIME);
        assert_eq!(entry[1], i64::from(Kind::InstantToGenuine as u32));
    }

    // And the keeper answered them, never the genuine function: the machine's own clock is where
    // it was, and a process that holds the capability moved the session instead.
    assert_the_machine_is_untouched(&report);
    if allowed(&report) {
        assert_the_clock_followed_every_setter(&report);
    } else {
        assert_refused(&report, "settimeofday", libc::EPERM);
    }
}

#[test]
fn a_declined_setter_reaches_the_null_keeper_too() {
    let wrapper = capability_or_note("a null session refusing a setter the hook declined");
    let dir = tempfile::tempdir().unwrap();
    let report = setters(
        dir.path(),
        &wrapper,
        &[
            ("LIBTIMEWARP_KEEPER", "null"),
            ("LIBTIMEWARP_HOOK", &hook_value("clock_group")),
        ],
    );

    let session = only_session(dir.path());
    let seen = setters_seen_by(&session, DECLINED_ENTRY);
    for func in [Func::Settimeofday, Func::ClockSettime, Func::Adjtime] {
        entry_for(&seen, func, clock::REALTIME);
    }

    // The keeper answered each of them as an unprivileged process would see, and the genuine
    // function never ran: the machine's clock is where it was, capability or no capability.
    for tag in ["clock_settime", "adjtime", "settimeofday"] {
        assert_refused(&report, tag, libc::EPERM);
    }
    assert_the_machine_is_untouched(&report);
    assert_near(
        "after_settimeofday",
        report.nanos("after_settimeofday"),
        report.before,
    );
}
