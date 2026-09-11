//! The reentrancy gate: one flag per thread, held while that thread is inside the hook.
//!
//! A hook that wants the faked value of another clock reads that clock the way any program would.
//! The gate is what keeps that nested read from calling the hook again: the flag is already set, so
//! the read skips the hook and takes the keeper's answer. One flag per thread, because the hook of
//! one thread says nothing about another's.
//!
//! The flag is thread-local with a const initializer and nothing to drop, so entering is a load, a
//! store, and a branch: no allocation, no lock, and nothing that could recurse into a symbol we
//! interpose.

use std::cell::Cell;

thread_local! {
    /// Whether this thread is inside the hook.
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

/// Proof that this thread holds the gate. Dropping it releases the gate.
///
/// A hook that leaves by `longjmp` skips this drop, so the gate stays held and that thread runs the
/// keeper alone from then on. That is accepted rather than prevented: catching it would mean the
/// clock read hot path writing shared memory, and the hot path writes none.
#[derive(Debug)]
pub(super) struct Held(());

impl Drop for Held {
    fn drop(&mut self) {
        INSIDE.set(false);
    }
}

/// Takes the gate, or `None` when this thread already holds it.
///
/// `None` is what makes a nested clock read from inside the hook run the keeper alone.
#[inline]
pub(super) fn enter() -> Option<Held> {
    if INSIDE.replace(true) {
        return None;
    }
    Some(Held(()))
}

/// Whether this thread is inside the hook. For the tests; the gate itself is [`enter`].
#[cfg(test)]
fn held() -> bool {
    INSIDE.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_is_free_until_it_is_taken_and_free_again_afterwards() {
        assert!(!held());
        {
            let _inside = enter().expect("a free gate lets the first caller in");
            assert!(held());
        }
        assert!(!held());
        assert!(enter().is_some(), "the gate was released on the way out");
    }

    #[test]
    fn a_nested_call_is_refused_and_the_outer_one_keeps_the_gate() {
        let outer = enter().expect("the outer call takes the gate");
        assert!(enter().is_none(), "a nested call runs the keeper alone");
        assert!(held());
        drop(outer);
        assert!(enter().is_some(), "the outer call released it");
    }

    #[test]
    fn one_thread_inside_the_hook_leaves_every_other_thread_free() {
        let outer = enter().expect("the outer call takes the gate");
        let elsewhere = std::thread::spawn(|| {
            let nested = enter().is_none();
            (held(), nested)
        })
        .join()
        .expect("the other thread ran");
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
        std::thread::spawn(|| {
            let _never_released = std::mem::ManuallyDrop::new(enter().expect("the gate was free"));
            for _ in 0..3 {
                assert!(enter().is_none(), "the thread runs the keeper from now on");
            }
        })
        .join()
        .expect("the thread ran");
        // And the thread that leaked it is the only one affected.
        assert!(enter().is_some());
    }
}
