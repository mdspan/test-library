//! The write lock's fork registry under every interleaving `loom` drives: an acquire racing a fork
//! for the moment between its `open` and its entry, with the fork copying the descriptors before
//! the memory and after it, two entries racing for one free slot, and what a child's handler
//! leaves of the parent's entries.
//!
//! The source is the process's own registry, mounted a second time over loom's atomics. A fork is
//! the thread that runs the parent's handler before it, copies what the child inherits, and runs
//! the parent's handler after. The kernel makes each copy inside the syscall, so each sits behind
//! a full barrier, and so does the `open` that puts a descriptor in the table.

use std::os::raw::c_int;
use std::ptr;

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering, fence};
use loom::thread;

#[path = "held.rs"]
#[expect(
    clippy::duplicate_mod,
    reason = "the second mount is the point: the same source over loom's atomics"
)]
mod held;

use held::{FREE, Registry, fd_of};

fn registry() -> Registry {
    Registry {
        slots: AtomicPtr::new(ptr::null_mut()),
        forks: AtomicU64::new(0),
    }
}

/// The descriptors a child's copy of the memory names, as its handler would find them.
fn named(registry: &Registry) -> Vec<c_int> {
    let mut found = Vec::new();
    let mut node = registry.slots.load(Ordering::Acquire);
    // Safety: every slot was leaked by `enter` and had `next` set before it was published.
    while let Some(slot) = unsafe { node.as_ref() } {
        let word = slot.word.load(Ordering::Relaxed);
        if word != FREE {
            found.push(fd_of(word));
        }
        node = slot.next.cast_mut();
    }
    found
}

/// The descriptor numbers the model's acquire opens, one per attempt.
const OPENED: [c_int; 2] = [10, 11];

/// Which the fork copies first, of the descriptor table and the memory.
#[derive(Clone, Copy, PartialEq, Eq)]
enum First {
    Descriptors,
    Memory,
}

/// One acquire, which opens and enters until no fork started in between, against one fork. Fails
/// if the acquire keeps a descriptor the fork handed its child with nothing in the child's memory
/// naming it, which is the copy the child's handler cannot close, or if no execution sends the
/// acquire round again, which would leave the check with nothing to check.
fn acquire_against_a_fork(first: First, went_round: &'static std::sync::atomic::AtomicBool) {
    loom::model(move || {
        let held = Arc::new(registry());
        let opened = Arc::new([AtomicBool::new(false), AtomicBool::new(false)]);
        let stranded = Arc::new([AtomicBool::new(false), AtomicBool::new(false)]);

        let fork = {
            let (held, opened, stranded) = (held.clone(), opened.clone(), stranded.clone());
            thread::spawn(move || {
                held.forking();
                fence(Ordering::SeqCst);
                let (copied, names) = if first == First::Descriptors {
                    let copied = opened.each_ref().map(|open| open.load(Ordering::Relaxed));
                    fence(Ordering::SeqCst);
                    (copied, named(&held))
                } else {
                    let names = named(&held);
                    fence(Ordering::SeqCst);
                    (
                        opened.each_ref().map(|open| open.load(Ordering::Relaxed)),
                        names,
                    )
                };
                fence(Ordering::SeqCst);
                for (attempt, fd) in OPENED.iter().enumerate() {
                    if copied[attempt] && !names.contains(fd) {
                        stranded[attempt].store(true, Ordering::Relaxed);
                    }
                }
                held.forked();
            })
        };

        let mut kept = None;
        for (attempt, &fd) in OPENED.iter().enumerate() {
            let generation = loop {
                if let Some(generation) = held.generation() {
                    break generation;
                }
                thread::yield_now();
            };
            fence(Ordering::SeqCst);
            opened[attempt].store(true, Ordering::Relaxed); // the `open`
            fence(Ordering::SeqCst);
            let entry = held.enter(generation, fd);
            if held.unforked_since(generation) {
                kept = Some(attempt);
                break;
            }
            assert!(entry.leave(), "the parent lost its own entry");
            went_round.store(true, Ordering::Relaxed);
        }
        fork.join().expect("the fork ran");

        let kept = kept.expect("an attempt that began after the one fork keeps its descriptor");
        assert!(
            !stranded[kept].load(Ordering::Relaxed),
            "the acquire kept a descriptor its child holds with nothing naming it"
        );
    });
    assert!(
        went_round.load(Ordering::Relaxed),
        "no execution had a fork start between an open and its entry"
    );
}

#[test]
fn a_fork_copying_the_descriptors_first_never_strands_a_kept_one() {
    static WENT_ROUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    acquire_against_a_fork(First::Descriptors, &WENT_ROUND);
}

#[test]
fn a_fork_copying_the_memory_first_never_strands_a_kept_one() {
    static WENT_ROUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    acquire_against_a_fork(First::Memory, &WENT_ROUND);
}

#[test]
fn two_entries_racing_for_one_free_slot_take_one_slot_each() {
    loom::model(|| {
        let held = Arc::new(registry());
        // A slot an earlier lock left free, which both entries walk to first.
        let earlier = held.enter(0, 3);
        assert!(earlier.leave());

        let other = {
            let held = held.clone();
            thread::spawn(move || held.enter(0, 11))
        };
        let here = held.enter(0, 10);
        let there = other.join().expect("the other entry ran");

        assert!(here.held() && there.held());
        assert_eq!((here.fd(), there.fd()), (10, 11));
        let mut found = named(&held);
        found.sort_unstable();
        assert_eq!(found, [10, 11]);
    });
}

#[test]
fn a_child_s_handler_closes_every_entry_and_leaves_the_copies_nothing_to_close() {
    loom::model(|| {
        let held = registry();
        let first = held.enter(0, 10);
        let second = held.enter(0, 11);

        // Two forks under way from the parent's threads, one of them this child's.
        held.forking();
        held.forking();
        let mut closed = Vec::new();
        held.forked_child(|fd| closed.push(fd));
        closed.sort_unstable();
        assert_eq!(closed, [10, 11]);

        // The child's copies hold nothing, and leaving closes nothing.
        assert!(!first.held() && !second.held());
        assert!(!first.leave() && !second.leave());

        // Nothing is under way in the child, and what it enters next under a number it reused is
        // not one of the copies.
        let generation = held
            .generation()
            .expect("no fork is under way in the child");
        let reused = held.enter(generation, 10);
        assert!(reused.held() && !first.held());
        assert!(!first.leave());
        assert!(reused.leave());
    });
}
