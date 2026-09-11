//! The timekeep page: page 1 of the session file, and what a FreeBSD reader that skips libc
//! computes its clock from.
//!
//! The kernel publishes a page of linear parameters and the Go runtime finds it through
//! `AT_TIMEKEEP` in the auxiliary vector. Unlike the vDSO the same runtime finds on Linux, the page
//! holds numbers rather than functions: a snapshot of a hardware counter, a scale, and two offsets,
//! and the reader does the arithmetic. A session that poses as that page writes the same numbers
//! from its own parameters, so the clock the runtime computes is the session's.
//!
//! The layout is not ours to choose. Every field below is `struct vdso_timekeep` and
//! `struct vdso_timehands` as the kernel declares them and as a reader has compiled them in, down
//! to the machine-dependent tail, and the publishing protocol is the kernel's own: fill the
//! timehands the current index does not name, give it a fresh generation, then move the index.
//!
//! The page goes stale on its own, because a difference of two counter readings overflows both the
//! mask and the scale within a second or so. What keeps it fresh is the
//! [updater](crate::updater), and what republishes it at once on a change is every writer.

pub mod counter;

use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering, fence};

use crate::platform::{self, clock};
use crate::session::layout::{Header, Keeper};
use crate::sync;
use crate::time::{Family, Nanos, Rate};

pub use counter::Basis;

/// The one revision of this layout the kernel has ever published, `VDSO_TK_VER_1`.
pub const VERSION: u32 = 1;

/// The counter this architecture reads in one instruction.
///
/// The page's algorithm word spells it `VDSO_TH_ALGO_X86_TSC` on `x86_64` and
/// `VDSO_TH_ALGO_ARM_GENTIM` on aarch64, which are the same number under two names.
pub const ALGO_NATIVE: u32 = 1;

/// How many sets of parameters the page carries. Two is enough to publish by toggling: a reader
/// only ever follows the index, and the one it is not following is the one being written.
pub const HANDS: usize = 2;

/// How many words the machine-dependent tail of a timehands holds. Thirty-two bytes on both
/// architectures we support, which is what keeps one layout serving both.
const MD_WORDS: usize = 8;

/// FreeBSD's `struct bintime`: whole seconds and a binary fraction of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Bintime {
    /// Whole seconds.
    pub sec: i64,
    /// The fraction of a second, in units of 2^-64.
    pub frac: u64,
}

/// 2^64, the unit [`Bintime::frac`] counts in, as the width the arithmetic below needs.
const FRACTION: u128 = 1 << u64::BITS;

/// A second in nanoseconds, as the same width.
const SECOND_FRAC: u128 = Nanos::SECOND as u128;

impl Bintime {
    /// The instant this many nanoseconds after the epoch of whatever clock it belongs to.
    #[must_use]
    pub fn from_nanos(value: Nanos) -> Self {
        let sec = value.0.div_euclid(Nanos::SECOND);
        let nanos = u128::from(value.0.rem_euclid(Nanos::SECOND).unsigned_abs());
        // The remainder is below a second, so the product stays far inside a `u128` and the
        // quotient far inside a `u64`.
        Self {
            sec,
            frac: u64::try_from((nanos * FRACTION) / SECOND_FRAC).unwrap_or(u64::MAX),
        }
    }

    /// Back to nanoseconds, saturating rather than wrapping on an instant no `i64` can hold.
    #[must_use]
    pub fn to_nanos(self) -> Nanos {
        // The fraction is below a second, so the quotient is below a billion and the conversion
        // back cannot fail.
        let nanos = i64::try_from((u128::from(self.frac) * SECOND_FRAC) >> u64::BITS).unwrap_or(0);
        Nanos(self.sec.saturating_mul(Nanos::SECOND).saturating_add(nanos))
    }

    /// This instant less `other`, which is how the page's boot offset is worked out.
    #[must_use]
    pub fn minus(self, other: Self) -> Self {
        let (frac, borrowed) = self.frac.overflowing_sub(other.frac);
        Self {
            sec: self
                .sec
                .saturating_sub(other.sec)
                .saturating_sub(i64::from(borrowed)),
            frac,
        }
    }

    /// This instant plus `other`, which is the sum a reader takes for an absolute clock.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        let (frac, carried) = self.frac.overflowing_add(other.frac);
        Self {
            sec: self
                .sec
                .saturating_add(other.sec)
                .saturating_add(i64::from(carried)),
            frac,
        }
    }

    /// This instant plus a raw fraction, which is the step a reader takes for the counter delta.
    #[must_use]
    pub fn plus_frac(self, by: u64) -> Self {
        let (frac, carried) = self.frac.overflowing_add(by);
        Self {
            sec: self.sec.saturating_add(i64::from(carried)),
            frac,
        }
    }
}

/// One whole set of page parameters, off the stack.
///
/// What a republisher builds and what a reader of the page walks away with, so the same value
/// serves writing the page and saying what the page currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hands {
    /// Which counter a reader takes, and how.
    pub algo: u32,
    /// The machine-dependent word beside it; see [`Basis::flavour`].
    pub flavour: u32,
    /// Fake seconds per counter tick, as a 64-bit binary fraction.
    pub scale: u64,
    /// The counter reading the offsets below were taken at.
    pub offset_count: u32,
    /// Which bits of a counter difference count.
    pub counter_mask: u32,
    /// What the elapsed clock showed at `offset_count`.
    pub offset: Bintime,
    /// What has to be added to the elapsed clock to reach the wall clock.
    pub boottime: Bintime,
}

impl Hands {
    /// What a reader of these parameters computes at counter reading `count`: the elapsed clock,
    /// and the wall clock when `absolute`.
    ///
    /// FreeBSD's own `binuptime`, arithmetic for arithmetic: the counter difference masked to the
    /// counter's width, multiplied by the scale, and added to the offset as a raw fraction. This is
    /// what makes a page of ours checkable without a reader to hand.
    #[must_use]
    pub fn showing(&self, count: u32, absolute: bool) -> Bintime {
        let delta = count.wrapping_sub(self.offset_count) & self.counter_mask;
        let shown = self
            .offset
            .plus_frac(self.scale.wrapping_mul(u64::from(delta)));
        if absolute {
            shown.plus(self.boottime)
        } else {
            shown
        }
    }
}

/// One `struct vdso_timehands`: the parameters a reader follows the current index to.
#[derive(Debug)]
#[repr(C)]
pub struct Timehands {
    /// Which counter a reader takes, and how.
    pub algo: AtomicU32,
    /// Zero while this set is being written, and a fresh nonzero number once it is whole.
    pub generation: AtomicU32,
    /// Fake seconds per counter tick, as a 64-bit binary fraction.
    pub scale: AtomicU64,
    /// The counter reading the offsets below were taken at.
    pub offset_count: AtomicU32,
    /// Which bits of a counter difference count.
    pub counter_mask: AtomicU32,
    /// Whole seconds of what the elapsed clock showed at `offset_count`.
    pub offset_sec: AtomicI64,
    /// The fraction beside it.
    pub offset_frac: AtomicU64,
    /// Whole seconds of the step from the elapsed clock to the wall clock.
    pub boottime_sec: AtomicI64,
    /// The fraction beside it.
    pub boottime_frac: AtomicU64,
    /// The machine-dependent tail. Word 0 is `th_x86_shift` on `x86_64` and `th_physical` on
    /// aarch64; the rest is reserved and stays zero, which is what the kernel leaves in the fields
    /// neither architecture's reader looks at.
    pub md: [AtomicU32; MD_WORDS],
}

/// One `struct vdso_timekeep`: the head of the page, and the sets of parameters behind it.
#[derive(Debug)]
#[repr(C)]
pub struct Timekeep {
    /// [`VERSION`]. A reader that does not know the number reads nothing here.
    pub version: AtomicU32,
    /// Nonzero once the page carries a whole set of parameters. A reader that finds this clear
    /// falls back to the system call, which is what makes an unpublished page harmless.
    pub enabled: AtomicU32,
    /// Which of [`Timekeep::hands`] a reader follows.
    pub current: AtomicU32,
    /// Alignment only; the layout puts nothing here.
    pub padding: AtomicU32,
    /// The sets of parameters, published by toggling [`Timekeep::current`].
    pub hands: [Timehands; HANDS],
}

const _: () = {
    // The shape the kernel fixes and a reader has compiled in. `th_scale` puts the whole structure
    // on an eight-byte alignment, which is what places the first timehands at 16.
    assert!(size_of::<Timehands>() == 88);
    assert!(align_of::<Timehands>() == 8);
    assert!(std::mem::offset_of!(Timekeep, hands) == 16);
    assert!(size_of::<Timekeep>() == 16 + HANDS * 88);
};

impl Timehands {
    /// Writes every word of one set, the generation first cleared and last set, so a reader either
    /// follows a whole set or retries.
    ///
    /// Every store is a Release, not just the generation's. The reader is somebody else's compiled
    /// code, which we cannot change and which loads the parameters without ordering them; the
    /// strongest thing a publisher can do for such a reader is make each word it writes carry its
    /// own release, so that a reader observing any one of them observes everything written before
    /// it — the generation going to zero above all, which is what the reader's own recheck then
    /// catches. Without that, two sets published by toggling leave one execution the recheck cannot
    /// see: a reader holding a set from two publishes ago, reading a word of the publish that
    /// reuses that set, and still reloading the generation it started with.
    fn store(&self, next: &Hands, generation: u32) {
        // Zero first: a reader that is already inside this set sees a generation it cannot match,
        // whichever of the words below it went on to load.
        self.generation.store(0, Ordering::Release);
        self.algo.store(next.algo, Ordering::Release);
        self.scale.store(next.scale, Ordering::Release);
        self.offset_count
            .store(next.offset_count, Ordering::Release);
        self.counter_mask
            .store(next.counter_mask, Ordering::Release);
        self.offset_sec.store(next.offset.sec, Ordering::Release);
        self.offset_frac.store(next.offset.frac, Ordering::Release);
        self.boottime_sec
            .store(next.boottime.sec, Ordering::Release);
        self.boottime_frac
            .store(next.boottime.frac, Ordering::Release);
        if let Some(word) = self.md.first() {
            word.store(next.flavour, Ordering::Release);
        }
        for word in self.md.iter().skip(1) {
            word.store(0, Ordering::Release);
        }
        // And the generation last, which is what says the set above it is whole.
        self.generation.store(generation, Ordering::Release);
    }

    /// Every word of one set, read the way a reader reads them. Ordering comes from the generation
    /// around it, not from here.
    fn load(&self) -> Hands {
        Hands {
            algo: self.algo.load(Ordering::Relaxed),
            flavour: self
                .md
                .first()
                .map_or(0, |word| word.load(Ordering::Relaxed)),
            scale: self.scale.load(Ordering::Relaxed),
            offset_count: self.offset_count.load(Ordering::Relaxed),
            counter_mask: self.counter_mask.load(Ordering::Relaxed),
            offset: Bintime {
                sec: self.offset_sec.load(Ordering::Relaxed),
                frac: self.offset_frac.load(Ordering::Relaxed),
            },
            boottime: Bintime {
                sec: self.boottime_sec.load(Ordering::Relaxed),
                frac: self.boottime_frac.load(Ordering::Relaxed),
            },
        }
    }
}

/// How many times a reader retries before giving up on a page somebody is republishing far too
/// often. A reader in the kernel's own libc spins without a bound; ours has one so that a caller of
/// [`Timekeep::load`] cannot be held for ever by a page it does not own.
const READ_TRIES: usize = 64;

impl Timekeep {
    /// Makes a fresh page one a reader will look at, but not yet one it will follow: the version it
    /// checks, and the enabled word still clear until the first publish.
    pub fn initialize(&self) {
        self.enabled.store(0, Ordering::Relaxed);
        self.current.store(0, Ordering::Relaxed);
        for hands in &self.hands {
            hands.generation.store(0, Ordering::Relaxed);
        }
        // Last, so a reader that finds the version it knows finds the words behind it too.
        self.version.store(VERSION, Ordering::Release);
    }

    /// Publishes `next`, by writing the set the current index does not name and then moving the
    /// index to it.
    ///
    /// The generation climbs with every publish, so a reader that straddles two of them finds a
    /// number it cannot match and goes round again. That is what makes two sets enough: the set a
    /// reader is following is never the set being written, and the third publish, which reuses the
    /// reader's set, cannot be mistaken for the first.
    pub fn publish(&self, next: &Hands) {
        let current = self.current.load(Ordering::Relaxed) as usize % HANDS;
        let index = (current + 1) % HANDS;
        let Some(hands) = self.hands.get(index) else {
            return;
        };
        let generation = match self
            .hands
            .get(current)
            .map_or(0, |live| live.generation.load(Ordering::Relaxed))
            .wrapping_add(1)
        {
            // Zero is what a set being written carries, so a generation never lands there.
            0 => 1,
            fresh => fresh,
        };
        hands.store(next, generation);

        // Release on both: a reader follows the index to the set, and the enabled word is what
        // lets it look at all.
        self.current
            .store(u32::try_from(index).unwrap_or(0), Ordering::Release);
        self.enabled.store(1, Ordering::Release);
    }

    /// What the page currently carries, or `None` when it carries nothing a reader would follow.
    ///
    /// The reader's own walk: the enabled word, the current index, the generation, the parameters,
    /// and then both the index and the generation again, because a publish that landed in between
    /// leaves what was read half of one set and half of another.
    #[must_use]
    pub fn load(&self) -> Option<Hands> {
        if self.version.load(Ordering::Acquire) != VERSION {
            return None;
        }
        for _ in 0..READ_TRIES {
            if self.enabled.load(Ordering::Acquire) == 0 {
                return None;
            }
            let current = self.current.load(Ordering::Acquire) as usize % HANDS;
            let hands = self.hands.get(current)?;
            let generation = hands.generation.load(Ordering::Acquire);
            let read = hands.load();
            fence(Ordering::Acquire);
            if generation != 0
                && generation == hands.generation.load(Ordering::Relaxed)
                && current == self.current.load(Ordering::Relaxed) as usize % HANDS
            {
                return Some(read);
            }
        }
        None
    }

    /// What a reader of the page computes right now, elapsed or wall clock, or `None` when the page
    /// carries nothing to compute from.
    ///
    /// `basis` is what says how to take the counter; a page published against another counter than
    /// this process can take answers `None` rather than a number nobody would recognise.
    #[must_use]
    pub fn reading(&self, basis: Basis, absolute: bool) -> Option<Nanos> {
        let hands = self.load()?;
        if hands.algo != basis.algo || hands.flavour != basis.flavour {
            return None;
        }
        Some(hands.showing(basis.read(), absolute).to_nanos())
    }
}

/// The two clocks a reader of the page computes, and the rate they share.
///
/// The page carries one scale, so the wall clock and the elapsed clock it serves advance together
/// between republishes. Which elapsed clock that is follows the family table: `CLOCK_MONOTONIC` is
/// the name a reader asks for, and the family it belongs to is the one whose rate the page carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    /// What the session shows for the wall clock.
    pub realtime: Nanos,
    /// What it shows for `CLOCK_MONOTONIC`.
    pub elapsed: Nanos,
    /// How fast the pair runs.
    pub rate: Rate,
}

/// Rewrites the page from the session's own parameters.
///
/// Done by the updater at every tick and by every writer as part of its publish, so a change
/// reaches a reader of the page in microseconds rather than at the next tick.
///
/// The clocks are read before the counter, deliberately: the parameters then describe an instant a
/// shade before the one they are anchored at, so the page runs a hair behind the session rather
/// than a hair ahead, and a reader that catches up is stepping forwards.
pub fn republish(header: &Header, page: &Timekeep, basis: Basis) -> Option<()> {
    let view = view(header)?;
    let count = basis.read();
    let offset = Bintime::from_nanos(view.elapsed);
    page.publish(&Hands {
        algo: basis.algo,
        flavour: basis.flavour,
        scale: scaled(basis.scale, view.rate),
        offset_count: count,
        counter_mask: basis.mask,
        offset,
        boottime: Bintime::from_nanos(view.realtime).minus(offset),
    });
    Some(())
}

/// Genuine seconds per tick turned into fake seconds per tick.
///
/// Saturating, because the page's scale is the one place the session's rate has a ceiling of its
/// own: past it the page publishes the fastest it can encode and the republish cadence makes up the
/// rest in forward steps.
fn scaled(genuine: u64, rate: Rate) -> u64 {
    let product = (u128::from(genuine) * u128::from(rate.0)) >> 32;
    u64::try_from(product).unwrap_or(u64::MAX)
}

/// What the page is written from: one validated read of the session's parameters, the two clocks
/// taken inside it so they agree with each other.
fn view(header: &Header) -> Option<View> {
    let elapsed_clock = clock::MONOTONIC;
    let family = clock::family_of(elapsed_clock)?;
    match Keeper::from_tag(header.keeper)? {
        Keeper::Affine => sync::read(header, |params| {
            Some(View {
                realtime: super::affine_now(params, Family::Realtime, clock::REALTIME)?,
                elapsed: super::affine_now(params, family, elapsed_clock)?,
                rate: super::family_rate(params, family),
            })
        }),
        // Genuine time underneath, so the page carries the kernel's own clocks at real speed.
        Keeper::Null => Some(View {
            realtime: platform::genuine_nanos(clock::REALTIME)?,
            elapsed: platform::genuine_nanos(elapsed_clock)?,
            rate: Rate::UNITY,
        }),
    }
}

#[cfg(test)]
mod tests;
