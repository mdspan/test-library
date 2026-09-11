//! The read path takes no lock and allocates nothing, and a log line follows the same rules, since
//! one can be written from wherever a clock read can happen.
//!
//! A counting allocator is a whole-binary thing, so this check lives in a test binary of its own.
//!
//! Creating a session reads a genuine clock and the system's randomness, so this needs a
//! filled-in platform arm.
#![cfg(any(target_os = "linux", target_os = "freebsd"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use common::config::ENV_LOG;
use common::log::{Level, Sink};
use common::platform::{clock, default_timebase};
use common::session::layout::WriterState;
use common::session::{self, Setup, clock_now, timestamp_shown, timestamp_stored};
use common::time::{Family, Nanos};

/// The clock a family tracks by default here. FreeBSD picks between two columns of the family
/// table by the running kernel's version, so a case that names a family's clock asks for it rather
/// than spelling it.
fn timebase_of(family: Family) -> libc::clockid_t {
    default_timebase(family).unwrap_or(clock::REALTIME)
}

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// Whether the calling thread is the one measuring. A global counter would also catch the test
// harness's own threads reporting a result beside us, which is one stray allocation charged to
// whichever measurement happened to be open. `Cell<bool>` has no destructor, so reading this
// registers nothing and allocates nothing on the way.
thread_local! {
    static COUNTING_HERE: Cell<bool> = const { Cell::new(false) };
}

/// The counter is one word for the whole binary, so two tests measuring at once would each see the
/// other's allocations. Taken for the length of a test, poison and all: a test that already failed
/// should not turn the next one into a second failure.
static MEASURING: Mutex<()> = Mutex::new(());

fn measuring() -> std::sync::MutexGuard<'static, ()> {
    MEASURING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Counts the allocations `body` makes on this thread, and nowhere else.
fn allocations_in(body: impl FnOnce()) -> usize {
    COUNTING_HERE.with(|counting| counting.set(true));
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    body();
    let counted = ALLOCATIONS.load(Ordering::Relaxed) - before;
    COUNTING_HERE.with(|counting| counting.set(false));
    counted
}

struct Counting;

impl Counting {
    /// Charges an allocation to the measurement open on this thread, if there is one.
    fn count() {
        // A thread on its way out has already destroyed its locals; nothing there is measuring.
        let _ = COUNTING_HERE.try_with(|counting| {
            if counting.get() {
                ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            }
        });
    }
}

// Safety: every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Safety: the caller's `layout` is passed on unchanged.
        unsafe {
            Self::count();
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
            Self::count();
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
        timebase_of(Family::Monotonic),
        timebase_of(Family::Boottime),
        clock::PROCESS_CPUTIME,
        clock::THREAD_CPUTIME,
    ];

    // Warm anything the first call might touch, then measure.
    for id in clocks {
        assert!(clock_now(header, id).is_some());
    }
    let counted = allocations_in(|| {
        let mut last = Nanos::ZERO;
        for _ in 0..1_000 {
            for id in clocks {
                assert!(clock_now(header, id).is_some());
            }
            let now = clock_now(header, clock::REALTIME).unwrap();
            assert!(now >= last);
            last = now;
        }
    });
    assert_eq!(counted, 0);
}

#[test]
fn converting_a_file_timestamp_allocates_nothing() {
    let _measuring = measuring();
    let dir = tempfile::tempdir().unwrap();
    let mut setup = Setup::plain(dir.path().join("libtimewarp"));
    setup.realtime.shift = Nanos(-7_200 * Nanos::SECOND);
    let session = session::create(&setup).unwrap();
    let header = session.header();

    // A `stat` and a `utime` run wherever the program can make one, so both directions are read
    // paths and neither may allocate.
    let stamps = [
        Nanos::ZERO,
        Nanos(1_700_000_000 * Nanos::SECOND),
        Nanos(i64::MAX),
    ];
    for stamp in stamps {
        assert!(timestamp_shown(header, stamp).is_some());
        assert!(timestamp_stored(header, stamp).is_some());
    }
    let counted = allocations_in(|| {
        for _ in 0..1_000 {
            for stamp in stamps {
                assert!(timestamp_shown(header, stamp).is_some());
                assert!(timestamp_stored(header, stamp).is_some());
            }
        }
    });
    assert_eq!(counted, 0);
}

#[test]
fn the_abandoned_writer_fallback_allocates_nothing_either() {
    let _measuring = measuring();
    let dir = tempfile::tempdir().unwrap();
    let session = session::create(&Setup::plain(dir.path().join("libtimewarp"))).unwrap();
    let header = session.header();

    // A writer that died between W3 and W7: every read in this process now waits out its patience
    // and comes back through the fallback, which opens the lock file, probes it, and closes it.
    header
        .seqlock
        .writer_state
        .store(WriterState::writing(4321).word(), Ordering::Release);
    header.seqlock.sequence.store(1, Ordering::Release);

    assert!(clock_now(header, clock::REALTIME).is_some());
    let counted = allocations_in(|| {
        for _ in 0..20 {
            assert!(clock_now(header, timebase_of(Family::Monotonic)).is_some());
        }
    });
    assert_eq!(counted, 0);
}

#[test]
fn writing_a_log_line_allocates_nothing() {
    let _measuring = measuring();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log").into_os_string();
    let sink = Sink::from_env(&|name: &str| (name == ENV_LOG).then(|| path.clone())).unwrap();

    // Warm anything the first line might touch, then measure.
    sink.write(Level::Error, format_args!("warm"));
    let counted = allocations_in(|| {
        for line in 0..1_000 {
            sink.write(
                Level::Error,
                format_args!("{line}: {} for {}", "a suspected writer death", 4321),
            );
        }
    });
    assert_eq!(counted, 0);
}
