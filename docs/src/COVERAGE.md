# What Gets Faked

Every program that reads the time, waits, or looks at file dates through the
C library is covered, whatever language it is written in. The library
always fakes everything in this chapter; there is no setting to pick among
the groups.

## Reading the Clock

| Platform | Functions |
| --- | --- |
| all | `clock_gettime`, `gettimeofday`, `time`, `timespec_get`, `ftime` |
| all, CPU time | `clock`, `times`, and `getrusage` for the process itself and, where the platform has it, the calling thread |
| Linux | `syscall()` with `SYS_clock_gettime`, `SYS_gettimeofday`, or `SYS_time`, and glibc's internal names for the functions above |
| macOS | `mach_absolute_time`, `mach_continuous_time`, `mach_approximate_time`, `mach_continuous_approximate_time`, `clock_gettime_nsec_np`, and `clock_get_time` on a port from `host_get_clock_service` |

A function that names no clock, `time` or `gettimeofday` for instance, reads
the wall clock. Each clock follows its [family](CLOCKS.md).

`getrusage` scales the user and system CPU times and leaves every other
field as the system reported it. `times` also returns a count of elapsed
ticks, which follows the Monotonic family.

## Sleeps, Timeouts, and Timers

| Group | Platform | Functions |
| --- | --- | --- |
| sleeps | all | `nanosleep`, `sleep`, `usleep`, `thrd_sleep` |
| sleeps | Linux, FreeBSD | `clock_nanosleep` |
| sleeps | Linux | `syscall()` with `SYS_nanosleep` or `SYS_clock_nanosleep` |
| sleeps | macOS | `mach_wait_until` |
| timeouts | all | `poll`, `select`, `pselect`, `pthread_cond_timedwait`, `aio_suspend` |
| timeouts | Linux, FreeBSD | `ppoll`, `pthread_mutex_timedlock`, `sem_timedwait`, `sigtimedwait`, `pthread_rwlock_timedrdlock`, `pthread_rwlock_timedwrlock`, `pthread_timedjoin_np`, `recvmmsg`, `cnd_timedwait`, `mtx_timedlock`, `mq_timedsend`, `mq_timedreceive` |
| timeouts | Linux | `epoll_wait`, `epoll_pwait`, `epoll_pwait2`, `sem_clockwait`, `pthread_cond_clockwait` |
| timeouts | FreeBSD, macOS | `kevent`, and on macOS `kevent64` |
| timers | all | `setitimer`, `getitimer`, `alarm` |
| timers | Linux, FreeBSD | `timer_settime`, `timer_gettime` |
| timers | Linux | `timerfd_settime`, `timerfd_gettime` |

A wait lasts as long as the faked clock says it should. At rate 10,
`sleep(10)` returns after one real second, and a deadline one faked hour
away arrives after six real minutes.

Each wait follows the family of the clock it measures:

- `nanosleep`, `poll`, `select`, `epoll`, `kevent`, and `mach_wait_until`
  follow Monotonic.
- `clock_nanosleep`, `timer_settime`, timerfds, and condition variables
  follow the clock they were given.
- `sem_timedwait` and `alarm` follow Realtime.
- `setitimer` follows Realtime for `ITIMER_REAL`, and Cpu for the other
  two.

**A wait is converted once, when it starts.** A change you make while a
program sleeps reaches that sleep's end only if the program sleeps again.
Timers behave the same way: a timer is converted when it is armed.

**Freezing is one way.** A sleep that starts or is running under rate 0
gets the longest wait there is, about 292 years, and setting the rate back
leaves it asleep. A thread sleeping when you freeze stays asleep until the
program is killed, and so does every thread that sleeps while the clock is
frozen, a zero-length sleep included. Plan on restarting a frozen program,
or freeze the wall clock alone with `LIBTIMEWARP_REALTIME_RATE=0`, which
leaves the waits that follow the monotonic clocks running.

## File Timestamps

A file you write under a faked clock shows a modification time from the
faked clock, and a file's dates follow the faked wall clock's speed.

| Platform | Functions |
| --- | --- |
| all | `stat`, `fstat`, `lstat`, `fstatat`, for the access, modification, change, and birth times; `utime`, `utimes`, `futimes`, `lutimes`, `utimensat`, `futimens` |
| Linux | `statx` |
| where they exist | the `64` and `$INODE64` spellings of the above |

Reading a date converts it from real time to faked time, and setting one
converts it back, so a file you touch now shows now to every program in the
session. A timestamp that would fall before 1970 on the faked clock shows
as 1970. Under a frozen wall clock, setting a date stores the real current
time.

## Setting the Time

`settimeofday`, `clock_settime(CLOCK_REALTIME, ...)`, and `adjtime` change
the session's wall clock, for every program in it, and leave the system
clock alone. `adjtime` applies its whole adjustment at once.

The call succeeds only where the system would have let the program set the
real clock: with the `CAP_SYS_TIME` capability on Linux, and as root on
FreeBSD and macOS. Any other program gets `EPERM`, as it would without the
library. `clock_settime` on any other clock is refused.

A refused call gets the error the system would have given it. Where a call
is wrong in more than one way, which error that is depends on the system:
a program without privilege that passes a malformed time gets `EINVAL` on
Linux and `EPERM` on FreeBSD and macOS, and FreeBSD answers `EPERM` even for
a clock that cannot be set. On macOS, `adjtime` with no adjustment needs
root, as it does without the library.

## What Stays Real

- `clock_getres`, and `adjtimex`, `ntp_adjtime`, and `stime`.
- On Linux, `syscall()` with any number not listed above.
- The clocks listed under [Which Clock Is in Which Family](CLOCKS.md#which-clock-is-in-which-family)
  as never faked.
- Programs that read the time without the C library, beyond the ones
  covered in [Platform Notes](PLATFORMS.md).
