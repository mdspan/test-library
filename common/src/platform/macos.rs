//! macOS.

use std::ffi::{c_char, c_void};
use std::io;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::time::Family;

/// What the header's platform field carries for macOS.
pub const OS_TAG: u32 = 3;

/// The clock ids of the families, by the names the family table gives them.
///
/// The two elapsed names are the reverse of Linux's: `CLOCK_MONOTONIC` keeps counting while the
/// machine sleeps and belongs to Boottime, and `CLOCK_UPTIME_RAW` stops and belongs to Monotonic.
pub mod clock {
    use libc::clockid_t;

    use crate::time::Family;

    /// The wall clock.
    pub const REALTIME: clockid_t = libc::CLOCK_REALTIME;
    /// Time since boot, not counting suspend.
    pub const MONOTONIC: clockid_t = libc::CLOCK_UPTIME_RAW;
    /// The same, read from the last tick rather than from the counter.
    pub const MONOTONIC_APPROX: clockid_t = libc::CLOCK_UPTIME_RAW_APPROX;
    /// Time since boot, counting suspend.
    pub const BOOTTIME: clockid_t = libc::CLOCK_MONOTONIC;
    /// The same, without the NTP slew.
    pub const BOOTTIME_RAW: clockid_t = libc::CLOCK_MONOTONIC_RAW;
    /// The same again, read from the last tick.
    pub const BOOTTIME_RAW_APPROX: clockid_t = libc::CLOCK_MONOTONIC_RAW_APPROX;
    /// The wall clock without leap seconds, which macOS does not keep separately.
    pub const TAI: clockid_t = libc::CLOCK_REALTIME;
    /// CPU time consumed by the whole process.
    pub const PROCESS_CPUTIME: clockid_t = libc::CLOCK_PROCESS_CPUTIME_ID;
    /// CPU time consumed by the calling thread.
    pub const THREAD_CPUTIME: clockid_t = libc::CLOCK_THREAD_CPUTIME_ID;

    /// Every clock a family holds, by the name a user writes for a timebase.
    ///
    /// The `mach_*_time` functions belong to these families too, and are not here: they are clocks
    /// without a clock id, so there is no id for a user to name one with.
    const NAMED: &[(&str, clockid_t)] = &[
        ("CLOCK_REALTIME", REALTIME),
        ("CLOCK_UPTIME_RAW", MONOTONIC),
        ("CLOCK_UPTIME_RAW_APPROX", MONOTONIC_APPROX),
        ("CLOCK_MONOTONIC", BOOTTIME),
        ("CLOCK_MONOTONIC_RAW", BOOTTIME_RAW),
        ("CLOCK_MONOTONIC_RAW_APPROX", BOOTTIME_RAW_APPROX),
        ("CLOCK_PROCESS_CPUTIME_ID", PROCESS_CPUTIME),
        ("CLOCK_THREAD_CPUTIME_ID", THREAD_CPUTIME),
    ];

    /// The clocks a user may name as a timebase. One list, because no clock here moves between the
    /// families with the kernel's version.
    #[must_use]
    pub const fn named() -> &'static [(&'static str, clockid_t)] {
        NAMED
    }

    /// The family a clock id belongs to, or `None` when the id is left on genuine time.
    #[must_use]
    pub const fn family_of(clock: clockid_t) -> Option<Family> {
        match clock {
            REALTIME => Some(Family::Realtime),
            MONOTONIC | MONOTONIC_APPROX => Some(Family::Monotonic),
            BOOTTIME | BOOTTIME_RAW | BOOTTIME_RAW_APPROX => Some(Family::Boottime),
            PROCESS_CPUTIME | THREAD_CPUTIME => Some(Family::Cpu),
            _ => None,
        }
    }

    /// Whether this clock reads the Realtime line plus the TAI offset.
    ///
    /// Never here: macOS has no clock id of its own for TAI, so there is none to carry it.
    #[must_use]
    pub const fn is_tai(_clock: clockid_t) -> bool {
        false
    }
}

/// The first clock in the family's column of the family table.
///
/// A family's zero always comes from it, and with no override it is the timebase too. Cpu has
/// none, because each of its clocks tracks passage through its own reading.
#[must_use]
pub const fn default_timebase(family: Family) -> Option<libc::clockid_t> {
    match family {
        Family::Realtime => Some(clock::REALTIME),
        Family::Monotonic => Some(clock::MONOTONIC),
        Family::Boottime => Some(clock::BOOTTIME),
        Family::Cpu => None,
    }
}

/// The id the kernel gives this thread.
///
/// What names a thread where thread-local storage cannot: a runtime that makes its own threads
/// without libc leaves the thread pointer holding something of its own.
#[must_use]
pub fn thread_id() -> u64 {
    let mut id: u64 = 0;
    // Safety: a null thread names the calling one, and the id lands in a live word.
    if unsafe { libc::pthread_threadid_np(0, &raw mut id) } != 0 {
        return 0;
    }
    id
}

/// Sets this thread's `errno`, which is how a refused call reports itself in libc's convention.
pub fn set_errno(value: libc::c_int) {
    // Safety: `__error` answers this thread's own `errno`, taking nothing.
    let location = unsafe { libc::__error() };
    // Safety: the location libc just named is live for as long as this thread is.
    unsafe { *location = value };
}

/// Where libSystem lives. Every process has it mapped before a line of its own runs, so it is
/// opened rather than loaded.
const LIBSYSTEM: &[u8] = b"/usr/lib/libSystem.B.dylib\0";

/// The handle a genuine function is asked of, or null before anyone has asked.
static IMAGE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

/// The image a genuine function comes from: libSystem itself.
///
/// Asked for by name rather than reached by a definition of ours, because there is no definition
/// of ours to reach past. dyld binds two-level, so a second `clock_gettime` in this library would
/// never be called; what puts us in front of the program is the interpose table, and the genuine
/// side of every row in it is the address below.
///
/// Relaxed both ways: the handle names an image the loader mapped before this process ran a line
/// of its own, so there is nothing for an acquire to order against. Two threads racing here open
/// it twice and store the same answer.
fn image() -> *mut c_void {
    let known = IMAGE.load(Ordering::Relaxed);
    if !known.is_null() {
        return known;
    }
    // Safety: the path is NUL-terminated, and `RTLD_NOLOAD` asks only whether the image is already
    // mapped, which in this process it always is.
    let opened = unsafe {
        libc::dlopen(
            LIBSYSTEM.as_ptr().cast::<c_char>(),
            libc::RTLD_LAZY | libc::RTLD_NOLOAD,
        )
    };
    // An image that will not open by name leaves the loader's own search, which finds the same
    // definition by another route: there is nothing of ours in front of it to search past.
    let handle = if opened.is_null() {
        super::RTLD_NEXT
    } else {
        opened
    };
    IMAGE.store(handle, Ordering::Relaxed);
    handle
}

/// libSystem's own definition of `name`, or `None` when it has none.
///
/// The way past every replacement of ours: there is no definition of ours to reach past, so the
/// image is asked by name instead of the loader being asked for the next one along. Every caller
/// remembers the answer, which never moves.
///
/// # Safety
/// `name` must be NUL-terminated.
#[must_use]
pub unsafe fn libsystem_symbol(name: &[u8]) -> Option<NonNull<c_void>> {
    // Safety: the caller guarantees the name, and the handle is the loader's own.
    let found = unsafe { libc::dlsym(image(), name.as_ptr().cast::<c_char>()) };
    if found.is_null() {
        // `dlerror` reports and clears, so leaving ours set would hand the program our message the
        // next time it asked about a failure of its own.
        // Safety: `dlerror` takes nothing and reports on the calling thread alone.
        let _reported = unsafe { libc::dlerror() };
        return None;
    }
    NonNull::new(found)
}

/// libSystem's definition of `name`, remembered after the first lookup.
///
/// Relaxed for the reason [`image`] is, and reaching the code through the pointer is an address
/// dependency.
fn found(slot: &AtomicPtr<c_void>, name: &[u8]) -> Option<NonNull<c_void>> {
    if let Some(known) = NonNull::new(slot.load(Ordering::Relaxed)) {
        return Some(known);
    }
    // Safety: the name is NUL-terminated by construction.
    let found = unsafe { libsystem_symbol(name) }?;
    slot.store(found.as_ptr(), Ordering::Relaxed);
    Some(found)
}

/// Declares a genuine function: a slot for libSystem's own definition, the lookup that fills it,
/// and a wrapper that calls it in libc's convention.
///
/// The name libSystem knows it by rather than the name spelled here, because every one of these is
/// a function we stand in front of, and the wrapper carries a name of its own so that the two can
/// live in one library.
///
/// A system with no such name leaves the wrapper answering the given refusal with `ENOSYS`, which
/// only a program calling a function its own system never had can reach.
macro_rules! genuine {
    ($(
        $(#[doc = $doc:expr])*
        fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty = $symbol:expr, $refused:expr;
    )*) => {
        $(
            mod $name {
                //! One genuine function: where libSystem's definition of it is kept, and the
                //! lookup that fills that in.
                use super::{AtomicPtr, c_void, found, ptr};

                /// libSystem's definition, or null until a lookup finds it.
                pub(super) static FOUND: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

                /// The name libSystem is asked for.
                pub(super) const SYMBOL: &[u8] = $symbol;

                /// Looks the name up now, so no later call has to.
                pub(super) fn prime() {
                    let _found = found(&FOUND, SYMBOL);
                }
            }

            $(#[doc = $doc])*
            ///
            /// # Safety
            /// The arguments must satisfy the contract libc gives the function of this name.
            #[must_use]
            #[inline]
            pub unsafe fn $name($($arg: $ty),*) -> $ret {
                /// What libSystem's definition is called through.
                type Signature = unsafe extern "C" fn($($ty),*) -> $ret;
                let Some(found) = found(&$name::FOUND, $name::SYMBOL) else {
                    set_errno(libc::ENOSYS);
                    return $refused;
                };
                // Safety: libSystem declares this name with this signature.
                let call = unsafe {
                    std::mem::transmute::<*mut c_void, Signature>(found.as_ptr())
                };
                // Safety: the caller guarantees the arguments, which pass on unchanged.
                unsafe { call($($arg),*) }
            }
        )*

        /// Finds every one of them now, from the constructor.
        ///
        /// A lookup takes the loader's own lock, and the first interposed call can come from
        /// anywhere, a library's constructor included. Doing them all here leaves nothing for such
        /// a call to find missing.
        pub fn prime_genuine() {
            $( $name::prime(); )*
        }
    };
}

genuine! {
    /// Reads a genuine clock. libc's convention.
    fn genuine_clock_gettime(clock: libc::clockid_t, ts: *mut libc::timespec) -> libc::c_int
        = b"clock_gettime\0", -1;

    /// Reads the genuine process times: the fields of `buf`, which may be null, and the elapsed
    /// ticks `times(3)` answers. libc's convention.
    ///
    /// libSystem builds this out of two `getrusage` calls and one `gettimeofday`, so the elapsed
    /// ticks it answers count from the epoch rather than from boot. Both of those calls are ones
    /// we stand in front of, and neither reaches us: dyld leaves the shared cache calling itself
    /// by its own names.
    fn genuine_times(buf: *mut libc::tms) -> libc::clock_t = b"times\0", !0;

    /// Reads the genuine resource usage of `who`. libc's convention.
    fn genuine_getrusage(who: libc::c_int, usage: *mut libc::rusage) -> libc::c_int
        = b"getrusage\0", -1;

    /// Sets the genuine wall clock, libc's convention, for a process this library is not faking.
    ///
    /// A null `tv` leaves the clock where it is and a null `tz` leaves the obsolete timezone
    /// alone, which is what the kernel makes of each, so both pass through as the program wrote
    /// them.
    fn genuine_settimeofday(tv: *const libc::timeval, tz: *const c_void) -> libc::c_int
        = b"settimeofday\0", -1;

    /// Sets a genuine clock, libc's convention, for a process this library is not faking.
    fn genuine_clock_settime(clock: libc::clockid_t, tp: *const libc::timespec) -> libc::c_int
        = b"clock_settime\0", -1;

    /// Makes the genuine one-shot offset adjustment, libc's convention, for a process this library
    /// is not faking.
    fn genuine_adjtime(delta: *const libc::timeval, olddelta: *mut libc::timeval) -> libc::c_int
        = b"adjtime\0", -1;
}

/// The widest delta the platform's own `adjtime` takes before it refuses, in seconds.
///
/// `adjtime` is a system call of its own here, taking the `timeval` as it comes, so the bound is
/// the range an instant holds rather than anything libc imposes.
pub const ADJTIME_SECONDS: i64 = i64::MAX / 1_000_000_000;

/// Whether the kernel would let this process set the clock: an effective uid of 0 here, since
/// macOS gates the call on the superuser rather than on a capability.
#[must_use]
pub fn may_set_the_clock() -> bool {
    // Safety: `geteuid` reads process state and cannot fail.
    let euid = unsafe { libc::geteuid() };
    euid == 0
}

/// `fcntl`'s open-file-description lock, waiting for the current holder. The kernel has had these
/// since 10.11, but an SDK older than macOS 14 hides the definitions, so the numbers are spelled
/// here rather than taken from bindings that follow a header which may not have them.
const F_OFD_SETLKW: libc::c_int = 91;
/// The question of whether such a lock is held.
const F_OFD_GETLK: libc::c_int = 92;

/// Takes the write lock on `fd`: an exclusive open-file-description lock on byte 0, blocking until
/// the current holder releases it. libc's convention, so a caller retries `EINTR` itself.
///
/// # Safety
/// `fd` must be an open descriptor on the lock file, opened for writing.
#[must_use]
pub unsafe fn take_write_lock(fd: libc::c_int) -> libc::c_int {
    let request = crate::sync::lock_request();
    // Safety: the caller guarantees `fd`, and `request` is a live `flock`.
    unsafe { libc::fcntl(fd, F_OFD_SETLKW, &raw const request) }
}

/// Asks the kernel whether the write lock on `fd` is held, taking nothing. Fills `request`, which
/// the caller has zeroed and filled with the question.
///
/// # Safety
/// `fd` must be an open descriptor on the lock file and `request` a live `flock`.
#[must_use]
pub unsafe fn probe_write_lock(fd: libc::c_int, request: *mut libc::flock) -> libc::c_int {
    // Safety: the caller guarantees both arguments.
    unsafe { libc::fcntl(fd, F_OFD_GETLK, request) }
}

/// How much randomness one `getentropy(2)` answers at most.
const ENTROPY_AT_ONCE: usize = 256;

/// Fills `buf` with randomness from the kernel.
///
/// # Errors
/// Whatever `getentropy(2)` refused with.
pub fn random_bytes(buf: &mut [u8]) -> io::Result<()> {
    for chunk in buf.chunks_mut(ENTROPY_AT_ONCE) {
        // Safety: the pointer and length name a live buffer of no more than one call's worth.
        let got = unsafe { libc::getentropy(chunk.as_mut_ptr().cast::<c_void>(), chunk.len()) };
        if got != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_family_table_places_every_clock_of_the_darwin_column() {
        for (id, family) in [
            (clock::REALTIME, Family::Realtime),
            (clock::MONOTONIC, Family::Monotonic),
            (clock::MONOTONIC_APPROX, Family::Monotonic),
            (clock::BOOTTIME, Family::Boottime),
            (clock::BOOTTIME_RAW, Family::Boottime),
            (clock::BOOTTIME_RAW_APPROX, Family::Boottime),
            (clock::PROCESS_CPUTIME, Family::Cpu),
            (clock::THREAD_CPUTIME, Family::Cpu),
        ] {
            assert_eq!(clock::family_of(id), Some(family), "clock {id}");
        }
    }

    #[test]
    fn the_two_elapsed_names_are_the_reverse_of_the_linux_ones() {
        // `CLOCK_MONOTONIC` keeps counting while the machine sleeps here, so it is the Boottime
        // family's, and `CLOCK_UPTIME_RAW` stops, so it is Monotonic's.
        assert_eq!(
            clock::family_of(libc::CLOCK_MONOTONIC),
            Some(Family::Boottime)
        );
        assert_eq!(
            clock::family_of(libc::CLOCK_UPTIME_RAW),
            Some(Family::Monotonic)
        );
        // And the timebases follow: Monotonic tracks `CLOCK_UPTIME_RAW`, Boottime tracks
        // `CLOCK_MONOTONIC`, and Realtime tracks the wall clock.
        assert_eq!(default_timebase(Family::Realtime), Some(clock::REALTIME));
        assert_eq!(
            default_timebase(Family::Monotonic),
            Some(libc::CLOCK_UPTIME_RAW)
        );
        assert_eq!(
            default_timebase(Family::Boottime),
            Some(libc::CLOCK_MONOTONIC)
        );
        assert_eq!(default_timebase(Family::Cpu), None);
    }

    #[test]
    fn there_is_no_clock_carrying_the_tai_offset() {
        for &(_, id) in clock::named() {
            assert!(!clock::is_tai(id), "clock {id}");
        }
    }

    #[test]
    fn only_the_superuser_may_set_the_clock() {
        // Safety: `geteuid` reads process state and cannot fail.
        let euid = unsafe { libc::geteuid() };
        assert_eq!(
            may_set_the_clock(),
            euid == 0,
            "the clock is the superuser's to set here, and nobody else's"
        );
    }

    #[test]
    fn the_genuine_functions_come_from_libsystem() {
        prime_genuine();
        assert!(!image().is_null(), "libSystem is mapped in every process");

        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // Safety: `ts` is a live, correctly typed buffer.
        let read = unsafe { genuine_clock_gettime(clock::REALTIME, &raw mut ts) };
        assert_eq!(read, 0);
        assert!(
            ts.tv_sec > 1_700_000_000,
            "the wall clock read {}",
            ts.tv_sec
        );
        for &(name, id) in clock::named() {
            // Safety: as above, for every clock this platform names.
            let read = unsafe { genuine_clock_gettime(id, &raw mut ts) };
            assert_eq!(read, 0, "{name}");
        }
    }

    #[test]
    fn a_refused_call_reports_itself_the_way_libc_does() {
        // Safety: an unknown clock id is refused before the kernel reads the buffer.
        let refused = unsafe { genuine_clock_gettime(0x7fff_ffff, ptr::null_mut()) };
        assert_eq!(refused, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::EINVAL)
        );
    }

    #[test]
    fn a_name_libsystem_never_had_is_refused_rather_than_called() {
        static MISSING: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
        assert_eq!(found(&MISSING, b"libtimewarp_no_such_symbol\0"), None);
        // The message the failure left is cleared, so the program's next `dlerror` is its own.
        // Safety: `dlerror` takes nothing and reports on this thread alone.
        assert!(unsafe { libc::dlerror() }.is_null());
    }
}
