//! Linux: `LD_PRELOAD` puts us ahead of libc, so defining the name is the whole mechanism.
//!
//! Four of these names are glibc's own internal aliases, which a program reaches when libc was
//! built to call them, and one is `syscall()`, which is Linux's way of asking for a call by number.
//! A Go runtime that skips libc is caught by the synthetic vDSO in [`crate::vdso`].

use std::ffi::c_char;

use common::platform::{SYSCALL_ARGS, genuine_syscall, set_errno};

use crate::interpose::{self, Timeb};
use crate::setter;
use crate::stamp::{self, Statx};
use crate::timer;
use crate::wait;

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
    if number == libc::SYS_nanosleep {
        // Safety: the program passes this number two `timespec`s.
        let result = unsafe { wait::nanosleep(as_ptr(arg0), as_ptr(arg1)) };
        return libc::c_long::from(result);
    }
    if number == libc::SYS_clock_nanosleep {
        // The kernel answers this number the negative of what refused it, which `genuine_syscall`
        // has already turned into -1 and an `errno`; libc's own function is the one that answers
        // the `errno` itself, so the two spellings part here.
        // Safety: the program passes this number a clock id, flags, and two `timespec`s.
        let refused = unsafe {
            wait::clock_nanosleep(as_clock(arg0), as_int(arg1), as_ptr(arg2), as_ptr(arg3))
        };
        if refused == 0 {
            return 0;
        }
        set_errno(refused);
        return -1;
    }
    // Safety: the number and the arguments are the program's own, passed on untouched.
    unsafe { genuine_syscall(number, [arg0, arg1, arg2, arg3, arg4, arg5]) }
}

/// The pointer a system call argument carries.
fn as_ptr<T>(arg: libc::c_long) -> *mut T {
    std::ptr::with_exposed_provenance_mut(usize::try_from(arg.cast_unsigned()).unwrap_or(0))
}

/// The `int` a system call argument carries: the low half of the register, which is what the
/// kernel reads it as.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the kernel takes this argument as an int, so a wider value loses the same half here \
              as it does there"
)]
const fn as_int(arg: libc::c_long) -> libc::c_int {
    arg as libc::c_int
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

// The sleeps and the timeouts. Each converts the duration or the deadline the program asked for
// and blocks in the genuine function; `wait` has the rules.

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn nanosleep(
    req: *const libc::timespec,
    rem: *mut libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::nanosleep(req, rem) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn clock_nanosleep(
    clock: libc::clockid_t,
    flags: libc::c_int,
    req: *const libc::timespec,
    rem: *mut libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::clock_nanosleep(clock, flags, req, rem) }
}

/// Whole seconds of sleep, which takes a count and so needs no contract.
#[unsafe(no_mangle)]
pub(super) extern "C" fn sleep(seconds: libc::c_uint) -> libc::c_uint {
    wait::sleep(seconds)
}

/// Microseconds of sleep, which takes a count and so needs no contract.
#[unsafe(no_mangle)]
pub(super) extern "C" fn usleep(micros: libc::useconds_t) -> libc::c_int {
    wait::usleep(micros)
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn thrd_sleep(
    duration: *const libc::timespec,
    remaining: *mut libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::thrd_sleep(duration, remaining) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn poll(
    fds: *mut libc::pollfd,
    nfds: libc::nfds_t,
    timeout: libc::c_int,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::poll(fds, nfds, timeout) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn ppoll(
    fds: *mut libc::pollfd,
    nfds: libc::nfds_t,
    timeout: *const libc::timespec,
    sigmask: *const libc::sigset_t,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::ppoll(fds, nfds, timeout, sigmask) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn select(
    nfds: libc::c_int,
    readfds: *mut libc::fd_set,
    writefds: *mut libc::fd_set,
    exceptfds: *mut libc::fd_set,
    timeout: *mut libc::timeval,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::select(nfds, readfds, writefds, exceptfds, timeout) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn pselect(
    nfds: libc::c_int,
    readfds: *mut libc::fd_set,
    writefds: *mut libc::fd_set,
    exceptfds: *mut libc::fd_set,
    timeout: *const libc::timespec,
    sigmask: *const libc::sigset_t,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::pselect(nfds, readfds, writefds, exceptfds, timeout, sigmask) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn epoll_wait(
    epfd: libc::c_int,
    events: *mut libc::epoll_event,
    maxevents: libc::c_int,
    timeout: libc::c_int,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::epoll_wait(epfd, events, maxevents, timeout) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn epoll_pwait(
    epfd: libc::c_int,
    events: *mut libc::epoll_event,
    maxevents: libc::c_int,
    timeout: libc::c_int,
    sigmask: *const libc::sigset_t,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::epoll_pwait(epfd, events, maxevents, timeout, sigmask) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn epoll_pwait2(
    epfd: libc::c_int,
    events: *mut libc::epoll_event,
    maxevents: libc::c_int,
    timeout: *const libc::timespec,
    sigmask: *const libc::sigset_t,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::epoll_pwait2(epfd, events, maxevents, timeout, sigmask) }
}

/// Reads no time: it learns which clock the variable's deadlines are read on.
///
/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn pthread_cond_init(
    cond: *mut libc::pthread_cond_t,
    attr: *const libc::pthread_condattr_t,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::pthread_cond_init(cond, attr) }
}

/// Reads no time either: it forgets what `pthread_cond_init` learned.
///
/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn pthread_cond_destroy(
    cond: *mut libc::pthread_cond_t,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::pthread_cond_destroy(cond) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn pthread_cond_timedwait(
    cond: *mut libc::pthread_cond_t,
    mutex: *mut libc::pthread_mutex_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::pthread_cond_timedwait(cond, mutex, abstime) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn pthread_cond_clockwait(
    cond: *mut libc::pthread_cond_t,
    mutex: *mut libc::pthread_mutex_t,
    clock: libc::clockid_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::pthread_cond_clockwait(cond, mutex, clock, abstime) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn pthread_mutex_timedlock(
    mutex: *mut libc::pthread_mutex_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::pthread_mutex_timedlock(mutex, abstime) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn pthread_rwlock_timedrdlock(
    rwlock: *mut libc::pthread_rwlock_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::pthread_rwlock_timedrdlock(rwlock, abstime) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn pthread_rwlock_timedwrlock(
    rwlock: *mut libc::pthread_rwlock_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::pthread_rwlock_timedwrlock(rwlock, abstime) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn pthread_timedjoin_np(
    thread: libc::pthread_t,
    retval: *mut *mut libc::c_void,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::pthread_timedjoin_np(thread, retval, abstime) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn sem_timedwait(
    sem: *mut libc::sem_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::sem_timedwait(sem, abstime) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn sem_clockwait(
    sem: *mut libc::sem_t,
    clock: libc::clockid_t,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::sem_clockwait(sem, clock, abstime) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn sigtimedwait(
    set: *const libc::sigset_t,
    info: *mut libc::siginfo_t,
    timeout: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::sigtimedwait(set, info, timeout) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn aio_suspend(
    list: *const *const libc::aiocb,
    nent: libc::c_int,
    timeout: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::aio_suspend(list, nent, timeout) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn recvmmsg(
    sockfd: libc::c_int,
    msgvec: *mut libc::mmsghdr,
    vlen: libc::c_uint,
    flags: libc::c_int,
    timeout: *mut libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::recvmmsg(sockfd, msgvec, vlen, flags, timeout) }
}

/// C11's own timed condition wait. Its variable and mutex are types we only ever pass along, so
/// they travel as the opaque pointers they are.
///
/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn cnd_timedwait(
    cond: *mut libc::c_void,
    mutex: *mut libc::c_void,
    deadline: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::cnd_timedwait(cond, mutex, deadline) }
}

/// C11's own timed lock, whose mutex is opaque for the reason `cnd_timedwait`'s is.
///
/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mtx_timedlock(
    mutex: *mut libc::c_void,
    deadline: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::mtx_timedlock(mutex, deadline) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mq_timedsend(
    mqdes: libc::mqd_t,
    msg: *const libc::c_char,
    len: libc::size_t,
    priority: libc::c_uint,
    abstime: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::mq_timedsend(mqdes, msg, len, priority, abstime) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mq_timedreceive(
    mqdes: libc::mqd_t,
    msg: *mut libc::c_char,
    len: libc::size_t,
    priority: *mut libc::c_uint,
    abstime: *const libc::timespec,
) -> libc::ssize_t {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::mq_timedreceive(mqdes, msg, len, priority, abstime) }
}

// The timers. Each arm converts the duration or the expiry the program asked for, and each query
// converts the genuine remaining back into the program's own units; `timer` has the rules.

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn setitimer(
    which: libc::c_int,
    new_value: *const libc::itimerval,
    old_value: *mut libc::itimerval,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { timer::setitimer(which, new_value, old_value) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn getitimer(
    which: libc::c_int,
    curr_value: *mut libc::itimerval,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { timer::getitimer(which, curr_value) }
}

/// Whole seconds of wall-clock alarm, which takes a count and so needs no contract.
#[unsafe(no_mangle)]
pub(super) extern "C" fn alarm(seconds: libc::c_uint) -> libc::c_uint {
    timer::alarm(seconds)
}

/// Reads no time: it learns which clock the timer it creates arms against.
///
/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn timer_create(
    clock: libc::clockid_t,
    sevp: *mut libc::sigevent,
    timerid: *mut libc::timer_t,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { timer::timer_create(clock, sevp, timerid) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn timer_settime(
    timerid: libc::timer_t,
    flags: libc::c_int,
    new_value: *const libc::itimerspec,
    old_value: *mut libc::itimerspec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { timer::timer_settime(timerid, flags, new_value, old_value) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn timer_gettime(
    timerid: libc::timer_t,
    curr_value: *mut libc::itimerspec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { timer::timer_gettime(timerid, curr_value) }
}

/// Reads no time either: it forgets which clock the timer it deletes was created on.
#[unsafe(no_mangle)]
pub(super) extern "C" fn timer_delete(timerid: libc::timer_t) -> libc::c_int {
    timer::timer_delete(timerid)
}

/// Reads no time either: it learns which clock the timerfd it creates arms against.
#[unsafe(no_mangle)]
pub(super) extern "C" fn timerfd_create(clock: libc::clockid_t, flags: libc::c_int) -> libc::c_int {
    timer::timerfd_create(clock, flags)
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn timerfd_settime(
    fd: libc::c_int,
    flags: libc::c_int,
    new_value: *const libc::itimerspec,
    old_value: *mut libc::itimerspec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { timer::timerfd_settime(fd, flags, new_value, old_value) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn timerfd_gettime(
    fd: libc::c_int,
    curr_value: *mut libc::itimerspec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { timer::timerfd_gettime(fd, curr_value) }
}

// The file timestamps. A read converts the buffer the genuine call filled, and a set converts a
// copy of what the program asked for; `stamp` has the rules.

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn stat(path: *const c_char, buf: *mut libc::stat) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::stat(path, buf) }
}

/// The same buffer under the name a program built for large files calls it by.
///
/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn stat64(path: *const c_char, buf: *mut libc::stat64) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::stat64(path, buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn fstat(fd: libc::c_int, buf: *mut libc::stat) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::fstat(fd, buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn fstat64(fd: libc::c_int, buf: *mut libc::stat64) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::fstat64(fd, buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn lstat(path: *const c_char, buf: *mut libc::stat) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::lstat(path, buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn lstat64(
    path: *const c_char,
    buf: *mut libc::stat64,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::lstat64(path, buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn fstatat(
    dirfd: libc::c_int,
    path: *const c_char,
    buf: *mut libc::stat,
    flags: libc::c_int,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::fstatat(dirfd, path, buf, flags) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn fstatat64(
    dirfd: libc::c_int,
    path: *const c_char,
    buf: *mut libc::stat64,
    flags: libc::c_int,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::fstatat64(dirfd, path, buf, flags) }
}

/// Linux's own `stat`, the one call that carries a birth time.
///
/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn statx(
    dirfd: libc::c_int,
    path: *const c_char,
    flags: libc::c_int,
    mask: libc::c_uint,
    buf: *mut Statx,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::statx(dirfd, path, flags, mask, buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn utime(
    path: *const c_char,
    times: *const libc::utimbuf,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::utime(path, times) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn utimes(
    path: *const c_char,
    times: *const libc::timeval,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::utimes(path, times) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn futimes(
    fd: libc::c_int,
    times: *const libc::timeval,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::futimes(fd, times) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn lutimes(
    path: *const c_char,
    times: *const libc::timeval,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::lutimes(path, times) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn utimensat(
    dirfd: libc::c_int,
    path: *const c_char,
    times: *const libc::timespec,
    flags: libc::c_int,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::utimensat(dirfd, path, times, flags) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn futimens(
    fd: libc::c_int,
    times: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::futimens(fd, times) }
}

// The `__xstat` family, exported only by a build targeting a glibc from before 2.33: a program
// built against such a glibc reaches `stat` and its neighbours through these names alone.

/// # Safety
/// Called by the program with libc's contract for this function.
#[cfg(glibc_pre_2_33)]
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn __xstat(
    version: libc::c_int,
    path: *const c_char,
    buf: *mut libc::stat,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::xstat(version, path, buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[cfg(glibc_pre_2_33)]
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn __xstat64(
    version: libc::c_int,
    path: *const c_char,
    buf: *mut libc::stat64,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::xstat64(version, path, buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[cfg(glibc_pre_2_33)]
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn __fxstat(
    version: libc::c_int,
    fd: libc::c_int,
    buf: *mut libc::stat,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::fxstat(version, fd, buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[cfg(glibc_pre_2_33)]
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn __fxstat64(
    version: libc::c_int,
    fd: libc::c_int,
    buf: *mut libc::stat64,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::fxstat64(version, fd, buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[cfg(glibc_pre_2_33)]
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn __lxstat(
    version: libc::c_int,
    path: *const c_char,
    buf: *mut libc::stat,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::lxstat(version, path, buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[cfg(glibc_pre_2_33)]
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn __lxstat64(
    version: libc::c_int,
    path: *const c_char,
    buf: *mut libc::stat64,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::lxstat64(version, path, buf) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[cfg(glibc_pre_2_33)]
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn __fxstatat(
    version: libc::c_int,
    dirfd: libc::c_int,
    path: *const c_char,
    buf: *mut libc::stat,
    flags: libc::c_int,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::fxstatat(version, dirfd, path, buf, flags) }
}

/// # Safety
/// Called by the program with libc's contract for this function.
#[cfg(glibc_pre_2_33)]
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn __fxstatat64(
    version: libc::c_int,
    dirfd: libc::c_int,
    path: *const c_char,
    buf: *mut libc::stat64,
    flags: libc::c_int,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::fxstatat64(version, dirfd, path, buf, flags) }
}
