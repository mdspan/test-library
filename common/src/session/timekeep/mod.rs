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
//!
//! The page is published by swap, and a swap lags the seqlock: between a change landing and the
//! page carrying it, a reader of the page computes from the old parameters. Republishing onto the
//! session's line without more would then step such a reader's clock back. So a republisher obeys
//! the rule a seqlock writer obeys — a swap-published linear clock is continuous at its swap
//! instant — and [`continuous`] is that rule: new parameters anchor at the value the page itself
//! shows, a page behind snaps forward onto the session's line, and a page ahead runs slightly under
//! the session's rate until the two rejoin. Only a step the user asked for, which the session's
//! discontinuity epoch records, is followed as it stands.

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

/// What the page shows for both of the clocks it serves, at one reading of the counter.
///
/// The pair a republisher anchors on: the page carries one scale and two offsets, so the two
/// clocks move together between republishes but each has a lead of its own over the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    /// What a reader computes for the wall clock.
    pub realtime: Nanos,
    /// What it computes for the elapsed clock.
    pub elapsed: Nanos,
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

    /// Both clocks at once, which is what a republisher anchors its new parameters at.
    #[must_use]
    pub fn shows(&self, count: u32) -> Shown {
        Shown {
            realtime: self.showing(count, true).to_nanos(),
            elapsed: self.showing(count, false).to_nanos(),
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

/// Page 1 of the session file: the structure a reader of the page follows, and the republishers'
/// own words behind it.
///
/// A reader knows the head of this page and nothing else, so what sits past the end of
/// [`Timekeep`] is ours to use. What lives there is what one republisher has to tell the next:
/// which republisher holds the page right now, when the last republish was, and which
/// discontinuity epoch it followed. None of it is on any reader's path.
#[derive(Debug)]
#[repr(C)]
pub struct Page {
    /// The parameters a reader follows, in the layout the kernel fixes.
    pub timekeep: Timekeep,
    /// What the republishers keep for each other.
    continuity: Continuity,
}

/// The republishers' own words: not part of any layout a reader knows.
#[derive(Debug)]
#[repr(C)]
struct Continuity {
    /// The genuine instant the republisher in flight took the page at, or zero when none holds it.
    publishing: AtomicI64,
    /// The genuine instant of the last republish, or zero before the first one.
    published_at: AtomicI64,
    /// The session's discontinuity epoch as of that republish.
    epoch: AtomicU64,
}

const _: () = {
    // The shape the kernel fixes and a reader has compiled in. `th_scale` puts the whole structure
    // on an eight-byte alignment, which is what places the first timehands at 16.
    assert!(size_of::<Timehands>() == 88);
    assert!(align_of::<Timehands>() == 8);
    assert!(std::mem::offset_of!(Timekeep, hands) == 16);
    assert!(size_of::<Timekeep>() == 16 + HANDS * 88);
    // A reader follows the page from its first byte, and everything of ours sits past what such a
    // reader knows, inside the one page the layout sets aside.
    assert!(std::mem::offset_of!(Page, timekeep) == 0);
    assert!(std::mem::offset_of!(Page, continuity) == size_of::<Timekeep>());
    assert!(size_of::<Page>() <= 4096);
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

/// The two clocks a reader of the page computes, the rate they share, and the epoch that says
/// whether the user has moved them.
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
    /// The session's discontinuity epoch: bumped by a move of a family's value, left alone by a
    /// change of its rate.
    pub epoch: u64,
}

impl Page {
    /// Makes a fresh page one a reader will look at, and clears what the republishers keep.
    pub fn initialize(&self) {
        self.continuity.publishing.store(0, Ordering::Relaxed);
        self.continuity.published_at.store(0, Ordering::Relaxed);
        self.continuity.epoch.store(0, Ordering::Relaxed);
        self.timekeep.initialize();
    }

    /// Takes the page for one republish, or `None` when another republisher holds it.
    ///
    /// Republishing reads the page and writes it, and two republishers doing that at once would
    /// undo each other: the later publish would anchor at a value the earlier one had already left
    /// behind, which is the backwards step this whole rule exists to prevent. There is no blocking
    /// here and none wanted, because whoever holds the page is publishing a value at least as fresh
    /// as ours, so standing aside loses nothing.
    ///
    /// A republisher that dies holding the page would wedge it for the session's life, so a page
    /// held for longer than [`HELD_TOO_LONG`] is broken and taken in the same step. A republish is
    /// a handful of loads and stores, so only a holder that is gone is ever broken — with the one
    /// exception every stopped writer shares, a holder stopped inside those stores.
    fn take(&self, now: Nanos) -> Option<Publishing<'_>> {
        // Zero is what says nobody holds the page, so a republisher taking it at that instant says
        // the next nanosecond instead.
        let mine = now.0.max(1);
        let Err(held) = self.continuity.publishing.compare_exchange(
            0,
            mine,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return Some(Publishing(self));
        };
        if now.0.saturating_sub(held) < HELD_TOO_LONG.0 {
            return None;
        }
        self.continuity
            .publishing
            .compare_exchange(held, mine, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
            .then_some(Publishing(self))
    }

    /// What the page shows for both its clocks at `count`, or `None` when it shows nothing a
    /// republisher may anchor on.
    ///
    /// That is a page nothing has published yet, a page written against a counter this process
    /// cannot take, and a page whose last republish is further back than the counter takes to come
    /// round: past that, what the page shows is a value from some wrap nobody can name, rather than
    /// a value a reader's clock came from.
    fn showing(&self, basis: Basis, count: u32, now: Nanos) -> Option<Shown> {
        let published_at = self.continuity.published_at.load(Ordering::Relaxed);
        if published_at == 0 || now.0.saturating_sub(published_at) >= wrap_period(basis).0 {
            return None;
        }
        let hands = self.timekeep.load()?;
        (hands.algo == basis.algo && hands.flavour == basis.flavour).then(|| hands.shows(count))
    }
}

/// The page held for one republish, released when this is dropped.
#[derive(Debug)]
struct Publishing<'a>(&'a Page);

impl Drop for Publishing<'_> {
    fn drop(&mut self) {
        // Release, so the next republisher's own take sees the epoch and the instant written under
        // this one.
        self.0.continuity.publishing.store(0, Ordering::Release);
    }
}

/// How long a page may be held for republishing before whoever holds it is taken to be gone.
///
/// A republish is a handful of loads and stores, which is nanoseconds, and no session republishes
/// less often than [`MOST_INTERVAL`], so this is far above the one and at the other.
const HELD_TOO_LONG: Nanos = MOST_INTERVAL;

/// The shortest republish interval that still leaves the updater asleep most of the time. A session
/// asking for less gets this.
pub const LEAST_INTERVAL: Nanos = Nanos(50_000);

/// The longest one, which a session asking for more than this gets.
///
/// A counter difference is multiplied by the page's scale in 64 bits, which at ordinary rates
/// leaves about a second of room and rather less at fast ones.
pub const MOST_INTERVAL: Nanos = Nanos(100 * 1_000_000);

/// How often this session's page is republished: what the header recorded at creation, held to a
/// range the page's own arithmetic survives.
///
/// Read from the header rather than from the environment, so an updater respawned long after the
/// creator is gone keeps the cadence the session was set up with, and so the drain a republisher
/// aims at matches the cadence that will deliver it.
#[must_use]
pub fn interval(header: &Header) -> Nanos {
    let recorded = i64::try_from(header.timekeep_interval).unwrap_or(MOST_INTERVAL.0);
    Nanos(recorded.clamp(LEAST_INTERVAL.0, MOST_INTERVAL.0))
}

/// How long the counter takes to come back round to where it started, in genuine nanoseconds.
///
/// A reader masks the difference of two counter readings to the counter's width, so parameters
/// older than this describe a clock nobody can compute: the difference has wrapped, and what comes
/// out is a small number of ticks rather than a large one.
fn wrap_period(basis: Basis) -> Nanos {
    let ticks = u128::from(basis.mask) + 1;
    // The scale is genuine seconds per tick in units of 2^-64, so the product is genuine seconds in
    // the same units, and the nanoseconds come out of the shift. Both stay far inside a `u128`: the
    // scale is bounded by the slowest counter we believe and the mask by its width.
    let nanos = (u128::from(basis.scale) * ticks * SECOND_FRAC) >> u64::BITS;
    Nanos(i64::try_from(nanos).unwrap_or(i64::MAX))
}

/// What a republish publishes: the two clocks it anchors at, and the rate it runs them at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Republish {
    /// What the page will show for the wall clock at the anchor instant.
    pub realtime: Nanos,
    /// What it will show for the elapsed clock there.
    pub elapsed: Nanos,
    /// How fast it will run until the next republish.
    pub rate: Rate,
}

/// The continuity rule: what to publish, given the session's own values, what the page shows at the
/// same instant, and whether the user has stepped the clock since the last republish.
///
/// A swap-published linear clock must be continuous at its swap instant, so new parameters anchor
/// at the value the page itself shows and the page never steps back of its own accord. What that
/// leaves is a lead over the session, which drains:
///
/// - **Behind the session.** Republishing onto the session's line is a forward step, so it snaps.
/// - **Ahead of it.** The page keeps its own value and runs under the session's rate, by enough to
///   give the lead back by the next republish and never by more than [`SLEW_DIVISOR`] allows, the
///   way NTP slews rather than steps. A frozen session publishes frozen, so the lead holds until
///   the next forward motion absorbs it.
/// - **Stepped.** A move of the value is the user's own discontinuity, and the wall clock follows
///   it in either direction. The elapsed clock follows forward only, which is what its API
///   promises.
#[must_use]
pub fn continuous(view: &View, shown: Option<Shown>, stepped: bool, interval: Nanos) -> Republish {
    let Some(shown) = shown else {
        // Nothing a reader has carried off yet, so there is nothing to be continuous with.
        return Republish {
            realtime: view.realtime,
            elapsed: view.elapsed,
            rate: view.rate,
        };
    };
    let realtime = if stepped {
        view.realtime
    } else {
        shown.realtime.max(view.realtime)
    };
    let elapsed = shown.elapsed.max(view.elapsed);
    // One scale serves both clocks, so the slew answers the further of the two leads. The other
    // clock falls behind the session by the difference and snaps forward at the next republish,
    // which is a forward step and allowed.
    let lead = realtime
        .0
        .saturating_sub(view.realtime.0)
        .max(elapsed.0.saturating_sub(view.elapsed.0))
        .max(0);
    Republish {
        realtime,
        elapsed,
        rate: drained(view.rate, Nanos(lead), interval),
    }
}

/// How far under the session's rate a page ahead of it may run: a rate change a reader sees as a
/// slew rather than as a stall, and the bound on how long a lead takes to drain.
const SLEW_DIVISOR: u64 = 8;

/// The rate a page leading the session by `lead` publishes.
///
/// Enough under the session's own to give the whole lead back by the next republish, where that is
/// a rate the page may run at, and the most the slew allows otherwise. A frozen session has no rate
/// to come down from, so the lead simply holds.
fn drained(rate: Rate, lead: Nanos, interval: Nanos) -> Rate {
    if lead.0 <= 0 {
        return rate;
    }
    let per_interval = (u128::from(lead.0.unsigned_abs()) << Rate::FRACTION_BITS)
        / u128::from(interval.0.max(1).unsigned_abs());
    let slewed = rate
        .0
        .saturating_sub(u64::try_from(per_interval).unwrap_or(u64::MAX));
    Rate(slewed.max(rate.0 - rate.0 / SLEW_DIVISOR))
}

/// Rewrites the page from the session's own parameters, continuous at the instant it does so.
///
/// Done by the updater at every tick and by every writer as part of its publish, so a change
/// reaches a reader of the page in microseconds rather than at the next tick. `None` when this
/// process cannot write the page now: another republisher holds it, the clocks will not read, or
/// the session's keeper is one we do not know.
///
/// The clocks are read before the counter, deliberately: the parameters then describe an instant a
/// shade before the one they are anchored at, so the page runs a hair behind the session rather
/// than a hair ahead, and a reader that catches up is stepping forwards.
pub fn republish(header: &Header, page: &Page, basis: Basis) -> Option<()> {
    let now = platform::genuine_nanos(clock::MONOTONIC)?;
    let _publishing = page.take(now)?;
    let view = view(header)?;
    let count = basis.read();
    let shown = page.showing(basis, count, now);
    let stepped = page.continuity.epoch.load(Ordering::Relaxed) != view.epoch;

    let next = continuous(&view, shown, stepped, interval(header));
    let offset = Bintime::from_nanos(next.elapsed);
    page.timekeep.publish(&Hands {
        algo: basis.algo,
        flavour: basis.flavour,
        scale: scaled(basis.scale, next.rate),
        offset_count: count,
        counter_mask: basis.mask,
        offset,
        boottime: Bintime::from_nanos(next.realtime).minus(offset),
    });
    // Under the page, so the next republisher reads these with the take that follows this one's
    // release. Both are the republishers' own; no reader of the page looks at either.
    page.continuity.epoch.store(view.epoch, Ordering::Relaxed);
    page.continuity
        .published_at
        .store(now.0.max(1), Ordering::Relaxed);
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
                epoch: params.epoch.load(Ordering::Relaxed),
            })
        }),
        // Genuine time underneath, so the page carries the kernel's own clocks at real speed. No
        // writer can move a null session, so nothing ever steps it.
        Keeper::Null => Some(View {
            realtime: platform::genuine_nanos(clock::REALTIME)?,
            elapsed: platform::genuine_nanos(elapsed_clock)?,
            rate: Rate::UNITY,
            epoch: 0,
        }),
    }
}

#[cfg(test)]
mod tests;
