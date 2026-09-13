//! A controller changing a session while the processes in it read the clock: every reader sees all
//! of a transaction or none of it, on its next read, and never a step the spec forbids. And a
//! controller reading one: what a target left in the session, including what went wrong with it.
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
    clippy::significant_drop_tightening,
    reason = "a transaction is consumed by `commit` or `abort`, which is where its lock is \
              released; the lint does not see through a method taking `self` and suggests a drop \
              that would not compile"
)]

mod harness;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};

use common::platform::{clock, genuine_nanos};
use common::session::layout::{FaultCode, Header, Keeper, Params};
use common::session::{self, Setup};
use common::sync::read;
use common::time::{Family, Nanos, Rate};
use controller::{Code, Controller};

const SECOND: i64 = Nanos::SECOND;
const HOUR: i64 = 3_600 * SECOND;
const DAY: i64 = 24 * HOUR;

/// The most clock readings the ticking program spends reaching its deadline. Generous, because the
/// budget is what a failure runs out of rather than what a pass uses.
const BUDGET: i64 = 200_000;

/// Both cases here fork, and a fork while another thread of this process is inside a transaction
/// would hand the child the in-process half of the write lock already held. The lock file a reader
/// probes is named once per process, too, so two sessions at once would cross their probes.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
}

#[test]
fn a_program_reading_its_clocks_follows_every_transaction_and_never_steps_back() {
    let _one = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    // A day ahead, so nothing but the last transaction reaches it and the program stops exactly
    // when that one lands.
    let deadline = harness::genuine_now() + DAY;

    let mut child = Command::new(harness::program("ticking"))
        .arg(deadline.to_string())
        .arg(BUDGET.to_string())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env(harness::PRELOAD, harness::library())
        .env("LIBTIMEWARP_SESSION_DIR", dir.path())
        // The transactions below take the wall clock away from the elapsed families, which is a
        // pair the timekeep page could not carry.
        .envs(harness::separate_family_rates().iter().copied())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Drained as it is printed, so the program keeps reading the clock instead of blocking on a
    // full pipe while the transactions below run.
    let mut printing = child.stdout.take().unwrap();
    let drained = std::thread::spawn(move || {
        let mut text = String::new();
        printing.read_to_string(&mut text).unwrap();
        text
    });

    let controller = Controller::attach(&wait_for_session(dir.path())).unwrap();
    let families = [Family::Realtime, Family::Monotonic, Family::Boottime];

    // Three families made five times faster, in one transaction.
    let mut transaction = controller.begin().unwrap();
    for family in families {
        transaction.set_rate(family, 5.0).unwrap();
    }
    transaction.commit().unwrap();
    settle();

    // A shift of each, the wall clock by an hour and the two nondecreasing families by ten
    // seconds, which is a move forward and so one they take.
    let mut transaction = controller.begin().unwrap();
    transaction.shift(Family::Realtime, Nanos(HOUR)).unwrap();
    transaction
        .shift(Family::Monotonic, Nanos(10 * SECOND))
        .unwrap();
    transaction
        .shift(Family::Boottime, Nanos(10 * SECOND))
        .unwrap();
    transaction.commit().unwrap();
    settle();

    // And back down again: the wall clock at half speed, the other two at real speed.
    let mut transaction = controller.begin().unwrap();
    transaction.set_rate(Family::Realtime, 0.5).unwrap();
    transaction.set_rate(Family::Monotonic, 1.0).unwrap();
    transaction.set_rate(Family::Boottime, 1.0).unwrap();
    transaction.commit().unwrap();
    settle();

    // The last one puts the wall clock past the value the program is waiting for, so the program
    // stopping is itself the proof that the change reached it.
    let mut transaction = controller.begin().unwrap();
    transaction
        .set_time(Family::Realtime, Nanos(deadline + SECOND))
        .unwrap();
    transaction.commit().unwrap();

    let status = child.wait().unwrap();
    let printed = drained.join().unwrap();
    assert!(
        status.success(),
        "the program never saw the wall clock the last transaction set",
    );

    let ticks = parse(&printed);
    assert!(ticks.len() > 50, "only {} readings", ticks.len());
    for (family, index) in [("realtime", 0), ("monotonic", 1), ("boottime", 2)] {
        for pair in ticks.windows(2) {
            assert!(
                pair[1][index] >= pair[0][index],
                "{family} stepped back from {} to {}",
                pair[0][index],
                pair[1][index],
            );
        }
    }

    let (first, last) = (ticks[0], ticks[ticks.len() - 1]);
    assert!(last[0] - first[0] >= DAY, "the wall clock never got there");
    assert!(last[1] - first[1] >= 10 * SECOND, "Monotonic never moved");
    assert!(last[2] - first[2] >= 10 * SECOND, "Boottime never moved");

    // Two deliberate steps of an hour or more reached the program: the shift and the set_time.
    // Everything else it saw was the clock running, which at five times speed over a wait of a
    // fifth of a millisecond is a millisecond at a time.
    let steps = ticks
        .windows(2)
        .filter(|pair| pair[1][0] - pair[0][0] >= HOUR)
        .count();
    assert!(steps >= 2, "{steps} steps of an hour or more, wanted two");
}

/// The rates one transaction gives the three families, alternating between the two. Every family
/// takes a different one, and no rate appears in both, so a copy mixing two transactions disagrees
/// with each of them.
const GENERATIONS: [[f64; 3]; 2] = [[2.0, 3.0, 4.0], [5.0, 6.0, 7.0]];

/// Readers per case, enough to have several in flight at once on any machine that runs the tests.
const READERS: usize = 6;

/// How long the readers keep reading, and how long the controller keeps publishing inside that.
const RUN_NS: i64 = 400 * 1_000_000;
const WRITE_NS: i64 = 200 * 1_000_000;

/// What a reader answers with, so a failure names itself without allocating or unwinding.
mod verdict {
    pub(crate) const WHOLE: i32 = 0;
    pub(crate) const MIXED: i32 = 2;
    pub(crate) const TOO_FEW_READS: i32 = 3;
}

#[test]
fn a_three_family_transaction_is_observed_whole() {
    let _one = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    // Every family takes a rate of its own here, which is a set the timekeep page could not
    // carry, so this session keeps none.
    let session = session::create(&Setup {
        emulate_vdso: false,
        ..Setup::plain(dir.path().join("libtimewarp"))
    })
    .unwrap();
    let header = session.header();
    let controller = Controller::attach(session.path()).unwrap();

    let rate = |value: f64| Rate::from_f64(value).unwrap();
    // The session starts at unity, so that triple is a whole copy too until the first commit.
    let whole = [
        [Rate::UNITY; 3],
        GENERATIONS[0].map(rate),
        GENERATIONS[1].map(rate),
    ];

    let until = deadline(RUN_NS);
    let readers: Vec<_> = (0..READERS)
        .map(|_| fork_child(|| watching(header, &whole, until)))
        .collect();

    let stop = deadline(WRITE_NS);
    let mut published = 0;
    while now() < stop {
        let mut transaction = controller.begin().unwrap();
        for (family, &value) in [Family::Realtime, Family::Monotonic, Family::Boottime]
            .into_iter()
            .zip(GENERATIONS[published % GENERATIONS.len()].iter())
        {
            transaction.set_rate(family, value).unwrap();
        }
        transaction.commit().unwrap();
        published += 1;
    }
    assert!(published > 10, "only {published} transactions got through");

    for pid in readers {
        assert_eq!(reap(pid), verdict::WHOLE, "reader {pid}");
    }
}

/// Reads the three families' rates through the seqlock until `until`, and answers whether every
/// copy it saw was one somebody published.
fn watching(header: &Header, whole: &[[Rate; 3]; 3], until: Nanos) -> i32 {
    let mut reads: u64 = 0;
    loop {
        let params = read(header, Params::load);
        let seen = [
            params.realtime.line.rate,
            params.monotonic.line.rate,
            params.boottime.line.rate,
        ];
        if !whole.contains(&seen) {
            return verdict::MIXED;
        }
        reads += 1;
        if reads.is_multiple_of(32) && now() >= until {
            break;
        }
    }
    // A reader that never got going would pass every check above without checking anything.
    if reads < 1_000 {
        return verdict::TOO_FEW_READS;
    }
    verdict::WHOLE
}

/// The one session file a run of the ticking program leaves in `dir`, waited for: the program
/// creates it in its constructor, which is a moment after the spawn returns.
fn wait_for_session(dir: &Path) -> PathBuf {
    let until = deadline(5 * SECOND);
    loop {
        if let Some(path) = harness::session_files(dir).into_iter().next() {
            return path;
        }
        assert!(now() < until, "the program never created a session");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// Long enough for the program to print several readings against what the last transaction
/// published, so every change has a before and an after in the stream.
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(30));
}

fn now() -> Nanos {
    genuine_nanos(clock::MONOTONIC).unwrap_or(Nanos::ZERO)
}

fn deadline(after: i64) -> Nanos {
    Nanos(now().0.saturating_add(after))
}

#[test]
fn a_controller_reads_the_fault_out_of_a_deliberately_misconfigured_session() {
    let _one = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    // A rate no rate can be. The creator cannot set up what it was asked for, so it publishes a
    // session on the null keeper with no hook and records the fault in it before any child can
    // attach.
    let report = harness::Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_RATE", "-1")
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();
    report.assert_wall_clock("probe", 0);

    let path = PathBuf::from(report.get("probe.shared"));
    let controller = Controller::attach(&path).unwrap();
    let info = controller.info();

    // What the controller learns is exactly what the chapter says it should: the session fell back
    // to genuine time, with no hook, and the fault names the process that hit it.
    assert_eq!(info.keeper, Keeper::Null);
    assert_eq!(info.hook, None);
    let fault = info.fault.expect("the creator recorded what went wrong");
    assert_eq!(fault.code, FaultCode::BadValue);
    assert_eq!(fault.pid, report.get("probe.pid").parse::<u32>().unwrap());

    // A null session still reads and still refuses every change.
    let showing = controller.get(Family::Realtime).unwrap();
    assert!((showing.0 - harness::genuine_now()).abs() < SECOND);
    assert_eq!(
        controller.begin().unwrap_err().code(),
        Code::UnsupportedForKeeper
    );
}

#[test]
fn a_controller_reads_the_hook_a_session_carries() {
    let _one = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    let hook = harness::hook_library("setter_log");
    let report = harness::Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_HOOK", format!("{}@4096", hook.display()))
        .run();

    let controller = Controller::attach(Path::new(report.get("probe.shared"))).unwrap();
    let info = controller.info();
    assert_eq!(info.keeper, Keeper::Affine);
    assert_eq!(info.hook.as_deref(), Some(hook.as_path()));
    assert_eq!(info.fault, None);
    // A hook sits in front of the keeper and changes none of this, so the families read as they
    // were set up and a transaction still opens.
    assert_eq!(info.realtime.rate, Rate::UNITY);
    assert_eq!(info.realtime.timebase, Some(clock::REALTIME));
    controller.begin().unwrap().abort();
}

/// The `tick=` lines of a run, each the wall clock, Monotonic and Boottime as the program read
/// them.
fn parse(printed: &str) -> Vec<[i64; 3]> {
    printed
        .lines()
        .filter_map(|line| line.strip_prefix("tick="))
        .filter_map(|reading| {
            let mut nanos = reading.split(' ').map(str::parse::<i64>);
            Some([
                nanos.next()?.ok()?,
                nanos.next()?.ok()?,
                nanos.next()?.ok()?,
            ])
        })
        .collect()
}

/// Runs `body` in a child process, which leaves through `_exit` with whatever it returned: a panic
/// there would unwind into a test harness that belongs to the parent.
fn fork_child(body: impl FnOnce() -> i32) -> libc::pid_t {
    // Safety: `fork` is called from a case holding `ONE_AT_A_TIME`, so no other case of this file
    // is inside a transaction.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
    if pid > 0 {
        return pid;
    }
    let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or(101);
    // Safety: `_exit` ends the child, so nothing here can be reached afterwards.
    unsafe { libc::_exit(code) }
}

fn reap(pid: libc::pid_t) -> i32 {
    let mut status = 0;
    // Safety: `pid` is a child of ours and `status` is a live buffer.
    unsafe { libc::waitpid(pid, &raw mut status, 0) };
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        128 + libc::WTERMSIG(status)
    }
}
