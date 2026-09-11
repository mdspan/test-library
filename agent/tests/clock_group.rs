//! The rest of the clock group: every remaining way a program can ask what time it is, checked
//! against the family it belongs to, and the calls that must stay genuine.
//!
//! The target is linked against the library rather than preloading it, which is how it reaches the
//! genuine-clock symbol and glibc's internal aliases in one process.
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

use common::platform::{clock, default_timebase};
use common::time::Family;
use harness::{HOUR, Report, Run, Seen, calls_seen, only_session};

/// Every function that answers the wall clock, by the key `clocks` reports it under. The three
/// `sys_` keys are `syscall()` asked for the same three numbers libc would use.
#[cfg(target_os = "linux")]
const WALL_CLOCK_READS: &[&str] = &[
    "clock_gettime",
    "gettimeofday",
    "time",
    "timespec_get",
    "ftime",
    "__clock_gettime",
    "__gettimeofday",
    "__time",
    "__ftime",
    "sys_clock_gettime",
    "sys_gettimeofday",
];

/// The same for FreeBSD, whose libc has no `ftime` and no internal aliases, and whose `syscall()`
/// the library does not take over.
#[cfg(target_os = "freebsd")]
const WALL_CLOCK_READS: &[&str] = &["clock_gettime", "gettimeofday", "time", "timespec_get"];

/// A second, which is the slack every wall-clock comparison here allows: the coarsest of these
/// functions answers in whole seconds.
const SECOND: i64 = 1_000_000_000;

/// A file of two pages, the second filled with `B`, for the six-argument `mmap` the target makes.
fn two_pages(dir: &Path) -> PathBuf {
    let page = 4096;
    let path = dir.join("two-pages");
    let mut bytes = vec![b'A'; page];
    bytes.extend(std::iter::repeat_n(b'B', page));
    std::fs::write(&path, bytes).unwrap();
    path
}

/// One run of `clocks`, which reports every function of the group, faked and genuine.
fn clocks(dir: &Path, env: &[(&str, &str)]) -> Report {
    let mut run = Run::new(harness::program_linked_to_library("clocks"))
        .without_library()
        .session_dir(dir)
        .arg(two_pages(dir).to_string_lossy());
    for (name, value) in env {
        run = run.env(name, value);
    }
    run.run()
}

/// Asserts that `value` is `wanted` to within a second.
fn assert_near(what: &str, value: i64, wanted: i64) {
    assert!(
        (value - wanted).abs() <= SECOND,
        "{what} = {value} is not {wanted} (off by {} ms)",
        (value - wanted) / 1_000_000,
    );
}

/// Asserts that `value` is `wanted` to within `percent` of it, plus `floor` for quantisation.
fn assert_close(what: &str, value: i64, wanted: i64, percent: i64, floor: i64) {
    let slack = floor + wanted.abs() * percent / 100;
    assert!(
        (value - wanted).abs() <= slack,
        "{what} = {value} is not {wanted} (off by {})",
        value - wanted
    );
}

#[test]
fn every_way_of_asking_the_wall_clock_is_faked() {
    let dir = tempfile::tempdir().unwrap();
    let report = clocks(dir.path(), &[("LIBTIMEWARP_SHIFT", "-2h")]);
    let genuine = report.nanos("realtime.genuine");

    for key in WALL_CLOCK_READS {
        assert_near(key, report.nanos(key), genuine - 2 * HOUR);
    }
    // `timespec_get` answers the base it was given, and zero for a base no C library defines.
    assert_eq!(report.nanos("timespec_get.base"), 1);
    assert_eq!(report.nanos("timespec_get.bad_base"), 0);
    #[cfg(target_os = "linux")]
    {
        if report.nanos("has_sys_time") == 1 {
            assert_near("sys_time", report.nanos("sys_time"), genuine - 2 * HOUR);
        }
        // `ftime`'s other two fields are the ones libc left at zero.
        assert_eq!(report.nanos("ftime.timezone"), 0);
        assert_eq!(report.nanos("ftime.dstflag"), 0);
    }
}

#[test]
fn without_a_session_every_one_of_them_reads_the_genuine_clock() {
    let dir = tempfile::tempdir().unwrap();
    let report = clocks(dir.path(), &[("LIBTIMEWARP_KEEPER", "null")]);
    let genuine = report.nanos("realtime.genuine");

    for key in WALL_CLOCK_READS {
        assert_near(key, report.nanos(key), genuine);
    }
}

#[test]
fn the_cpu_group_scales_what_the_process_consumed() {
    let dir = tempfile::tempdir().unwrap();
    let report = clocks(dir.path(), &[("LIBTIMEWARP_CPU_RATE", "8")]);

    // `clock` answers in CLOCKS_PER_SEC units rather than nanoseconds.
    assert_eq!(report.nanos("a.clocks_per_sec"), 1_000_000);
    let consumed = report.nanos("a.process_cpu.genuine");
    assert!(
        consumed > 100_000_000,
        "the target consumed only {consumed}"
    );
    assert_close(
        "a.clock",
        report.nanos("a.clock") * 1_000,
        8 * consumed,
        5,
        10_000_000,
    );

    // `times`' fields are clock ticks of consumed CPU, which is the same family.
    let ticks_per_second = report.nanos("a.ticks_per_second");
    assert!(ticks_per_second > 0);
    assert_close(
        "a.times.cpu",
        report.nanos("a.times.cpu"),
        8 * report.nanos("a.times.cpu.genuine"),
        5,
        8,
    );
    // And `getrusage` scales the user and system times together.
    assert_close(
        "a.rusage.cpu",
        report.nanos("a.rusage.cpu"),
        8 * report.nanos("a.rusage.cpu.genuine"),
        5,
        10_000_000,
    );
    assert_close(
        "a.thread_rusage.cpu",
        report.nanos("a.thread_rusage.cpu"),
        8 * report.nanos("a.thread_rusage.cpu.genuine"),
        5,
        10_000_000,
    );
}

#[test]
fn getrusage_leaves_every_field_but_the_two_times_alone() {
    let dir = tempfile::tempdir().unwrap();
    let report = clocks(dir.path(), &[("LIBTIMEWARP_CPU_RATE", "8")]);

    for field in ["maxrss", "minflt"] {
        assert_eq!(
            report.nanos(&format!("a.rusage.{field}")),
            report.nanos(&format!("a.rusage.{field}.genuine")),
            "getrusage changed ru_{field}"
        );
    }
}

#[test]
fn a_bare_rate_leaves_consumption_alone() {
    let dir = tempfile::tempdir().unwrap();
    let report = clocks(dir.path(), &[("LIBTIMEWARP_RATE", "10")]);

    assert_close(
        "a.clock",
        report.nanos("a.clock") * 1_000,
        report.nanos("a.process_cpu.genuine"),
        5,
        10_000_000,
    );
    assert_close(
        "a.times.cpu",
        report.nanos("a.times.cpu"),
        report.nanos("a.times.cpu.genuine"),
        5,
        2,
    );
}

#[test]
fn the_elapsed_time_times_returns_follows_monotonic() {
    let dir = tempfile::tempdir().unwrap();
    // The Cpu rate is there to prove the two answers of one call follow different families.
    let report = clocks(
        dir.path(),
        &[
            ("LIBTIMEWARP_MONOTONIC_RATE", "10"),
            ("LIBTIMEWARP_CPU_RATE", "2"),
        ],
    );

    let faked = report.nanos("b.times.elapsed") - report.nanos("a.times.elapsed");
    let genuine = report.nanos("b.times.elapsed.genuine") - report.nanos("a.times.elapsed.genuine");
    assert!(genuine > 0, "no genuine time passed between the two rounds");
    assert_close("the elapsed time", faked, 10 * genuine, 15, 4);

    // The fields of the same call followed Cpu, at its own rate.
    assert_close(
        "b.times.cpu",
        report.nanos("b.times.cpu"),
        2 * report.nanos("b.times.cpu.genuine"),
        10,
        8,
    );
}

#[test]
fn the_calls_that_must_stay_genuine_stay_genuine() {
    let dir = tempfile::tempdir().unwrap();
    let faked = clocks(
        dir.path(),
        &[("LIBTIMEWARP_RATE", "10"), ("LIBTIMEWARP_CPU_RATE", "8")],
    );
    let plain = Run::new(harness::program_linked_to_library("clocks"))
        .without_library()
        .session_dir(dir.path())
        .env("LIBTIMEWARP_KEEPER", "null")
        .run();

    // `clock_getres` answers the genuine resolution, whatever rate the family runs at.
    for key in ["getres.realtime", "getres.monotonic", "getres.process_cpu"] {
        assert_eq!(faked.nanos(key), plain.nanos(key), "{key} was faked");
    }
    // `adjtimex` and `ntp_adjtime` read the kernel's own clock, which our shift never reaches.
    // FreeBSD's `timex` carries no clock reading at all, so there the call itself is the claim.
    #[cfg(target_os = "linux")]
    for key in ["adjtimex.time", "ntp_adjtime.time"] {
        assert_near(key, faked.nanos(key), faked.nanos("realtime.genuine"));
    }
    assert!(faked.nanos("ntp_adjtime.rc") >= 0);
    // An id from `clock_getcpuclockid` or `pthread_getcpuclockid` is a reading about somebody, so
    // it stays genuine even when it names this very process.
    for key in ["own_cpuclockid", "pthread_cpuclockid"] {
        assert_close(
            key,
            faked.nanos(key),
            faked.nanos(&format!("{key}.genuine")),
            1,
            50_000_000,
        );
    }
    // And a dynamic clock id reaches the kernel, which refuses it the same way either side.
    // Only Linux has such ids; FreeBSD names a hardware clock no other way.
    #[cfg(target_os = "linux")]
    {
        assert_eq!(faked.nanos("dynamic.rc"), -1);
        assert_eq!(
            faked.nanos("dynamic.errno"),
            faked.nanos("dynamic.genuine_errno")
        );
    }
}

/// `syscall()` is Linux's own way of asking for a call by number, and the one platform where the
/// library takes the function over.
#[cfg(target_os = "linux")]
#[test]
fn syscall_hands_every_other_number_to_the_kernel_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let report = clocks(dir.path(), &[("LIBTIMEWARP_SHIFT", "-2h")]);

    // A number that takes no argument, and one whose answer would be wrong if we had touched it.
    assert_eq!(report.nanos("sys_getpid"), report.nanos("getpid"));
    assert_eq!(report.nanos("sys_clock_getres.rc"), 0);
    assert_eq!(
        report.nanos("sys_clock_getres"),
        report.nanos("getres.realtime")
    );
    // A number the kernel has never heard of comes back the way libc reports one: -1 and ENOSYS.
    assert_eq!(report.nanos("sys_unknown.rc"), -1);
    assert_eq!(report.nanos("sys_unknown.errno"), i64::from(libc::ENOSYS));
    // And a call that fills all six argument registers still lands where it was aimed: the second
    // page of the file, mapped at an offset the sixth argument carried.
    assert_eq!(
        report.nanos("mmap.byte"),
        i64::from(b'B'),
        "the six arguments of mmap did not reach the kernel"
    );
}

#[test]
fn every_function_of_the_group_reaches_the_hook_with_its_own_func_and_kind() {
    let dir = tempfile::tempdir().unwrap();
    let hook = harness::hook_library("clock_group");
    let report = clocks(
        dir.path(),
        &[
            ("LIBTIMEWARP_HOOK", &format!("{}@4096", hook.display())),
            ("LIBTIMEWARP_CPU_RATE", "8"),
        ],
    );
    assert_ne!(report.get("shared"), "");

    let session = only_session(dir.path());
    let seen = calls_seen(&session);
    assert!(!seen.is_empty(), "the hook was never reached");

    // Every function of the clock group, with the clock it names. A call that names no clock reads
    // CLOCK_REALTIME; `times` is two answers from two families, and `getrusage` names the thread
    // when the program asked about the thread.
    let realtime = i64::from(clock::REALTIME);
    let process_cpu = i64::from(clock::PROCESS_CPUTIME);
    let thread_cpu = i64::from(clock::THREAD_CPUTIME);
    // The clock the Monotonic family tracks, which the elapsed time of `times` follows and which
    // the target reads by name. FreeBSD picks it from the running kernel.
    let monotonic = i64::from(default_timebase(Family::Monotonic).unwrap());
    let named_monotonic = i64::from(libc::CLOCK_MONOTONIC);
    // `ftime` is glibc's, and the FreeBSD arm has no such call to catch.
    let ftime: &[(&str, i64, i64)] = if cfg!(target_os = "linux") {
        &[("ftime", 5, i64::from(libc::CLOCK_REALTIME))]
    } else {
        &[]
    };
    for &(what, func, clock) in [
        ("clock_gettime", 1, realtime),
        ("clock_gettime on the monotonic clock", 1, named_monotonic),
        ("gettimeofday", 2, realtime),
        ("time", 3, realtime),
        ("timespec_get", 4, realtime),
        ("clock", 6, process_cpu),
        ("the fields of times", 7, process_cpu),
        ("the elapsed time of times", 7, monotonic),
        ("getrusage", 8, process_cpu),
        ("getrusage on the calling thread", 8, thread_cpu),
    ]
    .iter()
    .chain(ftime)
    {
        assert!(
            seen.contains(&Seen {
                func,
                kind: 1,
                clock
            }),
            "{what} did not reach the hook as function {func} on clock {clock}: {seen:?}"
        );
    }

    // Nothing else read a clock: every call of the group is a reading coming towards the program.
    // The one other function this program reaches is the `nanosleep` it pauses in, which is a
    // duration going the other way and belongs to the sleeps.
    for call in &seen {
        if call.func == 100 {
            assert_eq!(
                call.kind, 3,
                "{call:?} is not a fake duration becoming genuine"
            );
            continue;
        }
        assert_eq!(
            call.kind, 1,
            "{call:?} is not an instant becoming a fake one"
        );
        assert!((1..=8).contains(&call.func), "{call:?} is not a clock read");
    }
}
