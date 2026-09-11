//! Which clock a condition variable reads its deadlines on.
//!
//! `pthread_cond_timedwait` names no clock: the deadline is read on whichever clock the variable
//! was built with. So the clock is learned where it is chosen, at `pthread_cond_init`, and
//! forgotten at `pthread_cond_destroy`. Neither call reads the time.
//!
//! Only a variable that asked for something other than `CLOCK_REALTIME` is kept here. That is the
//! default, it is what a variable built from `PTHREAD_COND_INITIALIZER` has without ever reaching
//! `pthread_cond_init`, and it is what an unknown variable reads, so keeping those would be
//! keeping the answer we already give.

use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

use common::platform::{clock, clock_id};

/// How many condition variables with a clock of their own this process remembers at once.
///
/// A program past this many is one whose later variables read the default. That costs a wait on
/// such a variable the family it follows, and nothing else: the deadline still converts, on
/// Realtime rather than on the clock the variable named.
const SLOTS: usize = 256;

/// How far a lookup walks from the slot a variable hashes to before it gives up. Past this the
/// table is crowded enough that walking further costs more than the default answer.
const PROBES: usize = 8;

/// A slot holding no variable.
const EMPTY: usize = 0;

/// A slot whose variable was destroyed. A lookup walks past one; an insert reuses it.
const FREED: usize = usize::MAX;

/// One variable's clock, or one of the two spellings of an empty slot.
#[derive(Debug)]
struct Slot {
    /// The address of the condition variable, or [`EMPTY`] or [`FREED`].
    cond: AtomicUsize,
    /// The clock that variable reads its deadlines on.
    clock: AtomicI32,
}

impl Slot {
    const fn new() -> Self {
        Self {
            cond: AtomicUsize::new(EMPTY),
            clock: AtomicI32::new(0),
        }
    }
}

/// Every variable this process remembers. Static, because a wait may be reached from anywhere the
/// program can call one, and nothing here may allocate.
static TABLE: Table = Table::new();

/// A fixed set of slots, walked by linear probing. The operations take the table they work on so a
/// case can exercise one of its own rather than the process's.
#[derive(Debug)]
struct Table([Slot; SLOTS]);

impl Table {
    const fn new() -> Self {
        Self([const { Slot::new() }; SLOTS])
    }

    /// The slot a variable's `probe`th try lands in. Multiplicative hashing, because condition
    /// variables come in arrays and the low bits of neighbouring addresses differ by one stride.
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

    /// Remembers that the variable at `key` reads its deadlines on `clock`.
    ///
    /// A table with no room leaves the variable unknown, which is the default answer rather than a
    /// wrong one.
    fn remember(&self, key: usize, clock: libc::clockid_t) {
        for probe in 0..PROBES {
            let Some(slot) = self.slot(key, probe) else {
                return;
            };
            let held = slot.cond.load(Ordering::Acquire);
            if held == key {
                slot.clock.store(clock_id(clock), Ordering::Release);
                return;
            }
            if (held == EMPTY || held == FREED)
                && slot
                    .cond
                    .compare_exchange(held, key, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            {
                // The clock lands after the key, so a lookup racing this one reads a stale clock.
                // Only a program waiting on a variable another of its threads is still
                // initialising can be in that race, and that program has already lost the
                // variable.
                slot.clock.store(clock_id(clock), Ordering::Release);
                return;
            }
        }
    }

    /// Forgets the variable at `key`.
    fn forget(&self, key: usize) {
        for probe in 0..PROBES {
            let Some(slot) = self.slot(key, probe) else {
                return;
            };
            match slot.cond.load(Ordering::Acquire) {
                // An empty slot ends the walk: nothing past it was ever reached from this key.
                EMPTY => return,
                held if held == key => {
                    slot.cond.store(FREED, Ordering::Release);
                    return;
                }
                _ => {}
            }
        }
    }

    /// The clock the variable at `key` reads its deadlines on, or the default when it is unknown.
    fn clock_of(&self, key: usize) -> libc::clockid_t {
        for probe in 0..PROBES {
            let Some(slot) = self.slot(key, probe) else {
                break;
            };
            match slot.cond.load(Ordering::Acquire) {
                EMPTY => break,
                held if held == key => return clock_id(slot.clock.load(Ordering::Acquire)),
                _ => {}
            }
        }
        clock::REALTIME
    }
}

/// The key a variable's address is, or `None` for the two addresses that spell an empty slot.
fn key_of(cond: *const libc::pthread_cond_t) -> Option<usize> {
    match cond.addr() {
        EMPTY | FREED => None,
        key => Some(key),
    }
}

/// Remembers that `cond` reads its deadlines on `clock`, or forgets it when that is the default an
/// unknown variable already reads.
pub(crate) fn remember(cond: *const libc::pthread_cond_t, clock: libc::clockid_t) {
    let Some(key) = key_of(cond) else {
        return;
    };
    if clock == clock::REALTIME {
        TABLE.forget(key);
    } else {
        TABLE.remember(key, clock);
    }
}

/// Forgets `cond`, which is what `pthread_cond_destroy` does.
pub(crate) fn forget(cond: *const libc::pthread_cond_t) {
    if let Some(key) = key_of(cond) {
        TABLE.forget(key);
    }
}

/// The clock `cond` reads its deadlines on: the one it was built with, or `CLOCK_REALTIME`, which
/// is both the default and what an unknown variable gets.
pub(crate) fn clock_of(cond: *const libc::pthread_cond_t) -> libc::clockid_t {
    key_of(cond).map_or(clock::REALTIME, |key| TABLE.clock_of(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key a condition variable at the `which`th address of an array would have.
    fn key(which: usize) -> usize {
        0x1_0000 + which * size_of::<libc::pthread_cond_t>()
    }

    #[test]
    fn a_variable_nobody_named_reads_the_default() {
        let table = Table::new();
        assert_eq!(table.clock_of(key(1)), clock::REALTIME);
        assert_eq!(clock_of(std::ptr::null()), clock::REALTIME);
    }

    #[test]
    fn a_variable_with_a_clock_of_its_own_keeps_it_until_it_is_destroyed() {
        let table = Table::new();
        table.remember(key(2), clock::MONOTONIC);
        assert_eq!(table.clock_of(key(2)), clock::MONOTONIC);
        table.forget(key(2));
        assert_eq!(table.clock_of(key(2)), clock::REALTIME);
    }

    #[test]
    fn re_initialising_with_the_default_forgets_the_clock() {
        // Through the process's own table, which is what an interposed call reaches.
        let cond: *const libc::pthread_cond_t = std::ptr::without_provenance(key(0xBEEF) | 1 << 40);
        remember(cond, clock::BOOTTIME);
        assert_eq!(clock_of(cond), clock::BOOTTIME);
        remember(cond, clock::REALTIME);
        assert_eq!(clock_of(cond), clock::REALTIME);
    }

    #[test]
    fn a_destroyed_slot_is_reused_and_the_walk_still_reaches_past_it() {
        let table = Table::new();
        // Two keys one probe apart, so the second sits past the first and finds it on the way.
        let (first, second) = (key(4), key(5));
        table.remember(first, clock::MONOTONIC);
        table.remember(second, clock::BOOTTIME);
        assert_eq!(table.clock_of(first), clock::MONOTONIC);
        assert_eq!(table.clock_of(second), clock::BOOTTIME);

        table.forget(first);
        assert_eq!(table.clock_of(first), clock::REALTIME);
        assert_eq!(
            table.clock_of(second),
            clock::BOOTTIME,
            "the walk stopped at the freed slot"
        );

        table.remember(first, clock::MONOTONIC);
        assert_eq!(table.clock_of(first), clock::MONOTONIC);
        assert_eq!(table.clock_of(second), clock::BOOTTIME);
    }

    #[test]
    fn a_full_table_leaves_a_variable_on_the_default_rather_than_a_wrong_clock() {
        let table = Table::new();
        let planted: Vec<usize> = (0..4 * SLOTS).map(key).collect();
        for &key in &planted {
            table.remember(key, clock::MONOTONIC);
        }
        // More variables than slots, so some were never recorded. Each reads either the clock it
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
