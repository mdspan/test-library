//! The timekeep page, the updater that keeps it fresh, and the Go runtime both exist for.
//!
//! A target that reads the clock without calling libc finds the page the auxiliary vector names and
//! does the arithmetic itself. These cases run two such targets: one in C that walks the page by
//! hand the way the Go runtime does, so the mechanism can be checked step by step, and one in Go, so
//! what a real runtime does with it is checked too.
//!
//! `AT_TIMEKEEP` and the page behind it are FreeBSD's, so these cases only apply there.
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
    reason = "a transaction is consumed by `commit` or `abort`, which is where its lock is \
              released; the lint does not see through a method taking `self` and suggests a drop \
              that would not compile"
)]

mod harness;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use common::platform::{clock, genuine_nanos};
use common::session::naming;
use common::time::{Family, Nanos};
use controller::Controller;

use harness::{HOUR, Report, Run};

const SECOND: i64 = Nanos::SECOND;
const DAY: i64 = 24 * HOUR;

/// How far apart two readings taken microseconds apart may be before they are not the same clock.
/// Wide enough for a machine under load, and far below any faked value a case sets.
const TOGETHER_NS: i64 = 50 * 1_000_000;

/// The most readings the ticking program spends reaching its deadline.
const BUDGET: i64 = 200_000;

/// How long a case waits for an updater to notice that nothing maps its session any more. The walk
/// runs every few seconds, so this is several of those over.
const UNTIL_IT_LEAVES: Duration = Duration::from_secs(40);

/// Whether this machine's own kernel page names a counter the C reader can take.
///
/// It always does on the hardware we support; a machine whose kernel chose some other timecounter
/// would leave the page unreadable by anything but libc, and a case that needs a genuine reading
/// through the page says so rather than failing.
fn page_readable(report: &Report) -> bool {
    report.get("page.ok") == "1"
}

#[test]
fn a_target_reading_through_the_page_sees_the_faked_clock() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("timekeep")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    assert_eq!(report.get("page.found"), "1", "no entry named a page");
    assert_eq!(report.get("page.enabled"), "1", "the page carries nothing");
    assert_eq!(report.get("page.ok"), "1", "the page reads no clock");
    report.assert_wall_clock("page", -2 * HOUR);
    report.assert_wall_clock("libc", -2 * HOUR);
}

#[test]
fn the_page_reader_and_the_libc_reader_in_one_process_agree() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("timekeep")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_TIME", "2020-01-01T00:00:00Z")
        .run();

    let gap = report.nanos("libc.clock_gettime") - report.nanos("page.clock_gettime");
    assert!(
        gap.abs() < TOGETHER_NS,
        "the two readers are {gap} ns apart"
    );
    // 2020-01-01T00:00:00Z, which is what both of them were set to.
    assert_eq!(report.nanos("page.time"), 1_577_836_800);
    // And the elapsed clock the page also serves moves with them.
    let elapsed = report.nanos("libc.monotonic") - report.nanos("page.monotonic");
    assert!(
        elapsed.abs() < TOGETHER_NS,
        "the elapsed clocks are {elapsed} ns apart",
    );
}

#[test]
fn turning_the_variable_off_leaves_the_kernel_page_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("timekeep")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .env("LIBTIMEWARP_EMULATE_VDSO", "0")
        .run();

    // Interposition is untouched, so a libc reader is faked as usual.
    report.assert_wall_clock("libc", -2 * HOUR);
    if !page_readable(&report) {
        return;
    }
    // And the page reader has the kernel's own page, so it reads genuine time.
    report.assert_wall_clock("page", 0);
    // With no page of ours, the session keeps no lease either.
    let session = harness::session_files(dir.path())
        .into_iter()
        .next()
        .unwrap();
    assert!(!naming::lease_path(&session).exists());
}

#[test]
fn one_page_serves_the_session_and_a_forked_child_inherits_it_working() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("timekeep")
        .arg("fork")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    // The child inherited the address space, so it inherited the page and the entry naming it, with
    // nothing of ours to do.
    for tag in ["page", "child_page", "libc", "child_libc"] {
        report.assert_wall_clock(tag, -2 * HOUR);
    }
    assert_eq!(report.get("child_page.ok"), "1");
}

#[test]
fn an_attacher_follows_the_session_rather_than_its_own_environment() {
    let dir = tempfile::tempdir().unwrap();
    // The creator is the wrapper, which reads the variable; the target it execs attaches, and an
    // attacher reads none of the setup variables.
    let off = Run::prog("timekeep")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .env("LIBTIMEWARP_EMULATE_VDSO", "0")
        .through(&sets("LIBTIMEWARP_EMULATE_VDSO=1"))
        .run();
    off.assert_wall_clock("libc", -2 * HOUR);
    if page_readable(&off) {
        off.assert_wall_clock("page", 0);
    }

    let dir = tempfile::tempdir().unwrap();
    let on = Run::prog("timekeep")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .env("LIBTIMEWARP_EMULATE_VDSO", "1")
        .through(&sets("LIBTIMEWARP_EMULATE_VDSO=0"))
        .run();
    on.assert_wall_clock("libc", -2 * HOUR);
    on.assert_wall_clock("page", -2 * HOUR);
}

/// A wrapper that execs the target with one more variable set, for a case where what the target's
/// own environment says has to lose to what the session recorded.
fn sets(variable: &str) -> Vec<String> {
    vec!["/usr/bin/env".to_owned(), variable.to_owned()]
}

#[test]
fn a_session_with_the_page_on_is_left_with_an_updater_holding_its_lease() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("timekeep").session_dir(dir.path()).run();

    // The fork execs and the intermediate is reaped on the spot, so the target never saw a child of
    // its own however it looked.
    assert_eq!(report.get("page.children"), "0", "a stray child was left");

    let session = harness::session_files(dir.path())
        .into_iter()
        .next()
        .unwrap();
    let lease = naming::lease_path(&session);
    assert!(lease.exists(), "a session with the page on keeps a lease");
    assert!(
        lease_held(&lease),
        "nothing holds the lease, so no updater was started",
    );
}

#[test]
fn an_updater_leaves_when_nothing_maps_its_session_and_the_next_attach_starts_another() {
    let dir = tempfile::tempdir().unwrap();
    let first = Run::prog("timekeep").session_dir(dir.path()).run();
    assert_eq!(first.get("page.ok"), "1");

    let session = harness::session_files(dir.path())
        .into_iter()
        .next()
        .unwrap();
    let lease = naming::lease_path(&session);
    assert!(lease_held(&lease));

    // Nothing maps the session once the target has gone. Descriptors play no part, so what the
    // updater is waiting on is the mapping disappearing with the process.
    let left = wait_until(UNTIL_IT_LEAVES, || !lease_held(&lease));
    assert!(
        left,
        "the updater is still holding the lease long after its session was left",
    );

    // And the next process to attach finds the lease released and starts one.
    let attached = Run::prog("timekeep")
        .session_dir(dir.path())
        .env("LIBTIMEWARP__SHARED_FILE", session.to_string_lossy())
        .run();
    assert_eq!(attached.get("page.ok"), "1");
    assert!(
        wait_until(Duration::from_secs(10), || lease_held(&lease)),
        "attaching to a session whose updater had gone started no new one",
    );
}

#[test]
fn the_interval_is_read_at_creation_and_recorded_for_whoever_respawns_the_updater() {
    let dir = tempfile::tempdir().unwrap();
    Run::prog("timekeep")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_TIMEKEEP_INTERVAL", "3ms")
        .run();

    let session = harness::only_session(dir.path());
    assert_eq!(session.header().timekeep_interval, 3_000_000);
}

#[test]
fn a_hook_with_the_page_left_on_is_a_bad_value() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let hook = harness::hook_library("arena_clock");
    let report = Run::prog("timekeep")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_HOOK", format!("{}@4096", hook.display()))
        // The harness turns the page off for a case that loads a hook; this one is about what
        // happens when it is left on.
        .env("LIBTIMEWARP_EMULATE_VDSO", "1")
        .env("LIBTIMEWARP_LOG", log.to_string_lossy())
        .run();

    // Genuine, never half-faked: no hook, and the clock the program reads is the machine's own.
    report.assert_wall_clock("libc", 0);
    let written = harness::log_of(&log);
    assert!(
        written.contains("LIBTIMEWARP_HOOK: "),
        "nothing said the hook could not be used: {written}",
    );

    // And a controller opening the session reads the fault the creator recorded in it.
    let session = harness::only_session(dir.path());
    assert!(session.header().fault().is_some(), "no fault was recorded");
}

#[test]
fn a_live_change_reaches_the_page_reader_and_the_libc_reader_together() {
    let dir = tempfile::tempdir().unwrap();
    // A day ahead, so nothing but the transaction below reaches it and the program stops exactly
    // when that one lands. The program stops only when both of its readers have passed it.
    let deadline = harness::genuine_now() + DAY;

    let mut child = Command::new(harness::program("timekeep"))
        .arg("tick")
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
    std::thread::sleep(Duration::from_millis(30));
    let mut transaction = controller.begin().unwrap();
    transaction
        .set_time(Family::Realtime, Nanos(deadline + SECOND))
        .unwrap();
    transaction.commit().unwrap();

    let status = child.wait().unwrap();
    let printed = drained.join().unwrap();
    if printed.contains("tick.unreadable=1") {
        return;
    }
    assert!(
        status.success(),
        "a reader never saw the wall clock the transaction set:\n{printed}",
    );

    let ticks = ticks(&printed);
    assert!(ticks.len() > 20, "only {} readings", ticks.len());
    // The writer republishes the page itself, so the change reaches a page reader within a tick or
    // two rather than at the updater's next heartbeat.
    for &[through_page, through_libc] in &ticks {
        let gap = through_libc - through_page;
        assert!(
            gap.abs() < TOGETHER_NS,
            "the two readers are {gap} ns apart"
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

// The Go runtime reads every clock without libc, so what it sees is the substituted page alone.

#[test]
fn a_go_program_sees_the_faked_clock_through_the_timekeep_page() {
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
    // The runtime measures its own spans on the monotonic clock, which the page also serves.
    let since = report.nanos("go.since");
    assert!(since >= 1_000_000, "time.Since measured {since} ns");
}

#[test]
fn a_go_program_reads_genuine_time_when_the_session_asked_for_no_page() {
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
    // And a build without the feature keeps no lease and no updater beside the session.
    let session = harness::session_files(dir.path())
        .into_iter()
        .next()
        .unwrap();
    assert!(!naming::lease_path(&session).exists());
}

/// Whether anybody holds the lease on `path`, asked of the kernel the way every probe asks.
fn lease_held(path: &Path) -> bool {
    use std::os::fd::AsRawFd as _;

    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut request = common::sync::lock_request();
    // Safety: the descriptor is open on the lease file and `request` is a live `flock`.
    let answered =
        unsafe { common::platform::probe_write_lock(file.as_raw_fd(), &raw mut request) } == 0;
    answered && i64::from(request.l_type) != i64::from(libc::F_UNLCK)
}

/// Spins until `settled` answers true, or the bound runs out.
fn wait_until(bound: Duration, settled: impl Fn() -> bool) -> bool {
    let until = std::time::Instant::now() + bound;
    while std::time::Instant::now() < until {
        if settled() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    settled()
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
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn now() -> Nanos {
    genuine_nanos(clock::MONOTONIC).unwrap_or(Nanos::ZERO)
}
