//! The narrow complete path: a shifted wall clock, seen by a preloaded program.
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

use common::platform::clock;
use common::time::Family;
use harness::{HOUR, Run};

fn session_files(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir.join("libtimewarp")) else {
        return Vec::new();
    };
    entries
        .map(|e| e.unwrap().file_name())
        .filter(|name| harness::is_session_file(name))
        .map(|name| name.to_string_lossy().into_owned())
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
fn a_relative_session_directory_still_reaches_a_child_after_a_change_of_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sessions")).unwrap();
    let probe = harness::program("probe");
    let report = Run::prog("execer")
        .arg(probe.to_string_lossy())
        .arg("/")
        .current_dir(dir.path())
        .env("LIBTIMEWARP_SESSION_DIR", "sessions")
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    report.assert_wall_clock("before", -2 * HOUR);
    report.assert_wall_clock("after", -2 * HOUR);
    let shared = Path::new(report.get("before.shared"));
    assert!(shared.is_absolute(), "{}", shared.display());
    assert_eq!(
        std::fs::canonicalize(shared.parent().unwrap()).unwrap(),
        std::fs::canonicalize(dir.path().join("sessions/libtimewarp")).unwrap()
    );
    assert_eq!(report.get("after.shared"), report.get("before.shared"));
}

#[test]
fn a_relative_inherited_path_still_reaches_a_child_after_a_change_of_directory() {
    let dir = tempfile::tempdir().unwrap();
    let first = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();
    let created = Path::new(first.get("probe.shared"));

    let probe = harness::program("probe");
    let report = Run::prog("execer")
        .arg(probe.to_string_lossy())
        .arg("/")
        .current_dir(created.parent().unwrap())
        .env(
            "LIBTIMEWARP__SHARED_FILE",
            created.file_name().unwrap().to_string_lossy(),
        )
        .run();

    report.assert_wall_clock("before", -2 * HOUR);
    report.assert_wall_clock("after", -2 * HOUR);
    let shared = Path::new(report.get("after.shared"));
    assert!(shared.is_absolute(), "{}", shared.display());
    assert_eq!(
        std::fs::canonicalize(shared).unwrap(),
        std::fs::canonicalize(created).unwrap()
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

/// Every name the clock group, the sleeps, the timeouts, the timers, the file timestamps and the
/// setters take over, glibc's internal aliases among them, the three calls that learn which clock
/// an object of the program's was built with, and the one symbol of our own that a hook or a
/// controller calls. Nothing else: a name here that the interposition table does not list would be
/// a name we answer for by accident.
///
/// macOS defines none of these: dyld would bind a second definition of a libc name to libc all the
/// same, so a replacement there carries a private name and reaches the program through a table.
/// The cases for that table are `tests/interpose.rs`.
#[cfg(target_os = "linux")]
const INTERPOSED: &[&str] = &[
    "__clock_gettime",
    "__ftime",
    "__gettimeofday",
    "__time",
    "adjtime",
    "aio_suspend",
    "alarm",
    "clock",
    "clock_gettime",
    "clock_nanosleep",
    "clock_settime",
    "cnd_timedwait",
    "epoll_pwait",
    "epoll_pwait2",
    "epoll_wait",
    "fstat",
    "fstat64",
    "fstatat",
    "fstatat64",
    "ftime",
    "futimens",
    "futimes",
    "getitimer",
    "getrusage",
    "gettimeofday",
    "libtimewarp_genuine_clock_gettime",
    "lstat",
    "lstat64",
    "lutimes",
    "mq_timedreceive",
    "mq_timedsend",
    "mtx_timedlock",
    "nanosleep",
    "poll",
    "ppoll",
    "pselect",
    "pthread_cond_clockwait",
    "pthread_cond_destroy",
    "pthread_cond_init",
    "pthread_cond_timedwait",
    "pthread_mutex_timedlock",
    "pthread_rwlock_timedrdlock",
    "pthread_rwlock_timedwrlock",
    "pthread_timedjoin_np",
    "recvmmsg",
    "select",
    "sem_clockwait",
    "sem_timedwait",
    "setitimer",
    "settimeofday",
    "sigtimedwait",
    "sleep",
    "stat",
    "stat64",
    "statx",
    "syscall",
    "thrd_sleep",
    "time",
    "timer_create",
    "timer_delete",
    "timer_gettime",
    "timer_settime",
    "timerfd_create",
    "timerfd_gettime",
    "timerfd_settime",
    "times",
    "timespec_get",
    "usleep",
    "utime",
    "utimensat",
    "utimes",
];

/// The same for FreeBSD. Five names Linux has are absent because this platform has no such call,
/// and five more because its libc reaches them through a call we already take over: `utime`,
/// `recvmmsg`, `thrd_sleep`, `cnd_timedwait` and `mtx_timedlock` are thin wrappers over `utimes`,
/// `ppoll`, `nanosleep`, `pthread_cond_timedwait` and `pthread_mutex_timedlock`, so defining them
/// too would convert the call twice. `kevent` is here and on no other list.
#[cfg(target_os = "freebsd")]
const INTERPOSED: &[&str] = &[
    "adjtime",
    "aio_suspend",
    "alarm",
    "clock",
    "clock_gettime",
    "clock_nanosleep",
    "clock_settime",
    "fstat",
    "fstatat",
    "futimens",
    "futimes",
    "getitimer",
    "getrusage",
    "gettimeofday",
    "kevent",
    "libtimewarp_genuine_clock_gettime",
    "lstat",
    "lutimes",
    "mq_timedreceive",
    "mq_timedsend",
    "nanosleep",
    "poll",
    "ppoll",
    "pselect",
    "pthread_cond_destroy",
    "pthread_cond_init",
    "pthread_cond_timedwait",
    "pthread_mutex_timedlock",
    "pthread_rwlock_timedrdlock",
    "pthread_rwlock_timedwrlock",
    "pthread_timedjoin_np",
    "select",
    "sem_timedwait",
    "setitimer",
    "settimeofday",
    "sigtimedwait",
    "sleep",
    "stat",
    "time",
    "timer_create",
    "timer_delete",
    "timer_gettime",
    "timer_settime",
    "times",
    "timespec_get",
    "usleep",
    "utimensat",
    "utimes",
];

/// The names glibc routed `stat` and its neighbours through until 2.33, which only a build
/// targeting such a glibc defines.
#[cfg(not(target_os = "macos"))]
const LEGACY_STAT: &[&str] = &[
    "__fxstat",
    "__fxstat64",
    "__fxstatat",
    "__fxstatat64",
    "__lxstat",
    "__lxstat64",
    "__xstat",
    "__xstat64",
];

/// Every name the library should define, sorted the way `nm` reports them.
#[cfg(not(target_os = "macos"))]
fn interposed_names() -> Vec<String> {
    let legacy = if cfg!(all(target_os = "linux", glibc_pre_2_33)) {
        LEGACY_STAT
    } else {
        &[]
    };
    let mut wanted: Vec<String> = INTERPOSED
        .iter()
        .chain(legacy)
        .map(|name| (*name).to_owned())
        .collect();
    wanted.sort();
    wanted
}

#[cfg(not(target_os = "macos"))]
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

    assert_eq!(exported, interposed_names());

    // And the pass-through list is not there at all: the way these stay genuine is that we never
    // define the name, so the program's call reaches libc as it would without us.
    for absent in ["clock_getres", "adjtimex", "ntp_adjtime", "stime"] {
        assert!(
            !exported.iter().any(|name| name == absent),
            "{absent} is interposed, and it should pass through"
        );
    }
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

/// The clocks the target read, grouped by the family the library's own table puts each in.
///
/// The target reports the id behind every name it read, so the grouping is this platform's, and on
/// FreeBSD the running kernel's. TAI is left out: it reads the Realtime line plus an offset, which
/// a case of its own covers.
fn by_family(report: &harness::Report) -> Vec<(Family, Vec<String>)> {
    Family::ALL
        .into_iter()
        .map(|family| {
            let group = report
                .under("id.")
                .into_iter()
                .filter(|(_, id)| {
                    let id: libc::clockid_t = id.parse().unwrap();
                    clock::family_of(id) == Some(family) && !clock::is_tai(id)
                })
                .map(|(name, _)| name)
                .collect();
            (family, group)
        })
        .collect()
}

#[test]
fn every_clock_of_a_family_reads_the_family_value() {
    let report = families(&[("LIBTIMEWARP_RATE", "5"), ("LIBTIMEWARP_SHIFT", "-2h")]);

    // Realtime, Monotonic, Boottime and Cpu, each with more than one clock in it here.
    for (family, group) in by_family(&report) {
        // Cpu scales each clock's own consumption, so its clocks have no one value to share.
        if family == Family::Cpu {
            continue;
        }
        assert!(group.len() > 1, "{family} holds only {group:?}");
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

    // Every clock of the family this platform has, which on FreeBSD includes the two slices of
    // process CPU time that exist nowhere else.
    let (_, group) = by_family(&report)
        .into_iter()
        .find(|(family, _)| *family == Family::Cpu)
        .unwrap();
    assert!(group.contains(&"process_cpu".to_owned()), "{group:?}");
    for clock in group {
        let before = report.nanos(&format!("a.{clock}.before"));
        let faked = report.nanos(&format!("a.{clock}"));
        let after = report.nanos(&format!("a.{clock}.genuine"));
        assert!(before > 10_000_000, "{clock} consumed only {before} ns");
        // Eight times a genuine reading taken between the two around it. The span can be wide:
        // FreeBSD splits `CLOCK_VIRTUAL` and `CLOCK_PROF` out of the precise total by statclock
        // ticks and keeps each slice from falling, so one tick between two readings can step them
        // by a share of everything consumed so far.
        assert!(
            (8 * before..=8 * after).contains(&faked),
            "{clock} read {faked}, outside eight times {before}..={after}"
        );
    }
}

#[test]
fn the_tai_clock_keeps_its_offset_from_the_wall_clock() {
    let report = families(&[("LIBTIMEWARP_TIME", "1990-01-01T00:00:00Z")]);
    if !clock::is_tai(report.nanos("id.tai").try_into().unwrap()) {
        // FreeBSD grew a TAI clock at 15.0, so a kernel below that has none to read, and macOS
        // keeps none at all.
        eprintln!("this platform has no TAI clock, so that case went unexercised");
        return;
    }
    if report.nanos("a.tai.genuine") < 0 {
        // FreeBSD refuses CLOCK_TAI until a time daemon has told the kernel the TAI offset.
        eprintln!("the kernel does not know the TAI offset, so that case went unexercised");
        return;
    }

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

/// The ids `clock_getcpuclockid` hands out, which macOS has none of: a CPU clock is asked for
/// through Mach there rather than named by an id a program can hand to `clock_gettime`.
#[cfg(not(target_os = "macos"))]
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
