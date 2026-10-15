//! The descriptors of the write locks this process holds, so a child forked while it holds one can
//! close its copy.
//!
//! Written once over whichever atomics the module above brings: the standard ones for the registry
//! the process keeps, and loom's where a test drives every interleaving of them.
//!
//! A fork copies every descriptor, and each copy keeps the open file description that owns a lock
//! alive, so a child that keeps running would hold the lock past its parent's release for as long
//! as it lives. The child's fork handler cannot reach the locks of threads that do not exist in
//! the child, so every lock's descriptor is entered here, and the handler closes each one it finds.
//!
//! A descriptor exists from its `open` and is entered only after, and a fork between the two would
//! hand the child a copy nothing here names. So forks are counted, as they start and while they
//! are under way, and an acquire that sees a fork start or under way between the read before its
//! `open` and the read after its entry closes that descriptor and opens another: it never locks
//! one a child might hold.
//!
//! The child's walk is loads, exchanges and the `close` it is handed, which is what a fork handler
//! may do, even for a fork made from a signal handler.

use std::os::raw::c_int;
use std::ptr;

use super::{AtomicPtr, AtomicU64, Ordering, fence};

/// A slot that names no descriptor. Its low half is -1, which no `open` hands out.
pub(super) const FREE: u64 = u64::MAX;

/// The low half of [`Registry::forks`]: forks under way.
const UNDER_WAY: u64 = 0xFFFF_FFFF;

/// One fork more started and under way, in [`Registry::forks`].
const STARTING: u64 = (1 << 32) | 1;

/// The slots, and the count of forks a descriptor is entered against.
pub(super) struct Registry {
    /// Slots, newest first, pushed and never unlinked or freed, so a fork handler walks them with
    /// no lock.
    pub(super) slots: AtomicPtr<Slot>,
    /// Forks started, in the high half, and forks under way, in the low half. A started count
    /// that moved, or any fork under way, means a fork may have copied the descriptors since.
    pub(super) forks: AtomicU64,
}

/// A place for one lock's descriptor.
#[derive(Debug)]
pub(super) struct Slot {
    /// [`FREE`], or the generation the descriptor was entered under in the high half and the
    /// descriptor in the low half.
    pub(super) word: AtomicU64,
    /// The slot pushed before this one, or null. Set before this slot is published.
    pub(super) next: *const Self,
}

// Safety: `word` is atomic, and `next` is written once, before the release that publishes the
// slot, and only read after an acquire that sees it published.
unsafe impl Sync for Slot {}

/// One lock's place in the registry: the slot, and the word it wrote there.
#[derive(Debug, Clone, Copy)]
pub(super) struct Entry {
    slot: &'static Slot,
    word: u64,
}

impl Entry {
    /// The descriptor this entry names.
    pub(super) fn fd(self) -> c_int {
        fd_of(self.word)
    }

    /// Whether this entry is still in its slot: until it leaves, and in a forked child, never.
    pub(super) fn held(self) -> bool {
        self.slot.word.load(Ordering::Relaxed) == self.word
    }

    /// Takes this entry out of its slot, and says whether its descriptor is still the caller's to
    /// close. In a forked child it is not: the child's handler closed it, and the number may name
    /// something else by now.
    pub(super) fn leave(self) -> bool {
        self.slot
            .word
            .compare_exchange(self.word, FREE, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }
}

/// The descriptor in a slot's word.
pub(super) fn fd_of(word: u64) -> c_int {
    u32::try_from(word & UNDER_WAY)
        .unwrap_or(u32::MAX)
        .cast_signed()
}

impl Registry {
    /// The generation to enter a descriptor under, or `None` while a fork is under way.
    ///
    /// Acquire, so a fork this sees finished copied the descriptors before anything opened after.
    pub(super) fn generation(&self) -> Option<u32> {
        let forks = self.forks.load(Ordering::Acquire);
        (forks & UNDER_WAY == 0).then(|| u32::try_from(forks >> 32).unwrap_or(u32::MAX))
    }

    /// Enters `fd`, opened after [`Registry::generation`] gave `generation`, in a free slot or a
    /// new one.
    pub(super) fn enter(&self, generation: u32, fd: c_int) -> Entry {
        let word = (u64::from(generation) << 32) | u64::from(fd.cast_unsigned());
        let mut node = self.slots.load(Ordering::Acquire);
        // Safety: every slot was leaked below, is never freed, and had `next` set before the
        // release that published it.
        while let Some(slot) = unsafe { node.as_ref() } {
            if slot
                .word
                .compare_exchange(FREE, word, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return Entry { slot, word };
            }
            node = slot.next.cast_mut();
        }
        let slot = Box::leak(Box::new(Slot {
            word: AtomicU64::new(word),
            next: ptr::null(),
        }));
        let mut head = self.slots.load(Ordering::Relaxed);
        loop {
            slot.next = head;
            match self
                .slots
                .compare_exchange_weak(head, slot, Ordering::Release, Ordering::Relaxed)
            {
                Ok(_) => return Entry { slot, word },
                Err(now) => head = now,
            }
        }
    }

    /// Whether no fork has started since [`Registry::generation`] gave `generation`. Asked after
    /// the entry.
    ///
    /// The fence here and the one in [`Registry::forking`] pair up: either this load sees the fork
    /// start, or the copy the fork makes after its fence sees the entry.
    pub(super) fn unforked_since(&self, generation: u32) -> bool {
        fence(Ordering::SeqCst);
        self.forks.load(Ordering::Relaxed) == u64::from(generation) << 32
    }

    /// The parent's handler before a fork: one fork more started, and under way. The fork's own
    /// syscall is a full barrier as well; the fence states the one [`Registry::unforked_since`]
    /// pairs with.
    pub(super) fn forking(&self) {
        self.forks.fetch_add(STARTING, Ordering::Relaxed);
        fence(Ordering::SeqCst);
    }

    /// The parent's handler after a fork: that fork no longer under way. Release, for
    /// [`Registry::generation`].
    pub(super) fn forked(&self) {
        self.forks.fetch_sub(1, Ordering::Release);
    }

    /// The child's handler after a fork: every slot freed, and each descriptor in one handed to
    /// `close`. Nothing is under way in the child, whatever was in the parent's other threads.
    pub(super) fn forked_child(&self, mut close: impl FnMut(c_int)) {
        let mut node = self.slots.load(Ordering::Acquire);
        // Safety: as in `enter`.
        while let Some(slot) = unsafe { node.as_ref() } {
            let word = slot.word.swap(FREE, Ordering::Relaxed);
            if word != FREE {
                close(fd_of(word));
            }
            node = slot.next.cast_mut();
        }
        self.forks.fetch_and(!UNDER_WAY, Ordering::Relaxed);
    }
}
