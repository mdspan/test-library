//! FreeBSD.

use std::io;
use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

use crate::time::Family;

/// What the header's platform field carries for FreeBSD.
pub const OS_TAG: u32 = 2;

/// The clock ids of the families, by the names the family table gives them.
///
/// Two of these ids are one clock under two names, because the headers spell the Linux names as
/// aliases: `CLOCK_REALTIME_COARSE` is `CLOCK_REALTIME_FAST` and `CLOCK_MONOTONIC_COARSE` is
/// `CLOCK_MONOTONIC_FAST`. `CLOCK_BOOTTIME` is an alias too, of a clock that changed at 15.0, so
/// it is the one name whose id [`named`] answers from the running kernel.
pub mod clock {
    use libc::clockid_t;

    use crate::time::Family;

    use super::counts_suspended;

    /// The wall clock.
    pub const REALTIME: clockid_t = libc::CLOCK_REALTIME;
    /// The wall clock, read with the hardware counter rather than from the last tick.
    pub const REALTIME_PRECISE: clockid_t = libc::CLOCK_REALTIME_PRECISE;
    /// The wall clock read from the last tick, which `CLOCK_REALTIME_COARSE` also names.
    pub const REALTIME_FAST: clockid_t = libc::CLOCK_REALTIME_FAST;
    /// The wall clock in whole seconds.
    pub const SECOND: clockid_t = libc::CLOCK_SECOND;
    /// The wall clock without leap seconds, which exists from 15.0.
    pub const TAI: clockid_t = 16;
    /// Time since boot. Through 14.x nothing counts suspended time; from 15.0 this one does, and
    /// `CLOCK_BOOTTIME` is another name for it.
    pub const MONOTONIC: clockid_t = libc::CLOCK_MONOTONIC;
    /// The same, read with the hardware counter.
    pub const MONOTONIC_PRECISE: clockid_t = libc::CLOCK_MONOTONIC_PRECISE;
    /// The same, read from the last tick, which `CLOCK_MONOTONIC_COARSE` also names.
    pub const MONOTONIC_FAST: clockid_t = libc::CLOCK_MONOTONIC_FAST;
    /// Time since boot, not counting suspended time from 15.0. Through 14.x `CLOCK_BOOTTIME` is
    /// another name for it.
    pub const UPTIME: clockid_t = libc::CLOCK_UPTIME;
    /// The same, read with the hardware counter.
    pub const UPTIME_PRECISE: clockid_t = libc::CLOCK_UPTIME_PRECISE;
    /// The same, read from the last tick.
    pub const UPTIME_FAST: clockid_t = libc::CLOCK_UPTIME_FAST;
    /// CPU time consumed by the whole process.
    pub const PROCESS_CPUTIME: clockid_t = libc::CLOCK_PROCESS_CPUTIME_ID;
    /// CPU time consumed by the calling thread.
    pub const THREAD_CPUTIME: clockid_t = libc::CLOCK_THREAD_CPUTIME_ID;
    /// The slice of process CPU time spent in user mode.
    pub const VIRTUAL: clockid_t = libc::CLOCK_VIRTUAL;
    /// Process CPU time in user and kernel mode together.
    pub const PROF: clockid_t = libc::CLOCK_PROF;

    /// Every clock a family holds through 14.x, by the name a user writes for a timebase.
    ///
    /// `CLOCK_BOOTTIME` is `CLOCK_UPTIME` here, and there is no TAI clock to name.
    const THROUGH_14: &[(&str, clockid_t)] = &[
        ("CLOCK_REALTIME", REALTIME),
        ("CLOCK_REALTIME_PRECISE", REALTIME_PRECISE),
        ("CLOCK_REALTIME_FAST", REALTIME_FAST),
        ("CLOCK_REALTIME_COARSE", REALTIME_FAST),
        ("CLOCK_SECOND", SECOND),
        ("CLOCK_MONOTONIC", MONOTONIC),
        ("CLOCK_MONOTONIC_PRECISE", MONOTONIC_PRECISE),
        ("CLOCK_MONOTONIC_FAST", MONOTONIC_FAST),
        ("CLOCK_MONOTONIC_COARSE", MONOTONIC_FAST),
        ("CLOCK_UPTIME", UPTIME),
        ("CLOCK_UPTIME_PRECISE", UPTIME_PRECISE),
        ("CLOCK_UPTIME_FAST", UPTIME_FAST),
        ("CLOCK_BOOTTIME", UPTIME),
        ("CLOCK_PROCESS_CPUTIME_ID", PROCESS_CPUTIME),
        ("CLOCK_THREAD_CPUTIME_ID", THREAD_CPUTIME),
        ("CLOCK_VIRTUAL", VIRTUAL),
        ("CLOCK_PROF", PROF),
    ];

    /// Every clock a family holds from 15.0, where `CLOCK_BOOTTIME` spells `CLOCK_MONOTONIC` and
    /// the TAI clock exists.
    const FROM_15: &[(&str, clockid_t)] = &[
        ("CLOCK_REALTIME", REALTIME),
        ("CLOCK_REALTIME_PRECISE", REALTIME_PRECISE),
        ("CLOCK_REALTIME_FAST", REALTIME_FAST),
        ("CLOCK_REALTIME_COARSE", REALTIME_FAST),
        ("CLOCK_SECOND", SECOND),
        ("CLOCK_TAI", TAI),
        ("CLOCK_MONOTONIC", MONOTONIC),
        ("CLOCK_MONOTONIC_PRECISE", MONOTONIC_PRECISE),
        ("CLOCK_MONOTONIC_FAST", MONOTONIC_FAST),
        ("CLOCK_MONOTONIC_COARSE", MONOTONIC_FAST),
        ("CLOCK_BOOTTIME", MONOTONIC),
        ("CLOCK_UPTIME", UPTIME),
        ("CLOCK_UPTIME_PRECISE", UPTIME_PRECISE),
        ("CLOCK_UPTIME_FAST", UPTIME_FAST),
        ("CLOCK_PROCESS_CPUTIME_ID", PROCESS_CPUTIME),
        ("CLOCK_THREAD_CPUTIME_ID", THREAD_CPUTIME),
        ("CLOCK_VIRTUAL", VIRTUAL),
        ("CLOCK_PROF", PROF),
    ];

    /// The clocks a user may name as a timebase, one list per column of the family table.
    #[must_use]
    pub fn named() -> &'static [(&'static str, clockid_t)] {
        if counts_suspended() {
            FROM_15
        } else {
            THROUGH_14
        }
    }

    /// The family a clock id belongs to, or `None` when the id is left on genuine time.
    ///
    /// The names crossed over at 15.0: through 14.x nothing counts suspended time and
    /// `CLOCK_MONOTONIC` is the Monotonic family's; from 15.0 it counts suspended time and belongs
    /// to Boottime, with `CLOCK_UPTIME` taking its place. Ids from `clock_getcpuclockid` fall
    /// through: a reading about somebody else is a measurement, and the honest answer is the
    /// genuine one.
    #[must_use]
    pub fn family_of(clock: clockid_t) -> Option<Family> {
        let counting = counts_suspended();
        match clock {
            REALTIME | REALTIME_PRECISE | REALTIME_FAST | SECOND => Some(Family::Realtime),
            TAI if counting => Some(Family::Realtime),
            MONOTONIC | MONOTONIC_PRECISE | MONOTONIC_FAST => Some(if counting {
                Family::Boottime
            } else {
                Family::Monotonic
            }),
            UPTIME | UPTIME_PRECISE | UPTIME_FAST => Some(if counting {
                Family::Monotonic
            } else {
                Family::Boottime
            }),
            PROCESS_CPUTIME | THREAD_CPUTIME | VIRTUAL | PROF => Some(Family::Cpu),
            _ => None,
        }
    }

    /// Whether this clock reads the Realtime line plus the TAI offset.
    #[must_use]
    pub fn is_tai(clock: clockid_t) -> bool {
        clock == TAI && counts_suspended()
    }
}

/// The first clock in the family's column of the family table.
///
/// A family's zero always comes from it, and with no override it is the timebase too. Cpu has
/// none, because each of its clocks tracks passage through its own reading. Which column applies
/// is the running kernel's to say; see [`counts_suspended`].
#[must_use]
pub fn default_timebase(family: Family) -> Option<libc::clockid_t> {
    let counting = counts_suspended();
    Some(match family {
        Family::Realtime => clock::REALTIME,
        Family::Monotonic if counting => clock::UPTIME,
        Family::Monotonic => clock::MONOTONIC,
        Family::Boottime if counting => clock::MONOTONIC,
        Family::Boottime => clock::UPTIME,
        Family::Cpu => return None,
    })
}

/// `__FreeBSD_version` of the first release where `CLOCK_MONOTONIC` and `CLOCK_BOOTTIME` count
/// suspended time and `CLOCK_UPTIME` is the one that stops: 15.0.
const CROSSOVER: i32 = 1_500_000;

/// The kernel's `__FreeBSD_version`, or 0 before anyone has asked and -1 for a kernel that would
/// not say.
static OSRELDATE: AtomicI32 = AtomicI32::new(0);

/// Whether the running kernel counts suspended time in `CLOCK_MONOTONIC` and `CLOCK_BOOTTIME`,
/// which is what decides between the two FreeBSD columns of the family table.
///
/// Asked of the kernel once and remembered, because a clock read reaches this. A kernel that will
/// not answer is read as the older layout, which every kernel before 15.0 has.
#[must_use]
pub fn counts_suspended() -> bool {
    osreldate() >= CROSSOVER
}

/// `kern.osreldate`, read once per process.
///
/// Relaxed throughout: two threads racing here both ask the kernel and store the same answer, and
/// nothing else is published alongside it.
fn osreldate() -> i32 {
    let known = OSRELDATE.load(Ordering::Relaxed);
    if known != 0 {
        return known;
    }
    let mut mib = [libc::CTL_KERN, libc::KERN_OSRELDATE];
    let mut value: libc::c_int = 0;
    let mut len = size_of::<libc::c_int>();
    // Safety: `mib` names a two-element sysctl, and `value` and `len` are live words of the types
    // that name says it answers in.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&raw mut value).cast::<libc::c_void>(),
            &raw mut len,
            std::ptr::null(),
            0,
        )
    };
    let answer = if read == 0 && value > 0 { value } else { -1 };
    OSRELDATE.store(answer, Ordering::Relaxed);
    answer
}

/// The id the kernel gives this thread.
///
/// What names a thread where thread-local storage cannot: a runtime that makes its own threads
/// without libc leaves the thread pointer holding something of its own.
#[must_use]
pub fn thread_id() -> u64 {
    let mut id: libc::c_long = 0;
    // Safety: `thr_self` writes this thread's own id into a live word and reads nothing.
    if unsafe { libc::thr_self(&raw mut id) } != 0 {
        return 0;
    }
    u64::try_from(id).unwrap_or(0)
}

/// Sets this thread's `errno`, which is how a refused call reports itself in libc's convention.
pub fn set_errno(value: libc::c_int) {
    // Safety: `__error` answers this thread's own `errno`, taking nothing.
    let location = unsafe { libc::__error() };
    // Safety: the location libc just named is live for as long as this thread is.
    unsafe { *location = value };
}

/// How many arguments a FreeBSD system call takes at most.
const SYSCALL_ARGS: usize = 6;

/// The system call numbers this arm uses, from `sys/syscall.h`. The libc crate spells none of them
/// for this platform.
mod sys {
    /// `getrusage(2)`.
    pub(super) const GETRUSAGE: i64 = 117;
    /// `settimeofday(2)`.
    pub(super) const SETTIMEOFDAY: i64 = 122;
    /// `adjtime(2)`.
    pub(super) const ADJTIME: i64 = 140;
    /// `clock_gettime(2)`.
    pub(super) const CLOCK_GETTIME: i64 = 232;
    /// `clock_settime(2)`.
    pub(super) const CLOCK_SETTIME: i64 = 233;
}

/// A pointer as a system call argument. `*mut` coerces, so one spelling serves both directions.
fn as_arg<T>(ptr: *const T) -> libc::c_long {
    libc::c_long::try_from(ptr.addr().cast_signed()).unwrap_or(0)
}

/// What the kernel answered: the value, and whether the call was refused.
///
/// FreeBSD reports a refusal in the carry flag and puts the error number where the value would go,
/// rather than returning the negative of it the way Linux does, so the two come back together.
struct Answer {
    value: libc::c_long,
    refused: bool,
}

/// Makes a system call, returning exactly what the kernel returned.
///
/// The instruction rather than libc's, because the agent exports these names too: a call through
/// libc from inside the library would come back to our own interposer. It takes no lock, allocates
/// nothing, and touches no shared state, so it is async-signal-safe wherever the program is.
///
/// # Safety
/// The number and the arguments must be a call the kernel will accept, with every pointer among
/// them valid for what that call does with it.
#[inline]
unsafe fn raw_syscall(number: libc::c_long, args: [libc::c_long; SYSCALL_ARGS]) -> Answer {
    let [a0, a1, a2, a3, a4, a5] = args;
    let value: libc::c_long;
    let carry: u8;
    #[cfg(target_arch = "x86_64")]
    // Safety: the caller guarantees the call. The kernel takes the number in `rax` and the
    // arguments in these six registers, answers in `rax` with the carry flag set on a refusal, and
    // clobbers `rdx`, `rcx` and `r11`. `setc` reads the flag before anything else can move it.
    unsafe {
        std::arch::asm!(
            "syscall",
            "setc {carry}",
            carry = lateout(reg_byte) carry,
            inlateout("rax") number => value,
            in("rdi") a0,
            in("rsi") a1,
            inlateout("rdx") a2 => _,
            in("r10") a3,
            in("r8") a4,
            in("r9") a5,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    #[cfg(target_arch = "aarch64")]
    // Safety: as above. The kernel takes the number in `x8` and the arguments in `x0` through
    // `x5`, answers in `x0` with the carry flag set on a refusal, and clobbers `x1`.
    unsafe {
        std::arch::asm!(
            "svc 0",
            "cset {carry:w}, cs",
            carry = lateout(reg) carry,
            in("x8") number,
            inlateout("x0") a0 => value,
            inlateout("x1") a1 => _,
            in("x2") a2,
            in("x3") a3,
            in("x4") a4,
            in("x5") a5,
            options(nostack),
        );
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    compile_error!(
        "libtimewarp supports x86_64 and aarch64. Add the system call instruction of the \
         architecture to raw_syscall in common/src/platform/freebsd.rs."
    );
    Answer {
        value,
        refused: carry != 0,
    }
}

/// Makes a system call the way libc's own stubs do: the kernel's answer, or -1 with `errno` set to
/// what it refused with.
///
/// # Safety
/// As [`raw_syscall`]: the number and the arguments must be a call the kernel will accept.
#[must_use]
#[inline]
unsafe fn genuine_syscall(
    number: libc::c_long,
    args: [libc::c_long; SYSCALL_ARGS],
) -> libc::c_long {
    // Safety: the caller guarantees the call.
    let answered = unsafe { raw_syscall(number, args) };
    if answered.refused {
        set_errno(libc::c_int::try_from(answered.value).unwrap_or(libc::EINVAL));
        return -1;
    }
    answered.value
}

/// libc's `clock_gettime`, the definition past ours, or null until [`prime_genuine`] has found it.
static LIBC_CLOCK: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());

/// What libc's `clock_gettime` is called through.
type LibcClockGettime = unsafe extern "C" fn(libc::clockid_t, *mut libc::timespec) -> libc::c_int;

/// Finds libc's `clock_gettime`, and ties it to the kernel's own timekeep page before the auxiliary
/// vector entry naming that page is withheld.
///
/// libc computes the time from the page the auxiliary vector names, taking its copy of that entry
/// the first time anybody asks and keeping the page it found for good. The read below is that
/// first time wherever nothing earlier was, so a session that withholds the entry afterwards leaves
/// libc on the kernel's page. Until this has run, and in a process that never runs it, the clock
/// is read through the system call.
///
/// A lookup takes the loader's lock, which is why this belongs to the constructor rather than to
/// the first read.
pub fn prime_genuine() {
    // Safety: the name is NUL-terminated.
    let Some(found) = (unsafe { super::genuine_symbol(b"clock_gettime\0") }) else {
        return;
    };
    // Safety: libc declares this name with this signature.
    let read =
        unsafe { std::mem::transmute::<*mut libc::c_void, LibcClockGettime>(found.as_ptr()) };
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Safety: `ts` is a live, correctly typed buffer.
    let _read = unsafe { read(clock::REALTIME, &raw mut ts) };
    LIBC_CLOCK.store(found.as_ptr(), Ordering::Relaxed);
}

/// Reads a genuine clock, libc's convention.
///
/// Through libc once [`prime_genuine`] has found it, which reads the kernel's timekeep page without
/// entering the kernel and makes the system call itself where the page cannot answer. Through the
/// system call before then, and for a null `ts`, which the system call refuses with `EFAULT` where
/// libc would write through it.
///
/// Relaxed, for the reason a genuine function's slot is: the answer names a function in a library
/// mapped before this process ran a line, and reaching its code is an address dependency.
///
/// # Safety
/// `ts` must be null or valid for writes of a `timespec`.
#[inline]
pub unsafe fn genuine_clock_gettime(
    clock: libc::clockid_t,
    ts: *mut libc::timespec,
) -> libc::c_int {
    let found = LIBC_CLOCK.load(Ordering::Relaxed);
    if !ts.is_null() && !found.is_null() {
        // Safety: the slot only ever holds libc's `clock_gettime`, which has this signature.
        let read = unsafe { std::mem::transmute::<*mut libc::c_void, LibcClockGettime>(found) };
        // Safety: the caller guarantees `ts`, and libc takes the clock id as it comes.
        return unsafe { read(clock, ts) };
    }
    // Safety: the caller guarantees `ts`, and the kernel takes the clock id as it comes.
    let result = unsafe {
        genuine_syscall(
            sys::CLOCK_GETTIME,
            [libc::c_long::from(clock), as_arg(ts), 0, 0, 0, 0],
        )
    };
    // The syscall answers 0 or -1, so the narrowing never changes what the caller sees.
    libc::c_int::try_from(result).unwrap_or(-1)
}

/// How many clock ticks a second holds in the units `times(2)` counts in.
///
/// FreeBSD fixes `CLK_TCK` at 128, and `sysconf(_SC_CLK_TCK)` answers that same constant, so the
/// scaling done here and the scaling done above the seam agree.
const TICKS_PER_SECOND: i64 = 128;

/// A `timeval` of consumed CPU time as a count of ticks, the way libc's own `times` counts it.
fn ticks_of(time: &libc::timeval) -> libc::clock_t {
    let ticks = time
        .tv_sec
        .saturating_mul(TICKS_PER_SECOND)
        .saturating_add(time.tv_usec / (1_000_000 / TICKS_PER_SECOND));
    libc::clock_t::try_from(ticks).unwrap_or(libc::clock_t::MAX)
}

/// Reads the genuine process times: the fields of `buf`, and the elapsed ticks `times(2)` answers,
/// or -1 when the kernel refused. libc's convention, and `buf` may be null.
///
/// FreeBSD has no `times` system call. libc builds the function out of two `getrusage` calls and
/// one clock read, and so does this, with `getrusage` through the raw call because the agent
/// exports that name.
///
/// # Safety
/// `buf` must be null or valid for writes of a `tms`.
#[inline]
pub unsafe fn genuine_times(buf: *mut libc::tms) -> libc::clock_t {
    if !buf.is_null() {
        // Safety: `rusage` is plain data, so an all-zero value is a valid one.
        let mut mine: libc::rusage = unsafe { std::mem::zeroed() };
        // Safety: the same, for the children's usage.
        let mut theirs: libc::rusage = unsafe { std::mem::zeroed() };
        // Safety: `mine` is a live, correctly typed buffer.
        if unsafe { genuine_getrusage(libc::RUSAGE_SELF, &raw mut mine) } != 0 {
            return -1;
        }
        // Safety: `theirs` is a live, correctly typed buffer.
        if unsafe { genuine_getrusage(libc::RUSAGE_CHILDREN, &raw mut theirs) } != 0 {
            return -1;
        }
        // Safety: `buf` is non-null, so the caller's guarantee makes it valid for this write.
        unsafe {
            *buf = libc::tms {
                tms_utime: ticks_of(&mine.ru_utime),
                tms_stime: ticks_of(&mine.ru_stime),
                tms_cutime: ticks_of(&theirs.ru_utime),
                tms_cstime: ticks_of(&theirs.ru_stime),
            };
        }
    }
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Safety: `ts` is a live, correctly typed buffer.
    if unsafe { genuine_clock_gettime(clock::MONOTONIC, &raw mut ts) } != 0 {
        return -1;
    }
    let ticks = ts
        .tv_sec
        .saturating_mul(TICKS_PER_SECOND)
        .saturating_add(ts.tv_nsec / (1_000_000_000 / TICKS_PER_SECOND));
    libc::clock_t::try_from(ticks).unwrap_or(libc::clock_t::MAX)
}

/// Reads the genuine resource usage of `who` into `usage`. libc's convention.
///
/// Through the raw system call, because the agent exports `getrusage`.
///
/// # Safety
/// `usage` must be valid for writes of a `rusage`.
#[inline]
pub unsafe fn genuine_getrusage(who: libc::c_int, usage: *mut libc::rusage) -> libc::c_int {
    // Safety: the caller guarantees `usage`, and the kernel takes `who` as it comes.
    let result = unsafe {
        genuine_syscall(
            sys::GETRUSAGE,
            [libc::c_long::from(who), as_arg(usage), 0, 0, 0, 0],
        )
    };
    libc::c_int::try_from(result).unwrap_or(-1)
}

/// Sets the genuine wall clock, libc's convention, for a process this library is not faking.
///
/// A null `tv` leaves the clock where it is and a null `tz` leaves the obsolete timezone alone,
/// which is what the kernel makes of each, so both pass through as the program wrote them.
///
/// # Safety
/// `tv` must be null or valid for reads of a `timeval`, and `tz` null or valid for reads of a
/// `timezone`.
#[must_use]
pub unsafe fn genuine_settimeofday(
    tv: *const libc::timeval,
    tz: *const libc::c_void,
) -> libc::c_int {
    // Safety: the caller guarantees both pointers, which the kernel reads and no more.
    let result =
        unsafe { genuine_syscall(sys::SETTIMEOFDAY, [as_arg(tv), as_arg(tz), 0, 0, 0, 0]) };
    libc::c_int::try_from(result).unwrap_or(-1)
}

/// Sets a genuine clock, libc's convention, for a process this library is not faking.
///
/// # Safety
/// `tp` must be null or valid for reads of a `timespec`.
#[must_use]
pub unsafe fn genuine_clock_settime(
    clock: libc::clockid_t,
    tp: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller guarantees `tp`, and the kernel takes the clock id as it comes.
    let result = unsafe {
        genuine_syscall(
            sys::CLOCK_SETTIME,
            [libc::c_long::from(clock), as_arg(tp), 0, 0, 0, 0],
        )
    };
    libc::c_int::try_from(result).unwrap_or(-1)
}

/// Makes the genuine one-shot offset adjustment, libc's convention, for a process this library is
/// not faking.
///
/// # Safety
/// `delta` must be null or valid for reads of a `timeval`, and `olddelta` null or valid for writes
/// of one.
#[must_use]
pub unsafe fn genuine_adjtime(
    delta: *const libc::timeval,
    olddelta: *mut libc::timeval,
) -> libc::c_int {
    // Safety: the caller guarantees both pointers, which the kernel reads and writes and no more.
    let result =
        unsafe { genuine_syscall(sys::ADJTIME, [as_arg(delta), as_arg(olddelta), 0, 0, 0, 0]) };
    libc::c_int::try_from(result).unwrap_or(-1)
}

/// The widest delta the platform's own `adjtime` takes before it refuses, in seconds.
///
/// `adjtime` is a system call of its own here, taking the `timeval` as it comes, so the bound is
/// the range an instant holds rather than anything libc imposes.
pub const ADJTIME_SECONDS: i64 = i64::MAX / 1_000_000_000;

/// The order the kernel checks a set of the clock in.
///
/// It copies the value in, then asks for privilege, and only then looks at the clock id and at the
/// value. A process without privilege hears `EPERM` for any set it can name, a clock that cannot be
/// set included.
pub const SET_ORDER: [super::SetCheck; 4] = [
    super::SetCheck::Pointer,
    super::SetCheck::Privilege,
    super::SetCheck::Clock,
    super::SetCheck::Value,
];

/// Whether `adjtime` with no delta is a question that needs no privilege: it is, and the kernel
/// answers it before it asks for any.
pub const ADJTIME_NULL_ASKS: bool = true;

/// The units `clock` answers in: `CLOCKS_PER_SEC`, which this platform's headers fix at 128 rather
/// than at the million an XSI system names, and which libc's own `clock` counts in.
pub const CLOCKS_PER_SEC: i64 = 128;

/// Whether the kernel would let this process set the clock: an effective uid of 0 here, since
/// FreeBSD gates the call on the superuser rather than on a capability.
#[must_use]
pub fn may_set_the_clock() -> bool {
    // Safety: `geteuid` reads process state and cannot fail.
    let euid = unsafe { libc::geteuid() };
    euid == 0
}

/// Takes the write lock on `fd`: an exclusive `flock` over the whole file, blocking until the
/// current holder releases it. libc's convention, so a caller retries `EINTR` itself.
///
/// `flock` rather than the `fcntl` locks Linux and macOS use, because FreeBSD has no
/// description-owned `fcntl` lock and these are description-owned. The kernel keeps both kinds in
/// one table per file, so [`probe_write_lock`] sees this holder.
///
/// # Safety
/// `fd` must be an open descriptor on the lock file, opened for writing.
#[must_use]
pub unsafe fn take_write_lock(fd: libc::c_int) -> libc::c_int {
    // Safety: the caller guarantees `fd`.
    unsafe { libc::flock(fd, libc::LOCK_EX) }
}

/// Asks the kernel whether the write lock on `fd` is held, taking nothing. Fills `request`, which
/// the caller has zeroed and filled with the question.
///
/// The answer covers the `flock` holder as well as any `fcntl` one, because the kernel keeps the
/// two in one table per file.
///
/// # Safety
/// `fd` must be an open descriptor on the lock file and `request` a live `flock`.
#[must_use]
pub unsafe fn probe_write_lock(fd: libc::c_int, request: *mut libc::flock) -> libc::c_int {
    // Safety: the caller guarantees both arguments.
    unsafe { libc::fcntl(fd, libc::F_GETLK, request) }
}

/// The auxiliary vector entry naming where the kernel mapped the timekeep page. The libc crate
/// spells none of this platform's tags, so it is spelled here.
pub const AT_TIMEKEEP: libc::c_int = 22;

/// Fills `buf` with randomness from the kernel.
///
/// # Errors
/// Whatever `getrandom(2)` refused with, other than an interruption, which is retried.
pub fn random_bytes(buf: &mut [u8]) -> io::Result<()> {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..).filter(|rest| !rest.is_empty()) {
        // Safety: the pointer and length name the unfilled tail of a live buffer.
        let got =
            unsafe { libc::getrandom(rest.as_mut_ptr().cast::<libc::c_void>(), rest.len(), 0) };
        if got < 0 {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
            continue;
        }
        filled += usize::try_from(got).unwrap_or(0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Nanos;

    /// The two columns of the family table, as the kernel's era picks between them.
    fn column(counting: bool) -> Vec<(libc::clockid_t, Family)> {
        let elapsed = |monotonic, boottime| if counting { boottime } else { monotonic };
        vec![
            (clock::REALTIME, Family::Realtime),
            (clock::REALTIME_PRECISE, Family::Realtime),
            (clock::REALTIME_FAST, Family::Realtime),
            (clock::SECOND, Family::Realtime),
            (
                clock::MONOTONIC,
                elapsed(Family::Monotonic, Family::Boottime),
            ),
            (
                clock::MONOTONIC_PRECISE,
                elapsed(Family::Monotonic, Family::Boottime),
            ),
            (
                clock::MONOTONIC_FAST,
                elapsed(Family::Monotonic, Family::Boottime),
            ),
            (clock::UPTIME, elapsed(Family::Boottime, Family::Monotonic)),
            (
                clock::UPTIME_PRECISE,
                elapsed(Family::Boottime, Family::Monotonic),
            ),
            (
                clock::UPTIME_FAST,
                elapsed(Family::Boottime, Family::Monotonic),
            ),
            (clock::PROCESS_CPUTIME, Family::Cpu),
            (clock::THREAD_CPUTIME, Family::Cpu),
            (clock::VIRTUAL, Family::Cpu),
            (clock::PROF, Family::Cpu),
        ]
    }

    #[test]
    fn the_family_table_places_every_clock_of_this_kernels_column() {
        for (id, family) in column(counts_suspended()) {
            assert_eq!(clock::family_of(id), Some(family), "clock {id}");
        }
    }

    #[test]
    fn the_two_columns_swap_the_names_that_crossed_over_at_15() {
        let through_14 = column(false);
        let from_15 = column(true);
        // The wall clocks and the Cpu clocks sit where they are in both.
        for (id, family) in [
            (clock::REALTIME, Family::Realtime),
            (clock::SECOND, Family::Realtime),
            (clock::VIRTUAL, Family::Cpu),
            (clock::PROF, Family::Cpu),
        ] {
            assert!(through_14.contains(&(id, family)));
            assert!(from_15.contains(&(id, family)));
        }
        // The two that crossed over swapped families, in both directions.
        assert!(through_14.contains(&(clock::MONOTONIC, Family::Monotonic)));
        assert!(from_15.contains(&(clock::MONOTONIC, Family::Boottime)));
        assert!(through_14.contains(&(clock::UPTIME, Family::Boottime)));
        assert!(from_15.contains(&(clock::UPTIME, Family::Monotonic)));
    }

    #[test]
    fn the_kernel_says_which_column_applies() {
        let release = osreldate();
        assert!(release > 1_000_000, "kern.osreldate answered {release}");
        assert_eq!(counts_suspended(), release >= CROSSOVER);
        // And the answer is remembered rather than asked again.
        assert_eq!(OSRELDATE.load(Ordering::Relaxed), release);
    }

    #[test]
    fn a_family_defaults_to_the_first_clock_of_its_column() {
        assert_eq!(default_timebase(Family::Realtime), Some(clock::REALTIME));
        let monotonic = default_timebase(Family::Monotonic).unwrap();
        let boottime = default_timebase(Family::Boottime).unwrap();
        assert_eq!(clock::family_of(monotonic), Some(Family::Monotonic));
        assert_eq!(clock::family_of(boottime), Some(Family::Boottime));
        assert_ne!(monotonic, boottime);
        // Cpu tracks passage through each clock's own reading, so it has none.
        assert_eq!(default_timebase(Family::Cpu), None);
    }

    #[test]
    fn the_names_a_user_may_write_are_this_kernels_own() {
        let named = clock::named();
        let boottime = named
            .iter()
            .find(|(name, _)| *name == "CLOCK_BOOTTIME")
            .map(|&(_, id)| id);
        let tai = named.iter().any(|(name, _)| *name == "CLOCK_TAI");
        if counts_suspended() {
            assert_eq!(boottime, Some(clock::MONOTONIC));
            assert!(tai, "15.0 has a TAI clock");
        } else {
            assert_eq!(boottime, Some(clock::UPTIME));
            assert!(!tai, "before 15.0 there is no TAI clock");
        }
        // The FreeBSD-only Cpu clocks are nameable in both.
        for name in ["CLOCK_VIRTUAL", "CLOCK_PROF"] {
            assert!(named.iter().any(|(known, _)| *known == name), "{name}");
        }
    }

    #[test]
    fn the_tai_clock_carries_the_offset_only_where_it_exists() {
        assert_eq!(clock::is_tai(clock::TAI), counts_suspended());
        assert!(!clock::is_tai(clock::REALTIME));
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
    fn a_refused_call_reports_itself_the_way_libc_does() {
        // Safety: an unknown clock id is refused before the kernel reads the buffer.
        let refused = unsafe { genuine_clock_gettime(0x7fff_ffff, std::ptr::null_mut()) };
        assert_eq!(refused, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::EINVAL)
        );
    }

    #[test]
    fn the_genuine_clocks_read() {
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
            // Safety: as above, for every clock this kernel's column names.
            let read = unsafe { genuine_clock_gettime(id, &raw mut ts) };
            assert_eq!(read, 0, "{name}");
        }
    }

    /// The monotonic clock through the system call, which is what libc's reading is held against.
    fn monotonic_by_number() -> Nanos {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // Safety: `ts` is a live, correctly typed buffer.
        let read = unsafe {
            genuine_syscall(
                sys::CLOCK_GETTIME,
                [
                    libc::c_long::from(clock::MONOTONIC),
                    as_arg(&raw mut ts),
                    0,
                    0,
                    0,
                    0,
                ],
            )
        };
        assert_eq!(read, 0);
        Nanos::from_timespec(&ts)
    }

    #[test]
    fn once_primed_the_genuine_clock_reads_through_libc() {
        prime_genuine();
        assert!(!LIBC_CLOCK.load(Ordering::Relaxed).is_null());

        let before = monotonic_by_number();
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // Safety: `ts` is a live, correctly typed buffer.
        let answered = unsafe { genuine_clock_gettime(clock::MONOTONIC, &raw mut ts) };
        let after = monotonic_by_number();
        assert_eq!(answered, 0);
        let read = Nanos::from_timespec(&ts);
        assert!(
            before <= read && read <= after,
            "libc read {read:?}, outside the system calls around it at {before:?} and {after:?}"
        );

        // Safety: as above; an unknown clock id is refused before anything is written.
        let refused = unsafe { genuine_clock_gettime(0x7fff_ffff, &raw mut ts) };
        assert_eq!(refused, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::EINVAL)
        );
    }

    #[test]
    fn the_process_times_come_out_of_the_calls_the_platform_has() {
        // Safety: `tms` is plain data, so an all-zero value is a valid one.
        let mut tms: libc::tms = unsafe { std::mem::zeroed() };
        // Safety: `tms` is a live, correctly typed buffer.
        let elapsed = unsafe { genuine_times(&raw mut tms) };
        assert!(elapsed > 0, "times answered {elapsed}");
        assert!(tms.tms_utime >= 0 && tms.tms_stime >= 0);
        // A null buffer is allowed, and answers the elapsed ticks all the same.
        // Safety: a null buffer leaves the fields unwritten.
        assert!(unsafe { genuine_times(std::ptr::null_mut()) } >= elapsed);
    }
}
