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
//! Once the keeps and forgets of a key have returned, one slot at most holds it. A keep writes its
//! clock into the first slot on the key's walk that holds the key, frees any later one, and claims
//! a free slot only where none holds it. Keeps of one key that race can each claim a slot, so a
//! claimer walks the key's slots again behind a sequentially consistent fence, which makes at
//! least one of any two claimers see the other. The slot nearest home wins: a claimer that finds
//! the key nearer home writes its clock there and frees its own, and one that finds it farther out
//! frees that slot. A forget frees every slot holding the key. No call waits on another, so a
//! signal handler can make any of them whatever it interrupted.
//!
//! A key is whatever number names the object, and every number is one: a condition variable's
//! address, a descriptor, a `timer_t`, which glibc counts up from zero, and a port. So a slot says
//! whether it is occupied in a word of its own rather than by a key no object could have.

use common::platform::{clock, clock_id};

use super::{AtomicBool, AtomicU64, AtomicUsize, Ordering, fence};

/// A slot nothing was ever put in, whose whole word is this. A lookup stops at one, because
/// nothing past it was reached from this key.
pub(super) const EMPTY: u64 = 0;

/// A slot whose object was forgotten. A lookup walks past one; an insert reuses it.
const FREED: u64 = 1;

/// A slot an insert has claimed and not yet written its key into.
const CLAIMING: u64 = 2;

/// A slot holding a key whose insert is still looking for other slots holding it. A lookup walks
/// past one; a keep or a forget of the key treats it as holding the key.
const PENDING: u64 = 3;

/// A slot holding one object, whose key and clock are both there to read.
const TAKEN: u64 = 4;

/// The bits of a [`Word`] that hold the phase.
const PHASE: u64 = 0b111;

/// The low half of a [`Word`], the phase and the generation, below the clock.
const TENURE: u64 = 0xFFFF_FFFF;

/// The multiplier a key is mixed with, Fibonacci hashing, because these objects come in arrays and
/// the low bits of neighbouring addresses differ by one stride.
const PHI: u64 = 0x9E37_79B9_7F4A_7C15;

/// A slot's state: the phase in the low three bits, above it the generation, which counts the
/// claims of the slot and so tells one object's tenure of it from the next, and the clock in the
/// high half. Every change of a slot is one exchange of this word, so a write meant for one
/// tenure never lands in another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Word(u64);

impl Word {
    /// [`EMPTY`], [`FREED`], [`CLAIMING`], [`PENDING`], or [`TAKEN`].
    const fn phase(self) -> u64 {
        self.0 & PHASE
    }

    /// Whether the slot holds a key, published or not.
    const fn live(self) -> bool {
        matches!(self.phase(), PENDING | TAKEN)
    }

    /// Whether an insert may claim the slot.
    const fn free(self) -> bool {
        matches!(self.phase(), EMPTY | FREED)
    }

    /// Whether both words belong to one claim of the slot.
    const fn same_tenure(self, other: Self) -> bool {
        (self.0 & TENURE & !PHASE) == (other.0 & TENURE & !PHASE)
    }

    fn clock(self) -> libc::clockid_t {
        let bits = u32::try_from(self.0 >> 32).unwrap_or(0);
        clock_id(bits.cast_signed())
    }

    const fn in_phase(self, phase: u64) -> Self {
        Self((self.0 & !PHASE) | phase)
    }

    fn with_clock(self, clock: libc::clockid_t) -> Self {
        let bits = clock_id::<_, i32>(clock).cast_unsigned();
        Self((self.0 & TENURE) | (u64::from(bits) << 32))
    }

    /// The word a claim of a free slot writes: the next generation, with `clock`.
    fn claimed(self, clock: libc::clockid_t) -> Self {
        let generation = (self.0 & TENURE).wrapping_add(PHASE + 1) & TENURE & !PHASE;
        Self(generation | CLAIMING).with_clock(clock)
    }
}

/// One object: its key, and a word with its clock and whether the key is there to read.
#[derive(Debug)]
pub(crate) struct Slot {
    /// A [`Word`].
    pub(super) state: AtomicU64,
    /// The key of the object this slot holds, written while the slot is [`CLAIMING`].
    pub(super) object: AtomicUsize,
}

impl Slot {
    /// The state, loaded Acquire, so the key written before the word is there to read.
    fn word(&self) -> Word {
        Word(self.state.load(Ordering::Acquire))
    }

    /// The word of this slot when, after reading `first`, it holds `key` in the same tenure.
    ///
    /// The key is loaded Acquire, which puts the second load of the state after the claim that
    /// wrote the key, so a slot claimed again between the two loads reads as another tenure.
    fn holding(&self, first: Word, key: usize) -> Option<Word> {
        if !first.live() || self.object.load(Ordering::Acquire) != key {
            return None;
        }
        let word = self.word();
        (word.live() && word.same_tenure(first)).then_some(word)
    }

    /// Claims this slot for `key` and `clock` while it is free and writes the key, and returns the
    /// word that says the key is there to read, or `None` once another insert has the slot.
    fn claim(&self, mut word: Word, key: usize, clock: libc::clockid_t) -> Option<Word> {
        while word.free() {
            let claimed = word.claimed(clock);
            match self.state.compare_exchange(
                word.0,
                claimed.0,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.object.store(key, Ordering::Release);
                    return Some(claimed.in_phase(PENDING));
                }
                Err(now) => word = Word(now),
            }
        }
        None
    }

    /// Writes `clock` into this slot as a published one for as long as it holds the key `word` was
    /// read with, and whether it did.
    fn adopt(&self, mut word: Word, clock: libc::clockid_t) -> bool {
        let tenure = word;
        loop {
            let adopted = word.in_phase(TAKEN).with_clock(clock);
            match self.state.compare_exchange(
                word.0,
                adopted.0,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(now) => word = Word(now),
            }
            if !word.live() || !word.same_tenure(tenure) {
                return false;
            }
        }
    }

    /// Frees this slot if it still holds the key `word` was read with.
    fn release(&self, mut word: Word) {
        let tenure = word;
        while let Err(now) = self.state.compare_exchange(
            word.0,
            word.in_phase(FREED).0,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            word = Word(now);
            if !word.live() || !word.same_tenure(tenure) {
                return;
            }
        }
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
    /// A key already held takes `clock` in the slot that holds it. A table with every slot taken
    /// leaves the object unrecorded and says so in [`Table::clock_of`] from then on.
    pub(crate) fn keep(&self, key: usize, clock: libc::clockid_t) {
        if self.settle(key, clock, None) {
            return;
        }
        let Some(own) = self.claim(key, clock) else {
            self.lost.store(true, Ordering::Relaxed);
            return;
        };
        // Of two keeps of one key that have each claimed a slot, this makes the walk of at least
        // one of them see the other's.
        fence(Ordering::SeqCst);
        self.settle(key, clock, Some(own));
    }

    /// Claims the first free slot on `key`'s walk, with the probe it lies at and the word that says
    /// the key is in it, or `None` when every slot holds an object.
    fn claim(&self, key: usize, clock: libc::clockid_t) -> Option<(usize, Word)> {
        (0..N).find_map(|probe| {
            let slot = self.slot(key, probe)?;
            let pending = slot.claim(slot.word(), key, clock)?;
            self.reach.fetch_max(probe + 1, Ordering::Relaxed);
            // The key is published by this, so a walk that sees the slot pending reads it.
            slot.state.store(pending.0, Ordering::Release);
            Some((probe, pending))
        })
    }

    /// Writes `clock` into the first slot on `key`'s walk that holds the key and frees every later
    /// one, and whether a slot was left holding it. `own` is the probe and the word of the slot
    /// this keep claimed, which holds the key like any other; once another keep or a forget has
    /// freed it, the walk ends there, because what lies past it is theirs to settle.
    fn settle(&self, key: usize, clock: libc::clockid_t, own: Option<(usize, Word)>) -> bool {
        let mut held = false;
        for probe in 0..self.reach() {
            let Some(slot) = self.slot(key, probe) else {
                break;
            };
            let mine = own.filter(|&(at, _)| at == probe);
            let word = if let Some((_, word)) = mine {
                word
            } else {
                let first = slot.word();
                if first.phase() == EMPTY {
                    break;
                }
                let Some(word) = slot.holding(first, key) else {
                    continue;
                };
                word
            };
            if held {
                slot.release(word);
            } else if slot.adopt(word, clock) {
                held = true;
            } else if mine.is_some() {
                return false;
            }
        }
        held
    }

    /// Forgets the object at `key`, in every slot that holds it.
    pub(crate) fn forget(&self, key: usize) {
        for probe in 0..self.reach() {
            let Some(slot) = self.slot(key, probe) else {
                return;
            };
            let first = slot.word();
            if first.phase() == EMPTY {
                return;
            }
            if let Some(word) = slot.holding(first, key) {
                slot.release(word);
            }
        }
    }

    /// The clock recorded for the object at `key`, or `None` when nothing recorded one.
    pub(crate) fn kept(&self, key: usize) -> Option<libc::clockid_t> {
        for probe in 0..self.reach() {
            let slot = self.slot(key, probe)?;
            let first = slot.word();
            if first.phase() == EMPTY {
                break;
            }
            if let Some(word) = slot.holding(first, key)
                && word.phase() == TAKEN
            {
                return Some(word.clock());
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

    /// How many slots hold `key`, published or not.
    #[cfg(test)]
    pub(super) fn holders(&self, key: usize) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.holding(slot.word(), key).is_some())
            .count()
    }
}

// One slot driven through an interleaving by hand, which a model checker over the table does not
// reach: the slot changes hands between a reader's first load and its load of the key.
#[cfg(test)]
mod tests {
    use super::*;

    fn published(slot: &Slot, key: usize, clock: libc::clockid_t) -> Word {
        let pending = slot.claim(slot.word(), key, clock).unwrap();
        slot.state.store(pending.0, Ordering::Release);
        assert!(slot.adopt(pending, clock));
        slot.word()
    }

    #[test]
    #[cfg_attr(loom, ignore = "loom's atomics work only inside a model")]
    fn a_word_read_before_the_slot_changed_hands_holds_no_key_and_moves_nothing() {
        let slot = Slot {
            state: AtomicU64::new(EMPTY),
            object: AtomicUsize::new(0),
        };
        let (gone, new) = (0x1_0000, 0x2_0000);
        let stale = published(&slot, gone, clock::PROCESS_CPUTIME);
        slot.release(stale);
        let current = published(&slot, new, clock::MONOTONIC);

        assert_eq!(slot.holding(stale, new), None);
        assert_eq!(slot.holding(current, new), Some(current));
        assert!(!slot.adopt(stale, clock::REALTIME));
        slot.release(stale);
        assert_eq!(slot.word(), current);
        assert_eq!(current.clock(), clock::MONOTONIC);
    }
}
