//! The read path takes no lock and allocates nothing.
//!
//! A counting allocator is a whole-binary thing, so this check lives in a test binary of its own.
//!
//! Creating a session reads a genuine clock and the system's randomness, so this needs a
//! filled-in platform arm.
#![cfg(target_os = "linux")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use common::session::{self, Setup, realtime_now};
use common::time::Nanos;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

// Safety: every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Safety: the caller's `layout` is passed on unchanged.
        unsafe {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            System.alloc(layout)
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // Safety: the caller's `ptr` and `layout` are passed on unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Safety: the caller's arguments are passed on unchanged.
        unsafe {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            System.realloc(ptr, layout, new_size)
        }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn reading_the_clock_allocates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let session = session::create(&Setup {
        dir: dir.path().join("libtimewarp"),
        realtime_shift: Nanos(-7_200 * Nanos::SECOND),
        emulate_vdso: true,
        fail_closed: false,
        timekeep_interval: Nanos(1_000_000),
    })
    .unwrap();
    let header = session.header();

    // Warm anything the first call might touch, then measure.
    let _ = realtime_now(header);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let mut last = Nanos::ZERO;
    for _ in 0..1_000 {
        let now = realtime_now(header).unwrap();
        assert!(now >= last);
        last = now;
    }
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), before);
}
