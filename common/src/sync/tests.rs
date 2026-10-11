//! The two flows against a real session file. The protocol's interleavings are model-checked
//! elsewhere; these cases pin the steps to the words they move and the calls they make.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{Anchor, WRITE_SETTLE_NS, WriteLock, begin, probe_held, read};
use crate::platform::{self, clock};
use crate::session::layout::{Header, Params, ParamsValue, WriterState};
use crate::session::{self, Session, SessionError, Setup, naming};
use crate::time::{Nanos, Rate};

/// The lock file a reader probes is named once per process, and a write lock is one per process
/// too, so two of these cases running at once would ask about each other's session. Taken for the
/// length of every case here, poison and all: a case that already failed should not turn the next
/// one into a second failure.
///
/// Creating or attaching to a session names the probe's file too, so a case elsewhere in the crate
/// that does either takes this around the call.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

pub(crate) fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A session of its own, in a directory that goes away with it.
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

    fn lock_path(&self) -> PathBuf {
        naming::lock_path(self.session.path())
    }

    fn counter(&self) -> u64 {
        self.header().seqlock.sequence.load(Ordering::Acquire)
    }

    /// The aftermath of a writer that died inside a section, as the seqlock records it.
    fn abandon(&self, shadow_complete: bool, shadow: &ParamsValue) {
        let seq = &self.header().seqlock;
        self.header().shadow.store(shadow);
        seq.writer_state.store(
            WriterState {
                pid: 4321,
                shadow_complete,
            }
            .word(),
            Ordering::Release,
        );
        seq.sequence.store(1, Ordering::Release);
    }
}

/// A copy nothing in a fresh session holds, so a reader returning it can only have read it.
fn distinct(from: ParamsValue) -> ParamsValue {
    let mut value = from;
    value.epoch = from.epoch.wrapping_add(1);
    for family in value.lines_mut() {
        family.line.fake_anchor = Nanos(family.line.fake_anchor.0 + 3600 * Nanos::SECOND);
    }
    value
}

/// What a rate change publishes: every family re-anchored at the W5 instant, so the old and new
/// lines meet there, and then run at the new rate.
fn at_rate(anchor: &Anchor, live: ParamsValue, rate: Rate) -> ParamsValue {
    let mut next = live;
    for family in next.lines_mut() {
        let Some(now) = anchor.of(family.timebase) else {
            continue;
        };
        family.line.fake_anchor = family.line.project(now);
        family.line.timebase_anchor = now;
        family.line.rate = rate;
    }
    next
}

#[test]
fn an_uncontended_read_returns_the_live_parameters() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    assert_eq!(fixture.counter() % 2, 0);
    assert_eq!(
        read(fixture.header(), Params::load),
        fixture.header().live.load()
    );
}

#[test]
fn a_publish_writes_both_copies_and_closes_the_section() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let opened = fixture.counter();
    let lock = begin(fixture.header(), &fixture.lock_path()).unwrap();

    lock.publish::<()>(fixture.header(), |anchor, live| {
        Ok(at_rate(anchor, live, Rate::from_f64(10.0).unwrap()))
    })
    .unwrap();
    drop(lock); // W12

    assert_eq!(fixture.counter(), opened + 2);
    assert_eq!(
        fixture
            .header()
            .seqlock
            .writer_state
            .load(Ordering::Acquire),
        0
    );
    let live = fixture.header().live.load();
    assert_eq!(live, fixture.header().shadow.load());
    for family in live.lines() {
        assert_eq!(family.line.rate, Rate::from_f64(10.0).unwrap());
    }
}

#[test]
fn a_refused_write_publishes_nothing_and_still_closes_the_section() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let before = fixture.header().live.load();
    let opened = fixture.counter();
    let lock = begin(fixture.header(), &fixture.lock_path()).unwrap();

    let refused = lock.publish(fixture.header(), |_, _| Err("would go backwards"));
    drop(lock); // W12

    assert_eq!(refused, Err("would go backwards"));
    assert_eq!(fixture.header().live.load(), before);
    assert_eq!(fixture.counter(), opened + 2);
    // Nothing was written and the flag never went up, so a death mid-abort is the recoverable one.
    assert_eq!(
        fixture
            .header()
            .seqlock
            .writer_state
            .load(Ordering::Acquire),
        0
    );
}

#[test]
fn the_anchor_is_sampled_a_settle_after_the_counter_goes_odd() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let lock = begin(fixture.header(), &fixture.lock_path()).unwrap();
    let before = platform::genuine_nanos(clock::MONOTONIC).unwrap();

    lock.publish::<()>(fixture.header(), |anchor, live| {
        let sampled = anchor.of(live.monotonic.timebase).unwrap();
        assert!(
            sampled.0 - before.0 >= WRITE_SETTLE_NS,
            "the anchor sits {} ns after the section opened",
            sampled.0 - before.0
        );
        Ok(live)
    })
    .unwrap();
}

#[test]
fn every_timebase_in_use_is_sampled_once() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let lock = begin(fixture.header(), &fixture.lock_path()).unwrap();

    lock.publish::<()>(fixture.header(), |anchor, mut live| {
        // Two families on one timebase re-anchor against one instant, not two.
        live.boottime.timebase = live.monotonic.timebase;
        let shared = Anchor::sample(&live);
        assert_eq!(
            shared.of(live.monotonic.timebase),
            shared.of(live.boottime.timebase)
        );
        assert!(anchor.of(live.realtime.timebase).is_some());
        assert_eq!(anchor.of(0x7fff_ffff), None);
        Ok(live)
    })
    .unwrap();
}

#[test]
fn a_write_section_runs_with_every_maskable_signal_blocked() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let lock = begin(fixture.header(), &fixture.lock_path()).unwrap();

    lock.publish::<()>(fixture.header(), |_, live| {
        assert!(blocked(libc::SIGUSR1));
        assert!(blocked(libc::SIGTERM));
        Ok(live)
    })
    .unwrap();
    drop(lock); // W12

    assert!(!blocked(libc::SIGUSR1));
}

fn blocked(signal: libc::c_int) -> bool {
    // Safety: `sigset_t` is a plain-old-data struct that `pthread_sigmask` fills before it is read.
    let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    // Safety: `mask` is a live `sigset_t`, and a null new mask asks without changing anything.
    unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &raw mut mask) };
    // Safety: `mask` is the live set just filled in.
    let member = unsafe { libc::sigismember(&raw const mask, signal) };
    member == 1
}

#[test]
fn a_repair_leaves_the_thread_s_signal_mask_as_it_found_it() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    fixture.abandon(true, &distinct(fixture.header().live.load()));

    // The repair holds a section open just as a publish does, and blocks signals across it for the
    // same reason, so what a caller can check from outside is that its own mask survives. A mask
    // of one signal, so restoring means putting this back rather than unblocking everything.
    let restore = block_only(libc::SIGUSR2);
    let _lock = begin(fixture.header(), &fixture.lock_path()).unwrap();

    assert!(
        blocked(libc::SIGUSR2),
        "the repair dropped the caller's mask"
    );
    assert!(!blocked(libc::SIGUSR1));
    set_mask(&restore);
}

/// Blocks `signal` and nothing else, answering with the mask this replaced.
fn block_only(signal: libc::c_int) -> libc::sigset_t {
    // Safety: `sigset_t` is a plain-old-data struct that `sigemptyset` fills before it is read.
    let mut one: libc::sigset_t = unsafe { std::mem::zeroed() };
    // Safety: the same, for the mask this replaces.
    let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
    // Safety: `one` is a live `sigset_t`.
    unsafe { libc::sigemptyset(&raw mut one) };
    // Safety: `one` is the live set just emptied.
    unsafe { libc::sigaddset(&raw mut one, signal) };
    // Safety: both sets are live, and this thread's mask is ours to change.
    unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &raw const one, &raw mut previous) };
    previous
}

fn set_mask(mask: &libc::sigset_t) {
    // Safety: `mask` is a live `sigset_t`, and this thread's mask is ours to change.
    unsafe {
        libc::pthread_sigmask(
            libc::SIG_SETMASK,
            std::ptr::from_ref(mask),
            std::ptr::null_mut(),
        )
    };
}

#[test]
fn the_probe_reports_the_write_lock_held_only_while_a_writer_holds_it() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    assert_eq!(probe_held(), Some(false));

    let lock = WriteLock::acquire(&fixture.lock_path()).unwrap();
    assert_eq!(probe_held(), Some(true));

    drop(lock);
    assert_eq!(probe_held(), Some(false));
}

#[test]
fn a_process_holding_the_write_lock_never_asks_the_kernel_about_it() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let lock = WriteLock::acquire(&fixture.lock_path()).unwrap();

    // With the file gone there is nothing to open, so an answer at all could only have come from
    // this process's own state. FreeBSD's probe reports the byte held to its holder too, which is
    // why a holder never asks.
    std::fs::remove_file(fixture.lock_path()).unwrap();
    assert_eq!(probe_held(), Some(true));

    // And once it is not the holder, the question is the kernel's again: no file, no answer.
    drop(lock);
    assert_eq!(probe_held(), None);
}

#[test]
fn a_reader_keeps_reading_while_a_writer_only_holds_the_lock() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let live = fixture.header().live.load();
    let _lock = begin(fixture.header(), &fixture.lock_path()).unwrap();

    // Between the begin of a transaction and its commit the counter stays even.
    assert_eq!(fixture.counter() % 2, 0);
    assert_eq!(read(fixture.header(), Params::load), live);
}

#[test]
fn a_reader_past_its_patience_returns_the_shadow_the_flag_names() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let whole = distinct(fixture.header().live.load());
    fixture.abandon(true, &whole);

    assert_eq!(read(fixture.header(), Params::load), whole);
}

#[test]
fn a_reader_past_its_patience_returns_the_live_words_when_the_flag_is_clear() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let live = fixture.header().live.load();
    // A shadow the dead writer never finished: whole-looking, and never to be returned.
    fixture.abandon(false, &distinct(live));

    assert_eq!(read(fixture.header(), Params::load), live);
}

#[test]
fn the_successor_finishes_a_write_whose_shadow_was_whole() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let whole = distinct(fixture.header().live.load());
    fixture.abandon(true, &whole);

    let _lock = begin(fixture.header(), &fixture.lock_path()).unwrap();

    assert_eq!(fixture.header().live.load(), whole);
    assert_eq!(fixture.counter() % 2, 0);
    assert_eq!(
        fixture
            .header()
            .seqlock
            .writer_state
            .load(Ordering::Acquire),
        0
    );
}

#[test]
fn the_successor_leaves_a_write_that_never_reached_the_flag_alone() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let live = fixture.header().live.load();
    fixture.abandon(false, &distinct(live));

    let _lock = begin(fixture.header(), &fixture.lock_path()).unwrap();

    assert_eq!(fixture.header().live.load(), live);
    assert_eq!(fixture.counter() % 2, 0);
}

#[test]
fn a_second_writer_of_this_process_waits_rather_than_converting_the_lock() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let lock = WriteLock::acquire(&fixture.lock_path()).unwrap();

    let path = fixture.lock_path();
    let second = std::thread::spawn(move || {
        drop(WriteLock::acquire(&path).unwrap());
    });
    // Re-locking through one description converts the lock instead of blocking, so without the
    // in-process mutex above it this thread would return at once.
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(!second.is_finished());

    drop(lock);
    second.join().unwrap();
}

#[test]
fn a_lock_file_that_is_not_ours_is_refused() {
    let _one = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a-directory-is-not-a-lock-file.lock");
    std::fs::create_dir(&path).unwrap();

    assert!(WriteLock::acquire(&path).is_err());
    assert!(super::install(&path).is_err());
}

#[test]
fn a_planted_fifo_in_a_lock_file_s_place_is_refused_rather_than_waited_on() {
    let _one = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("planted.lock");
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // Safety: `c_path` is a live NUL-terminated path in a directory of ours.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

    // Opening a FIFO without a writer blocks until one arrives, which for the probe would be
    // wherever a clock read can happen. Both doors return instead.
    assert!(super::install(&path).is_err());
    assert!(WriteLock::acquire(&path).is_err());
}

#[test]
fn the_lock_file_is_private_to_us() {
    let _one = one_at_a_time();
    let fixture = fixture().unwrap();
    let mode = std::fs::metadata(fixture.lock_path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}
