//! The reentrancy gate: one claim per thread, held while that thread is inside the hook.
//!
//! A hook that wants the faked value of another clock reads that clock the way any program would.
//! The gate is what keeps that nested read from calling the hook again: the thread's claim is
//! already there, so the read skips the hook and takes the keeper's answer. Per thread, because the
//! hook of one thread says nothing about another's.
//!
//! A thread is named by the id the kernel gives it rather than by thread-local storage. Storage is
//! libc's, reached through the thread pointer, and a runtime that makes its own threads without
//! libc sets that pointer to something of its own: the Go runtime does, and a thread-local read on
//! such a thread reads somebody else's memory. The id costs one call that neither allocates nor
//! locks, and only a session with a hook ever pays it.
//!
//! The claims live in a fixed set of slots walked by linear probing, like every other table this
//! library keeps for objects it must recognise again. A thread that finds no free slot in its
//! window runs the keeper alone, which is the answer the gate gives a nested call too: the wrong
//! answer to give is the hook twice.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::platform::thread_id;

/// How many threads can be inside the hook at once.
const SLOTS: usize = 256;

/// How far a claim walks from the slot a thread hashes to before it gives up.
const PROBES: usize = 8;

/// A slot nobody holds. No thread id is zero, so a slot says free by holding one.
const FREE: u64 = 0;

/// Who is inside the hook. The operations take the gate they work on, so a case can exercise one of
/// its own rather than the process's.
#[derive(Debug)]
pub(super) struct Gate([AtomicU64; SLOTS]);

/// The one gate of this process.
static INSIDE: Gate = Gate::new();

impl Gate {
    const fn new() -> Self {
        Self([const { AtomicU64::new(FREE) }; SLOTS])
    }

    /// Takes the gate, or `None` when this thread already holds it or the window is full.
    #[inline]
    fn take(&self) -> Option<Held<'_>> {
        let me = thread_id();
        if me == FREE {
            // A thread we cannot name is one whose nested call we could not tell apart, so it runs
            // the keeper.
            return None;
        }
        // The whole window first: a claim of this thread's own can only have been written by this
        // thread, so a scan that missed it would be the one mistake this table must not make.
        for probe in 0..PROBES {
            if self.slot(me, probe)?.load(Ordering::Acquire) == me {
                return None;
            }
        }
        for probe in 0..PROBES {
            let slot = self.slot(me, probe)?;
            if slot
                .compare_exchange(FREE, me, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return Some(Held(slot));
            }
        }
        None
    }

    /// The slot a thread's `probe`th try lands in. Multiplicative hashing, because thread ids come
    /// out of the kernel in a run and neighbouring ones would otherwise share a window.
    fn slot(&self, thread: u64, probe: usize) -> Option<&AtomicU64> {
        let mixed = thread.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let at = usize::try_from(mixed >> 32).ok()?.wrapping_add(probe) % SLOTS;
        self.0.get(at)
    }

    /// Whether this thread is inside the hook. For the tests; the gate itself is [`Gate::take`].
    #[cfg(test)]
    fn held(&self) -> bool {
        let me = thread_id();
        (0..PROBES).any(|probe| {
            self.slot(me, probe)
                .is_some_and(|slot| slot.load(Ordering::Acquire) == me)
        })
    }
}

/// Proof that this thread holds the gate. Dropping it releases the gate.
///
/// A hook that leaves by `longjmp` skips this drop, so the claim stays and that thread runs the
/// keeper alone from then on. That is accepted rather than prevented: catching it would mean the
/// clock read hot path writing shared memory, and the hot path writes none.
#[derive(Debug)]
pub(super) struct Held<'gate>(&'gate AtomicU64);

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.0.store(FREE, Ordering::Release);
    }
}

/// Takes the gate, or `None` when this thread already holds it.
///
/// `None` is what makes a nested clock read from inside the hook run the keeper alone.
#[inline]
pub(super) fn enter() -> Option<Held<'static>> {
    INSIDE.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_is_free_until_it_is_taken_and_free_again_afterwards() {
        let gate = Gate::new();
        assert!(!gate.held());
        {
            let _inside = gate.take().expect("a free gate lets the first caller in");
            assert!(gate.held());
        }
        assert!(!gate.held());
        assert!(
            gate.take().is_some(),
            "the gate was released on the way out"
        );
    }

    #[test]
    fn a_nested_call_is_refused_and_the_outer_one_keeps_the_gate() {
        let gate = Gate::new();
        let outer = gate.take().expect("the outer call takes the gate");
        assert!(gate.take().is_none(), "a nested call runs the keeper alone");
        assert!(gate.held());
        drop(outer);
        assert!(gate.take().is_some(), "the outer call released it");
    }

    #[test]
    fn one_thread_inside_the_hook_leaves_every_other_thread_free() {
        let gate = Gate::new();
        let outer = gate.take().expect("the outer call takes the gate");
        let elsewhere = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let nested = gate.take().is_none();
                    (gate.held(), nested)
                })
                .join()
                .expect("the other thread ran")
        });
        assert_eq!(
            elsewhere,
            (false, false),
            "another thread's gate answered for this one"
        );
        drop(outer);
    }

    /// A hook leaving by `longjmp` skips the release, which is documented rather than prevented.
    #[test]
    fn a_gate_that_is_never_released_keeps_that_thread_on_the_keeper() {
        let gate = Gate::new();
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _never_released =
                        std::mem::ManuallyDrop::new(gate.take().expect("the gate was free"));
                    for _ in 0..3 {
                        assert!(
                            gate.take().is_none(),
                            "the thread runs the keeper from now on"
                        );
                    }
                })
                .join()
                .expect("the thread ran");
        });
        // And the thread that leaked it is the only one affected.
        assert!(gate.take().is_some());
    }

    /// The gate the process shares is the one the hook takes, and it starts free.
    #[test]
    fn the_process_gate_lets_a_call_in_and_refuses_the_one_inside_it() {
        let held = enter().expect("the gate is free");
        assert!(enter().is_none(), "a nested call runs the keeper alone");
        drop(held);
    }

    /// Every thread has a name of its own, which is what the table keys on.
    #[test]
    fn a_thread_is_named_by_something_no_other_thread_answers() {
        let mine = thread_id();
        assert_ne!(mine, FREE);
        let theirs = std::thread::spawn(thread_id).join().expect("it ran");
        assert_ne!(mine, theirs);
    }

    /// Threads crowding one window take the keeper rather than the hook twice.
    #[test]
    fn more_threads_than_a_window_holds_leaves_the_rest_on_the_keeper() {
        let gate = Gate::new();
        let me = thread_id();
        for probe in 0..PROBES {
            // A claim of somebody else's, which is what a crowded window is full of.
            let planted = u64::MAX - u64::try_from(probe).unwrap_or(0);
            gate.slot(me, probe)
                .expect("the window is inside the table")
                .store(planted, Ordering::Release);
        }
        assert!(gate.take().is_none(), "a full window answers the keeper");

        gate.slot(me, PROBES - 1)
            .expect("the window is inside the table")
            .store(FREE, Ordering::Release);
        assert!(
            gate.take().is_some(),
            "and lets a thread in once there is room"
        );
    }
}
