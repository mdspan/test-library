//! The slots and the walk, written once over whichever atomics the module above brings: the
//! standard ones for the tables the process keeps, and loom's where a test drives every
//! interleaving of them.
//!
//! A table is a fixed set of slots walked by linear probing. An insert walks as far as it must to
//! find a free slot, so it comes away empty only when every slot holds an object. The farthest any
//! insert has walked is as far as a lookup walks, so a table that was never crowded keeps its
//! lookups short. A lookup also stops at a slot nothing was ever put in, because nothing past it
//! was reached from this key.
//!
//! A key is whatever number names the object, and every number is one: a condition variable's
//! address, a descriptor, a `timer_t`, which glibc counts up from zero, and a port. So a slot says
//! whether it is occupied in a word of its own rather than by a key no object could have.

use common::platform::{clock, clock_id};

use super::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};

/// A slot nothing was ever put in. A lookup stops at one, because nothing past it was reached from
/// this key.
pub(super) const EMPTY: u32 = 0;

/// A slot holding one object, whose key and clock are both there to read.
const TAKEN: u32 = 1;

/// A slot whose object was forgotten. A lookup walks past one; an insert reuses it.
const FREED: u32 = 2;

/// A slot an insert has claimed and not yet filled. A lookup walks past one, and finds the object
/// once the insert has published it.
const CLAIMING: u32 = 3;

/// The multiplier a key is mixed with, Fibonacci hashing, because these objects come in arrays and
/// the low bits of neighbouring addresses differ by one stride.
const PHI: u64 = 0x9E37_79B9_7F4A_7C15;

/// One object's clock, and the word that says whether the other two are there to read.
#[derive(Debug)]
pub(crate) struct Slot {
    /// [`EMPTY`], [`TAKEN`], [`FREED`], or [`CLAIMING`].
    pub(super) state: AtomicU32,
    /// The key of the object this slot holds.
    pub(super) object: AtomicUsize,
    /// The clock that object was built with.
    pub(super) clock: AtomicI32,
}

impl Slot {
    /// The clock this slot holds for `key`, or `None` when it holds another object or none.
    ///
    /// The state is loaded Acquire and the other two words Relaxed, because the Release store that
    /// published them is what this acquires. An insert that has claimed the slot and not yet
    /// published is not [`TAKEN`], so nothing half-written is ever read.
    fn held(&self, key: usize) -> Option<libc::clockid_t> {
        (self.state.load(Ordering::Acquire) == TAKEN && self.object.load(Ordering::Relaxed) == key)
            .then(|| clock_id(self.clock.load(Ordering::Relaxed)))
    }
}

/// `N` slots, and what a lookup needs to know about the inserts into them. The operations take the
/// table they work on so a case can exercise one of its own rather than the process's.
#[derive(Debug)]
pub(crate) struct Table<const N: usize> {
    /// Where the objects are.
    pub(super) slots: [Slot; N],
    /// How many slots, its own first, the farthest insert has walked from a key's own slot. Only
    /// ever raised, and raised before the insert publishes, so a lookup made after an insert walks
    /// at least as far as that insert went.
    pub(super) reach: AtomicUsize,
    /// Whether an insert has once found every slot taken and left its object unrecorded.
    pub(super) lost: AtomicBool,
}

impl<const N: usize> Table<N> {
    /// The slot a key's walk starts from.
    pub(super) fn home(key: usize) -> usize {
        let mixed = u64::try_from(key).unwrap_or(0).wrapping_mul(PHI);
        usize::try_from(mixed >> 32).unwrap_or(0) % N
    }

    /// The slot a key's `probe`th try lands in.
    fn slot(&self, key: usize, probe: usize) -> Option<&Slot> {
        self.slots.get(Self::home(key).wrapping_add(probe) % N)
    }

    /// How many slots a lookup walks before it gives up.
    ///
    /// Relaxed: a lookup the program made after an insert is ordered after it by the program's own
    /// synchronization, and that already makes this load see what the insert raised.
    fn reach(&self) -> usize {
        self.reach.load(Ordering::Relaxed).min(N)
    }

    /// Remembers that the object at `key` was built with `clock`, or forgets it when that is the
    /// default an unknown object already reads.
    pub(crate) fn record(&self, key: usize, clock: libc::clockid_t) {
        if clock == clock::REALTIME {
            self.forget(key);
        } else {
            self.keep(key, clock);
        }
    }

    /// Remembers that the object at `key` was built with `clock`, whichever clock that is.
    ///
    /// What a caller needs when every clock is worth a slot: a clock port, whose two Mach clocks
    /// are the wall clock and an elapsed one, and a timer, which nothing builds without asking.
    ///
    /// A table with every slot taken leaves the object unrecorded and says so in
    /// [`Table::clock_of`] from then on.
    pub(crate) fn keep(&self, key: usize, clock: libc::clockid_t) {
        for probe in 0..N {
            let Some(slot) = self.slot(key, probe) else {
                return;
            };
            if slot.held(key).is_some() {
                slot.clock.store(clock_id(clock), Ordering::Relaxed);
                return;
            }
            let state = slot.state.load(Ordering::Acquire);
            if (state == EMPTY || state == FREED)
                && slot
                    .state
                    .compare_exchange(state, CLAIMING, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            {
                slot.object.store(key, Ordering::Relaxed);
                slot.clock.store(clock_id(clock), Ordering::Relaxed);
                self.reach.fetch_max(probe + 1, Ordering::Relaxed);
                // The two words above are published by this one, so a lookup that sees the slot
                // taken sees the whole of what is in it.
                slot.state.store(TAKEN, Ordering::Release);
                return;
            }
        }
        self.lost.store(true, Ordering::Relaxed);
    }

    /// Forgets the object at `key`.
    pub(crate) fn forget(&self, key: usize) {
        for probe in 0..self.reach() {
            let Some(slot) = self.slot(key, probe) else {
                return;
            };
            if slot.state.load(Ordering::Acquire) == EMPTY {
                return;
            }
            if slot.held(key).is_some() {
                slot.state.store(FREED, Ordering::Release);
                return;
            }
        }
    }

    /// The clock recorded for the object at `key`, or `None` when nothing recorded one.
    pub(crate) fn kept(&self, key: usize) -> Option<libc::clockid_t> {
        for probe in 0..self.reach() {
            let slot = self.slot(key, probe)?;
            if slot.state.load(Ordering::Acquire) == EMPTY {
                break;
            }
            if let Some(clock) = slot.held(key) {
                return Some(clock);
            }
        }
        None
    }

    /// The clock the object at `key` was built with: the one recorded, or the default when nothing
    /// was and the table has never been full. `None` once it has, because an object with nothing
    /// recorded may then be one it had no room for.
    ///
    /// Relaxed: an insert that found no room and the lookup of the object it was for are ordered by
    /// the program's own synchronization.
    pub(crate) fn clock_of(&self, key: usize) -> Option<libc::clockid_t> {
        self.kept(key)
            .or_else(|| (!self.lost.load(Ordering::Relaxed)).then_some(clock::REALTIME))
    }
}
