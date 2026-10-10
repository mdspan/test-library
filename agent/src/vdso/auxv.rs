//! The auxiliary vector: the list of facts the kernel leaves above the environment for a program
//! that wants to know how it was started.
//!
//! One of those facts is where the kernel put the thing a target reads the clock from:
//! `AT_SYSINFO_EHDR` for the Linux vDSO, and `AT_TIMEKEEP` for the FreeBSD timekeep page. A target
//! that skips libc finds the clock by walking this list, so rewriting the entry is what puts us in
//! front of such a target: on Linux it points at a vDSO of ours, and on FreeBSD it is zeroed, which
//! sends the Go runtime to the fallback routines interposed in its own text. The list is the
//! process's own, on the initial stack, and it is written in place: there is nowhere else a program
//! looks.
//!
//! The vector sits just past the environment's terminator, which is the only way to its address.
//! So it is found in the constructor, before anything of ours or of the program's can move
//! `environ` off the stack it started on, and what is found is checked against the value libc
//! itself answers for the same entry.
//!
//! On FreeBSD the walk also keeps `AT_PHDR`, `AT_PHENT`, and `AT_PHNUM`, which locate the Go
//! fallback routines in the program's own text: `elf_aux_info` answers none of the three.

use std::ffi::c_char;
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use common::elf::{AT_PHDR, AT_PHENT, AT_PHNUM};

unsafe extern "C" {
    /// The environment as libc keeps it: the array the kernel left on the stack, until a libc that
    /// has to grow it moves the array somewhere else.
    static mut environ: *mut *mut c_char;
}

/// The entry this platform names the clock's source in: the vDSO on Linux, and the timekeep page
/// on FreeBSD.
#[cfg(target_os = "linux")]
const TAG: libc::c_ulong = common::platform::AT_SYSINFO_EHDR;
#[cfg(target_os = "freebsd")]
const TAG: libc::c_ulong = common::platform::AT_TIMEKEEP as libc::c_ulong;

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

/// The [`TAG`] entry of this process, or null when it was not found.
static SLOT: AtomicPtr<Entry> = AtomicPtr::new(ptr::null_mut());

/// What the kernel left in that entry, or zero when this process has nothing there.
static GENUINE: AtomicUsize = AtomicUsize::new(0);

/// This process's `AT_PHDR`, `AT_PHENT`, and `AT_PHNUM`, in that order, as the walk found them, or
/// zero for any the walk did not find.
#[cfg(target_os = "freebsd")]
static PROGRAM_HEADERS: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];

/// Finds the entry, before the program or we can move the environment it sits behind.
///
/// Called once, from the constructor. Relaxed stores, because the constructor is the whole of this
/// process at the time and every later reader is a thread it started afterwards.
pub(super) fn prime() {
    let Some(vector) = find() else {
        return;
    };
    let Some(entry) = entry_of(vector, TAG) else {
        return;
    };
    GENUINE.store(entry.value.load(Ordering::Relaxed), Ordering::Relaxed);
    SLOT.store(ptr::from_ref(entry).cast_mut(), Ordering::Relaxed);
    #[cfg(target_os = "freebsd")]
    for (kept, tag) in PROGRAM_HEADERS.iter().zip([AT_PHDR, AT_PHENT, AT_PHNUM]) {
        if let Some(entry) = entry_of(vector, tag) {
            kept.store(entry.value.load(Ordering::Relaxed), Ordering::Relaxed);
        }
    }
}

/// What the kernel left in the entry, or `None` when this process has nothing there to pose as or
/// the vector could not be reached.
#[cfg(target_os = "linux")]
pub(super) fn genuine() -> Option<usize> {
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

/// Zeroes the timekeep entry, so a Go runtime that reads it when it starts finds no page and falls
/// back to the routine interposed in its own text. Says whether there was an entry to zero.
///
/// The same store [`point_at`] makes, Release for the same reason: the fallback it steers the
/// runtime onto reads the session this process installed first.
#[cfg(target_os = "freebsd")]
#[cfg_attr(
    not(feature = "vdso"),
    expect(
        dead_code,
        reason = "the FreeBSD pose path withholds the entry through this; a build without the \
                  vdso feature does not compile that path and leaves it unused"
    )
)]
pub(super) fn withhold_timekeep() -> bool {
    point_at(0)
}

/// This process's `AT_PHDR`, `AT_PHENT`, and `AT_PHNUM`: where its program header array is, how
/// long one entry is, and how many there are. `None` when any is absent.
///
/// A target that reads the clock without libc carries that code in the segments these headers
/// describe.
#[cfg_attr(
    not(any(test, all(target_os = "freebsd", feature = "vdso"))),
    expect(
        dead_code,
        reason = "only the FreeBSD pose path reads the headers through this; elsewhere it is \
                  unused"
    )
)]
pub(super) fn program_headers() -> Option<(usize, usize, usize)> {
    let [phdr, phent, phnum] = program_header_entries();
    (phdr != 0 && phent != 0 && phnum != 0).then_some((phdr, phent, phnum))
}

/// The three values through libc's own reader, zero for any it does not answer.
#[cfg(target_os = "linux")]
fn program_header_entries() -> [usize; 3] {
    [AT_PHDR, AT_PHENT, AT_PHNUM].map(|tag| {
        // Safety: `getauxval` takes a tag and answers a number, touching nothing of ours.
        usize::try_from(unsafe { libc::getauxval(tag) }).unwrap_or(0)
    })
}

/// The three values as the constructor's walk kept them.
#[cfg(target_os = "freebsd")]
fn program_header_entries() -> [usize; 3] {
    PROGRAM_HEADERS
        .each_ref()
        .map(|kept| kept.load(Ordering::Relaxed))
}

/// This process's vector, or `None` when it cannot be reached or carries no [`TAG`] entry.
fn find() -> Option<&'static [Entry]> {
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

/// The vector past the environment `strings` holds, up to its terminator, or `None` when what
/// follows the environment is not a vector whose [`TAG`] entry carries `wanted`.
fn walk(strings: *mut *mut c_char, wanted: usize) -> Option<&'static [Entry]> {
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
        if entry.tag > MAX_TAG {
            return None;
        }
        if entry.tag == AT_NULL {
            // Safety: every entry ahead of the terminator was read above as one of the vector's.
            let vector = unsafe { std::slice::from_raw_parts(entries, index) };
            return entry_of(vector, TAG)
                .is_some_and(|found| found.value.load(Ordering::Relaxed) == wanted)
                .then_some(vector);
        }
    }
    None
}

/// The entry of `vector` under `tag`.
fn entry_of(vector: &'static [Entry], tag: libc::c_ulong) -> Option<&'static Entry> {
    vector.iter().find(|entry| entry.tag == tag)
}

/// What libc says the entry carries, or zero when this process has nothing there.
///
/// This is what says the walk landed on the vector rather than on a copy of the environment
/// somebody moved to the heap, so it has to come from libc's own reader rather than from the same
/// walk: `getauxval` here, and `elf_aux_info` where that is the reader libc gives.
#[cfg(target_os = "linux")]
fn genuine_value() -> libc::c_ulong {
    // Safety: `getauxval` takes a tag and answers a number, touching nothing of ours.
    unsafe { libc::getauxval(TAG) }
}

/// The same, through the reader this platform's libc gives.
#[cfg(target_os = "freebsd")]
fn genuine_value() -> libc::c_ulong {
    let mut at: *mut libc::c_void = ptr::null_mut();
    let len = libc::c_int::try_from(size_of::<*mut libc::c_void>()).unwrap_or(0);
    // Safety: the buffer is a live pointer-sized word, which is what this entry answers in.
    let asked =
        unsafe { libc::elf_aux_info(common::platform::AT_TIMEKEEP, (&raw mut at).cast(), len) };
    if asked != 0 {
        return 0;
    }
    libc::c_ulong::try_from(at.addr()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the constructor found, which it had to find before naming the session in the
    /// environment: a libc that grows the environment moves it off the stack the vector sits on,
    /// and the walk past it stops being a walk to the vector.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_entry_was_found_and_what_the_kernel_left_in_it_was_kept() {
        let genuine = genuine().expect("a program the kernel started carries the entry");
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

    /// A walk that lands on a vector carrying the entry keeps every entry up to the terminator,
    /// those after the entry included.
    #[test]
    fn a_walk_that_lands_on_a_vector_keeps_all_of_it() {
        let word = |value: libc::c_ulong| {
            ptr::without_provenance_mut::<c_char>(usize::try_from(value).unwrap())
        };
        // One variable and the environment's terminator, then two entries and the terminator.
        let planted = [
            word(0x4000_0000),
            word(0),
            word(TAG),
            word(0x5000),
            word(AT_PHNUM),
            word(9),
            word(AT_NULL),
            word(0),
        ];
        let vector = walk(planted.as_ptr().cast_mut(), 0x5000).expect("the entry is there");
        assert_eq!(vector.len(), 2);
        let phnum = entry_of(vector, AT_PHNUM).expect("the entry after it is kept");
        assert_eq!(phnum.value.load(Ordering::Relaxed), 9);

        assert!(walk(planted.as_ptr().cast_mut(), 0x6000).is_none());
    }

    /// The program headers this process reports locate an image that covers its own code, which is
    /// what the scan for the Go fallback routines runs over.
    #[test]
    fn the_program_headers_are_found_and_describe_this_image() {
        let (phdr, phent, phnum) = program_headers().expect("a program the kernel started");
        assert_eq!(phent, size_of::<common::elf::Phdr>());
        // Safety: the three values are this process's own, from the auxiliary vector.
        let image = unsafe { common::elf::MainImage::locate(phdr, phent, phnum) };
        assert!(image.is_some(), "the headers located no image");
    }
}
