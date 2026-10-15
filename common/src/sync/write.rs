//! The write flow: **W1** through **W12**, and the repair a dead predecessor leaves behind.

use std::hint;
use std::io;
use std::path::Path;
use std::sync::atomic::{Ordering, fence};

use crate::platform::{self, clock};
use crate::session::layout::{Header, ParamsValue, WriterState};
use crate::sync::lock::WriteLock;
use crate::sync::read;
use crate::time::Nanos;

/// How far the genuine clock must advance between the counter going odd and the anchor being
/// sampled.
///
/// Two small slips let a reader compute from the old parameters for a moment after the counter
/// goes odd: the store takes time to become visible on another core, and the hardware can run a
/// reader's sample out of order. Both are a few hundred nanoseconds, and this is larger than
/// either, so every value the old line ever produced sits at or below the anchor.
pub const WRITE_SETTLE_NS: i64 = 50_000;

/// The settle's own bound, so a stuck clock proceeds instead of hanging every reader.
const SETTLE_SPINS: u32 = 1 << 20;

/// **W1**: takes the write lock, and repairs a seqlock a dead predecessor left open.
///
/// Acquiring the lock does not say whether the previous owner died; the seqlock does. A counter
/// left odd means the predecessor died inside a write section, and this repairs it before the
/// caller writes anything of its own. An owner pid still recorded under an even counter means it
/// died between the lock and W3, with nothing to repair.
///
/// The counter is even when this returns, so readers keep reading while the lock is held and only
/// other writers wait.
///
/// # Errors
/// [`io::Error`] when the lock file cannot be opened, is not ours, or cannot be locked.
pub fn begin(header: &Header, lock_path: &Path) -> io::Result<WriteLock> {
    let lock = WriteLock::acquire(lock_path)?;
    repair(header);
    Ok(lock)
}

/// Finishes a dead writer's section, on the same shadow-complete flag the reader's fallback
/// branches on, so the two converge on one value.
fn repair(header: &Header) {
    let seq = &header.seqlock;
    let counter = seq.sequence.load(Ordering::Acquire);
    if counter & 1 == 0 {
        return;
    }
    // A section is open across everything below, and this thread is the only one that can close
    // it, so it runs under the mask a write section runs under and for the same reason: a handler
    // that read the clock here would spin on a counter only this thread moves, behind a lock only
    // this thread releases.
    let _masked = SignalMask::block_all();

    let state = WriterState::from_word(seq.writer_state.load(Ordering::Acquire));
    if state.shadow_complete {
        // The shadow is whole and the live words may be torn, so the write finishes here.
        header.live.store(&header.shadow.load());
    }
    // Nobody is inside a section once the counter is even, so the word says so. A fallback reader
    // whose recheck catches this retries, which is the answer it would have reached anyway.
    seq.writer_state.store(0, Ordering::Release);
    seq.sequence
        .store(counter.wrapping_add(1), Ordering::Release);
    read::report_death(counter, state.pid);
}

impl WriteLock {
    /// **W2** through **W11**: one write section, published whole or not at all.
    ///
    /// `compute` is **W6**. It receives the anchor sampled at **W5** and the parameters that are
    /// live at that instant, and returns the copy to publish. Returning an error aborts: nothing
    /// has been written, the shadow-complete flag is still clear, and the section closes looking
    /// to a reader exactly like a writer that died between W3 and W7.
    ///
    /// Every maskable signal is blocked across the section. A handler that ran inside it and read
    /// the clock would spin on a counter only the interrupted section can move, hanging the thread
    /// and every reader in the session with it.
    ///
    /// # Errors
    /// Whatever `compute` refused with.
    pub fn publish<E>(
        &self,
        header: &Header,
        compute: impl FnOnce(&Anchor, ParamsValue) -> Result<ParamsValue, E>,
    ) -> Result<(), E> {
        self.section(header, compute)
    }

    /// **W2** through **W11**, the section itself, with the signal mask held across exactly it.
    #[expect(
        clippy::unused_self,
        reason = "the receiver is the proof that the caller holds the write lock, which is what \
                  keeps this the only section in flight"
    )]
    fn section<E>(
        &self,
        header: &Header,
        compute: impl FnOnce(&Anchor, ParamsValue) -> Result<ParamsValue, E>,
    ) -> Result<(), E> {
        let seq = &header.seqlock;
        let pid = std::process::id();
        let _masked = SignalMask::block_all();

        seq.writer_state
            .store(WriterState::writing(pid).word(), Ordering::Release); // W2
        // Even, because `begin` repaired anything odd and the write lock keeps every other writer
        // out for as long as this section runs.
        let opened = seq.sequence.load(Ordering::Relaxed);
        // Release, so a reader that sees the odd counter sees W2's word too, never the cleared word
        // before it, which a later W10 or repair writes again and R10 would take for unchanged.
        seq.sequence
            .store(opened.wrapping_add(1), Ordering::Release); // W3
        fence(Ordering::SeqCst); // W3 fence: the settle below depends on it

        settle(); // W4

        let live = header.live.load();
        let anchor = Anchor::sample(&live); // W5
        let published = compute(&anchor, live); // W6

        if let Ok(next) = &published {
            header.shadow.store(next); // W7
            seq.writer_state
                .store(WriterState::published(pid).word(), Ordering::Release); // W8
            // The release store orders only what precedes it. The fence keeps every W9 store
            // behind the flag, so a reader whose R9 saw one finds the flag set at R10.
            fence(Ordering::Release); // W8 fence
            header.live.store(next); // W9
        }
        seq.writer_state.store(0, Ordering::Release); // W10
        seq.sequence
            .store(opened.wrapping_add(2), Ordering::Release); // W11
        published.map(drop)
    }
}

/// **W4**: spins until the genuine clock has advanced by [`WRITE_SETTLE_NS`].
fn settle() {
    let Some(start) = platform::genuine_nanos(clock::MONOTONIC) else {
        return;
    };
    let until = Nanos(start.0.saturating_add(WRITE_SETTLE_NS));
    for _ in 0..SETTLE_SPINS {
        match platform::genuine_nanos(clock::MONOTONIC) {
            Some(now) if now >= until => return,
            Some(_) => hint::spin_loop(),
            None => return,
        }
    }
}

/// **W5**: the genuine instants the new parameters are anchored against, one per timebase the
/// session uses.
///
/// Sampled inside the section, so the old and new lines agree at the sampled instant and the time
/// readers observe is continuous across the change. Two families sharing a timebase share its
/// instant, so one publish re-anchors them against the same point.
///
/// The readings are genuine: they go through the platform seam rather than through anything this
/// library fakes, so a controller running inside a target it has preloaded still samples the
/// operating system's own clock.
#[derive(Debug, Clone, Copy)]
pub struct Anchor {
    timebases: [libc::clockid_t; Self::MOST],
    instants: [Nanos; Self::MOST],
    sampled: usize,
}

impl Anchor {
    /// One timebase per family that carries a line is as many as a session can use.
    const MOST: usize = 3;

    /// Reads every timebase `params` names, one after the other, each one once.
    pub(super) fn sample(params: &ParamsValue) -> Self {
        let mut anchor = Self {
            timebases: [0; Self::MOST],
            instants: [Nanos::ZERO; Self::MOST],
            sampled: 0,
        };
        for family in params.lines() {
            if anchor.of(family.timebase).is_some() {
                continue;
            }
            let Some(now) = platform::genuine_nanos(family.timebase) else {
                continue;
            };
            if let Some(slot) = anchor.timebases.get_mut(anchor.sampled) {
                *slot = family.timebase;
            }
            if let Some(slot) = anchor.instants.get_mut(anchor.sampled) {
                *slot = now;
            }
            anchor.sampled += 1;
        }
        anchor
    }

    /// The instant `timebase` read at, or `None` when the kernel refused that clock.
    #[must_use]
    pub fn of(&self, timebase: libc::clockid_t) -> Option<Nanos> {
        let taken = self.sampled.min(Self::MOST);
        let position = self
            .timebases
            .get(..taken)?
            .iter()
            .position(|&known| known == timebase)?;
        self.instants.get(position).copied()
    }
}

/// Every maskable signal blocked, restored when this is dropped.
///
/// `SIGSTOP` and `SIGKILL` cannot be masked; a writer stopped inside the section is the
/// stopped-writer case the design accepts, and a killed one is the death the repair handles.
struct SignalMask {
    previous: libc::sigset_t,
}

impl SignalMask {
    fn block_all() -> Self {
        // Safety: `sigset_t` is a plain-old-data struct that `sigfillset` fills before it is read.
        let mut all: libc::sigset_t = unsafe { std::mem::zeroed() };
        // Safety: the same, for the mask this replaces.
        let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
        // Safety: `all` is a live `sigset_t`.
        unsafe { libc::sigfillset(&raw mut all) };
        // Safety: both sets are live, and the thread's mask is ours to change.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &raw const all, &raw mut previous) };
        Self { previous }
    }
}

impl Drop for SignalMask {
    fn drop(&mut self) {
        // Safety: `previous` is the live mask this thread had before the section.
        unsafe {
            libc::pthread_sigmask(
                libc::SIG_SETMASK,
                &raw const self.previous,
                std::ptr::null_mut(),
            )
        };
    }
}
