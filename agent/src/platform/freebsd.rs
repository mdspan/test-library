//! FreeBSD: `LD_PRELOAD` puts us ahead of libc, so defining the name is the whole mechanism, as
//! on Linux.
//!
//! Every group is here. A handful of the names Linux exports are not, because this platform has no
//! such call to catch: `ftime` and glibc's internal aliases are glibc's own, `syscall()` is Linux's
//! way of asking for a call by number, `epoll` and the timerfd have no counterpart, `statx` and the
//! `64` spellings of `stat` are Linux's too, and the `clockwait` pair arrived with glibc. `kevent`
//! is the one name here that Linux has no counterpart for.
//!
//! Five more are left out for a different reason. This libc keeps `utime`, `recvmmsg`,
//! `thrd_sleep`, `cnd_timedwait` and `mtx_timedlock` as thin wrappers that call, through the
//! loader, a function we already take over: `utimes`, `ppoll`, `nanosleep`,
//! `pthread_cond_timedwait` and `pthread_mutex_timedlock` respectively. Defining the outer name
//! as well would convert the call on the way in and again on the way down, so each is taken over
//! at the call it delegates to, which converts it exactly once. glibc reaches the same five
//! through internal names no loader can reach, which is why the Linux arm defines them itself.
//!
//! The timekeep page that catches Go lands here later.

use std::ffi::c_char;

use crate::interpose;
use crate::setter;
use crate::stamp;
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

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn time(t: *mut libc::time_t) -> libc::time_t {
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

/// The platform's own wait on a set of events, which Linux spells with `epoll` instead.
///
/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn kevent(
    kq: libc::c_int,
    changelist: *const libc::kevent,
    nchanges: libc::c_int,
    eventlist: *mut libc::kevent,
    nevents: libc::c_int,
    timeout: *const libc::timespec,
) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { wait::kevent(kq, changelist, nchanges, eventlist, nevents, timeout) }
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

// The file timestamps. A read converts the buffer the genuine call filled, and a set converts a
// copy of what the program asked for; `stamp` has the rules. The buffer here carries a birth time
// besides the three POSIX names, which is faked with the rest.

/// # Safety
/// Called by the program with libc's contract for this function.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn stat(path: *const c_char, buf: *mut libc::stat) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::stat(path, buf) }
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
pub(super) unsafe extern "C" fn lstat(path: *const c_char, buf: *mut libc::stat) -> libc::c_int {
    // Safety: the caller is the program, calling under libc's contract.
    unsafe { stamp::lstat(path, buf) }
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
