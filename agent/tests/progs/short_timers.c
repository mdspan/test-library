/* Arms every timer for the least its own struct can name, argv[1] times over: that many
   nanoseconds in a `timespec` and that many microseconds in a `timeval`. Each timer reports whether
   it fired, and each query of an interval that short what interval it read.

   The suite runs it on a clock fast enough that the genuine duration comes to less than the struct
   can carry, or slow enough that a remaining converted back does, so a field the program set has
   to stay set through the arm and through the query. `alarm` as argv[2] adds `alarm(1)`, which
   only a clock millions of times faster than the genuine one brings under a genuine microsecond.

   Linked against the library rather than preloading it, so the genuine clock comes from the
   library's own symbol. Every wait here spins on that clock around a check that takes no timeout,
   so no rate reaches how long a timer is given to fire.

   `<key>.fired` is 1 for a timer that fired within GRACE_NS of genuine time and 0 for one that did
   not, which is a timer its arm disarmed. `<key>.interval` is the nanoseconds of interval a query
   read. A timer this system does not have reports `<key>.available=0` instead. */
#define _GNU_SOURCE
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

/* How long a timer is given to fire, in genuine time. Every timer here is due within microseconds,
   or a tick of consumption for the CPU timer, so one still waiting after this was disarmed. */
#define GRACE_NS (2 * NS_PER_S)

/* The expiry a timer is armed for when only its interval is queried: seconds of genuine time away
   at the fastest rate the suite sets, and inside what macOS and FreeBSD accept of a `setitimer` at
   the slowest. */
#define QUERY_NS (10000000 * NS_PER_S)

/* How many of its struct's least units every timer is armed for, from argv[1]. */
static long long units;

static long long nanos(const struct timespec *ts) {
    return (long long)ts->tv_sec * NS_PER_S + ts->tv_nsec;
}

static long long micros(const struct timeval *tv) {
    return (long long)tv->tv_sec * NS_PER_S + (long long)tv->tv_usec * 1000LL;
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

static struct timeval val_of(long long ns) {
    struct timeval tv;
    tv.tv_sec = (time_t)(ns / NS_PER_S);
    tv.tv_usec = (suseconds_t)((ns % NS_PER_S) / 1000);
    return tv;
}

/* Whether `signo` is pending, which is how a timer firing a blocked signal says it fired. */
static int pending(int signo) {
    sigset_t set;
    sigemptyset(&set);
    if (sigpending(&set) != 0) {
        return 0;
    }
    return sigismember(&set, signo) == 1;
}

/* Takes a fired timer's signal off the pending set, so the next timer on it starts clean. */
static void consume(int signo) {
    sigset_t set;
    int caught = 0;
    if (!pending(signo)) {
        return;
    }
    sigemptyset(&set);
    sigaddset(&set, signo);
    sigwait(&set, &caught);
}

/* Spins until `fired(arg)` says so or GRACE_NS has passed, and reports which. The spin is also the
   consumption a CPU timer counts down. */
static void await(const char *key, int (*fired)(int), int arg) {
    long long until = genuine_mono() + GRACE_NS;
    int done = fired(arg);
    while (!done && genuine_mono() < until) {
        done = fired(arg);
    }
    printf("%s.available=1\n%s.fired=%d\n", key, key, done);
}

static void interval(const char *key, long long ns) {
    printf("%s.available=1\n%s.interval=%lld\n", key, key, ns);
}

/* ---- the interval timers and the alarm, which every platform has ---- */

static void interval_timers(int with_alarm) {
    struct itimerval value, current, old;

    memset(&value, 0, sizeof value);
    value.it_value = val_of(units * 1000);
    setitimer(ITIMER_REAL, &value, NULL);
    await("setitimer_real", pending, SIGALRM);
    consume(SIGALRM);

    setitimer(ITIMER_PROF, &value, NULL);
    await("itimer_prof", pending, SIGPROF);
    consume(SIGPROF);

    if (with_alarm) {
        alarm(1);
        await("alarm", pending, SIGALRM);
        consume(SIGALRM);
    }

    /* Queried, and read again as the old value of the arm that disarms it. */
    memset(&value, 0, sizeof value);
    value.it_value = val_of(QUERY_NS);
    value.it_interval = val_of(units * 1000);
    setitimer(ITIMER_REAL, &value, NULL);
    memset(&current, 0, sizeof current);
    getitimer(ITIMER_REAL, &current);
    interval("getitimer", micros(&current.it_interval));

    memset(&value, 0, sizeof value);
    memset(&old, 0, sizeof old);
    setitimer(ITIMER_REAL, &value, &old);
    interval("setitimer_old", micros(&old.it_interval));
}

/* ---- the POSIX timers, which macOS does not have ---- */

#ifndef __APPLE__
static void posix_timer(void) {
    struct sigevent event;
    struct itimerspec value, current;
    timer_t timer;

    memset(&event, 0, sizeof event);
    event.sigev_notify = SIGEV_SIGNAL;
    event.sigev_signo = SIGUSR1;
    if (timer_create(CLOCK_MONOTONIC, &event, &timer) != 0) {
        printf("timer_settime.available=0\ntimer_gettime.available=0\n");
        return;
    }

    memset(&value, 0, sizeof value);
    value.it_value = spec_of(units);
    timer_settime(timer, 0, &value, NULL);
    await("timer_settime", pending, SIGUSR1);
    consume(SIGUSR1);

    memset(&value, 0, sizeof value);
    value.it_value = spec_of(QUERY_NS);
    value.it_interval = spec_of(units);
    timer_settime(timer, 0, &value, NULL);
    memset(&current, 0, sizeof current);
    timer_gettime(timer, &current);
    interval("timer_gettime", nanos(&current.it_interval));
    timer_delete(timer);
}
#endif

/* ---- the timerfd, which only Linux has ---- */

#ifdef __linux__
/* Whether the timerfd has fired, which a read that does not block answers. */
static int expired(int fd) {
    uint64_t ticks = 0;
    return read(fd, &ticks, sizeof ticks) == (ssize_t)sizeof ticks;
}

static void timerfd(void) {
    struct itimerspec value, current;
    int fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);

    if (fd < 0) {
        printf("timerfd_settime.available=0\ntimerfd_gettime.available=0\n");
        return;
    }

    memset(&value, 0, sizeof value);
    value.it_value = spec_of(units);
    timerfd_settime(fd, 0, &value, NULL);
    await("timerfd_settime", expired, fd);

    memset(&value, 0, sizeof value);
    value.it_value = spec_of(QUERY_NS);
    value.it_interval = spec_of(units);
    timerfd_settime(fd, 0, &value, NULL);
    memset(&current, 0, sizeof current);
    timerfd_gettime(fd, &current);
    interval("timerfd_gettime", nanos(&current.it_interval));
    close(fd);
}
#endif

int main(int argc, char **argv) {
    sigset_t blocked;

    if (argc < 2) {
        return 2;
    }
    units = atoll(argv[1]);

    /* Every signal a timer here fires with, blocked so it stays pending rather than reaching a
       handler. */
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGALRM);
    sigaddset(&blocked, SIGPROF);
    sigaddset(&blocked, SIGUSR1);
    sigprocmask(SIG_BLOCK, &blocked, NULL);

    interval_timers(argc > 2 && strcmp(argv[2], "alarm") == 0);
#ifndef __APPLE__
    posix_timer();
#endif
#ifdef __linux__
    timerfd();
#endif
    fflush(stdout);
    return 0;
}
