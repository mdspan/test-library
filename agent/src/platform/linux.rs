//! Linux: `LD_PRELOAD` puts us ahead of libc, so defining the name is the whole mechanism.
//!
//! Three of these names are glibc's own internal aliases, which a program reaches when libc was
//! built to call them, and one is `syscall()`, which is Linux's way of asking for a call by number.
//! The synthetic vDSO that catches Go lands here later.

use common::platform::{SYSCALL_ARGS, genuine_syscall};

use crate::interpose::{self, Timeb};
use crate::setter;

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn clock_gettime(
    clock: libc::clockid_t,
    ts: *mut libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::clock_gettime(clock, ts) }
}

/// glibc's internal alias of `clock_gettime`, which a program built against an older one calls.
///
/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn __clock_gettime(
    clock: libc::clockid_t,
    ts: *mut libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::clock_gettime(clock, ts) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn gettimeofday(
    tv: *mut libc::timeval,
    tz: *mut libc::c_void,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::gettimeofday(tv, tz) }
}

/// glibc's internal alias of `gettimeofday`.
///
/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn __gettimeofday(
    tv: *mut libc::timeval,
    tz: *mut libc::c_void,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::gettimeofday(tv, tz) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn time(t: *mut libc::time_t) -> libc::time_t {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::time(t) }
}

/// glibc's internal alias of `time`.
///
/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn __time(t: *mut libc::time_t) -> libc::time_t {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::time(t) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn timespec_get(
    ts: *mut libc::timespec,
    base: libc::c_int,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::timespec_get(ts, base) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn ftime(tp: *mut Timeb) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::ftime(tp) }
}

/// glibc's internal alias of `ftime`.
///
/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn __ftime(tp: *mut Timeb) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::ftime(tp) }
}

/// The CPU time this process has consumed, which takes no argument and so needs no contract.
#[unsafe(no_mangle)]
pub(super) extern "C" fn clock() -> libc::clock_t {
    interpose::clock()
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn times(buf: *mut libc::tms) -> libc::clock_t {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::times(buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn getrusage(
    who: libc::c_int,
    usage: *mut libc::rusage,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { interpose::getrusage(who, usage) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn settimeofday(
    tv: *const libc::timeval,
    tz: *const libc::c_void,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { setter::settimeofday(tv, tz) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn clock_settime(
    clock: libc::clockid_t,
    tp: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { setter::clock_settime(clock, tp) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn adjtime(
    delta: *const libc::timeval,
    olddelta: *mut libc::timeval,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { setter::adjtime(delta, olddelta) }
}

/// The number of `time(2)`, where the architecture has one. The generic system call table, which
/// aarch64 uses, has none: there a program asks for the wall clock by another number.
#[cfg(target_arch = "x86_64")]
const SYS_TIME: Option<libc::c_long> = Some(libc::SYS_time);
#[cfg(not(target_arch = "x86_64"))]
const SYS_TIME: Option<libc::c_long> = None;

/// A call asked for by number. The three numbers that read a clock are the interposed reads; every
/// other number reaches the kernel exactly as it was given, and answers exactly what it answered.
///
/// libc declares this variadic, and this takes the six arguments a Linux system call can have
/// instead, which is the same register layout a variadic call builds and the same one libc's own
/// `syscall` reads. A call that passed fewer arguments leaves the rest holding whatever the
/// registers held, which the kernel ignores for a number that does not use them.
///
/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn syscall(
    number: libc::c_long,
    arg0: libc::c_long,
    arg1: libc::c_long,
    arg2: libc::c_long,
    arg3: libc::c_long,
    arg4: libc::c_long,
    arg5: libc::c_long,
) -> libc::c_long {
    if number == libc::SYS_clock_gettime {
        // Safety: the program passes this number a clock id and a `timespec`.
        let result = unsafe { interpose::clock_gettime(as_clock(arg0), as_ptr(arg1)) };
        return libc::c_long::from(result);
    }
    if number == libc::SYS_gettimeofday {
        // Safety: the program passes this number a `timeval` and a `timezone`.
        let result = unsafe { interpose::gettimeofday(as_ptr(arg0), as_ptr(arg1)) };
        return libc::c_long::from(result);
    }
    if SYS_TIME == Some(number) {
        // Safety: the program passes this number a `time_t`.
        return unsafe { interpose::time(as_ptr(arg0)) };
    }
    // Safety: the number and the arguments are the program's own, passed on untouched.
    unsafe { genuine_syscall(number, [arg0, arg1, arg2, arg3, arg4, arg5]) }
}

/// The pointer a system call argument carries.
fn as_ptr<T>(arg: libc::c_long) -> *mut T {
    std::ptr::with_exposed_provenance_mut(usize::try_from(arg.cast_unsigned()).unwrap_or(0))
}

/// The clock id a system call argument carries: the low half of the register, which is what the
/// kernel reads it as.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the kernel takes this argument as an int, so a wider value loses the same half here \
              as it does there"
)]
const fn as_clock(arg: libc::c_long) -> libc::clockid_t {
    arg as libc::clockid_t
}

/// The six arguments above are the whole of what a system call takes, which the seam agrees with.
const _: () = assert!(SYSCALL_ARGS == 6);
