/* Arms one timer for a fake duration and blocks until it fires, reporting the genuine time it
   took, so a suite case can change the session while the timer is already armed, or hold it under
   a frozen clock and see that nothing releases it.

   A timerfd where the platform has one, a POSIX timer on the monotonic clock where it does not,
   and the wall-clock interval timer on macOS, which has neither. Either way the timer is armed
   with a relative duration, unless argv[2] is `past`, and the suite sets the rate of whichever
   family it follows.

   The three are reached through one interface in nanoseconds, because `struct itimerspec` is a
   POSIX timers type and a platform without those timers does not declare it.

   Linked against the library rather than preloading it, so the genuine clock comes from the
   library's own symbol. argv[1] is the fake duration in milliseconds. `past` as argv[2] arms the
   timer for an absolute expiry one nanosecond after its clock's epoch instead, a deadline already
   gone, which macOS, with no absolute form, refuses. `disarmed=` goes out first, carrying what a
   timer disarmed on this session reports it has left, and `armed=1` after the conversion, so a
   case knows exactly when to make its change. */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>
#ifdef __linux__
#include <sys/timerfd.h>
#endif

extern int libtimewarp_genuine_clock_gettime(clockid_t clock, struct timespec *ts);

#define NS_PER_S 1000000000LL

static long long nanos(const struct timespec *ts) {
    return (long long)ts->tv_sec * NS_PER_S + ts->tv_nsec;
}

static long long genuine_mono(void) {
    struct timespec ts;
    if (libtimewarp_genuine_clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return nanos(&ts);
}

#ifndef __APPLE__
static struct timespec spec_of(long long ns) {
    struct timespec ts;
    ts.tv_sec = (time_t)(ns / NS_PER_S);
    ts.tv_nsec = (long)(ns % NS_PER_S);
    return ts;
}
#endif

#if defined(__linux__)
/* One timerfd on the monotonic clock, which the three calls below arm, query and wait on. */
static int timer;

static int make_timer(void) {
    timer = timerfd_create(CLOCK_MONOTONIC, 0);
    return timer >= 0 ? 0 : -1;
}

static void arm(long long ns) {
    struct itimerspec value;
    memset(&value, 0, sizeof value);
    value.it_value = spec_of(ns);
    timerfd_settime(timer, 0, &value, NULL);
}

static int arm_at(long long ns) {
    struct itimerspec value;
    memset(&value, 0, sizeof value);
    value.it_value = spec_of(ns);
    return timerfd_settime(timer, TFD_TIMER_ABSTIME, &value, NULL);
}

static long long left(void) {
    struct itimerspec value;
    memset(&value, 0, sizeof value);
    timerfd_gettime(timer, &value);
    return nanos(&value.it_value);
}

/* Blocks until it fires, which is a read of the count of expiries. */
static int await(void) {
    uint64_t ticks = 0;
    return read(timer, &ticks, sizeof ticks) == (ssize_t)sizeof ticks ? 0 : -1;
}
#elif defined(__APPLE__)
/* The wall-clock interval timer, firing a signal this process blocks and waits for. It is the one
   timer this platform has, so the family it follows is Realtime rather than an elapsed one. */
static int make_timer(void) {
    sigset_t blocked;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGALRM);
    sigprocmask(SIG_BLOCK, &blocked, NULL);
    return 0;
}

static struct timeval val_of(long long ns) {
    struct timeval tv;
    tv.tv_sec = (time_t)(ns / NS_PER_S);
    tv.tv_usec = (suseconds_t)((ns % NS_PER_S) / 1000);
    return tv;
}

static void arm(long long ns) {
    struct itimerval value;
    memset(&value, 0, sizeof value);
    value.it_value = val_of(ns);
    setitimer(ITIMER_REAL, &value, NULL);
}

static int arm_at(long long ns) {
    (void)ns;
    return -1;
}

static long long left(void) {
    struct itimerval value;
    memset(&value, 0, sizeof value);
    getitimer(ITIMER_REAL, &value);
    return (long long)value.it_value.tv_sec * NS_PER_S
           + (long long)value.it_value.tv_usec * 1000LL;
}

static int await(void) {
    sigset_t set;
    int caught = 0;
    sigemptyset(&set);
    sigaddset(&set, SIGALRM);
    while (sigwait(&set, &caught) != 0 && errno == EINTR) {
    }
    return 0;
}
#else
/* The same, as a POSIX timer on the monotonic clock firing a signal this process waits for. */
static timer_t timer;

static int make_timer(void) {
    struct sigevent event;
    sigset_t blocked;

    sigemptyset(&blocked);
    sigaddset(&blocked, SIGUSR1);
    sigprocmask(SIG_BLOCK, &blocked, NULL);

    memset(&event, 0, sizeof event);
    event.sigev_notify = SIGEV_SIGNAL;
    event.sigev_signo = SIGUSR1;
    return timer_create(CLOCK_MONOTONIC, &event, &timer);
}

static void arm(long long ns) {
    struct itimerspec value;
    memset(&value, 0, sizeof value);
    value.it_value = spec_of(ns);
    timer_settime(timer, 0, &value, NULL);
}

static int arm_at(long long ns) {
    struct itimerspec value;
    memset(&value, 0, sizeof value);
    value.it_value = spec_of(ns);
    return timer_settime(timer, TIMER_ABSTIME, &value, NULL);
}

static long long left(void) {
    struct itimerspec value;
    memset(&value, 0, sizeof value);
    timer_gettime(timer, &value);
    return nanos(&value.it_value);
}

static int await(void) {
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    while (sigwaitinfo(&set, NULL) < 0 && errno == EINTR) {
    }
    return 0;
}
#endif

int main(int argc, char **argv) {
    long long fake, started;

    if (argc < 2) {
        return 2;
    }
    fake = atoll(argv[1]) * 1000000LL;

    if (make_timer() != 0) {
        return 3;
    }

    /* Armed and then disarmed: a timer with nothing left reports nothing left, whatever the rate,
       so a freeze cannot turn a disarm into the longest duration there is. */
    arm(fake);
    arm(0);
    printf("disarmed=%lld\n", left());

    printf("fake=%lld\n", fake);
    if (argc > 2 && strcmp(argv[2], "past") == 0) {
        if (arm_at(1) != 0) {
            return 5;
        }
    } else {
        arm(fake);
    }
    printf("armed=1\n");
    fflush(stdout);

    started = genuine_mono();
    if (await() != 0) {
        return 4;
    }
    printf("genuine=%lld\n", genuine_mono() - started);
    fflush(stdout);
    return 0;
}
