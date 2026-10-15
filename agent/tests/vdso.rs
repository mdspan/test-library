//! The synthetic vDSO, and the Go runtime it exists for.
//!
//! A target that reads the clock without calling libc finds the object the auxiliary vector names
//! and calls into it. These cases run two such targets: one in C that walks the object by hand the
//! way the Go runtime does, so the mechanism can be checked step by step, and one in Go, so what a
//! real runtime does with it is checked too.
//!
//! The synthetic vDSO is Linux's, so these cases only apply there.
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
    reason = "a transaction is consumed by `commit` or `abort`, which is where its lock is \
              released; the lint does not see through a method taking `self` and suggests a drop \
              that would not compile"
)]

mod harness;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::platform::{clock, genuine_nanos};
use common::time::{Family, Nanos};
use controller::Controller;

use harness::{HOUR, Run};

const SECOND: i64 = Nanos::SECOND;
const DAY: i64 = 24 * HOUR;

/// How far apart two readings taken microseconds apart may be before they are not the same clock.
/// Wide enough for a machine under load, and far below any faked value a case sets.
const TOGETHER_NS: i64 = 50 * 1_000_000;

/// The most readings the ticking program spends reaching its deadline. Generous, because the
/// budget is what a failure runs out of rather than what a pass uses.
const BUDGET: i64 = 200_000;

#[test]
fn a_target_reading_through_the_vdso_sees_the_faked_clock() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("vdso")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    assert_eq!(report.get("vdso.entries"), "1");
    report.assert_wall_clock("vdso", -2 * HOUR);
    report.assert_wall_clock("libc", -2 * HOUR);
}

#[test]
fn the_vdso_reader_and_the_libc_reader_in_one_process_agree() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("vdso")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_TIME", "2020-01-01T00:00:00Z")
        .run();

    for key in ["clock_gettime", "gettimeofday"] {
        let gap = report.nanos(&format!("libc.{key}")) - report.nanos(&format!("vdso.{key}"));
        assert!(
            gap.abs() < TOGETHER_NS,
            "the two readers are {gap} ns apart on {key}",
        );
    }
    assert_eq!(report.nanos("libc.time"), report.nanos("vdso.time"));
    // 2020-01-01T00:00:00Z, which is what both of them were set to.
    assert_eq!(report.nanos("vdso.time"), 1_577_836_800);
}

#[test]
fn the_auxiliary_vector_points_at_the_object_we_built() {
    let dir = tempfile::tempdir().unwrap();
    let genuine = Run::prog("vdso").without_library().run();
    let faked = Run::prog("vdso").session_dir(dir.path()).run();

    assert_eq!(
        genuine.get("vdso.kernels_own"),
        "1",
        "the vector points at the kernel's own object when nothing has moved it",
    );
    assert_eq!(
        faked.get("vdso.kernels_own"),
        "0",
        "the vector still points at the kernel's object",
    );
}

#[test]
fn the_entries_that_pass_through_answer_what_the_kernel_answers() {
    let dir = tempfile::tempdir().unwrap();
    let genuine = Run::prog("vdso").without_library().run();
    let faked = Run::prog("vdso")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    for key in ["vdso.getres.found", "vdso.getcpu.found"] {
        assert_eq!(faked.get(key), genuine.get(key), "{key}");
    }
    assert_eq!(faked.get("vdso.getres"), genuine.get("vdso.getres"));
    // aarch64's kernel vDSO has no `getcpu`, so ours has none to forward and there is no answer
    // either object gives; the pair above is what says so.
    if faked.get("vdso.getcpu.found") == "1" {
        assert_eq!(faked.get("vdso.getcpu"), "0", "getcpu refused");
    }
}

#[test]
fn turning_the_variable_off_leaves_the_kernel_object_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("vdso")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .env("LIBTIMEWARP_EMULATE_VDSO", "0")
        .run();

    // Interposition is untouched, so a libc reader is faked as usual.
    report.assert_wall_clock("libc", -2 * HOUR);
    // And the vDSO reader has the kernel's own object, so it reads genuine time.
    report.assert_wall_clock("vdso", 0);
    assert_eq!(report.get("vdso.kernels_own"), "1");
}

#[test]
fn an_attacher_follows_the_session_rather_than_its_own_environment() {
    let dir = tempfile::tempdir().unwrap();
    // The creator is the wrapper, which reads the variable; the target it execs attaches, and an
    // attacher reads none of the setup variables.
    let off = Run::prog("vdso")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .env("LIBTIMEWARP_EMULATE_VDSO", "0")
        .through(&sets("LIBTIMEWARP_EMULATE_VDSO=1"))
        .run();
    off.assert_wall_clock("libc", -2 * HOUR);
    off.assert_wall_clock("vdso", 0);

    let dir = tempfile::tempdir().unwrap();
    let on = Run::prog("vdso")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .env("LIBTIMEWARP_EMULATE_VDSO", "1")
        .through(&sets("LIBTIMEWARP_EMULATE_VDSO=0"))
        .run();
    on.assert_wall_clock("libc", -2 * HOUR);
    on.assert_wall_clock("vdso", -2 * HOUR);
}

/// A wrapper that execs the target with one more variable set, for a case where what the target's
/// own environment says has to lose to what the session recorded.
fn sets(variable: &str) -> Vec<String> {
    vec!["/usr/bin/env".to_owned(), variable.to_owned()]
}

#[test]
fn a_live_change_reaches_the_vdso_reader_and_the_libc_reader_together() {
    let dir = tempfile::tempdir().unwrap();
    // A day ahead, so nothing but the transaction below reaches it and the program stops exactly
    // when that one lands. The program stops only when both of its readers have passed it.
    let deadline = harness::genuine_now() + DAY;

    let mut child = Command::new(harness::program("vdso"))
        .arg(deadline.to_string())
        .arg(BUDGET.to_string())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LD_PRELOAD", harness::library())
        .env("LIBTIMEWARP_SESSION_DIR", dir.path())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Drained as it is printed, so the program keeps reading the clock instead of blocking on a
    // full pipe while the transaction below runs.
    let mut printing = child.stdout.take().unwrap();
    let drained = std::thread::spawn(move || {
        let mut text = String::new();
        printing.read_to_string(&mut text).unwrap();
        text
    });

    let controller = Controller::attach(&wait_for_session(dir.path())).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    let mut transaction = controller.begin().unwrap();
    transaction
        .set_time(Family::Realtime, Nanos(deadline + SECOND))
        .unwrap();
    transaction.commit().unwrap();

    let status = child.wait().unwrap();
    let printed = drained.join().unwrap();
    assert!(
        status.success(),
        "a reader never saw the wall clock the transaction set:\n{printed}",
    );

    let ticks = ticks(&printed);
    assert!(ticks.len() > 20, "only {} readings", ticks.len());
    for &[through_vdso, through_libc] in &ticks {
        let gap = through_libc - through_vdso;
        assert!(
            gap.abs() < TOGETHER_NS,
            "the two readers are {gap} ns apart",
        );
    }
    let (first, last) = (ticks[0], ticks[ticks.len() - 1]);
    for index in 0..2 {
        assert!(
            last[index] - first[index] >= DAY,
            "reader {index} never got there",
        );
    }
}

#[test]
fn a_build_without_the_feature_says_so_and_fakes_everything_else() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let hook = harness::hook_library("arena_clock");
    let report = Run::prog("vdso")
        .under_library(harness::library_without_vdso())
        .session_dir(dir.path())
        .env("LIBTIMEWARP_HOOK", format!("{}@4096", hook.display()))
        .env("LIBTIMEWARP_LOG", log.to_string_lossy())
        .run();

    let line = harness::log_of(&log);
    assert!(
        line.contains("LIBTIMEWARP_EMULATE_VDSO"),
        "the build said nothing about the variable it ignores: {line}",
    );
    assert_eq!(line.lines().count(), 1, "more than one line: {line}");

    // The hook answers the clock, which is the whole of what such a build promises: everything but
    // the vDSO works as it always does.
    assert_eq!(report.nanos("libc.time"), 1_577_836_800);
    assert_eq!(report.get("vdso.kernels_own"), "1");
    // And the target still reads genuine time through the object the kernel left it.
    report.assert_wall_clock("vdso", 0);

    // A controller still reads and changes the session it left behind.
    let session = harness::only_session(dir.path());
    let controller = Controller::attach(session.path()).unwrap();
    let mut transaction = controller.begin().unwrap();
    transaction.set_rate(Family::Realtime, 2.0).unwrap();
    transaction.commit().unwrap();
    assert_eq!(
        controller.info().realtime.rate,
        common::time::Rate::from_f64(2.0).unwrap(),
    );
}

#[test]
fn a_build_without_the_feature_says_nothing_when_the_session_asked_for_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    Run::prog("vdso")
        .under_library(harness::library_without_vdso())
        .session_dir(dir.path())
        .env("LIBTIMEWARP_EMULATE_VDSO", "0")
        .env("LIBTIMEWARP_LOG", log.to_string_lossy())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run()
        .assert_wall_clock("libc", -2 * HOUR);

    assert_eq!(harness::log_of(&log), "", "a line about nothing ignored");
}

// The Go runtime reads every clock without libc, so what it sees is the synthetic vDSO's alone.

#[test]
fn a_go_program_sees_the_faked_clock_through_the_vdso() {
    let Some(program) = harness::go_program("go_clocks") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let report = Run::new(&program)
        .arg("now")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    let now = report.nanos("go.now");
    let low = report.before - 2 * HOUR - SECOND;
    let high = report.after - 2 * HOUR + SECOND;
    assert!(
        (low..=high).contains(&now),
        "time.Now is {now}, not in {low}..={high}",
    );
    // The runtime measures its own spans on the monotonic clock, which it reads the same way.
    let since = report.nanos("go.since");
    assert!(since >= 1_000_000, "time.Since measured {since} ns");
}

#[test]
fn a_go_program_reads_genuine_time_when_the_session_asked_for_no_vdso() {
    let Some(program) = harness::go_program("go_clocks") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let report = Run::new(&program)
        .arg("now")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .env("LIBTIMEWARP_EMULATE_VDSO", "0")
        .run();

    let now = report.nanos("go.now");
    assert!(
        (report.before - SECOND..=report.after + SECOND).contains(&now),
        "time.Now is {now}, which is not the genuine clock",
    );
}

#[test]
fn a_go_program_under_a_build_without_the_feature_reads_genuine_time() {
    let Some(program) = harness::go_program("go_clocks") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let report = Run::new(&program)
        .arg("now")
        .under_library(harness::library_without_vdso())
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    let now = report.nanos("go.now");
    assert!(
        (report.before - SECOND..=report.after + SECOND).contains(&now),
        "time.Now is {now}, which is not the genuine clock",
    );
}

/// File timestamps stay genuine for Go: `os.Stat` is a raw `fstatat`, which interposition never
/// sees. The documented behaviour, pinned rather than worked around.
#[test]
fn a_go_program_reads_genuine_timestamps_off_files() {
    let Some(program) = harness::go_program("go_clocks") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let report = Run::new(&program)
        .arg("stat")
        .arg(files.path().to_string_lossy())
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    let stamp = report.nanos("go.stat");
    assert!(
        (report.before - SECOND..=report.after + SECOND).contains(&stamp),
        "the file it wrote shows {stamp}, which is not the genuine clock",
    );
    // While the clock the same program reads is faked, which is what makes the two disagree.
    let now = report.nanos("go.now");
    assert!(
        stamp - now > HOUR,
        "the clock was not faked, so the timestamp proves nothing",
    );
}

/// The kernel side of a Go wait stays genuine: the runtime hands the remaining fake duration to the
/// kernel unscaled. Under a rate above one a sleep therefore takes about as long in genuine time as
/// it asked for, and the runtime's own clock says the whole faked span passed. The documented
/// behaviour, pinned rather than worked around.
#[test]
fn a_go_sleep_blocks_for_the_genuine_duration_it_asked_for() {
    let Some(program) = harness::go_program("go_clocks") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let report = Run::new(&program)
        .arg("sleep")
        .arg("200")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_RATE", "5")
        .run();

    let asked = report.nanos("go.sleep.asked");
    let measured = report.nanos("go.sleep.measured");
    let genuine = report.after - report.before;
    assert!(
        genuine >= asked / 2,
        "the sleep took {genuine} ns of genuine time, so the rate reached the kernel after all",
    );
    assert!(
        measured >= 3 * asked,
        "the runtime measured {measured} ns across a sleep of {asked} ns at five times speed",
    );
}

/// And under a rate below one the runtime wakes early, sees the timer is still due, and waits
/// again, converging on the instant the faked clock promised.
#[test]
fn a_go_sleep_under_a_slow_clock_converges_on_the_instant_it_promised() {
    let Some(program) = harness::go_program("go_clocks") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let report = Run::new(&program)
        .arg("sleep")
        .arg("200")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_RATE", "0.5")
        .run();

    let asked = report.nanos("go.sleep.asked");
    let measured = report.nanos("go.sleep.measured");
    assert!(
        measured >= asked && measured < 2 * asked,
        "the runtime measured {measured} ns across a sleep of {asked} ns at half speed",
    );
    let genuine = report.after - report.before;
    assert!(
        genuine >= 3 * asked / 2,
        "the sleep took {genuine} ns of genuine time, so it never waited the second time",
    );
}

// A Go runtime built without cgo keeps its goroutine pointer in x28 on aarch64, and its signal
// handler takes the pointer from there. These programs read the clock while the process signals
// itself without pause, so signals land in the middle of reads through the synthetic vDSO.

/// How many wall-clock reads a signalled program makes: each is two calls through an entry.
const SIGNALLED_READS: &str = "20000";

/// Runs the signalled program under `library` with `env`, and checks it read the faked clock
/// through every signal.
fn signalled(library: PathBuf, env: &[(&str, &str)]) {
    let Some(program) = harness::go_program("go_clocks") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut run = Run::new(&program)
        .under_library(library)
        .arg("signals")
        .arg(SIGNALLED_READS)
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .within(std::time::Duration::from_secs(120));
    for (name, value) in env {
        run = run.env(name, value);
    }
    let report = run.run();

    assert!(report.nanos("go.signals.sent") > 0, "no signal was sent");
    assert_eq!(report.get("go.signals.reads"), SIGNALLED_READS);
    let now = report.nanos("go.now");
    let low = report.before - 2 * HOUR - SECOND;
    let high = report.after - 2 * HOUR + SECOND;
    assert!(
        (low..=high).contains(&now),
        "time.Now is {now}, not in {low}..={high}",
    );
}

/// The hook is code this library did not compile, so it may use x28: this one overwrites it and
/// spins, and a signal handled meanwhile would start the handler on that value.
#[test]
fn a_go_program_takes_signals_while_a_hook_overwrites_x28() {
    let hook = harness::hook_library("clobbers_x28");
    signalled(
        harness::library(),
        &[("LIBTIMEWARP_HOOK", &hook.to_string_lossy())],
    );
}

/// The library's own read path under a release build, which is where the compiler is free to
/// keep a value in x28.
#[cfg(target_arch = "aarch64")]
#[test]
fn a_go_program_takes_signals_while_reading_through_a_release_build() {
    signalled(harness::release_library(), &[]);
}

/// The pairs of readings the ticking mode printed.
fn ticks(printed: &str) -> Vec<[i64; 2]> {
    printed
        .lines()
        .filter_map(|line| line.strip_prefix("tick="))
        .filter_map(|values| {
            let mut numbers = values.split_whitespace().map(str::parse::<i64>);
            Some([numbers.next()?.ok()?, numbers.next()?.ok()?])
        })
        .collect()
}

fn wait_for_session(dir: &Path) -> PathBuf {
    let until = Nanos(now().0.saturating_add(5 * SECOND));
    loop {
        if let Some(path) = harness::session_files(dir).into_iter().next() {
            return path;
        }
        assert!(now() < until, "the program never created a session");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn now() -> Nanos {
    genuine_nanos(clock::MONOTONIC).unwrap_or(Nanos::ZERO)
}
