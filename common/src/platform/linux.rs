//! Linux.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::elf::Genuine;
use crate::time::Family;
use crate::time::ticks::Times;

/// What the header's platform field carries for Linux.
pub const OS_TAG: u32 = 1;

/// The clock ids of the families, by the names the family table gives them.
pub mod clock {
    use libc::clockid_t;

    use crate::time::Family;

    /// The wall clock.
    pub const REALTIME: clockid_t = libc::CLOCK_REALTIME;
    /// The wall clock read from the last tick, without a hardware counter.
    pub const REALTIME_COARSE: clockid_t = libc::CLOCK_REALTIME_COARSE;
    /// The wall clock a timer may wake the machine from suspend for.
    pub const REALTIME_ALARM: clockid_t = libc::CLOCK_REALTIME_ALARM;
    /// The wall clock without leap seconds.
    pub const TAI: clockid_t = libc::CLOCK_TAI;
    /// Time since boot, not counting suspend.
    pub const MONOTONIC: clockid_t = libc::CLOCK_MONOTONIC;
    /// The same, read from the last tick.
    pub const MONOTONIC_COARSE: clockid_t = libc::CLOCK_MONOTONIC_COARSE;
    /// The same, without the NTP slew.
    pub const MONOTONIC_RAW: clockid_t = libc::CLOCK_MONOTONIC_RAW;
    /// Time since boot, counting suspend.
    pub const BOOTTIME: clockid_t = libc::CLOCK_BOOTTIME;
    /// The same, which a timer may wake the machine from suspend for.
    pub const BOOTTIME_ALARM: clockid_t = libc::CLOCK_BOOTTIME_ALARM;
    /// CPU time consumed by the whole process.
    pub const PROCESS_CPUTIME: clockid_t = libc::CLOCK_PROCESS_CPUTIME_ID;
    /// CPU time consumed by the calling thread.
    pub const THREAD_CPUTIME: clockid_t = libc::CLOCK_THREAD_CPUTIME_ID;

    /// Every clock a family holds, by the name a user writes for a timebase.
    const NAMED: &[(&str, clockid_t)] = &[
        ("CLOCK_REALTIME", REALTIME),
        ("CLOCK_REALTIME_COARSE", REALTIME_COARSE),
        ("CLOCK_REALTIME_ALARM", REALTIME_ALARM),
        ("CLOCK_TAI", TAI),
        ("CLOCK_MONOTONIC", MONOTONIC),
        ("CLOCK_MONOTONIC_COARSE", MONOTONIC_COARSE),
        ("CLOCK_MONOTONIC_RAW", MONOTONIC_RAW),
        ("CLOCK_BOOTTIME", BOOTTIME),
        ("CLOCK_BOOTTIME_ALARM", BOOTTIME_ALARM),
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
    ///
    /// Everything Linux encodes below zero falls through: a dynamic clock names a hardware device
    /// that exists to measure true time, and an id from `clock_getcpuclockid` or
    /// `pthread_getcpuclockid` is a reading about somebody else.
    #[must_use]
    pub const fn family_of(clock: clockid_t) -> Option<Family> {
        match clock {
            REALTIME | REALTIME_COARSE | REALTIME_ALARM | TAI => Some(Family::Realtime),
            MONOTONIC | MONOTONIC_COARSE | MONOTONIC_RAW => Some(Family::Monotonic),
            BOOTTIME | BOOTTIME_ALARM => Some(Family::Boottime),
            PROCESS_CPUTIME | THREAD_CPUTIME => Some(Family::Cpu),
            _ => None,
        }
    }

    /// Whether this clock reads the Realtime line plus the TAI offset.
    #[must_use]
    pub const fn is_tai(clock: clockid_t) -> bool {
        clock == TAI
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

/// How many arguments a Linux system call takes at most.
pub const SYSCALL_ARGS: usize = 6;

/// The highest negative return the kernel uses for an error. Anything below it is a value.
const ERRNO_CEILING: libc::c_long = -4095;

/// A pointer as a system call argument. `*mut` coerces, so one spelling serves both directions.
fn as_arg<T>(ptr: *const T) -> libc::c_long {
    libc::c_long::try_from(ptr.addr().cast_signed()).unwrap_or(0)
}

/// Makes a system call, returning exactly what the kernel returned: a value, or the negative of
/// what it refused with.
///
/// The instruction rather than libc's `syscall`, because the interposer exports that symbol too: a
/// call to it from inside the interposer would come back to its own definition. It takes no lock,
/// allocates nothing, and touches no shared state, so it is async-signal-safe wherever the program
/// is.
///
/// # Safety
/// The number and the arguments must be a call the kernel will accept, with every pointer among
/// them valid for what that call does with it.
#[inline]
unsafe fn raw_syscall(number: libc::c_long, args: [libc::c_long; SYSCALL_ARGS]) -> libc::c_long {
    let [a0, a1, a2, a3, a4, a5] = args;
    let result: libc::c_long;
    #[cfg(target_arch = "x86_64")]
    // Safety: the caller guarantees the call. The kernel takes the number in `rax` and the
    // arguments in these six registers, answers in `rax`, and clobbers `rcx` and `r11`.
    unsafe {
        std::arch::asm!(
            "syscall",
            inlateout("rax") number => result,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
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
    // `x5`, and answers in `x0`.
    unsafe {
        std::arch::asm!(
            "svc 0",
            in("x8") number,
            inlateout("x0") a0 => result,
            in("x1") a1,
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
         architecture to raw_syscall in common/src/platform/linux.rs."
    );
    result
}

/// Makes a system call the way libc's own `syscall` does: the kernel's answer, or -1 with `errno`
/// set to what it refused with.
///
/// # Safety
/// As [`raw_syscall`]: the number and the arguments must be a call the kernel will accept.
#[must_use]
#[inline]
pub unsafe fn genuine_syscall(
    number: libc::c_long,
    args: [libc::c_long; SYSCALL_ARGS],
) -> libc::c_long {
    // Safety: the caller guarantees the call.
    in_libc_convention(unsafe { raw_syscall(number, args) })
}

/// The kernel's answer in libc's convention: a value as it stands, and a refusal as -1 with `errno`
/// set to it.
fn in_libc_convention(answered: libc::c_long) -> libc::c_long {
    if (ERRNO_CEILING..0).contains(&answered) {
        set_errno(libc::c_int::try_from(-answered).unwrap_or(libc::EINVAL));
        return -1;
    }
    answered
}

/// The id the kernel gives this thread.
///
/// What names a thread where thread-local storage cannot: a runtime that makes its own threads
/// without libc leaves the thread pointer holding something of its own.
///
/// The raw system call, because libc's wrapper reports a failure in `errno`, which is the storage
/// this exists to avoid. `gettid` cannot fail.
#[must_use]
pub fn thread_id() -> u64 {
    // Safety: `gettid` takes nothing, writes nothing, and answers this thread's own id.
    let id = unsafe { raw_syscall(libc::SYS_gettid, [0; SYSCALL_ARGS]) };
    u64::try_from(id).unwrap_or(0)
}

/// The size of the signal set the kernel's `rt_sigprocmask` takes, which is not libc's `sigset_t`.
#[cfg(target_arch = "aarch64")]
const KERNEL_SIGSET: libc::c_long = 8;

/// Runs `cold` with every signal this thread can block held back, and lets them through once it
/// has returned.
///
/// A Go runtime built without cgo keeps its goroutine pointer in x28 and its signal handler takes
/// the pointer from there, so a signal handled on a Go thread while x28 holds anything else starts
/// the handler on garbage. A synthetic vDSO entry runs on such a thread, and this is how it runs
/// code that may use x28: a hook, which is somebody else's code, and anything that calls into libc
/// or formats a log line. A signal held back is handled once the old mask is back, by which point
/// `cold` has put the register back as the calling convention requires.
///
/// `cold` runs as a function of its own, so the prologue that saves x28 before reusing it is held
/// too. Two raw system calls, for the reason [`thread_id`] makes one, with the kernel's own signal
/// set.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn hold_signals<R>(cold: impl FnOnce() -> R) -> R {
    let every: u64 = !0;
    let mut before: u64 = 0;
    // Safety: both sets are live words of the size named, and the kernel leaves `SIGKILL` and
    // `SIGSTOP` out of any mask it is handed.
    let held = unsafe {
        raw_syscall(
            libc::SYS_rt_sigprocmask,
            [
                libc::c_long::from(libc::SIG_BLOCK),
                as_arg(&raw const every),
                as_arg(&raw mut before),
                KERNEL_SIGSET,
                0,
                0,
            ],
        )
    } == 0;
    let answer = run_held(cold);
    if held {
        // Safety: `before` is the mask the kernel handed back above.
        let _restored = unsafe {
            raw_syscall(
                libc::SYS_rt_sigprocmask,
                [
                    libc::c_long::from(libc::SIG_SETMASK),
                    as_arg(&raw const before),
                    0,
                    KERNEL_SIGSET,
                    0,
                    0,
                ],
            )
        };
    }
    answer
}

/// Runs `cold` as it stands.
///
/// The Go runtime's signal handler on `x86_64` takes its goroutine pointer from thread-local
/// storage, which no code under an entry moves.
#[cfg(not(target_arch = "aarch64"))]
#[inline]
pub fn hold_signals<R>(cold: impl FnOnce() -> R) -> R {
    cold()
}

/// Calls `cold`, as a function of its own.
#[cfg(target_arch = "aarch64")]
#[inline(never)]
fn run_held<R>(cold: impl FnOnce() -> R) -> R {
    cold()
}

/// Sets this thread's `errno`, which is how a refused call reports itself in libc's convention.
pub fn set_errno(value: libc::c_int) {
    // Safety: `__errno_location` answers this thread's own `errno`, taking nothing.
    let location = unsafe { libc::__errno_location() };
    // Safety: the location libc just named is live for as long as this thread is.
    unsafe { *location = value };
}

/// The auxiliary vector entry naming where the kernel mapped the vDSO, which the libc crate spells
/// for Android alone.
pub const AT_SYSINFO_EHDR: libc::c_ulong = 33;

/// What the kernel's vDSO calls its `clock_gettime` on this architecture.
const VDSO_CLOCK_GETTIME: &[u8] = if cfg!(target_arch = "x86_64") {
    b"__vdso_clock_gettime"
} else {
    b"__kernel_clock_gettime"
};

/// Where the kernel's vDSO keeps its `clock_gettime`: [`UNSOUGHT`] before a lookup, [`ABSENT`]
/// after one that found nothing, and the function's address otherwise.
static VDSO_CLOCK: AtomicUsize = AtomicUsize::new(UNSOUGHT);

/// [`VDSO_CLOCK`] before anybody has looked.
const UNSOUGHT: usize = 0;

/// [`VDSO_CLOCK`] once a lookup found no function to call. No function sits at address one.
const ABSENT: usize = 1;

/// What the vDSO's `clock_gettime` is called through. It answers an `int`, which leaves the upper
/// half of the return register the callee's to leave as it likes.
type VdsoClockGettime = unsafe extern "C" fn(libc::clockid_t, *mut libc::timespec) -> libc::c_int;

/// Finds the kernel's vDSO clock now, while the auxiliary vector still names the kernel's object.
///
/// The interposer points that entry at a synthetic vDSO of its own once a session is installed. A
/// lookup made after that refuses the synthetic object's entries, which call back into us, and
/// settles on the system call for good, so the constructor looks first.
pub fn prime_genuine() {
    let _found = vdso_clock_gettime();
}

/// The kernel's vDSO `clock_gettime`, or `None` when this process has none to call.
///
/// Relaxed both ways, for the reason a genuine function's slot is: the vDSO was mapped before this
/// process ran a line, and two threads racing here find the same answer and store it twice. The
/// lookup reads memory alone, so it is async-signal-safe like the call it stands in front of.
fn vdso_clock_gettime() -> Option<VdsoClockGettime> {
    let mut address = VDSO_CLOCK.load(Ordering::Relaxed);
    if address == UNSOUGHT {
        address = find_vdso_clock_gettime().unwrap_or(ABSENT);
        VDSO_CLOCK.store(address, Ordering::Relaxed);
    }
    if address == ABSENT {
        return None;
    }
    // Safety: the address is a function the kernel's vDSO defines under this name, with this
    // signature, and the vDSO stays mapped for the life of the process.
    Some(unsafe {
        std::mem::transmute::<*const (), VdsoClockGettime>(std::ptr::with_exposed_provenance(
            address,
        ))
    })
}

/// Looks the kernel's `clock_gettime` up in the object the auxiliary vector names.
fn find_vdso_clock_gettime() -> Option<usize> {
    // Safety: `getauxval` takes a tag and answers a number, touching nothing of ours.
    let base = usize::try_from(unsafe { libc::getauxval(AT_SYSINFO_EHDR) }).ok()?;
    if base == 0 {
        return None;
    }
    // Safety: the address is the one the auxiliary vector carries for the vDSO.
    let object = unsafe { Genuine::parse(base) }?;
    object.own_address_of(VDSO_CLOCK_GETTIME)
}

/// Reads a genuine clock, libc's convention.
///
/// Through the kernel's vDSO, which answers without entering the kernel for every clock it keeps
/// and makes the system call itself for the rest. Never through libc: the interposer exports
/// `clock_gettime`, so calling it under `LD_PRELOAD` would come back to the interposer's own
/// definition.
///
/// # Safety
/// `ts` must be valid for writes of a `timespec`.
#[inline]
pub unsafe fn genuine_clock_gettime(
    clock: libc::clockid_t,
    ts: *mut libc::timespec,
) -> libc::c_int {
    // Safety: the caller guarantees `ts`.
    let answered = in_libc_convention(unsafe { kernel_clock_gettime(clock, ts) });
    // The kernel answers 0 or a refusal, so the narrowing never changes what the caller sees.
    libc::c_int::try_from(answered).unwrap_or(-1)
}

/// Reads a genuine clock the way the kernel's own vDSO answers: zero, or the negative of what it
/// refused with, with `errno` left alone.
///
/// The convention a vDSO entry owes its caller, which is not libc's. `errno` lives in thread-local
/// storage, and a runtime that calls the vDSO from a thread of its own making may have none set up
/// the way libc expects, so an entry that can be reached from there reports the refusal in its
/// answer instead of writing anywhere.
///
/// The vDSO where there is one. A null `ts` goes to the system call, which refuses it with
/// `EFAULT` where the vDSO would write through it.
///
/// # Safety
/// `ts` must be null or valid for writes of a `timespec`.
#[must_use]
#[inline]
pub unsafe fn kernel_clock_gettime(
    clock: libc::clockid_t,
    ts: *mut libc::timespec,
) -> libc::c_long {
    if !ts.is_null()
        && let Some(read) = vdso_clock_gettime()
    {
        // Safety: the caller guarantees `ts`, and the vDSO takes the clock id as it comes.
        return libc::c_long::from(unsafe { read(clock, ts) });
    }
    // Safety: the caller guarantees `ts`, and the kernel takes the clock id as it comes.
    unsafe {
        raw_syscall(
            libc::SYS_clock_gettime,
            [libc::c_long::from(clock), as_arg(ts), 0, 0, 0, 0],
        )
    }
}

/// Reads the genuine wall clock into a `timeval`, in the convention [`kernel_clock_gettime`]
/// answers in and for the same reason.
///
/// # Safety
/// `tv` must be null or valid for writes of a `timeval`, and `tz` null or valid for writes of a
/// `timezone`.
#[must_use]
#[inline]
pub unsafe fn kernel_gettimeofday(tv: *mut libc::timeval, tz: *mut libc::c_void) -> libc::c_long {
    // Safety: the caller guarantees both pointers, which the kernel writes and no more.
    unsafe { raw_syscall(libc::SYS_gettimeofday, [as_arg(tv), as_arg(tz), 0, 0, 0, 0]) }
}

/// Reads the genuine process times `times(2)` reports, or `None` with `errno` set when the kernel
/// refuses. The kernel's `clock_t` is 64 bits here, as wide as the counts.
///
/// Through the raw syscall, avoiding libc for the reason [`genuine_clock_gettime`] does: the
/// interposer exports `times`.
#[must_use]
#[inline]
pub fn genuine_times() -> Option<Times> {
    let mut tms = libc::tms {
        tms_utime: 0,
        tms_stime: 0,
        tms_cutime: 0,
        tms_cstime: 0,
    };
    // Safety: `tms` is a live, correctly typed buffer, which is all the kernel writes.
    let elapsed =
        unsafe { genuine_syscall(libc::SYS_times, [as_arg(&raw mut tms), 0, 0, 0, 0, 0]) };
    (elapsed != -1).then_some(Times {
        user: tms.tms_utime,
        system: tms.tms_stime,
        children_user: tms.tms_cutime,
        children_system: tms.tms_cstime,
        elapsed,
    })
}

/// Reads the genuine resource usage of `who` into `usage`. libc's convention.
///
/// Through the raw syscall, avoiding libc for the reason [`genuine_clock_gettime`] does: the
/// interposer exports `getrusage`.
///
/// # Safety
/// `usage` must be valid for writes of a `rusage`.
#[inline]
pub unsafe fn genuine_getrusage(who: libc::c_int, usage: *mut libc::rusage) -> libc::c_int {
    // Safety: the caller guarantees `usage`, and the kernel takes `who` as it comes.
    let result = unsafe {
        genuine_syscall(
            libc::SYS_getrusage,
            [libc::c_long::from(who), as_arg(usage), 0, 0, 0, 0],
        )
    };
    libc::c_int::try_from(result).unwrap_or(-1)
}

/// Sets the genuine wall clock, libc's convention, for a process this library is not faking.
///
/// The kernel's own call rather than libc's, which the library exports itself. A null `tv` leaves
/// the clock where it is and a null `tz` leaves the obsolete timezone alone, which is what the
/// kernel makes of each, so both pass through as the program wrote them.
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
        unsafe { genuine_syscall(libc::SYS_settimeofday, [as_arg(tv), as_arg(tz), 0, 0, 0, 0]) };
    // The syscall answers 0 or -1, so the narrowing never changes what the caller sees.
    libc::c_int::try_from(result).unwrap_or(-1)
}

/// Sets a genuine clock, libc's convention, for a process this library is not faking.
///
/// Through the raw syscall, avoiding libc for the reason [`genuine_clock_gettime`] does: the
/// interposer exports `clock_settime`.
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
            libc::SYS_clock_settime,
            [libc::c_long::from(clock), as_arg(tp), 0, 0, 0, 0],
        )
    };
    libc::c_int::try_from(result).unwrap_or(-1)
}

/// Makes the genuine one-shot offset adjustment, libc's convention, for a process this library is
/// not faking.
///
/// Linux has no `adjtime` system call, so this is glibc's construction of it: one `adjtimex` with
/// `ADJ_OFFSET_SINGLESHOT` for a delta, or `ADJ_OFFSET_SS_READ` for none, which reads the one-shot
/// adjustment without privilege and sets nothing. Either way the answer carries the adjustment
/// that was pending before, in microseconds.
///
/// # Safety
/// `delta` must be null or valid for reads of a `timeval`, and `olddelta` null or valid for writes
/// of one.
#[must_use]
pub unsafe fn genuine_adjtime(
    delta: *const libc::timeval,
    olddelta: *mut libc::timeval,
) -> libc::c_int {
    // Safety: `timex` is a plain-old-data struct, so an all-zero value is a valid one.
    let mut request: libc::timex = unsafe { std::mem::zeroed() };
    request.modes = libc::ADJ_OFFSET_SS_READ;
    if !delta.is_null() {
        // Safety: `delta` is non-null, so the caller's guarantee makes it valid for this read.
        let asked = unsafe { &*delta };
        // libc normalises the pair and measures the seconds against its own bound before it makes
        // the call, so a delta wider than the field takes is refused here rather than by the
        // kernel, which would call it a question of privilege instead.
        let seconds = asked
            .tv_sec
            .saturating_add(asked.tv_usec / MICROS_PER_SECOND);
        if !(-ADJTIME_SECONDS..=ADJTIME_SECONDS).contains(&seconds) {
            set_errno(libc::EINVAL);
            return -1;
        }
        request.modes = libc::ADJ_OFFSET_SINGLESHOT;
        request.offset = seconds
            .saturating_mul(MICROS_PER_SECOND)
            .saturating_add(asked.tv_usec % MICROS_PER_SECOND);
    }
    // Safety: `request` is a live `timex`, which is the whole of what this number reads.
    let state = unsafe {
        genuine_syscall(
            libc::SYS_adjtimex,
            [as_arg(&raw mut request), 0, 0, 0, 0, 0],
        )
    };
    // Unlike every other call here, a successful `adjtimex` answers the clock's state rather than
    // zero, so only a negative answer is a failure.
    if state < 0 {
        return -1;
    }
    if !olddelta.is_null() {
        // Safety: `olddelta` is non-null, so the caller's guarantee makes it valid for this write.
        unsafe {
            *olddelta = libc::timeval {
                tv_sec: request.offset / MICROS_PER_SECOND,
                tv_usec: request.offset % MICROS_PER_SECOND,
            };
        }
    }
    0
}

/// Microseconds in a second, the unit a one-shot `adjtimex` offset is counted in.
const MICROS_PER_SECOND: i64 = 1_000_000;

/// The widest delta libc's own `adjtime` takes here before it refuses with `EINVAL`, in seconds.
///
/// Linux has no `adjtime` system call, so libc folds the delta into the microseconds field of a
/// `timex`, whose room it measures as an `int`, and keeps two seconds of margin below that. The
/// bound is libc's rather than the kernel's, and it is what a program sees without us.
pub const ADJTIME_SECONDS: i64 = libc::c_int::MAX as i64 / 1_000_000 - 2;

/// The order the kernel checks a set of the clock in.
///
/// The clock id, then the pointer, then the value, and only then the capability. A malformed set is
/// `EINVAL` here whatever the caller holds.
pub const SET_ORDER: [super::SetCheck; 4] = [
    super::SetCheck::Clock,
    super::SetCheck::Pointer,
    super::SetCheck::Value,
    super::SetCheck::Privilege,
];

/// Whether `adjtime` with no delta is a question that needs no privilege: it is, since libc makes
/// it an `adjtimex` that sets nothing.
pub const ADJTIME_NULL_ASKS: bool = true;

/// The units `clock` answers in: the million `CLOCKS_PER_SEC` names on an XSI system.
pub const CLOCKS_PER_SEC: i64 = 1_000_000;

/// Whether the kernel would let this process set the clock, which only the kernel can answer.
///
/// `settimeofday` with neither a time nor a timezone runs the privilege check a set of the wall
/// clock runs, `CAP_SYS_TIME` in the initial user namespace and every security module's settime
/// hook, and with nothing to set it stops there. So a capability held in a user namespace of the
/// process's own is no privilege, as it is none to the kernel, and a seccomp filter that refuses
/// the call refuses the set. Nothing is written to `errno`.
#[must_use]
pub fn may_set_the_clock() -> bool {
    // Safety: with both pointers null the kernel reads nothing and writes nothing.
    let answer = unsafe { raw_syscall(libc::SYS_settimeofday, [0; SYSCALL_ARGS]) };
    answer == 0
}

/// Takes the write lock on `fd`: an exclusive open-file-description lock on byte 0, blocking until
/// the current holder releases it. libc's convention, so a caller retries `EINTR` itself.
///
/// # Safety
/// `fd` must be an open descriptor on the lock file, opened for writing.
#[must_use]
pub unsafe fn take_write_lock(fd: libc::c_int) -> libc::c_int {
    let request = crate::sync::lock_request();
    // Safety: the caller guarantees `fd`, and `request` is a live `flock`.
    unsafe { libc::fcntl(fd, libc::F_OFD_SETLKW, &raw const request) }
}

/// Asks the kernel whether the write lock on `fd` is held, taking nothing. Fills `request`, which
/// the caller has zeroed and filled with the question.
///
/// # Safety
/// `fd` must be an open descriptor on the lock file and `request` a live `flock`.
#[must_use]
pub unsafe fn probe_write_lock(fd: libc::c_int, request: *mut libc::flock) -> libc::c_int {
    // Safety: the caller guarantees both arguments.
    unsafe { libc::fcntl(fd, libc::F_OFD_GETLK, request) }
}

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

    /// The monotonic clock through the system call, which is what the vDSO's reading is held
    /// against.
    fn monotonic_by_number() -> Nanos {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // Safety: `ts` is a live, correctly typed buffer.
        let read = unsafe {
            raw_syscall(
                libc::SYS_clock_gettime,
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

    /// The signals this thread holds back, as the kernel's own eight-byte set.
    fn held() -> u64 {
        let mut mask: u64 = 0;
        // Safety: a null set changes nothing, and the old one lands in a live word of the size
        // named.
        let read = unsafe {
            raw_syscall(
                libc::SYS_rt_sigprocmask,
                [
                    libc::c_long::from(libc::SIG_BLOCK),
                    0,
                    as_arg(&raw mut mask),
                    8,
                    0,
                    0,
                ],
            )
        };
        assert_eq!(read, 0);
        mask
    }

    #[test]
    fn signals_are_held_while_the_cold_path_runs_and_let_through_after() {
        let before = held();
        let during = hold_signals(held);
        assert_eq!(held(), before, "the old mask is not back");
        if cfg!(target_arch = "aarch64") {
            // The standard signals, which are every one the Go runtime handles. An emulator may
            // keep a few real-time signals of its own back from the mask.
            let standard: u64 = (1 << 31) - 1;
            let unblockable = (1 << (libc::SIGKILL - 1)) | (1 << (libc::SIGSTOP - 1));
            assert_eq!(
                during & standard,
                standard & !unblockable,
                "not every signal was held"
            );
        } else {
            assert_eq!(during, before, "x86_64 has nothing to hold signals for");
        }
    }

    #[test]
    fn the_genuine_clock_reads_through_the_kernel_vdso() {
        prime_genuine();
        assert!(
            vdso_clock_gettime().is_some(),
            "no vDSO clock was found, so every genuine read is a system call"
        );

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
            "the vDSO read {read:?}, outside the system calls around it at {before:?} and {after:?}"
        );
    }

    #[test]
    fn a_refusal_through_the_vdso_answers_what_the_kernel_would() {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // Safety: `ts` is a live, correctly typed buffer, and an unknown clock id is refused.
        let refused = unsafe { kernel_clock_gettime(0x7fff_ffff, &raw mut ts) };
        assert_eq!(refused, -libc::c_long::from(libc::EINVAL));
        // Safety: as above.
        let refused = unsafe { genuine_clock_gettime(0x7fff_ffff, &raw mut ts) };
        assert_eq!(refused, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::EINVAL)
        );
    }

    #[cfg(target_env = "gnu")]
    #[test]
    fn a_null_delta_reads_the_adjustment_still_under_way_as_glibc_does() {
        let unset = libc::timeval {
            tv_sec: -1,
            tv_usec: -1,
        };
        let (mut ours, mut glibc) = (unset, unset);
        // Safety: a null delta sets nothing, and each answer goes to a live `timeval`.
        let answered = unsafe { genuine_adjtime(std::ptr::null(), &raw mut ours) };
        // Safety: as above, through glibc's own `adjtime`, which this test binary is not faking.
        let wanted = unsafe { libc::adjtime(std::ptr::null(), &raw mut glibc) };
        assert_eq!((answered, wanted), (0, 0));
        assert_eq!(
            (ours.tv_sec, ours.tv_usec),
            (glibc.tv_sec, glibc.tv_usec),
            "a null delta reported something other than the one-shot adjustment still under way"
        );
    }

    #[test]
    fn the_probe_agrees_with_the_capability_check_adjtimex_makes() {
        // A tick adjustment the kernel calls absurd: refused for the capability before the value
        // is looked at, and for the value after, so it sets nothing either way.
        // Safety: `timex` is a plain-old-data struct, so an all-zero value is a valid one.
        let mut absurd: libc::timex = unsafe { std::mem::zeroed() };
        absurd.modes = libc::ADJ_TICK;
        // Safety: `absurd` is a live `timex`, which is the whole of what this number reads.
        let answer =
            unsafe { raw_syscall(libc::SYS_adjtimex, [as_arg(&raw mut absurd), 0, 0, 0, 0, 0]) };
        assert_ne!(answer, 0, "the kernel took a tick of zero");
        assert_eq!(
            may_set_the_clock(),
            answer == -libc::c_long::from(libc::EINVAL),
            "settimeofday and adjtimex disagree about CAP_SYS_TIME (adjtimex answered {answer})"
        );
    }

    #[test]
    fn a_null_buffer_is_refused_rather_than_written_through() {
        // Safety: a null buffer goes to the system call, which refuses it without writing.
        let refused = unsafe { kernel_clock_gettime(clock::MONOTONIC, std::ptr::null_mut()) };
        assert_eq!(refused, -libc::c_long::from(libc::EFAULT));
    }
}
