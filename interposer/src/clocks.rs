//! Which clock an object of the program's was built with.
//!
//! Several calls name no clock of their own: a `pthread_cond_timedwait` reads its deadline on
//! whichever clock the variable was built with, and a `timer_settime` or a `timerfd_settime` arms
//! against whichever clock the timer was created on. So the clock is learned where it is chosen,
//! at the call that creates the object, and looked up again at the call that waits or arms.
//!
//! A table is static in the caller, because a wait or an arm may be reached from anywhere the
//! program can call one, and nothing here may allocate. [`table`] has the slots and the walk. A
//! condition variable built with `CLOCK_REALTIME` is not kept: that is the default, it is what a
//! variable nobody named reads, so keeping those would be keeping the answer we already give.
//!
//! macOS reaches for the same slots for something else: a Mach clock port is a name the kernel
//! hands out and `clock_get_time` reads, so the clock it stands for is learned at
//! `host_get_clock_service` and looked up again at the read. A port that named neither Mach clock
//! has no answer of ours at all rather than the default one, which is what [`Table::kept`] is for
//! beside [`Table::clock_of`].

// The condition variables and the timers are Linux's and FreeBSD's, and the clock ports are
// macOS's, so no one platform reaches for the whole of this.
#![cfg_attr(
    target_os = "macos",
    allow(
        dead_code,
        reason = "macOS keeps only the clock ports here: a condition variable there reads the wall \
                  clock, and the platform has neither a POSIX timer nor a timerfd to create"
    )
)]

mod table;

#[cfg(all(test, loom))]
mod interleavings;

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering, fence};

use common::platform::clock;
use table::{EMPTY, Slot};

/// The clocks an object whose clock is unknown is looked for among: the wall clock, which is the
/// default, and the monotonic clock, which is the usual choice of a program that makes one, and
/// the only other clock glibc lets a condition variable have. An object on any other clock has its
/// deadlines read on whichever of these two lies nearer.
pub(crate) const LIKELIEST: [libc::clockid_t; 2] = [clock::REALTIME, clock::MONOTONIC];

/// How many objects one table holds at once: 64 KiB of zeroes per table, which the system pages in
/// only as slots are used.
///
/// A table with every slot taken leaves a later object unrecorded, and from then on a lookup that
/// finds nothing answers that the clock is unknown rather than the default. Such an object's
/// absolute deadline is read on whichever of [`LIKELIEST`] reads nearer it: its own clock where it
/// is one of the two and the deadline lies less than half the way to the other's reading, and
/// otherwise the nearer of the two, which puts a `CLOCK_BOOTTIME` deadline off by the time the
/// machine has spent suspended. A relative arm or a query of such a timer runs on genuine time. A
/// table that has once been full also makes every lookup that finds nothing walk all of it.
const SLOTS: usize = 4096;

/// Every table the process keeps.
pub(crate) type Table = table::Table<SLOTS>;

impl Slot {
    const fn new() -> Self {
        Self {
            state: AtomicU64::new(EMPTY),
            object: AtomicUsize::new(0),
        }
    }
}

impl<const N: usize> table::Table<N> {
    pub(crate) const fn new() -> Self {
        Self {
            slots: [const { Slot::new() }; N],
            reach: AtomicUsize::new(0),
            lost: AtomicBool::new(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use common::platform::clock_id;

    use super::*;

    /// The key an object at the `which`th address of an array would have.
    fn key(which: usize) -> usize {
        0x1_0000 + which * 48
    }

    /// A third clock, apart from the default and the monotonic one, under whatever name this
    /// platform gives it: the first clock of the Boottime family's column.
    fn boottime() -> libc::clockid_t {
        common::platform::default_timebase(common::time::Family::Boottime)
            .expect("every family but Cpu tracks a clock")
    }

    /// `count` keys whose walks all start from the same slot of a table of `N`.
    fn crowding<const N: usize>(count: usize) -> Vec<usize> {
        let home = table::Table::<N>::home(key(0));
        (0..)
            .map(key)
            .filter(|&key| table::Table::<N>::home(key) == home)
            .take(count)
            .collect()
    }

    #[test]
    fn an_object_nobody_named_reads_the_default() {
        let table = Table::new();
        assert_eq!(table.clock_of(key(1)), Some(clock::REALTIME));
    }

    #[test]
    fn every_number_is_a_key_a_slot_can_hold() {
        // Zero is what glibc's first `timer_create` hands back, and the whole range's ends are what
        // a table that spelled an empty slot with a key would have refused.
        let table = Table::new();
        for key in [0, 1, usize::MAX, usize::MAX - 1] {
            table.record(key, clock::MONOTONIC);
            assert_eq!(table.clock_of(key), Some(clock::MONOTONIC), "{key}");
        }
    }

    #[test]
    fn an_object_with_a_clock_of_its_own_keeps_it_until_it_is_forgotten() {
        let table = Table::new();
        table.record(key(2), clock::MONOTONIC);
        assert_eq!(table.clock_of(key(2)), Some(clock::MONOTONIC));
        table.forget(key(2));
        assert_eq!(table.clock_of(key(2)), Some(clock::REALTIME));
    }

    #[test]
    fn recording_the_default_forgets_the_clock() {
        let table = Table::new();
        table.record(key(3), boottime());
        assert_eq!(table.clock_of(key(3)), Some(boottime()));
        table.record(key(3), clock::REALTIME);
        assert_eq!(table.clock_of(key(3)), Some(clock::REALTIME));
        // And recording it afresh puts it back, which is what a descriptor reused is.
        table.record(key(3), clock::MONOTONIC);
        assert_eq!(table.clock_of(key(3)), Some(clock::MONOTONIC));
    }

    #[test]
    fn a_forgotten_slot_is_reused_and_the_walk_still_reaches_past_it() {
        let table = Table::new();
        // Two keys with one home, so the second sits past the first and finds it on the way.
        let [first, second] = crowding::<SLOTS>(2)[..] else {
            panic!("two keys share a home");
        };
        table.record(first, clock::MONOTONIC);
        table.record(second, boottime());
        assert_eq!(table.clock_of(first), Some(clock::MONOTONIC));
        assert_eq!(table.clock_of(second), Some(boottime()));

        table.forget(first);
        assert_eq!(table.clock_of(first), Some(clock::REALTIME));
        assert_eq!(
            table.clock_of(second),
            Some(boottime()),
            "the walk stopped at the forgotten slot"
        );

        table.record(first, clock::MONOTONIC);
        assert_eq!(table.clock_of(first), Some(clock::MONOTONIC));
        assert_eq!(table.clock_of(second), Some(boottime()));
    }

    #[test]
    fn a_key_recorded_again_behind_a_freed_slot_is_forgotten_whole() {
        let table = Table::new();
        let [first, second] = crowding::<SLOTS>(2)[..] else {
            panic!("two keys share a home");
        };
        table.record(first, clock::MONOTONIC);
        table.record(second, clock::MONOTONIC);
        // The first key's slot is free and lies on the second's walk, ahead of where it is held.
        table.record(first, clock::REALTIME);
        table.record(second, clock::MONOTONIC);
        assert_eq!(table.holders(second), 1);
        assert_eq!(table.clock_of(second), Some(clock::MONOTONIC));
        table.record(second, clock::REALTIME);
        assert_eq!(table.holders(second), 0);
        assert_eq!(table.clock_of(second), Some(clock::REALTIME));
        assert_eq!(table.clock_of(first), Some(clock::REALTIME));
    }

    #[test]
    fn a_key_kept_again_behind_a_freed_slot_takes_the_new_clock_and_is_forgotten_whole() {
        let table = Table::new();
        let [first, second] = crowding::<SLOTS>(2)[..] else {
            panic!("two keys share a home");
        };
        table.keep(first, clock::MONOTONIC);
        table.keep(second, clock::MONOTONIC);
        table.forget(first);
        table.keep(second, boottime());
        assert_eq!(table.holders(second), 1);
        assert_eq!(table.kept(second), Some(boottime()));
        // The slot ahead of it is still free for the next key from that home.
        table.keep(first, clock::REALTIME);
        assert_eq!(table.kept(first), Some(clock::REALTIME));
        assert_eq!(table.kept(second), Some(boottime()));
        table.forget(second);
        assert_eq!(table.kept(second), None);
        assert_eq!(table.kept(first), Some(clock::REALTIME));
    }

    #[test]
    fn a_kept_object_is_told_apart_from_one_nobody_recorded() {
        let table = Table::new();
        // The default clock too, which `record` would have forgotten: a clock port naming the wall
        // clock is a port we answer for, and one nobody handed out is not.
        table.keep(key(6), clock::REALTIME);
        table.keep(key(7), clock::MONOTONIC);
        assert_eq!(table.kept(key(6)), Some(clock::REALTIME));
        assert_eq!(table.kept(key(7)), Some(clock::MONOTONIC));
        assert_eq!(table.kept(key(8)), None);
        // And the default answer is still the default for whoever wants one.
        assert_eq!(table.clock_of(key(8)), Some(clock::REALTIME));
        assert_eq!(table.clock_of(key(7)), Some(clock::MONOTONIC));
    }

    #[test]
    fn keys_that_share_a_home_fill_the_whole_table_before_one_goes_unrecorded() {
        let table = table::Table::<16>::new();
        let crowd = crowding::<16>(17);
        for (which, &key) in crowd.iter().enumerate() {
            table.keep(key, clock_id(which));
        }
        // The first sixteen took every slot between them, the last of them sixteen probes from
        // home, and each is found where its walk put it.
        for (which, &key) in crowd.iter().take(16).enumerate() {
            assert_eq!(table.kept(key), Some(clock_id(which)), "{which}");
        }
        assert_eq!(table.reach.load(Ordering::Relaxed), 16);
        assert_eq!(table.kept(crowd[16]), None);
        assert!(table.lost.load(Ordering::Relaxed));
    }

    #[test]
    fn a_lookup_walks_as_far_as_the_farthest_insert_and_no_farther_than_it_must() {
        let table = table::Table::<16>::new();
        assert_eq!(table.reach.load(Ordering::Relaxed), 0);
        let crowd = crowding::<16>(3);
        table.keep(crowd[0], clock::MONOTONIC);
        assert_eq!(table.reach.load(Ordering::Relaxed), 1);
        table.keep(crowd[1], clock::MONOTONIC);
        table.keep(crowd[2], boottime());
        assert_eq!(table.reach.load(Ordering::Relaxed), 3);
        // A walk that got that far stays reachable after the slots before it are freed.
        table.forget(crowd[0]);
        table.forget(crowd[1]);
        assert_eq!(table.kept(crowd[2]), Some(boottime()));
        // And a slot freed on the way is where the next key from that home lands.
        table.keep(crowd[1], clock::MONOTONIC);
        assert_eq!(table.reach.load(Ordering::Relaxed), 3);
        assert_eq!(table.kept(crowd[1]), Some(clock::MONOTONIC));
    }

    #[test]
    fn a_full_table_says_the_clock_of_an_object_it_never_recorded_is_unknown() {
        let table = table::Table::<16>::new();
        let planted: Vec<usize> = (0..64).map(key).collect();
        for &key in &planted {
            table.record(key, clock::MONOTONIC);
        }
        // Every slot went to an object before any was left out, and each object reads the clock it
        // asked for or none at all, never somebody else's and never the default.
        let mut recorded = 0;
        for &key in &planted {
            match table.clock_of(key) {
                Some(clock::MONOTONIC) => recorded += 1,
                None => {}
                other => panic!("{key:x} reads {other:?}, which is nobody's"),
            }
        }
        assert_eq!(recorded, 16);
        // A variable built with the default clock reaches no slot, so once the table has been full
        // it is as unknown as the ones left out, and stays so after room is made.
        assert_eq!(table.clock_of(key(1_000)), None);
        for &key in &planted {
            table.forget(key);
        }
        table.record(key(1_001), clock::REALTIME);
        assert_eq!(table.clock_of(key(1_001)), None);
        table.record(key(1_002), clock::MONOTONIC);
        assert_eq!(table.clock_of(key(1_002)), Some(clock::MONOTONIC));
    }

    #[test]
    fn a_full_table_takes_an_object_again_once_one_is_forgotten() {
        let table = table::Table::<4>::new();
        let crowd = crowding::<4>(5);
        for &key in &crowd[..4] {
            table.keep(key, clock::MONOTONIC);
        }
        table.forget(crowd[2]);
        table.keep(crowd[4], boottime());
        assert!(!table.lost.load(Ordering::Relaxed));
        assert_eq!(table.kept(crowd[4]), Some(boottime()));
        assert_eq!(table.kept(crowd[2]), None);
        assert_eq!(table.kept(crowd[3]), Some(clock::MONOTONIC));
    }
}
