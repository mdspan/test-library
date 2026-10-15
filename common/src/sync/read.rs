//! The read flow: **R1** through **R10**.
//!
//! The ordinary path is three loads and a fence around whatever the caller computes. The fallback
//! is what a reader takes when the counter has sat odd for far longer than any write section, and
//! it turns a writer's death into a value rather than a hang.

use std::hint;
use std::sync::atomic::{Ordering, fence};

use crate::log::{self, Level, OncePerIncident};
use crate::platform::{self, clock};
use crate::session::layout::{Header, Params, WriterState};
use crate::sync::lock;
use crate::time::Nanos;

/// How long the counter must sit odd and unchanged before a reader suspects the writer died.
///
/// Far longer than a write section, which is a few tens of microseconds: a reader that spins is
/// back well inside this nearly always, and one that is not has a writer that stopped.
pub const READ_PATIENCE_NS: i64 = 1_000_000;

/// Spins between genuine clock readings while waiting. A short wait, which is every wait a healthy
/// session has, then costs no syscall at all.
const SPINS_PER_READING: u32 = 64;

/// The spin's own bound, for a clock that will not read. Without it a reader whose genuine clock
/// fails would wait on a deadline it can never reach.
const SPINS_WITHOUT_A_CLOCK: u32 = 1 << 20;

/// Reads the parameters through the seqlock, returning what `compute` made of a whole copy.
///
/// `compute` is called on the live parameters, or on the shadow when a dead writer left that copy
/// as the whole one. It may see torn words: the validation after it is what decides whether the
/// value survives, so the arithmetic inside it must hold for any bit pattern.
///
/// Nothing here writes shared memory, allocates, or takes a lock, so a clock read is safe wherever
/// the program can make one, signal handlers included.
#[inline]
pub fn read<T>(header: &Header, compute: impl Fn(&Params) -> T) -> T {
    let mut patience = Patience::new();
    loop {
        let counter = header.seqlock.sequence.load(Ordering::Acquire); // R1
        if counter & 1 == 1 {
            if patience.spent(counter)
                && let Some(value) = recover(header, counter, &compute)
            {
                return value;
            }
            continue;
        }
        // R2 and R3. The sample sits inside `compute`, after the word naming the clock to take it
        // from, which is the only order those two can go in. Nothing rests on their order: a
        // timestamp instruction carries no ordering wherever it sits, and the settle a writer
        // spins out is what covers a sample that drifts.
        let value = compute(&header.live);
        fence(Ordering::Acquire); // R4 fence
        if header.seqlock.sequence.load(Ordering::Relaxed) == counter {
            // R4 reload
            return value;
        }
    }
}

/// **R5** through **R10**: the fallback, for a section whose writer may have died.
///
/// `None` sends the reader back to spinning, which is what every answer here that is not a whole
/// value means: whoever moved the words is making progress.
///
/// With signals held back, since the probe calls into libc and the report formats a log line,
/// and either may use x28 on a Go thread inside a synthetic vDSO entry; see
/// [`platform::hold_signals`].
#[cold]
fn recover<T>(header: &Header, counter: u64, compute: &impl Fn(&Params) -> T) -> Option<T> {
    platform::hold_signals(|| recover_held(header, counter, compute))
}

/// [`recover`] itself, which runs with signals held back.
fn recover_held<T>(header: &Header, counter: u64, compute: &impl Fn(&Params) -> T) -> Option<T> {
    let seq = &header.seqlock;
    let state = seq.writer_state.load(Ordering::Acquire); // R5
    // A lock we cannot ask about is one that might be held, which is the answer that keeps the
    // reader spinning rather than the one that lets it return.
    if lock::probe_held().unwrap_or(true) {
        // R6
        hint::spin_loop();
        return None;
    }
    if seq.sequence.load(Ordering::Acquire) != counter {
        // R7: the counter moved while the probe ran, so the lock was released and taken again and
        // the state loaded at R5 belongs to a section that has already closed.
        return None;
    }
    // R8 and R9. Flag set means the shadow holds the whole copy; flag clear means the live words
    // were never touched, so they are the authoritative one.
    let whole = if WriterState::from_word(state).shadow_complete {
        &header.shadow
    } else {
        &header.live
    };
    let value = compute(whole);
    fence(Ordering::Acquire); // R10 fence
    if seq.sequence.load(Ordering::Relaxed) != counter {
        return None;
    }
    if seq.writer_state.load(Ordering::Relaxed) != state {
        // R5 can load a stale word, and this is what catches it.
        return None;
    }
    report_death(counter, WriterState::from_word(state).pid);
    Some(value)
}

/// The incident this process has already reported, so a section every clock read runs into writes
/// one line rather than flooding the log.
static SUSPECTED: OncePerIncident = OncePerIncident::new();

/// One `warn` line per process per stuck section, from whichever thread wins the claim.
///
/// The reader that returns a value for a suspected death writes it, and so does the writer that
/// repairs one, since a process that does both saw one incident and not two.
pub(super) fn report_death(counter: u64, pid: u32) {
    if log::enabled(Level::Warn) && SUSPECTED.claim(counter) {
        log::write_line(
            Level::Warn,
            format_args!(
                "the writer in pid {pid} left a write section open; reading the copy it left whole"
            ),
        );
    }
}

/// How long a reader has spun on one odd counter.
struct Patience {
    counter: u64,
    spins: u32,
    deadline: Option<Nanos>,
}

impl Patience {
    /// Nothing waited on yet. Zero is even, so the first odd counter always starts a fresh wait.
    const fn new() -> Self {
        Self {
            counter: 0,
            spins: 0,
            deadline: None,
        }
    }

    /// Spins once, and says whether this counter has now sat unchanged for longer than any write
    /// section takes. A counter that moved resets the wait: the writer is alive and working.
    ///
    /// True comes back once every [`SPINS_PER_READING`] spins rather than on every call, so a
    /// writer that is alive and stopped, which readers wait out for as long as it takes, costs one
    /// probe per that many spins instead of one per spin.
    fn spent(&mut self, counter: u64) -> bool {
        if counter != self.counter {
            *self = Self {
                counter,
                ..Self::new()
            };
        }
        hint::spin_loop();
        // Wrapping, not saturating: the count is only ever read against the interval below, and a
        // count pinned at a value that is not a multiple of it would strand the reader short of
        // the fallback forever.
        self.spins = self.spins.wrapping_add(1);
        if !self.spins.is_multiple_of(SPINS_PER_READING) {
            return false;
        }
        // The genuine clock, since the faked one is what this reader is waiting to compute.
        let Some(now) = platform::genuine_nanos(clock::MONOTONIC) else {
            return self.spins >= SPINS_WITHOUT_A_CLOCK;
        };
        let Some(deadline) = self.deadline else {
            self.deadline = Some(Nanos(now.0.saturating_add(READ_PATIENCE_NS)));
            return false;
        };
        now >= deadline
    }
}
