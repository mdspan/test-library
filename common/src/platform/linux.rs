//! Linux.

use std::io;

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
    pub const NAMED: &[(&str, clockid_t)] = &[
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

/// How many arguments a Linux system call takes at most.
pub const SYSCALL_ARGS: usize = 6;

/// The highest negative return the kernel uses for an error. Anything below it is a value.
const ERRNO_CEILING: libc::c_long = -4095;

/// A pointer as a system call argument.
fn as_arg<T>(ptr: *mut T) -> libc::c_long {
    libc::c_long::try_from(ptr.addr().cast_signed()).unwrap_or(0)
}

/// Makes a system call, returning exactly what the kernel returned: a value, or the negative of
/// what it refused with.
///
/// The instruction rather than libc's `syscall`, because the agent exports that symbol too: a call
/// to it from inside the library would come back to our own interposer. It takes no lock, allocates
/// nothing, and touches no shared state, so it is async-signal-safe wherever the program is.
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
    let result = unsafe { raw_syscall(number, args) };
    if (ERRNO_CEILING..0).contains(&result) {
        // Safety: `__errno_location` answers this thread's own `errno`, taking nothing.
        let errno = unsafe { libc::__errno_location() };
        // Safety: the location libc just named is live for as long as this thread is.
        unsafe { *errno = libc::c_int::try_from(-result).unwrap_or(libc::EINVAL) };
        return -1;
    }
    result
}

/// Reads a genuine clock through the raw syscall.
///
/// Not through libc: the agent exports `clock_gettime`, so calling it under `LD_PRELOAD` would
/// resolve back to our own interposer and recurse until the stack ran out. The genuine vDSO goes in
/// front of this later, for speed.
///
/// # Safety
/// `ts` must be valid for writes of a `timespec`.
#[inline]
pub unsafe fn genuine_clock_gettime(
    clock: libc::clockid_t,
    ts: *mut libc::timespec,
) -> libc::c_int {
    // Safety: the caller guarantees `ts`, and the kernel takes the clock id as it comes.
    let result = unsafe {
        genuine_syscall(
            libc::SYS_clock_gettime,
            [libc::c_long::from(clock), as_arg(ts), 0, 0, 0, 0],
        )
    };
    // The syscall answers 0 or -1, so the narrowing never changes what the caller sees.
    libc::c_int::try_from(result).unwrap_or(-1)
}

/// Reads the genuine process times: the fields of `buf`, and the elapsed ticks `times(2)` answers,
/// or -1 when the kernel refused. libc's convention, and `buf` may be null.
///
/// Through the raw syscall for the reason [`genuine_clock_gettime`] is: the agent exports `times`.
///
/// # Safety
/// `buf` must be null or valid for writes of a `tms`.
#[inline]
pub unsafe fn genuine_times(buf: *mut libc::tms) -> libc::clock_t {
    // Safety: the caller guarantees `buf`.
    let result = unsafe { genuine_syscall(libc::SYS_times, [as_arg(buf), 0, 0, 0, 0, 0]) };
    libc::clock_t::try_from(result).unwrap_or(-1)
}

/// Reads the genuine resource usage of `who` into `usage`. libc's convention.
///
/// Through the raw syscall for the reason [`genuine_clock_gettime`] is: the agent exports
/// `getrusage`.
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
