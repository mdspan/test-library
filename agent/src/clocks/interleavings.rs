//! The table under every interleaving `loom` drives: two inserts claiming slots on one walk, a
//! lookup racing an insert, a lookup the program has ordered after one, a forget racing an insert
//! from the same home, two keeps of one key, alone and racing a forget of it, a keep racing a
//! forget of its key, keeps of two keys from one home, a keep whose slot is freed and taken by
//! another key under it, and two inserts racing for the last free slot.
//!
//! The source is the process's own table, mounted a second time over loom's atomics, in a table of
//! a slot or two so that every walk crosses another.

use common::platform::clock;
use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering, fence};
use loom::thread;

#[path = "table.rs"]
#[expect(
    clippy::duplicate_mod,
    reason = "the second mount is the point: the same source over loom's atomics"
)]
mod table;

use table::{EMPTY, Slot, Table};

impl<const N: usize> Table<N> {
    fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| Slot {
                state: AtomicU64::new(EMPTY),
                object: AtomicUsize::new(0),
            }),
            reach: AtomicUsize::new(0),
            lost: AtomicBool::new(false),
        }
    }
}

/// Two keys whose walks start from the same slot of a table of `N`, or `None` when no key near the
/// first shares its home.
fn sharing_a_home<const N: usize>() -> Option<(usize, usize)> {
    let first = 0x1_0000;
    let second = (1..=64)
        .map(|which| first + which * 48)
        .find(|&key| Table::<N>::home(key) == Table::<N>::home(first))?;
    Some((first, second))
}

#[test]
fn two_inserts_on_one_walk_both_land() {
    loom::model(|| {
        let table = Arc::new(Table::<2>::new());
        let (first, second) = sharing_a_home::<2>().expect("two keys share a home");
        let other = {
            let table = Arc::clone(&table);
            thread::spawn(move || table.keep(second, clock::PROCESS_CPUTIME))
        };
        table.record(first, clock::MONOTONIC);
        other.join().expect("the other insert ran");

        assert_eq!(table.kept(first), Some(clock::MONOTONIC));
        assert_eq!(table.kept(second), Some(clock::PROCESS_CPUTIME));
        assert!(!table.lost.load(Ordering::Relaxed));
    });
}

#[test]
fn a_lookup_racing_an_insert_finds_the_whole_object_or_nothing() {
    loom::model(|| {
        let table = Arc::new(Table::<2>::new());
        let (first, second) = sharing_a_home::<2>().expect("two keys share a home");
        // The home is taken, so the insert raced lands one probe along.
        table.keep(first, clock::PROCESS_CPUTIME);
        let other = {
            let table = Arc::clone(&table);
            thread::spawn(move || table.keep(second, clock::MONOTONIC))
        };
        let seen = table.kept(second);
        assert!(
            matches!(seen, None | Some(clock::MONOTONIC)),
            "a racing lookup read {seen:?}, which is half an insert"
        );
        other.join().expect("the insert ran");
        assert_eq!(table.kept(second), Some(clock::MONOTONIC));
    });
}

#[test]
fn a_lookup_ordered_after_an_insert_walks_as_far_as_the_insert_went() {
    loom::model(|| {
        let table = Arc::new(Table::<2>::new());
        let ready = Arc::new(AtomicBool::new(false));
        let (first, second) = sharing_a_home::<2>().expect("two keys share a home");
        table.keep(first, clock::PROCESS_CPUTIME);
        let other = {
            let (table, ready) = (Arc::clone(&table), Arc::clone(&ready));
            thread::spawn(move || {
                table.keep(second, clock::MONOTONIC);
                // The program's own handoff, which is all a lookup of the object can lean on.
                ready.store(true, Ordering::Release);
            })
        };
        if ready.load(Ordering::Acquire) {
            assert_eq!(table.kept(second), Some(clock::MONOTONIC));
            assert_eq!(table.clock_of(second), Some(clock::MONOTONIC));
        }
        other.join().expect("the insert ran");
    });
}

#[test]
fn a_forget_racing_an_insert_from_the_same_home_leaves_the_insert_found() {
    loom::model(|| {
        let table = Arc::new(Table::<2>::new());
        let (first, second) = sharing_a_home::<2>().expect("two keys share a home");
        table.keep(first, clock::PROCESS_CPUTIME);
        let other = {
            let table = Arc::clone(&table);
            thread::spawn(move || table.forget(first))
        };
        table.keep(second, clock::MONOTONIC);
        other.join().expect("the forget ran");

        assert_eq!(table.kept(first), None);
        assert_eq!(table.kept(second), Some(clock::MONOTONIC));
        assert!(!table.lost.load(Ordering::Relaxed));
    });
}

#[test]
fn two_keeps_of_one_key_leave_it_held_once() {
    loom::model(|| {
        let table = Arc::new(Table::<2>::new());
        let key = 0x1_0000;
        let other = {
            let table = Arc::clone(&table);
            thread::spawn(move || table.keep(key, clock::PROCESS_CPUTIME))
        };
        table.keep(key, clock::MONOTONIC);
        other.join().expect("the other keep ran");

        assert_eq!(
            table.holders(key),
            1,
            "two keeps left the key in both slots"
        );
        let seen = table.kept(key);
        assert!(
            matches!(seen, Some(clock::MONOTONIC | clock::PROCESS_CPUTIME)),
            "two keeps left {seen:?}"
        );
        table.forget(key);
        assert_eq!(table.holders(key), 0);
    });
}

#[test]
fn two_keeps_of_one_key_racing_a_forget_of_it_leave_it_held_once_at_most() {
    loom::model(|| {
        let table = Arc::new(Table::<2>::new());
        let key = 0x1_0000;
        let keeps = [clock::MONOTONIC, clock::PROCESS_CPUTIME].map(|clock| {
            let table = Arc::clone(&table);
            thread::spawn(move || table.keep(key, clock))
        });
        table.forget(key);
        for keep in keeps {
            keep.join().expect("a keep ran");
        }

        assert!(
            table.holders(key) <= 1,
            "two keeps and a forget left two slots"
        );
        table.forget(key);
        assert_eq!(table.holders(key), 0);
    });
}

#[test]
fn a_keep_racing_a_forget_leaves_the_new_clock_or_none_and_never_the_old() {
    loom::model(|| {
        let table = Arc::new(Table::<2>::new());
        let (first, second) = sharing_a_home::<2>().expect("two keys share a home");
        table.keep(first, clock::REALTIME);
        table.keep(second, clock::PROCESS_CPUTIME);
        // The slot ahead of the second key's is free when the race starts.
        table.forget(first);
        let other = {
            let table = Arc::clone(&table);
            thread::spawn(move || table.forget(second))
        };
        table.keep(second, clock::MONOTONIC);
        other.join().expect("the forget ran");

        assert!(
            table.holders(second) <= 1,
            "a keep racing a forget left two slots"
        );
        let seen = table.kept(second);
        assert!(
            matches!(seen, None | Some(clock::MONOTONIC)),
            "a keep racing a forget left {seen:?}"
        );
        table.forget(second);
        assert_eq!(table.holders(second), 0);
    });
}

#[test]
fn keeps_of_two_keys_from_one_home_leave_each_held_once() {
    loom::model(|| {
        let table = Arc::new(Table::<2>::new());
        let (first, second) = sharing_a_home::<2>().expect("two keys share a home");
        table.keep(first, clock::PROCESS_CPUTIME);
        table.keep(second, clock::PROCESS_CPUTIME);
        table.forget(first);
        let other = {
            let table = Arc::clone(&table);
            thread::spawn(move || table.keep(first, clock::REALTIME))
        };
        table.keep(second, clock::MONOTONIC);
        other.join().expect("the other keep ran");

        assert_eq!(table.holders(first), 1);
        assert_eq!(table.holders(second), 1);
        assert_eq!(table.kept(first), Some(clock::REALTIME));
        assert_eq!(table.kept(second), Some(clock::MONOTONIC));
        table.forget(second);
        assert_eq!(table.kept(second), None);
        assert_eq!(table.kept(first), Some(clock::REALTIME));
        assert!(!table.lost.load(Ordering::Relaxed));
    });
}

#[test]
fn a_keep_that_loses_its_entry_to_a_forget_leaves_the_next_object_in_the_slot_its_own_clock() {
    loom::model(|| {
        let table = Arc::new(Table::<2>::new());
        let (first, second) = sharing_a_home::<2>().expect("two keys share a home");
        table.keep(first, clock::PROCESS_CPUTIME);
        let other = {
            let table = Arc::clone(&table);
            thread::spawn(move || {
                table.forget(first);
                // Lands in the slot just freed, unless the keep it races has taken that again.
                table.keep(second, clock::REALTIME);
            })
        };
        table.keep(first, clock::MONOTONIC);
        other.join().expect("the forget and the other keep ran");

        assert_eq!(table.holders(second), 1);
        assert!(table.holders(first) <= 1);
        assert_eq!(table.kept(second), Some(clock::REALTIME));
        let seen = table.kept(first);
        assert!(
            matches!(seen, None | Some(clock::MONOTONIC)),
            "a keep racing a forget left {seen:?}"
        );
    });
}

#[test]
fn of_two_inserts_racing_for_the_last_slot_one_lands_and_the_other_reads_as_unknown() {
    loom::model(|| {
        let table = Arc::new(Table::<1>::new());
        let (first, second) = (0x1_0000, 0x2_0000);
        let other = {
            let table = Arc::clone(&table);
            thread::spawn(move || table.keep(second, clock::PROCESS_CPUTIME))
        };
        table.keep(first, clock::MONOTONIC);
        other.join().expect("the other insert ran");

        let (landed, left_out) = match (table.kept(first), table.kept(second)) {
            (Some(clock::MONOTONIC), None) => (first, second),
            (None, Some(clock::PROCESS_CPUTIME)) => (second, first),
            other => panic!("the one slot holds {other:?}"),
        };
        assert!(table.lost.load(Ordering::Relaxed));
        assert!(table.clock_of(landed).is_some());
        assert_eq!(table.clock_of(left_out), None);
    });
}
