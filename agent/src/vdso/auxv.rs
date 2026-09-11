//! The auxiliary vector: the list of facts the kernel leaves above the environment for a program
//! that wants to know how it was started.
//!
//! One of those facts is `AT_SYSINFO_EHDR`, where the vDSO is mapped. A target that skips libc
//! finds the clock by walking this list, so pointing the entry at an object of ours is what puts
//! us in front of such a target. The list is the process's own, on the initial stack, and it is
//! written in place: there is nowhere else a program looks.
//!
//! The vector sits just past the environment's terminator, which is the only way to its address.
//! So it is found in the constructor, before anything of ours or of the program's can move
//! `environ` off the stack it started on, and what is found is checked against the value libc
//! itself answers for the same entry.

use std::ffi::c_char;
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

unsafe extern "C" {
    /// The environment as libc keeps it: the array the kernel left on the stack, until a libc that
    /// has to grow it moves the array somewhere else.
    static mut environ: *mut *mut c_char;
}

/// The entry naming where the vDSO is mapped. The libc crate spells this one for Android alone, so
/// it is spelled here.
const AT_SYSINFO_EHDR: libc::c_ulong = 33;

/// The entry that ends the vector.
const AT_NULL: libc::c_ulong = 0;

/// How far the vector is walked before what is being walked is taken for something other than one.
/// Longer than any kernel's, which runs to a couple of dozen entries.
const MAX_ENTRIES: usize = 64;

/// The highest tag a kernel has ever used, with room above it. A word past this is not a tag, so it
/// says the walk left the vector: the environment was moved off the stack before we looked.
const MAX_TAG: libc::c_ulong = 1024;

/// The vector counts both of an entry's words in the width a pointer has, which is what lets the
/// value below be read and written as the address it carries.
const _: () = assert!(size_of::<usize>() == size_of::<libc::c_ulong>());

/// One entry: which fact, and what it carries.
#[derive(Debug)]
#[repr(C)]
struct Entry {
    /// Which fact this is. [`AT_NULL`] ends the vector.
    tag: libc::c_ulong,
    /// What it carries.
    ///
    /// Atomic because this word is what a thread the program starts later reads to find the vDSO,
    /// and the release of the store below is what publishes the object behind the address.
    value: AtomicUsize,
}

/// The `AT_SYSINFO_EHDR` entry of this process, or null when it was not found.
static SLOT: AtomicPtr<Entry> = AtomicPtr::new(ptr::null_mut());

/// Where the kernel mapped the genuine vDSO, or zero when this process has none.
static GENUINE: AtomicUsize = AtomicUsize::new(0);

/// Finds the entry, before the program or we can move the environment it sits behind.
///
/// Called once, from the constructor. Relaxed stores, because the constructor is the whole of this
/// process at the time and every later reader is a thread it started afterwards.
pub(super) fn prime() {
    let Some(entry) = find() else {
        return;
    };
    GENUINE.store(entry.value.load(Ordering::Relaxed), Ordering::Relaxed);
    SLOT.store(ptr::from_ref(entry).cast_mut(), Ordering::Relaxed);
}

/// Where the kernel mapped the genuine vDSO, or `None` when this process has none to pose as or
/// the vector could not be reached.
pub(super) fn genuine_vdso() -> Option<usize> {
    if SLOT.load(Ordering::Relaxed).is_null() {
        return None;
    }
    let base = GENUINE.load(Ordering::Relaxed);
    (base != 0).then_some(base)
}

/// Points the entry at `base`, and says whether there was an entry to point.
pub(super) fn point_at(base: usize) -> bool {
    let slot = SLOT.load(Ordering::Relaxed);
    let Some(entry) = (!slot.is_null()).then(|| {
        // Safety: the pointer names an entry of this process's own auxiliary vector, which lives
        // as long as the process does.
        unsafe { &*slot }
    }) else {
        return false;
    };
    // Release: whoever loads this address goes on to read the object behind it, so every store
    // that built the object has to be visible to that reader first.
    entry.value.store(base, Ordering::Release);
    true
}

/// The `AT_SYSINFO_EHDR` entry of this process's vector, or `None` when the vector cannot be
/// reached or carries no such entry.
fn find() -> Option<&'static Entry> {
    // A raw pointer, because libc may replace what it points at, and read from the constructor
    // because that is before the program has run a line that could.
    let held = &raw const environ;
    // Safety: `environ` is libc's own pointer to the environment, which it keeps for the life of
    // the process.
    let strings = unsafe { ptr::read(held) };
    if strings.is_null() {
        return None;
    }
    // What libc answers for the same entry, which is what says the walk landed on the vector
    // rather than on a copy of the environment somebody moved to the heap.
    let wanted = usize::try_from(genuine_value()).ok()?;
    walk(strings, wanted)
}

/// The entry past the environment `strings` holds, or `None` when what follows it is not a vector
/// carrying `wanted`.
fn walk(strings: *mut *mut c_char, wanted: usize) -> Option<&'static Entry> {
    let mut strings = strings;
    // Past the environment's own terminator is where the vector starts.
    loop {
        // Safety: the environment is a NUL-terminated array of strings, so this walk stops at its
        // terminator.
        let string = unsafe { *strings };
        if string.is_null() {
            break;
        }
        // Safety: as above; the terminator has not been reached, so there is another element.
        strings = unsafe { strings.add(1) };
    }
    // Safety: the terminator is the last element of the array, and the vector follows it.
    let entries: *const Entry = unsafe { strings.add(1) }.cast();

    for index in 0..MAX_ENTRIES {
        // Safety: the walk stops at the terminator, at a tag no kernel uses, and at the bound
        // above, so nothing past the end of a vector is read as one.
        let at = unsafe { entries.add(index) };
        // Safety: as above.
        let entry = unsafe { &*at };
        if entry.tag == AT_NULL || entry.tag > MAX_TAG {
            return None;
        }
        if entry.tag == AT_SYSINFO_EHDR && entry.value.load(Ordering::Relaxed) == wanted {
            return Some(entry);
        }
    }
    None
}

/// What libc says `AT_SYSINFO_EHDR` carries, or zero when this process has no vDSO.
fn genuine_value() -> libc::c_ulong {
    // Safety: `getauxval` takes a tag and answers a number, touching nothing of ours.
    unsafe { libc::getauxval(AT_SYSINFO_EHDR) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the constructor found, which it had to find before naming the session in the
    /// environment: a libc that grows the environment moves it off the stack the vector sits on,
    /// and the walk past it stops being a walk to the vector.
    #[test]
    fn the_entry_was_found_and_what_the_kernel_left_in_it_was_kept() {
        let genuine = genuine_vdso().expect("a program the kernel started carries the entry");
        // Safety: the address is what the kernel left in the vector, where it maps an ELF object.
        let magic = unsafe { ptr::with_exposed_provenance::<[u8; 4]>(genuine).read() };
        assert_eq!(magic, [0x7f, b'E', b'L', b'F'], "the entry named no object");
    }

    /// A libc that grows the environment copies it to the heap, where what follows the copy is
    /// whatever the allocator put there. The walk has to refuse that rather than read it as a
    /// vector.
    #[test]
    fn a_walk_that_lands_outside_a_vector_finds_nothing() {
        // A terminator, and past it the words a copy on the heap is followed by: addresses, which
        // are numbers no kernel uses for a tag.
        let planted: [*mut c_char; 3] = [
            ptr::null_mut(),
            ptr::without_provenance_mut(0x4000_0000),
            ptr::without_provenance_mut(0x4000_1000),
        ];
        assert!(walk(planted.as_ptr().cast_mut(), 0).is_none());
    }
}
