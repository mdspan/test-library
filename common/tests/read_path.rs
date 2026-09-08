//! The read path takes no lock and allocates nothing, and a log line follows the same rules, since
//! one can be written from wherever a clock read can happen.
//!
//! A counting allocator is a whole-binary thing, so this check lives in a test binary of its own.
//!
//! Creating a session reads a genuine clock and the system's randomness, so this needs a
//! filled-in platform arm.
#![cfg(target_os = "linux")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use common::config::ENV_LOG;
use common::log::{Level, Sink};
use common::platform::clock;
use common::session::{self, Setup, clock_now};
use common::time::Nanos;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

/// The counter is one word for the whole binary, so two tests measuring at once would each see the
/// other's allocations. Taken for the length of a test, poison and all: a test that already failed
/// should not turn the next one into a second failure.
static MEASURING: Mutex<()> = Mutex::new(());

fn measuring() -> std::sync::MutexGuard<'static, ()> {
    MEASURING.lock().unwrap_or_else(PoisonError::into_inner)
}

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
    let _measuring = measuring();
    let dir = tempfile::tempdir().unwrap();
    let mut setup = Setup::plain(dir.path().join("libtimewarp"));
    setup.realtime.shift = Nanos(-7_200 * Nanos::SECOND);
    let session = session::create(&setup).unwrap();
    let header = session.header();

    // Every family, because each takes a different arm of the keeper's match.
    let clocks = [
        clock::REALTIME,
        clock::TAI,
        clock::MONOTONIC,
        clock::BOOTTIME,
        clock::PROCESS_CPUTIME,
        clock::THREAD_CPUTIME,
    ];

    // Warm anything the first call might touch, then measure.
    for id in clocks {
        assert!(clock_now(header, id).is_some());
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let mut last = Nanos::ZERO;
    for _ in 0..1_000 {
        for id in clocks {
            assert!(clock_now(header, id).is_some());
        }
        let now = clock_now(header, clock::REALTIME).unwrap();
        assert!(now >= last);
        last = now;
    }
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), before);
}

#[test]
fn writing_a_log_line_allocates_nothing() {
    let _measuring = measuring();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log").into_os_string();
    let sink = Sink::from_env(&|name: &str| (name == ENV_LOG).then(|| path.clone())).unwrap();

    // Warm anything the first line might touch, then measure.
    sink.write(Level::Error, format_args!("warm"));
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for line in 0..1_000 {
        sink.write(
            Level::Error,
            format_args!("{line}: {} for {}", "a suspected writer death", 4321),
        );
    }
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), before);
}
