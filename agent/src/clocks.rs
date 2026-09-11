//! Which clock an object of the program's was built with.
//!
//! Several calls name no clock of their own: a `pthread_cond_timedwait` reads its deadline on
//! whichever clock the variable was built with, and a `timer_settime` or a `timerfd_settime` arms
//! against whichever clock the timer was created on. So the clock is learned where it is chosen,
//! at the call that creates the object, and looked up again at the call that waits or arms.
//!
//! A table is a fixed set of slots walked by linear probing. It is static in the caller, because a
//! wait or an arm may be reached from anywhere the program can call one, and nothing here may
//! allocate. Only an object that asked for something other than `CLOCK_REALTIME` is kept: that is
//! the default, it is what an object nobody named reads, so keeping those would be keeping the
//! answer we already give.
//!
//! A key is whatever number names the object, and every number is one: a condition variable's
//! address, a descriptor, and a `timer_t`, which glibc counts up from zero. So a slot says whether
//! it is occupied in a word of its own rather than by a key no object could have.

use std::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering};

use common::platform::{clock, clock_id};

/// How many objects with a clock of their own one table holds at once.
///
/// A program past this many is one whose later objects read the default. That costs such an object
/// the family it follows, and nothing else: the deadline or the arm still converts, on Realtime
/// rather than on the clock the object named.
const SLOTS: usize = 256;

/// How far a lookup walks from the slot a key hashes to before it gives up. Past this the table is
/// crowded enough that walking further costs more than the default answer.
const PROBES: usize = 8;

/// A slot nothing was ever put in. A lookup stops at one, because nothing past it was reached from
/// this key.
const EMPTY: u32 = 0;

/// A slot holding one object, whose key and clock are both there to read.
const TAKEN: u32 = 1;

/// A slot whose object was forgotten. A lookup walks past one; an insert reuses it.
const FREED: u32 = 2;

/// A slot an insert has claimed and not yet filled. A lookup walks past one, and finds the object
/// once the insert has published it.
const CLAIMING: u32 = 3;

/// One object's clock, and the word that says whether the other two are there to read.
#[derive(Debug)]
struct Slot {
    /// [`EMPTY`], [`TAKEN`], [`FREED`], or [`CLAIMING`].
    state: AtomicU32,
    /// The key of the object this slot holds.
    object: AtomicUsize,
    /// The clock that object was built with.
    clock: AtomicI32,
}

impl Slot {
    const fn new() -> Self {
        Self {
            state: AtomicU32::new(EMPTY),
            object: AtomicUsize::new(0),
            clock: AtomicI32::new(0),
        }
    }

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

/// A fixed set of slots, walked by linear probing. The operations take the table they work on so a
/// case can exercise one of its own rather than the process's.
#[derive(Debug)]
pub(crate) struct Table([Slot; SLOTS]);

impl Table {
    pub(crate) const fn new() -> Self {
        Self([const { Slot::new() }; SLOTS])
    }

    /// The slot a key's `probe`th try lands in. Multiplicative hashing, because these objects come
    /// in arrays and the low bits of neighbouring addresses differ by one stride.
    fn slot(&self, key: usize, probe: usize) -> Option<&Slot> {
        let mixed = u64::try_from(key)
            .unwrap_or(0)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let at = usize::try_from(mixed >> 32)
            .unwrap_or(0)
            .wrapping_add(probe)
            % SLOTS;
        self.0.get(at)
    }

    /// Remembers that the object at `key` was built with `clock`, or forgets it when that is the
    /// default an unknown object already reads.
    pub(crate) fn record(&self, key: usize, clock: libc::clockid_t) {
        if clock == clock::REALTIME {
            self.forget(key);
        } else {
            self.remember(key, clock);
        }
    }

    /// Remembers that the object at `key` was built with `clock`.
    ///
    /// A table with no room leaves the object unknown, which is the default answer rather than a
    /// wrong one.
    fn remember(&self, key: usize, clock: libc::clockid_t) {
        for probe in 0..PROBES {
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
                // The two words above are published by this one, so a lookup that sees the slot
                // taken sees the whole of what is in it.
                slot.state.store(TAKEN, Ordering::Release);
                return;
            }
        }
    }

    /// Forgets the object at `key`.
    pub(crate) fn forget(&self, key: usize) {
        for probe in 0..PROBES {
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

    /// The clock the object at `key` was built with, or the default when it is unknown.
    pub(crate) fn clock_of(&self, key: usize) -> libc::clockid_t {
        for probe in 0..PROBES {
            let Some(slot) = self.slot(key, probe) else {
                break;
            };
            if slot.state.load(Ordering::Acquire) == EMPTY {
                break;
            }
            if let Some(clock) = slot.held(key) {
                return clock;
            }
        }
        clock::REALTIME
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key an object at the `which`th address of an array would have.
    fn key(which: usize) -> usize {
        0x1_0000 + which * 48
    }

    #[test]
    fn an_object_nobody_named_reads_the_default() {
        let table = Table::new();
        assert_eq!(table.clock_of(key(1)), clock::REALTIME);
    }

    #[test]
    fn every_number_is_a_key_a_slot_can_hold() {
        // Zero is what glibc's first `timer_create` hands back, and the whole range's ends are what
        // a table that spelled an empty slot with a key would have refused.
        let table = Table::new();
        for key in [0, 1, usize::MAX, usize::MAX - 1] {
            table.record(key, clock::MONOTONIC);
            assert_eq!(table.clock_of(key), clock::MONOTONIC, "{key}");
        }
    }

    #[test]
    fn an_object_with_a_clock_of_its_own_keeps_it_until_it_is_forgotten() {
        let table = Table::new();
        table.record(key(2), clock::MONOTONIC);
        assert_eq!(table.clock_of(key(2)), clock::MONOTONIC);
        table.forget(key(2));
        assert_eq!(table.clock_of(key(2)), clock::REALTIME);
    }

    #[test]
    fn recording_the_default_forgets_the_clock() {
        let table = Table::new();
        table.record(key(3), clock::BOOTTIME);
        assert_eq!(table.clock_of(key(3)), clock::BOOTTIME);
        table.record(key(3), clock::REALTIME);
        assert_eq!(table.clock_of(key(3)), clock::REALTIME);
        // And recording it afresh puts it back, which is what a descriptor reused is.
        table.record(key(3), clock::MONOTONIC);
        assert_eq!(table.clock_of(key(3)), clock::MONOTONIC);
    }

    #[test]
    fn a_forgotten_slot_is_reused_and_the_walk_still_reaches_past_it() {
        let table = Table::new();
        // Two keys one probe apart, so the second sits past the first and finds it on the way.
        let (first, second) = (key(4), key(5));
        table.record(first, clock::MONOTONIC);
        table.record(second, clock::BOOTTIME);
        assert_eq!(table.clock_of(first), clock::MONOTONIC);
        assert_eq!(table.clock_of(second), clock::BOOTTIME);

        table.forget(first);
        assert_eq!(table.clock_of(first), clock::REALTIME);
        assert_eq!(
            table.clock_of(second),
            clock::BOOTTIME,
            "the walk stopped at the forgotten slot"
        );

        table.record(first, clock::MONOTONIC);
        assert_eq!(table.clock_of(first), clock::MONOTONIC);
        assert_eq!(table.clock_of(second), clock::BOOTTIME);
    }

    #[test]
    fn a_full_table_leaves_an_object_on_the_default_rather_than_a_wrong_clock() {
        let table = Table::new();
        let planted: Vec<usize> = (0..4 * SLOTS).map(key).collect();
        for &key in &planted {
            table.record(key, clock::MONOTONIC);
        }
        // More objects than slots, so some were never recorded. Each reads either the clock it
        // asked for or the default, and never somebody else's.
        let mut recorded = 0;
        for &key in &planted {
            match table.clock_of(key) {
                clock::MONOTONIC => recorded += 1,
                clock::REALTIME => {}
                other => panic!("{key:x} reads {other}, which is nobody's"),
            }
        }
        assert!(recorded > 0 && recorded <= SLOTS, "{recorded} recorded");
    }
}
