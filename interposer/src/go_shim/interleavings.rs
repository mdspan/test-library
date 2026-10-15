//! The protocol the shims' assembly claims and gives back a scratch stack by, written out in Rust:
//! run once in an ordinary test build, and under every interleaving `loom` drives in a build with
//! `--cfg loom`, in an arena of a slot or two so that walks meet. The cases: two readers whose
//! threads hash to one slot, readers finding the arena full and mapping spares at once, a freed
//! spare serving the next reader, a nested read beside another thread's, and a refused mapping.
//!
//! Each step carries the ordering of the weaker of its two encodings, arm64's against x86's plain
//! moves and locked exchanges. A free slot is spotted with a relaxed load (`ldr`) and claimed with
//! an exchange that acquires and releases (`ldaxr`/`stlxr`), and given back with a releasing store
//! (`stlr`). The spares' head and links are read with acquiring loads (`ldar`), and a new spare's
//! words are written with plain stores before the exchange that publishes it. The in-flight word is
//! the thread's own, and a nested read runs on the same thread, so it is a plain cell. A stack's
//! memory is a cell every read writes, so two reads on one stack that the protocol does not order
//! are a race `loom` reports.

use std::cell::Cell;
use std::ptr;

#[cfg(loom)]
use loom::cell::UnsafeCell;
#[cfg(loom)]
use loom::sync::Arc;
#[cfg(loom)]
use loom::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
#[cfg(loom)]
use loom::thread;

#[cfg(not(loom))]
use std::sync::Arc;
#[cfg(not(loom))]
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
#[cfg(not(loom))]
use std::thread;

use super::{PHI, SLOT_SHIFT};

/// The standard library's cell with the one method of `loom`'s the model calls, so one model serves
/// both builds.
#[cfg(not(loom))]
struct UnsafeCell<T>(std::cell::UnsafeCell<T>);

#[cfg(not(loom))]
impl<T> UnsafeCell<T> {
    const fn new(value: T) -> Self {
        Self(std::cell::UnsafeCell::new(value))
    }

    fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        f(self.0.get())
    }
}

/// A scratch stack: its try-lock word, the link a spare carries to the spare pushed before it, and
/// the memory a read runs on.
struct Stack {
    busy: AtomicU64,
    next: AtomicPtr<Self>,
    frames: UnsafeCell<u64>,
}

// Safety: only the read holding a stack touches its frames, which is the protocol under test, and
// `loom` checks every one of those accesses.
unsafe impl Sync for Stack {}

impl Stack {
    fn new() -> Self {
        Self {
            busy: AtomicU64::new(0),
            next: AtomicPtr::new(ptr::null_mut()),
            frames: UnsafeCell::new(0),
        }
    }

    /// What a read does with the stack it runs on: writes there and reads back what it wrote.
    fn occupy(&self, key: u64) {
        self.frames.with_mut(|frames| {
            // Safety: the cell is live, and the protocol gives it to this read alone.
            unsafe { frames.write(key) };
            // Safety: as above.
            let read = unsafe { frames.read() };
            assert_eq!(read, key, "another read ran on this stack");
        });
    }
}

/// Which path a read took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path {
    /// The read path, on a scratch stack.
    Read,
    /// The genuine path, on the caller's stack.
    Genuine,
}

/// One thread's side of the protocol: the key its thread pointer hashes from, its in-flight word,
/// and the memory a mapping of its gets, or `None` where the kernel refuses one.
struct Reader<'a> {
    key: u64,
    in_flight: Cell<bool>,
    fresh: Cell<Option<&'a Stack>>,
}

/// The arena of `N` slots and the spares pushed past it.
struct Stacks<const N: usize> {
    arena: [Stack; N],
    spares: AtomicPtr<Stack>,
}

impl<const N: usize> Stacks<N> {
    fn new() -> Self {
        Self {
            arena: std::array::from_fn(|_| Stack::new()),
            spares: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// The slot a key's walk starts from: the shim's hash, folded into `N`.
    fn home(key: u64) -> usize {
        usize::try_from(key.wrapping_mul(PHI) >> SLOT_SHIFT).unwrap() % N
    }

    /// The stack a read runs on, or `None` for the genuine path.
    fn claim<'a>(&'a self, reader: &Reader<'a>) -> Option<&'a Stack> {
        if reader.in_flight.get() {
            return None;
        }
        reader.in_flight.set(true);
        let home = Self::home(reader.key);
        for probe in 0..N {
            let stack = &self.arena[(home + probe) % N];
            if take(&stack.busy) {
                return Some(stack);
            }
        }
        let mut spare = self.spares.load(Ordering::Acquire);
        // Safety: a spare on the list lives as long as the stacks do.
        while let Some(stack) = unsafe { spare.as_ref() } {
            if take(&stack.busy) {
                return Some(stack);
            }
            spare = stack.next.load(Ordering::Acquire);
        }
        let Some(stack) = reader.fresh.take() else {
            reader.in_flight.set(false);
            return None;
        };
        stack.busy.store(1, Ordering::Relaxed);
        let mut head = self.spares.load(Ordering::Acquire);
        loop {
            stack.next.store(head, Ordering::Relaxed);
            match self.spares.compare_exchange(
                head,
                ptr::from_ref(stack).cast_mut(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(stack),
                Err(now) => head = now,
            }
        }
    }

    /// Gives back the stack a read ran on.
    fn release(stack: &Stack, reader: &Reader<'_>) {
        stack.busy.store(0, Ordering::Release);
        reader.in_flight.set(false);
    }

    /// A whole read: claim a stack, run on it, give it back.
    fn read<'a>(&'a self, reader: &Reader<'a>) -> Path {
        self.claim(reader).map_or(Path::Genuine, |stack| {
            stack.occupy(reader.key);
            Self::release(stack, reader);
            Path::Read
        })
    }

    /// How many spares are on the list.
    fn spares(&self) -> usize {
        let mut count = 0;
        let mut spare = self.spares.load(Ordering::Acquire);
        // Safety: as in `claim`.
        while let Some(stack) = unsafe { spare.as_ref() } {
            count += 1;
            spare = stack.next.load(Ordering::Acquire);
        }
        count
    }
}

/// Claims a free stack: a relaxed look first, then the exchange, as the assembly does.
fn take(busy: &AtomicU64) -> bool {
    busy.load(Ordering::Relaxed) == 0 && busy.swap(1, Ordering::AcqRel) == 0
}

/// The stacks of an arena of `N`, and the memory a mapping gets for each of `M` readers.
struct World<const N: usize, const M: usize> {
    stacks: Stacks<N>,
    fresh: [Stack; M],
}

impl<const N: usize, const M: usize> World<N, M> {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stacks: Stacks::new(),
            fresh: std::array::from_fn(|_| Stack::new()),
        })
    }

    /// The reader keyed `key` whose mapping gets the `which`th fresh stack.
    fn reader(&self, key: u64, which: usize) -> Reader<'_> {
        Reader {
            key,
            in_flight: Cell::new(false),
            fresh: Cell::new(self.fresh.get(which)),
        }
    }
}

/// The reader keyed `key` whose mapping the kernel refuses.
const fn refused<'a>(key: u64) -> Reader<'a> {
    Reader {
        key,
        in_flight: Cell::new(false),
        fresh: Cell::new(None),
    }
}

/// Two keys whose walks start from the same slot of an arena of `N`.
fn sharing_a_home<const N: usize>() -> (u64, u64) {
    let first = 0x7f00_0000_1000;
    let second = (1..=64)
        .map(|which| first + which * 0x1000)
        .find(|&key| Stacks::<N>::home(key) == Stacks::<N>::home(first))
        .expect("two keys share a home");
    (first, second)
}

/// Runs `scenario` once, or under every interleaving when `loom` drives the build.
fn explore(scenario: impl Fn() + Sync + Send + 'static) {
    #[cfg(loom)]
    loom::model(scenario);
    #[cfg(not(loom))]
    scenario();
}

#[test]
fn two_readers_whose_threads_share_a_slot_both_take_the_read_path() {
    explore(|| {
        let world = World::<2, 2>::new();
        let (first, second) = sharing_a_home::<2>();
        let other = {
            let world = Arc::clone(&world);
            thread::spawn(move || world.stacks.read(&world.reader(second, 1)))
        };
        let here = world.stacks.read(&world.reader(first, 0));
        let there = other.join().unwrap();

        assert_eq!((here, there), (Path::Read, Path::Read));
        assert_eq!(
            world.stacks.spares(),
            0,
            "two reads of a two-slot arena mapped a spare"
        );
    });
}

#[test]
fn readers_finding_the_arena_full_map_spares_at_once_and_publish_every_one() {
    explore(|| {
        let world = World::<1, 3>::new();
        // A read in flight holds the one slot throughout.
        let holder = world.reader(1, 0);
        let held = world
            .stacks
            .claim(&holder)
            .expect("the arena's slot was free");
        let spawn = |key: u64, which: usize| {
            let world = Arc::clone(&world);
            thread::spawn(move || {
                let reader = world.reader(key, which);
                let path = world.stacks.read(&reader);
                (path, reader.fresh.get().is_none())
            })
        };
        let first = spawn(2, 1);
        let second = spawn(3, 2);
        let (first, first_mapped) = first.join().unwrap();
        let (second, second_mapped) = second.join().unwrap();

        assert_eq!((first, second), (Path::Read, Path::Read));
        let mapped = usize::from(first_mapped) + usize::from(second_mapped);
        assert!(
            mapped >= 1,
            "neither reader mapped a spare past a full arena"
        );
        assert_eq!(
            world.stacks.spares(),
            mapped,
            "a mapped spare is missing from the list"
        );
        Stacks::<1>::release(held, &holder);
    });
}

#[test]
fn a_spare_given_back_serves_the_next_reader_without_a_mapping() {
    explore(|| {
        let world = World::<1, 1>::new();
        let holder = world.reader(1, 0);
        let held = world
            .stacks
            .claim(&holder)
            .expect("the arena's slot was free");
        // Takes the reader's own mapping only if the arena and every spare are held.
        let other = {
            let world = Arc::clone(&world);
            thread::spawn(move || {
                let reader = world.reader(2, 0);
                world.stacks.read(&reader)
            })
        };
        assert_eq!(other.join().unwrap(), Path::Read);
        assert_eq!(world.stacks.spares(), 1);

        // A reader that can map nothing still finds the spare free.
        assert_eq!(world.stacks.read(&refused(3)), Path::Read);
        Stacks::<1>::release(held, &holder);
    });
}

#[test]
fn a_spare_given_back_while_another_reader_walks_is_never_run_on_twice() {
    explore(|| {
        let world = World::<1, 4>::new();
        let holder = world.reader(1, 0);
        let held = world
            .stacks
            .claim(&holder)
            .expect("the arena's slot was free");
        // Two reads in a row on each of two threads, each read with a mapping of its own to fall
        // back on: the second read of one thread may find the other's spare given back.
        let spawn = |key: u64, which: usize| {
            let world = Arc::clone(&world);
            thread::spawn(move || {
                let (once, again) = (world.reader(key, which), world.reader(key, which + 1));
                let paths = (world.stacks.read(&once), world.stacks.read(&again));
                let mapped = [&once, &again]
                    .iter()
                    .filter(|reader| reader.fresh.get().is_none())
                    .count();
                (paths, mapped)
            })
        };
        let first = spawn(2, 0);
        let second = spawn(3, 2);
        let (first, first_mapped) = first.join().unwrap();
        let (second, second_mapped) = second.join().unwrap();

        assert_eq!(first, (Path::Read, Path::Read));
        assert_eq!(second, (Path::Read, Path::Read));
        assert_eq!(
            world.stacks.spares(),
            first_mapped + second_mapped,
            "a mapped spare is missing from the list"
        );
        Stacks::<1>::release(held, &holder);
    });
}

#[test]
fn a_nested_read_is_the_one_read_that_takes_the_genuine_path() {
    explore(|| {
        let world = World::<1, 2>::new();
        let other = {
            let world = Arc::clone(&world);
            thread::spawn(move || world.stacks.read(&world.reader(2, 1)))
        };
        let reader = world.reader(1, 0);
        let outer = world
            .stacks
            .claim(&reader)
            .expect("an outer read takes the read path");
        // A signal handler's read on the same thread, while the outer read is in flight.
        assert_eq!(world.stacks.read(&reader), Path::Genuine);
        assert!(
            reader.in_flight.get(),
            "the nested read ended the outer one's flight"
        );
        assert_eq!(
            outer.busy.load(Ordering::Relaxed),
            1,
            "the nested read gave back the outer read's stack"
        );
        outer.occupy(1);
        Stacks::<1>::release(outer, &reader);

        assert_eq!(other.join().unwrap(), Path::Read);
        assert_eq!(world.stacks.read(&reader), Path::Read);
    });
}

#[test]
fn a_refused_mapping_sends_the_read_to_the_genuine_path_and_ends_its_flight() {
    explore(|| {
        let world = World::<1, 1>::new();
        let holder = world.reader(1, 0);
        let held = world
            .stacks
            .claim(&holder)
            .expect("the arena's slot was free");
        let reader = refused(2);
        assert_eq!(world.stacks.read(&reader), Path::Genuine);
        assert!(
            !reader.in_flight.get(),
            "a refused read left its thread marked as reading"
        );

        Stacks::<1>::release(held, &holder);
        assert_eq!(world.stacks.read(&reader), Path::Read);
    });
}
