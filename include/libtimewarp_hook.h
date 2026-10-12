/*
 * libtimewarp_hook.h -- the ABI a hook library implements.
 *
 * A hook library sits in front of the timekeeper. libtimewarp interposes the
 * program's calls, carries the session, and hands every call to the fixed
 * symbol libtimewarp_hook, which answers it or declines it. A declined call
 * gets the keeper's answer, so a hook implements what it cares about and
 * leaves the rest alone.
 *
 * Build one with, for example:
 *
 *     cc -shared -fPIC -o myhook.so myhook.c
 *     LIBTIMEWARP_HOOK=/path/to/myhook.so@4096 LD_PRELOAD=libtimewarp.so ./prog
 *
 * The number after @ is the size of the arena, a page-aligned block of shared
 * memory every process of the session sees at the same contents. It defaults
 * to 0, which means no arena at all.
 */

#ifndef LIBTIMEWARP_HOOK_H
#define LIBTIMEWARP_HOOK_H

#include <stdint.h>
#include <time.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * What libtimewarp_hook returns. Status is a separate return and never a
 * sentinel, because every int64_t is a legal answer.
 *
 * DECLINED is zero, so a hook that answers nothing, or one whose switch falls
 * through, leaves the keeper's answer standing rather than handing the program
 * whatever *out happened to hold.
 */
enum libtimewarp_status {
    /* Let the keeper answer this call. *out is ignored. */
    LIBTIMEWARP_DECLINED = 0,
    /* *out is what the program sees, unaltered. */
    LIBTIMEWARP_HANDLED = 1
};

/*
 * Which conversion this call is. Four kinds cover every interposed function,
 * so a hook can ignore func and switch on the shape alone.
 */
enum libtimewarp_kind {
    /* Reserved: a zeroed call names no kind. */
    LIBTIMEWARP_KIND_NONE = 0,
    /* A genuine reading becomes the fake one: clock reads, timestamp reads. */
    LIBTIMEWARP_INSTANT_TO_FAKE = 1,
    /* A fake instant becomes the genuine one: timestamp sets, clock sets. */
    LIBTIMEWARP_INSTANT_TO_GENUINE = 2,
    /* A fake duration becomes the genuine one: sleeps, timeouts, timer arms.
       A timer arm answered with 0 or less fires at once rather than being
       disarmed. */
    LIBTIMEWARP_DURATION_TO_GENUINE = 3,
    /* A genuine remaining becomes the fake one: timer queries. */
    LIBTIMEWARP_DURATION_TO_FAKE = 4
};

/*
 * Which interposed function this call answers.
 *
 * The enumeration covers every function libtimewarp takes over on any
 * platform, so a hook built once runs against every one of them. A value is
 * fixed for good: a new function takes a new number rather than renumbering
 * its neighbours. Functions that are aliases of one another report the name
 * they alias, so glibc's __clock_gettime and syscall(SYS_clock_gettime) both
 * arrive as LIBTIMEWARP_FUNC_CLOCK_GETTIME.
 */
enum libtimewarp_func {
    /* Reserved: a zeroed call names no function. */
    LIBTIMEWARP_FUNC_NONE = 0,

    /* Clocks. */
    LIBTIMEWARP_FUNC_CLOCK_GETTIME = 1,
    LIBTIMEWARP_FUNC_GETTIMEOFDAY = 2,
    LIBTIMEWARP_FUNC_TIME = 3,
    LIBTIMEWARP_FUNC_TIMESPEC_GET = 4,
    LIBTIMEWARP_FUNC_FTIME = 5,
    LIBTIMEWARP_FUNC_CLOCK = 6,
    LIBTIMEWARP_FUNC_TIMES = 7,
    LIBTIMEWARP_FUNC_GETRUSAGE = 8,
    LIBTIMEWARP_FUNC_MACH_ABSOLUTE_TIME = 9,
    LIBTIMEWARP_FUNC_MACH_CONTINUOUS_TIME = 10,
    LIBTIMEWARP_FUNC_MACH_APPROXIMATE_TIME = 11,
    LIBTIMEWARP_FUNC_MACH_CONTINUOUS_APPROXIMATE_TIME = 12,
    LIBTIMEWARP_FUNC_CLOCK_GETTIME_NSEC_NP = 13,
    LIBTIMEWARP_FUNC_CLOCK_GET_TIME = 14,

    /* Sleeps. */
    LIBTIMEWARP_FUNC_NANOSLEEP = 100,
    LIBTIMEWARP_FUNC_SLEEP = 101,
    LIBTIMEWARP_FUNC_USLEEP = 102,
    LIBTIMEWARP_FUNC_THRD_SLEEP = 103,
    LIBTIMEWARP_FUNC_CLOCK_NANOSLEEP = 104,
    LIBTIMEWARP_FUNC_MACH_WAIT_UNTIL = 105,

    /* Timeouts. */
    LIBTIMEWARP_FUNC_POLL = 200,
    LIBTIMEWARP_FUNC_PPOLL = 201,
    LIBTIMEWARP_FUNC_SELECT = 202,
    LIBTIMEWARP_FUNC_PSELECT = 203,
    LIBTIMEWARP_FUNC_EPOLL_WAIT = 204,
    LIBTIMEWARP_FUNC_EPOLL_PWAIT = 205,
    LIBTIMEWARP_FUNC_EPOLL_PWAIT2 = 206,
    LIBTIMEWARP_FUNC_KEVENT = 207,
    LIBTIMEWARP_FUNC_PTHREAD_COND_TIMEDWAIT = 208,
    LIBTIMEWARP_FUNC_PTHREAD_COND_CLOCKWAIT = 209,
    LIBTIMEWARP_FUNC_PTHREAD_MUTEX_TIMEDLOCK = 210,
    LIBTIMEWARP_FUNC_PTHREAD_RWLOCK_TIMEDRDLOCK = 211,
    LIBTIMEWARP_FUNC_PTHREAD_RWLOCK_TIMEDWRLOCK = 212,
    LIBTIMEWARP_FUNC_PTHREAD_TIMEDJOIN_NP = 213,
    LIBTIMEWARP_FUNC_SEM_TIMEDWAIT = 214,
    LIBTIMEWARP_FUNC_SEM_CLOCKWAIT = 215,
    LIBTIMEWARP_FUNC_SIGTIMEDWAIT = 216,
    LIBTIMEWARP_FUNC_AIO_SUSPEND = 217,
    LIBTIMEWARP_FUNC_RECVMMSG = 218,
    LIBTIMEWARP_FUNC_CND_TIMEDWAIT = 219,
    LIBTIMEWARP_FUNC_MTX_TIMEDLOCK = 220,
    LIBTIMEWARP_FUNC_MQ_TIMEDSEND = 221,
    LIBTIMEWARP_FUNC_MQ_TIMEDRECEIVE = 222,
    LIBTIMEWARP_FUNC_KEVENT64 = 223,

    /* Timers. */
    LIBTIMEWARP_FUNC_SETITIMER = 300,
    LIBTIMEWARP_FUNC_GETITIMER = 301,
    LIBTIMEWARP_FUNC_ALARM = 302,
    LIBTIMEWARP_FUNC_TIMER_SETTIME = 303,
    LIBTIMEWARP_FUNC_TIMER_GETTIME = 304,
    LIBTIMEWARP_FUNC_TIMERFD_SETTIME = 305,
    LIBTIMEWARP_FUNC_TIMERFD_GETTIME = 306,

    /* File timestamps. */
    LIBTIMEWARP_FUNC_STAT = 400,
    LIBTIMEWARP_FUNC_FSTAT = 401,
    LIBTIMEWARP_FUNC_LSTAT = 402,
    LIBTIMEWARP_FUNC_FSTATAT = 403,
    LIBTIMEWARP_FUNC_STATX = 404,
    LIBTIMEWARP_FUNC_UTIME = 405,
    LIBTIMEWARP_FUNC_UTIMES = 406,
    LIBTIMEWARP_FUNC_FUTIMES = 407,
    LIBTIMEWARP_FUNC_LUTIMES = 408,
    LIBTIMEWARP_FUNC_UTIMENSAT = 409,
    LIBTIMEWARP_FUNC_FUTIMENS = 410,

    /* Setting the time. */
    LIBTIMEWARP_FUNC_SETTIMEOFDAY = 500,
    LIBTIMEWARP_FUNC_CLOCK_SETTIME = 501,
    LIBTIMEWARP_FUNC_ADJTIME = 502
};

/*
 * One interposed call. It travels by pointer, so a field added later is no
 * break: read the fields you know and ignore the rest.
 */
struct libtimewarp_call {
    uint32_t func;    /* which interposed function, from enum libtimewarp_func */
    uint32_t kind;    /* which conversion, from enum libtimewarp_kind */
    int32_t clock;    /* the clock id the call names; CLOCK_REALTIME when it names none */
    int64_t value;    /* the number to convert, in nanoseconds */
    int64_t real_ns;  /* the genuine reading of clock, now */
    int64_t keeper;   /* what the keeper would return for this call */
    void *arena;      /* the shared user state, or NULL when size is 0 */
    uint64_t size;    /* the arena's size in bytes, fixed at session creation */
};

/*
 * Required. Runs on every interposed call, in every process of the session.
 *
 * Returns LIBTIMEWARP_HANDLED after writing *out, which the program then sees
 * unaltered, or LIBTIMEWARP_DECLINED to let the keeper answer. A handled
 * setter is the one exception: the program wants success rather than a number,
 * so *out is ignored there.
 *
 * This runs wherever its process reads the clock, which can include signal
 * handlers and borrowed stacks such as the Go runtime's. A hook meant to hold
 * up there stays async-signal-safe, keeps its frames small, and allocates
 * nothing. A hook that wants a monotonic clock to stay monotonic returns
 * values that never decrease, across every thread and every process of the
 * session. libtimewarp checks neither; both are the hook's to choose.
 *
 * A nested clock read from inside a hook skips the hook and returns the
 * keeper's answer, so a hook can read the faked value of another clock. The
 * genuine value of any clock comes from libtimewarp_genuine_clock_gettime
 * below. Leave by returning: a longjmp out keeps the reentrancy gate held for
 * that thread, which runs the keeper alone from then on.
 */
int32_t libtimewarp_hook(const struct libtimewarp_call *call, int64_t *out);

/*
 * Optional. Runs once, in the process that creates the session, before any
 * process can see the arena, and so before any call reaches libtimewarp_hook.
 * The clock reads genuine here: the session is not published yet.
 *
 * A return of 0 lets the session publish. Anything else is a bad value:
 * libtimewarp logs it, records a fault in the session, and the process runs on
 * genuine time.
 */
int32_t libtimewarp_hook_init(void *arena, uint64_t size);

/*
 * Exported by libtimewarp, for a hook that wants a reading the library has not
 * touched. Every other clock in the process is faked, so this is the only way
 * back to what the operating system says. libc's convention: 0, or -1 with
 * errno set.
 */
int libtimewarp_genuine_clock_gettime(clockid_t clock, struct timespec *ts);

#ifdef __cplusplus
}
#endif

#endif /* LIBTIMEWARP_HOOK_H */
