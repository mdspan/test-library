/* Makes one call under whatever session the suite set, and says how it returned if it ever does,
   so a case can freeze the clock over the call and see that it blocks rather than failing.

   argv[1] names the call and argv[2] is the fake duration, in milliseconds, it asks for. A wait
   waits that long. A timer arms an interval timer for that long, `alarm` for a second, reports
   what arming answered and what the timer then has left, and waits for the signal it fires.

   Every timer reports `armed.rc`, `armed.errno` and `armed.left`: the nanoseconds a query says the
   timer has left, or for `alarm` the seconds a second `alarm` says the first one had. Then
   `waiting=1` goes out, just before the call that blocks. A wait that returns reports `rc` and
   `errno`, and a timer that fires reports `fired=1`.

   macOS keeps more than one spelling of `select` and `pselect`, which differ in what they refuse,
   and the headers here pick the `$DARWIN_EXTSN` ones. The conforming spellings are named with an
   asm label, and so is the `select` of 10.4 that `x86_64` keeps. */
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>
#if defined(__FreeBSD__) || defined(__APPLE__)
#include <sys/event.h>
#endif

#define NS_PER_S 1000000000LL

#ifdef __APPLE__
typedef int select_fn(int, fd_set *, fd_set *, fd_set *, struct timeval *);
typedef int pselect_fn(int, fd_set *, fd_set *, fd_set *, const struct timespec *,
                       const sigset_t *);
#ifdef __x86_64__
extern select_fn conforming_select __asm("_select$1050");
extern pselect_fn conforming_pselect __asm("_pselect$1050");
extern select_fn legacy_select __asm("_select");
#else
extern select_fn conforming_select __asm("_select");
extern pselect_fn conforming_pselect __asm("_pselect");
#endif
#endif

/* The fake duration the call asks for, from argv[2]. */
static long long fake_ns;

static struct timespec spec(void) {
    struct timespec ts;
    ts.tv_sec = (time_t)(fake_ns / NS_PER_S);
    ts.tv_nsec = (long)(fake_ns % NS_PER_S);
    return ts;
}

static struct timeval val(void) {
    struct timeval tv;
    tv.tv_sec = (time_t)(fake_ns / NS_PER_S);
    tv.tv_usec = (suseconds_t)((fake_ns % NS_PER_S) / 1000);
    return tv;
}

static void waiting(void) {
    printf("waiting=1\n");
    fflush(stdout);
}

static int returned(int rc) {
    printf("rc=%d\nerrno=%d\n", rc, rc < 0 ? errno : 0);
    fflush(stdout);
    return 0;
}

/* Blocks until the timer fires its signal, which every timer here sends to a process that keeps it
   blocked. */
static int fired(int signal) {
    sigset_t set;
    int caught = 0;

    waiting();
    sigemptyset(&set);
    sigaddset(&set, signal);
    while (sigwait(&set, &caught) != 0) {
    }
    printf("fired=1\n");
    fflush(stdout);
    return 0;
}

static int interval_timer(int which, int signal) {
    struct itimerval value;
    int rc;

    memset(&value, 0, sizeof value);
    value.it_value = val();
    errno = 0;
    rc = setitimer(which, &value, NULL);
    printf("armed.rc=%d\narmed.errno=%d\n", rc, rc < 0 ? errno : 0);
    memset(&value, 0, sizeof value);
    getitimer(which, &value);
    printf("armed.left=%lld\n",
           (long long)value.it_value.tv_sec * NS_PER_S + (long long)value.it_value.tv_usec * 1000);
    return fired(signal);
}

static int alarms(void) {
    unsigned int left;

    alarm(1);
    left = alarm(1);
    printf("armed.rc=0\narmed.errno=0\narmed.left=%u\n", left);
    return fired(SIGALRM);
}

static int wait_named(const char *call) {
    struct timespec ts = spec();
    struct timeval tv = val();

    if (strcmp(call, "nanosleep") == 0) {
        waiting();
        return returned(nanosleep(&ts, NULL));
    }
    if (strcmp(call, "poll") == 0) {
        waiting();
        return returned(poll(NULL, 0, (int)(fake_ns / 1000000)));
    }
    if (strcmp(call, "select") == 0) {
        waiting();
        return returned(select(0, NULL, NULL, NULL, &tv));
    }
    if (strcmp(call, "pselect") == 0) {
        waiting();
        return returned(pselect(0, NULL, NULL, NULL, &ts, NULL));
    }
#ifdef __APPLE__
    if (strcmp(call, "select_conforming") == 0) {
        waiting();
        return returned(conforming_select(0, NULL, NULL, NULL, &tv));
    }
    if (strcmp(call, "pselect_conforming") == 0) {
        waiting();
        return returned(conforming_pselect(0, NULL, NULL, NULL, &ts, NULL));
    }
#ifdef __x86_64__
    if (strcmp(call, "select_legacy") == 0) {
        waiting();
        return returned(legacy_select(0, NULL, NULL, NULL, &tv));
    }
#endif
#endif
#if defined(__FreeBSD__) || defined(__APPLE__)
    if (strcmp(call, "kevent") == 0) {
        struct kevent event;
        int kq = kqueue();
        waiting();
        return returned(kevent(kq, NULL, 0, &event, 1, &ts));
    }
#endif
#ifdef __APPLE__
    if (strcmp(call, "kevent64") == 0) {
        struct kevent64_s event;
        int kq = kqueue();
        waiting();
        return returned(kevent64(kq, NULL, 0, &event, 1, 0, &ts));
    }
#endif
    fprintf(stderr, "no call named %s here\n", call);
    return 2;
}

int main(int argc, char **argv) {
    sigset_t blocked;

    if (argc < 3) {
        return 2;
    }
    fake_ns = atoll(argv[2]) * 1000000LL;

    sigemptyset(&blocked);
    sigaddset(&blocked, SIGALRM);
    sigaddset(&blocked, SIGVTALRM);
    sigprocmask(SIG_BLOCK, &blocked, NULL);

    if (strcmp(argv[1], "setitimer") == 0) {
        return interval_timer(ITIMER_REAL, SIGALRM);
    }
    if (strcmp(argv[1], "setitimer_virtual") == 0) {
        return interval_timer(ITIMER_VIRTUAL, SIGVTALRM);
    }
    if (strcmp(argv[1], "alarm") == 0) {
        return alarms();
    }
    return wait_named(argv[1]);
}
