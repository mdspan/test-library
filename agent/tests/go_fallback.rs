//! The Go runtime faked through its own clock fallback, which is how FreeBSD reaches a runtime that
//! reads the clock without calling libc.
//!
//! With the timekeep auxiliary-vector entry withheld, `time.Now` and the monotonic clock
//! `time.Since` subtracts fall through to `runtime.fallback_walltime`/`fallback_nanotime`, whose
//! entries the library rewrites to jump into its own read path. So a Go target reads the faked clock
//! the same seqlock read flow every interposed libc name runs, with no lease file and no helper
//! process. These cases run a real Go runtime against that path, including with Realtime and
//! Monotonic at rates of their own and with a hook and the emulation on.
//!
//! `AT_TIMEKEEP` and the fallback routines are FreeBSD's, so these cases only apply there. A machine
//! with no Go toolchain builds no target and the cases say so rather than failing.
#![cfg(target_os = "freebsd")]
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
              does not see through a method taking `self` and suggests a drop that would not compile"
)]

mod harness;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use common::platform::{clock, genuine_nanos};
use common::session::layout::Keeper;
use common::time::{Family, Nanos};
use controller::Controller;

use harness::{HOUR, Run, only_session};

const SECOND: i64 = Nanos::SECOND;
const DAY: i64 = 24 * HOUR;

/// How far apart two readings taken microseconds apart may be before they are not the same clock.
/// Wide enough for a machine under load, and far below any faked value a case sets.
const TOGETHER_NS: i64 = 50 * 1_000_000;

/// The instant `arena_clock`'s `init` writes into the arena: 2020-01-01T00:00:00Z.
const ARENA_NS: i64 = 1_577_836_800_000_000_000;

#[test]
fn a_go_program_sees_the_faked_clock_through_the_fallback() {
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
fn the_fallback_session_keeps_no_lease_and_starts_no_helper() {
    let Some(program) = harness::go_program("go_clocks") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let report = Run::new(&program)
        .arg("now")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    // A faked reading says the fallback took over, so what follows is a live faked session rather
    // than a target left on genuine time.
    let now = report.nanos("go.now");
    assert!(now < report.before - HOUR, "the clock was not faked: {now}");

    // The session leaves no lease file, and no helper process outlives the target.
    let session = only_session(dir.path());
    assert!(
        lease_files(dir.path()).is_empty(),
        "a .lease file was left beside the session",
    );
    assert!(
        !helper_process_for(session.path()),
        "a helper process outlived the target it was meant to serve",
    );
}

#[test]
fn independent_realtime_and_monotonic_rates_reach_a_go_reader() {
    let Some(program) = harness::go_program("go_clocks") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let report = Run::new(&program)
        .arg("rates")
        .arg("40")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_REALTIME_RATE", "5")
        .env("LIBTIMEWARP_MONOTONIC_RATE", "1")
        .run();

    // A rate for each family, not one rate for the pair, and the creator publishes it without a
    // fault.
    let session = only_session(dir.path());
    assert_eq!(session.header().keeper, Keeper::Affine as u32);
    assert_eq!(
        session.header().fault(),
        None,
        "independent rates were refused"
    );

    let wall = report.nanos("go.rate.wall");
    let mono = report.nanos("go.rate.mono");
    assert!(wall > 0 && mono > 0, "wall {wall}, mono {mono}");
    // Realtime at five times speed and Monotonic at one, both read through the fallback: the wall
    // clock covers far more ground than the monotonic clock over the one genuine interval.
    assert!(
        wall >= 3 * mono,
        "wall moved {wall} ns against the monotonic {mono} ns, so the rates were not independent",
    );
}

#[test]
fn a_hook_runs_with_the_emulation_on_under_a_go_reader() {
    let Some(program) = harness::go_program("go_clocks") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let hook = harness::hook_library("arena_clock");
    let report = Run::new(&program)
        .arg("now")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_HOOK", format!("{}@4096", hook.display()))
        .env("LIBTIMEWARP_EMULATE_VDSO", "1")
        .run();

    // A hook with the fallback interposition on is not a bad value: the session carries the hook
    // and records no fault.
    let session = only_session(dir.path());
    assert_eq!(
        session.header().fault(),
        None,
        "a hook with the emulation on was refused",
    );
    let recorded = Path::new(std::str::from_utf8(session.header().hook_path()).unwrap());
    assert_eq!(recorded, hook);

    // The runtime's wall clock reached the hook through the fallback and read the arena instant.
    assert_eq!(report.nanos("go.now"), ARENA_NS);
}

#[test]
fn a_go_reader_under_cpu_profiling_still_reads_the_faked_clock() {
    let Some(program) = harness::go_program("go_clocks") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    // CPU profiling raises SIGPROF about a hundred times a second; the runtime's handler reads the
    // clock, so a sample taken during an in-flight read drives the shim's reentrancy guard onto
    // the genuine path. The program's own readings stay coherent all the same.
    let report = Run::new(&program)
        .arg("rates")
        .arg("200")
        .session_dir(dir.path())
        .env("TIMEWARP_TEST_GO_PROFILE", "1")
        .run();

    let wall = report.nanos("go.rate.wall");
    let mono = report.nanos("go.rate.mono");
    assert!(
        wall >= 100_000_000 && mono >= 100_000_000,
        "wall {wall}, mono {mono}: a read was lost under profiling",
    );
    // One rate for both families, so the two clocks covered about the same ground; a nested read
    // that corrupted a result slot would show up as a wild gap between them.
    assert!(
        wall < 3 * mono && mono < 3 * wall,
        "wall {wall} and mono {mono} diverged under profiling",
    );
}

#[test]
fn a_go_program_reads_genuine_time_when_the_session_asked_for_no_fallback() {
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
    assert!(lease_files(dir.path()).is_empty());
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
    // A build without the feature interposes no fallback routine and keeps no lease.
    assert!(lease_files(dir.path()).is_empty());
}

#[test]
fn a_live_change_reaches_a_go_reader_and_a_libc_reader_together() {
    let Some(program) = harness::go_program("go_pair") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    // A day ahead, so nothing but the transaction below reaches it; the transaction sets the wall
    // clock past it in one step, and the readings after carry that step.
    let deadline = harness::genuine_now() + DAY;

    let mut child = Command::new(&program)
        // More pairs than the case has time for, two hundred microseconds of faketime apart.
        .arg("1000000")
        .arg("200")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env(harness::PRELOAD, harness::library())
        .env("LIBTIMEWARP_SESSION_DIR", dir.path())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Drained as it is printed, so the program keeps reading the clock instead of blocking on a
    // full pipe while the transaction below lands.
    let mut printing = child.stdout.take().unwrap();
    let drained = std::thread::spawn(move || {
        let mut text = String::new();
        printing.read_to_string(&mut text).unwrap();
        text
    });

    let controller = Controller::attach(&wait_for_session(dir.path())).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    let mut transaction = controller.begin().unwrap();
    transaction
        .set_time(Family::Realtime, Nanos(deadline + SECOND))
        .unwrap();
    transaction.commit().unwrap();

    // Long enough after the change for many readings to carry it before the program is stopped.
    std::thread::sleep(Duration::from_millis(500));
    drop(child.kill());
    drop(child.wait());
    let printed = drained.join().unwrap();

    let pairs = readings(&printed);
    assert!(
        pairs.len() > 20,
        "only {} readings:\n{printed}",
        pairs.len()
    );
    // The two readers never disagree: both reach the one read path, so a change is published to
    // both at once.
    for &[through_fallback, through_libc] in &pairs {
        let gap = through_libc - through_fallback;
        assert!(
            gap.abs() < TOGETHER_NS,
            "the two readers are {gap} ns apart"
        );
    }
    // And the change reached both: the last readings are past the instant it set.
    let last = pairs[pairs.len() - 1];
    assert!(
        last[0] >= deadline,
        "the Go reader never reached the set time"
    );
    assert!(
        last[1] >= deadline,
        "the libc reader never reached the set time"
    );
}

/// Every `.lease` file beside the sessions in `dir`.
fn lease_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir.join("libtimewarp")) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "lease").then_some(path)
        })
        .collect()
}

/// Whether any process still carries `session` on its command line.
fn helper_process_for(session: &Path) -> bool {
    Command::new("pgrep")
        .arg("-f")
        .arg(session)
        .output()
        .is_ok_and(|out| !out.stdout.is_empty())
}

/// The pairs of readings `go_pair` printed: what the runtime read through the fallback, and what
/// libc answered beside it.
fn readings(printed: &str) -> Vec<[i64; 2]> {
    printed
        .lines()
        .filter_map(|line| line.strip_prefix("pair="))
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
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn now() -> Nanos {
    genuine_nanos(clock::MONOTONIC).unwrap_or(Nanos::ZERO)
}
