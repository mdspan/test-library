//! The seqlock across processes: readers spinning on the clock while a writer changes the rate
//! underneath them, and writers killed inside a section.
//!
//! Every reader is a process of its own, forked from the one that made the session, so the
//! parameters they read really do live in shared memory and the writer they race really can be
//! killed. Children never panic and never allocate on the checking path: they return an exit code
//! and leave through `_exit`, since the test harness around them is the parent's, not theirs.
//!
//! Creating a session reads a genuine clock and the system's randomness, so this needs a
//! filled-in platform arm.
#![cfg(any(target_os = "linux", target_os = "freebsd"))]

use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard, PoisonError};

use common::platform::{clock, default_timebase, genuine_nanos};
use common::session::layout::{Header, Params, ParamsValue, WriterState};
use common::session::{self, Session, SessionError, Setup, clock_now, naming};
use common::sync::{WriteLock, begin, lock_request, probe_held, read};
use common::time::{Family, Nanos, Rate};

/// The clock a family tracks by default here. FreeBSD picks between two columns of the family
/// table by the running kernel's version, so a case that names a family's clock asks for it rather
/// than spelling it.
fn timebase_of(family: Family) -> libc::clockid_t {
    default_timebase(family).unwrap_or(clock::REALTIME)
}

/// Readers per case. Enough to have several in flight at once on any machine that runs the tests.
const READERS: usize = 6;

/// How long the readers keep reading. Long enough to span a whole run of publishes, or a kill and
/// the repair after it.
const RUN_NS: i64 = 400 * 1_000_000;

/// How long the writer keeps publishing, inside the readers' run.
const WRITE_NS: i64 = 200 * 1_000_000;

/// The rates the writer alternates between. Far apart, so a value computed from a mix of old and
/// new words is nowhere near one computed from either.
const RATES: [f64; 2] = [1.0, 50.0];

/// What a reader answers with, so a failure names itself without allocating or unwinding.
mod verdict {
    pub(crate) const WHOLE: i32 = 0;
    pub(crate) const NO_READING: i32 = 2;
    pub(crate) const WENT_BACKWARDS: i32 = 3;
    pub(crate) const TORN: i32 = 4;
    pub(crate) const TOO_FEW_READS: i32 = 5;
    pub(crate) const WRONG_VALUE: i32 = 6;
}

/// The lock file a reader probes is named once per process, so a case whose readers probed another
/// case's lock file would wait on a writer that has nothing to do with them. Taken for the length
/// of every case here, poison and all: a case that already failed should not turn the next one into
/// a second failure.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Fixture {
    _dir: tempfile::TempDir,
    session: Session,
}

fn fixture() -> Result<Fixture, SessionError> {
    let dir = tempfile::tempdir()?;
    let session = session::create(&Setup::plain(dir.path().join("libtimewarp")))?;
    Ok(Fixture { _dir: dir, session })
}

impl Fixture {
    fn header(&self) -> &Header {
        self.session.header()
    }

    fn lock_path(&self) -> std::path::PathBuf {
        naming::lock_path(self.session.path())
    }
}

/// What a rate change publishes: every family re-anchored at the W5 instant, so the old and new
/// lines meet there, and every rate word carrying the same rate.
fn at_rate(
    anchor: impl Fn(libc::clockid_t) -> Option<Nanos>,
    live: ParamsValue,
    rate: Rate,
) -> ParamsValue {
    let mut next = live;
    for family in next.lines_mut() {
        let Some(now) = anchor(family.timebase) else {
            continue;
        };
        family.line.fake_anchor = family.line.project(now);
        family.line.timebase_anchor = now;
        family.line.rate = rate;
    }
    next.cpu_rate = rate;
    next
}

/// Whether a copy is one a writer could have published: this session's writers set every rate word
/// to the same rate, so a copy mixing old and new words shows them disagreeing.
fn whole(params: &ParamsValue) -> bool {
    params
        .lines()
        .iter()
        .all(|family| family.line.rate == params.cpu_rate)
}

fn now() -> Nanos {
    genuine_nanos(timebase_of(Family::Monotonic)).unwrap_or(Nanos::ZERO)
}

fn deadline(after: i64) -> Nanos {
    Nanos(now().0.saturating_add(after))
}

/// Reads the clock and the parameters until `until`, checking both on every pass.
fn reading_until(header: &Header, until: Nanos, expected: Option<ParamsValue>) -> i32 {
    let mut last = Nanos(i64::MIN);
    let mut reads: u64 = 0;
    loop {
        let Some(monotonic) = clock_now(header, timebase_of(Family::Monotonic)) else {
            return verdict::NO_READING;
        };
        if monotonic < last {
            return verdict::WENT_BACKWARDS;
        }
        last = monotonic;

        let params = read(header, Params::load);
        if !whole(&params) {
            return verdict::TORN;
        }
        if expected.is_some_and(|want| want != params) {
            return verdict::WRONG_VALUE;
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

fn fork_child(body: impl FnOnce() -> i32) -> libc::pid_t {
    // Safety: `fork` is called from a test that has spawned no threads of its own.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
    if pid > 0 {
        return pid;
    }
    // A panic here would unwind into the harness's machinery, which belongs to the parent.
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

fn all_whole(readers: Vec<libc::pid_t>) {
    for pid in readers {
        assert_eq!(reap(pid), verdict::WHOLE, "reader {pid}");
    }
}

fn spawn_readers(header: &Header, until: Nanos, expected: Option<ParamsValue>) -> Vec<libc::pid_t> {
    (0..READERS)
        .map(|_| fork_child(|| reading_until(header, until, expected)))
        .collect()
}

/// A pipe the child says "I am inside the section" down, so the kill lands where the test means it
/// to rather than wherever the scheduler happened to leave it.
struct Doorbell {
    read_end: libc::c_int,
    write_end: libc::c_int,
}

impl Doorbell {
    fn new() -> Self {
        let mut ends = [0; 2];
        // Safety: `ends` is a live pair of descriptors for `pipe` to fill.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        Self {
            read_end: ends[0],
            write_end: ends[1],
        }
    }

    fn ring(&self) {
        let byte = 1u8;
        // Safety: the descriptor is ours and the buffer is one live byte.
        unsafe { libc::write(self.write_end, std::ptr::from_ref(&byte).cast(), 1) };
    }

    fn wait(&self) {
        let mut byte = 0u8;
        // Safety: the descriptor is ours and the buffer is one live byte.
        let got = unsafe { libc::read(self.read_end, std::ptr::from_mut(&mut byte).cast(), 1) };
        assert_eq!(got, 1, "the writer never reached the section");
    }
}

impl Drop for Doorbell {
    fn drop(&mut self) {
        for fd in [self.read_end, self.write_end] {
            // Safety: both descriptors are ours and nothing else holds them.
            unsafe { libc::close(fd) };
        }
    }
}

/// Kills `pid` and waits for the kernel to finish with it, so the write lock it held is released
/// before the caller looks.
fn kill_and_reap(pid: libc::pid_t) {
    // Safety: `pid` is a child of ours.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    assert_eq!(reap(pid), 128 + libc::SIGKILL);
}

/// Blocks until something kills this process. Every maskable signal is blocked inside a write
/// section, so only the kill gets through.
fn wait_to_be_killed() -> ! {
    loop {
        // Safety: `pause` waits for a signal and touches nothing of ours.
        unsafe { libc::pause() };
    }
}

/// What the kernel fills in when asked about a lock another process holds.
fn probe_of(lock_path: &Path) -> libc::flock {
    // An empty path names nothing, so a name with a NUL in it fails the open below rather than
    // here. No path a case builds has one.
    let path = std::ffi::CString::new(lock_path.as_os_str().as_bytes()).unwrap_or_default();
    // Safety: `path` is a live NUL-terminated path.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    assert!(fd >= 0, "open: {}", std::io::Error::last_os_error());
    let mut request = lock_request();
    // Safety: `fd` is open on the lock file and `request` is a live `flock`.
    let answered = unsafe { common::platform::probe_write_lock(fd, &raw mut request) };
    // Safety: we opened this descriptor and nothing else holds it.
    unsafe { libc::close(fd) };
    assert_eq!(answered, 0, "{}", std::io::Error::last_os_error());
    request
}

/// One probe parser serves all three platforms: a held lock is a `l_type` other than `F_UNLCK`
/// with a `l_pid` of -1, which is what a lock owned by an open file description answers to.
#[test]
fn a_held_lock_answers_the_probe_the_same_way_wherever_it_is_asked() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let lock_path = fixture.lock_path();
    assert_eq!(probe_held(), Some(false));

    // The holder is a process of its own, so this really is the kernel's answer rather than ours.
    let doorbell = Doorbell::new();
    let holder = fork_child(|| {
        let _lock = WriteLock::acquire(&lock_path).unwrap();
        doorbell.ring();
        wait_to_be_killed()
    });
    doorbell.wait();

    let answered = probe_of(&lock_path);
    assert_ne!(i64::from(answered.l_type), i64::from(libc::F_UNLCK));
    assert_eq!(answered.l_pid, -1, "the answer named a pid");
    assert_eq!(probe_held(), Some(true));

    // And death releases it, which is the whole reason a reader may ask.
    kill_and_reap(holder);
    assert_eq!(probe_held(), Some(false));
    assert_eq!(
        i64::from(probe_of(&lock_path).l_type),
        i64::from(libc::F_UNLCK)
    );
}

#[test]
fn readers_hold_up_while_a_writer_changes_the_rate_under_them() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let lock_path = fixture.lock_path();
    let readers = spawn_readers(fixture.header(), deadline(RUN_NS), None);

    let until = deadline(WRITE_NS);
    let mut published = 0;
    while now() < until {
        let rate = Rate::from_f64(RATES[published % RATES.len()]).unwrap();
        publish_rate(fixture.header(), &lock_path, rate).unwrap();
        published += 1;
    }
    assert!(published > 10, "only {published} publishes got through");

    all_whole(readers);
    assert_eq!(
        fixture.header().seqlock.sequence.load(Ordering::Acquire) % 2,
        0
    );
}

/// One rate change, W1 through W12.
fn publish_rate(header: &Header, lock_path: &Path, rate: Rate) -> std::io::Result<()> {
    let lock = begin(header, lock_path)?;
    let published = lock.publish::<()>(header, None, |anchor, live| {
        Ok(at_rate(|c| anchor.of(c), live, rate))
    });
    drop(lock); // W12
    published.map_err(|()| std::io::Error::other("a rate change nothing refuses"))
}

#[test]
fn a_writer_killed_before_its_shadow_was_whole_leaves_the_live_words_standing() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let header = fixture.header();
    let lock_path = fixture.lock_path();
    let live = header.live.load();
    let readers = spawn_readers(header, deadline(RUN_NS), Some(live));

    // A writer that dies at W6: the counter is odd, the flag has never gone up, and the live words
    // are untouched, so they are the copy every reader and the next writer must land on.
    let doorbell = Doorbell::new();
    let writer = fork_child(|| {
        let lock = begin(header, &lock_path).unwrap();
        lock.publish::<()>(header, None, |_, _| {
            doorbell.ring();
            wait_to_be_killed()
        })
        .unwrap();
        0
    });
    doorbell.wait();
    assert_eq!(header.seqlock.sequence.load(Ordering::Acquire) % 2, 1);
    kill_and_reap(writer);

    // The fallback turns the abandoned section into a value rather than a hang.
    assert_eq!(read(header, Params::load), live);

    let _lock = begin(header, &lock_path).unwrap();
    assert_eq!(header.live.load(), live);
    assert_eq!(header.seqlock.sequence.load(Ordering::Acquire) % 2, 0);
    assert_eq!(header.seqlock.writer_state.load(Ordering::Acquire), 0);

    all_whole(readers);
}

#[test]
fn a_writer_killed_with_its_shadow_whole_hands_everyone_the_value_it_meant_to_publish() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let header = fixture.header();
    let lock_path = fixture.lock_path();
    // Anchored at an instant taken here rather than inside a section, which is safe only because
    // this is the value a dead writer is about to be given credit for and the rate goes up, so no
    // reader sees the clock retreat.
    let intended = at_rate(
        genuine_nanos,
        header.live.load(),
        Rate::from_f64(RATES[1]).unwrap(),
    );
    // The readers span the death, so both the old copy and the intended one are values they may
    // legitimately return; what none of them may return is a mix of the two.
    let readers = spawn_readers(header, deadline(RUN_NS), None);

    // The aftermath of a death midway through W9, written out by hand: the shadow is whole, the
    // flag is up, and the live words are half old and half new. `publish` passes through this
    // state in a few nanoseconds, which no kill can be aimed at, so the child holds the write lock
    // and puts the seqlock in it by the same steps in the same order.
    let doorbell = Doorbell::new();
    let writer = fork_child(|| {
        let _lock = begin(header, &lock_path).unwrap();
        let seq = &header.seqlock;
        let pid = std::process::id();
        seq.writer_state
            .store(WriterState::writing(pid).word(), Ordering::Release); // W2
        seq.sequence.store(1, Ordering::Release); // W3
        header.shadow.store(&intended); // W7
        seq.writer_state
            .store(WriterState::published(pid).word(), Ordering::Release); // W8
        header
            .live
            .realtime
            .store(intended.realtime.line, intended.realtime.timebase); // W9, torn
        doorbell.ring();
        wait_to_be_killed()
    });
    doorbell.wait();
    kill_and_reap(writer);

    // Flag set, so the fallback follows it to the whole copy.
    assert_eq!(read(header, Params::load), intended);

    // And the next writer's repair branches on the same flag, so the two converge on one value.
    let _lock = begin(header, &lock_path).unwrap();
    assert_eq!(header.live.load(), intended);
    assert_eq!(header.seqlock.sequence.load(Ordering::Acquire) % 2, 0);

    all_whole(readers);
}
