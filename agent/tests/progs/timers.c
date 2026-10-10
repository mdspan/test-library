/* Every timer the library takes over, each armed for one fake duration and each measured against
   the genuine time it actually took to fire, plus what a query reports while it is armed.

   The timerfd is Linux's alone and the POSIX timers are Linux's and FreeBSD's, so a platform
   without them reports none of their keys and the suite's own list for that platform does not name
   them. macOS has the three interval timers and nothing else.

   Linked against the library rather than preloading it, so the genuine clock comes from the
   library's own symbol and the measurement is not the one under test.

   argv[1] is the fake duration, in milliseconds, every timer is armed for. `alarm` takes whole
   seconds and so always asks for one; the CPU timers ask for a fixed amount of consumption. argv[2]
   is `cpu` to run the timer that has to burn that consumption to fire, and anything else to leave
   it out, because only the runs that measure it are worth the CPU.

   Each timer reports three facts: `<key>.fake`, the nanoseconds of faketime it was armed for,
   `<key>.genuine`, the nanoseconds of genuine time it took to fire, and `<key>.rc`, what the arm
   answered. A query reports `<key>.fake` and `<key>.remaining`, the nanoseconds the timer said it
   had left. A timer this system does not have reports `<key>.available=0` instead.

   Each line goes straight to the descriptor. libSystem's own calls reach the library's rows as the
   program's do, and a buffered stream stats its descriptor before its first write, which the hook
   case would count as a call of this target's. */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>
#ifdef __linux__
#include <sys/timerfd.h>
#endif
#include <unistd.h>

extern int libtimewarp_genuine_clock_gettime(clockid_t clock, struct timespec *ts);

#define NS_PER_S 1000000000LL

/* The fake duration every timer is armed for, from argv[1]. */
static long long base_ns;

/* The fake consumption the CPU timers are armed for. Fixed, because the Cpu family has a rate of
   its own and a run has to burn every nanosecond of it. */
#define CPU_BASE_NS 200000000LL

/* The most genuine CPU time a run will burn waiting for a CPU timer that is never going to fire. */
#define CPU_GUARD_NS 5000000000LL

static long long nanos(const struct timespec *ts) {
    return (long long)ts->tv_sec * NS_PER_S + ts->tv_nsec;
}

static long long micros(const struct timeval *tv) {
    return (long long)tv->tv_sec * NS_PER_S + (long long)tv->tv_usec * 1000LL;
}

/* The genuine monotonic clock, which is what every elapsed measurement here is made against. */
static long long genuine_mono(void) {
    struct timespec ts;
    if (libtimewarp_genuine_clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return nanos(&ts);
}

/* The genuine CPU time this process has consumed, which is what a CPU timer counts down. */
static long long genuine_cpu(void) {
    struct timespec ts;
    if (libtimewarp_genuine_clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &ts) != 0) {
        return 0;
    }
    return nanos(&ts);
}

/* A clock as the program sees it, which is where an absolute expiry is measured from. */
static long long faked(clockid_t id) {
    struct timespec ts;
    if (clock_gettime(id, &ts) != 0) {
        return 0;
    }
    return nanos(&ts);
}

static struct timespec spec_of(long long ns) {
    struct timespec ts;
    ts.tv_sec = (time_t)(ns / NS_PER_S);
    ts.tv_nsec = (long)(ns % NS_PER_S);
    return ts;
}

static struct timeval val_of(long long ns) {
    struct timeval tv;
    tv.tv_sec = (time_t)(ns / NS_PER_S);
    tv.tv_usec = (suseconds_t)((ns % NS_PER_S) / 1000);
    return tv;
}

/* Writes one formatted line to stdout. */
static void say(const char *format, ...) {
    char line[4200];
    const char *next = line;
    size_t left;
    va_list args;
    int length;

    va_start(args, format);
    length = vsnprintf(line, sizeof line, format, args);
    va_end(args);
    if (length < 0) {
        return;
    }
    left = (size_t)length < sizeof line ? (size_t)length : sizeof line - 1;
    while (left > 0) {
        ssize_t wrote = write(STDOUT_FILENO, next, left);
        if (wrote <= 0) {
            return;
        }
        next += wrote;
        left -= (size_t)wrote;
    }
}

static long long started;

static void begin(void) { started = genuine_mono(); }

static void done(const char *key, long long fake, long long rc) {
    long long elapsed = genuine_mono() - started;
    say("%s.available=1\n", key);
    say("%s.fake=%lld\n", key, fake);
    say("%s.genuine=%lld\n", key, elapsed);
    say("%s.rc=%lld\n", key, rc);
}

static void queried(const char *key, long long fake, long long remaining) {
    say("%s.available=1\n", key);
    say("%s.fake=%lld\n", key, fake);
    say("%s.remaining=%lld\n", key, remaining);
}

static void skipped(const char *key) { say("%s.available=0\n", key); }

/* Waits for one of the timer signals, which main blocks so that it is caught here rather than
   killing the process. macOS has no `sigwaitinfo`, and `sigwait` answers the same question. */
static void wait_for(int signo) {
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, signo);
#ifdef __APPLE__
    {
        int caught = 0;
        while (sigwait(&set, &caught) != 0 && errno == EINTR) {
        }
    }
#else
    while (sigwaitinfo(&set, NULL) < 0 && errno == EINTR) {
    }
#endif
}

/* ---- the interval timers ---- */

static void interval_timers(void) {
    struct itimerval new_value, old_value, current;

    /* Armed for twice the base and then re-armed for the base: the old value the second arm
       reports is the first arm's remaining, in the program's own units. */
    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = val_of(2 * base_ns);
    setitimer(ITIMER_REAL, &new_value, NULL);

    memset(&old_value, 0, sizeof old_value);
    new_value.it_value = val_of(base_ns);
    begin();
    setitimer(ITIMER_REAL, &new_value, &old_value);
    queried("setitimer_old", 2 * base_ns, micros(&old_value.it_value));

    memset(&current, 0, sizeof current);
    getitimer(ITIMER_REAL, &current);
    queried("getitimer_real", base_ns, micros(&current.it_value));

    wait_for(SIGALRM);
    done("setitimer_real", base_ns, 0);

    /* A zero expiry disarms, and a disarmed timer has nothing left rather than something. */
    new_value.it_value = val_of(base_ns);
    setitimer(ITIMER_REAL, &new_value, NULL);
    memset(&new_value, 0, sizeof new_value);
    setitimer(ITIMER_REAL, &new_value, NULL);
    memset(&current, 0, sizeof current);
    getitimer(ITIMER_REAL, &current);
    queried("disarmed_real", 0, micros(&current.it_value));
}

/* The two interval timers that count consumption rather than elapsed time. `ITIMER_VIRTUAL` counts
   user-mode execution, so this burns it; `ITIMER_PROF` is armed and disarmed, because the family it
   follows is what matters and the two follow the same one. */
static void cpu_timers(int burn) {
    struct itimerval new_value;
    sigset_t pending;
    long long consumed;
    volatile double sink = 1.0;

    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = val_of(CPU_BASE_NS);
    consumed = genuine_cpu();
    if (!burn || setitimer(ITIMER_VIRTUAL, &new_value, NULL) != 0) {
        skipped("itimer_virtual");
    } else {
        for (;;) {
            int spin;
            sigemptyset(&pending);
            sigpending(&pending);
            if (sigismember(&pending, SIGVTALRM)) {
                break;
            }
            if (genuine_cpu() - consumed > CPU_GUARD_NS) {
                break;
            }
            for (spin = 0; spin < 50000; spin++) {
                sink = sink * 1.000001 + 1.0;
            }
        }
        say("itimer_virtual.available=1\n");
        say("itimer_virtual.fake=%lld\n", CPU_BASE_NS);
        say("itimer_virtual.genuine=%lld\n", genuine_cpu() - consumed);
        say("itimer_virtual.rc=0\n");
    }

    new_value.it_value = val_of(CPU_BASE_NS);
    say("itimer_prof.armed=%d\n", setitimer(ITIMER_PROF, &new_value, NULL));
    memset(&new_value, 0, sizeof new_value);
    setitimer(ITIMER_PROF, &new_value, NULL);
}

/* ---- the alarm ---- */

static void alarms(void) {
    unsigned int left;

    /* Ten seconds of faketime, cancelled at once: what the cancel answers is what the first alarm
       still had, in the program's own seconds. */
    alarm(10);
    left = alarm(0);
    queried("alarm_remaining", 10 * NS_PER_S, (long long)left * NS_PER_S);

    begin();
    alarm(1);
    wait_for(SIGALRM);
    done("alarm", NS_PER_S, 0);
}

/* ---- the POSIX timers, which macOS does not have ---- */

#ifndef __APPLE__
static void posix_timers(void) {
    struct sigevent event;
    struct itimerspec new_value, current;
    timer_t steady, wall;

    memset(&event, 0, sizeof event);
    event.sigev_notify = SIGEV_SIGNAL;
    event.sigev_signo = SIGUSR1;

    if (timer_create(CLOCK_MONOTONIC, &event, &steady) != 0) {
        skipped("timer_settime");
        skipped("timer_absolute");
        skipped("timer_gettime");
        skipped("timer_realtime");
        return;
    }

    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = spec_of(base_ns);
    begin();
    timer_settime(steady, 0, &new_value, NULL);
    memset(&current, 0, sizeof current);
    timer_gettime(steady, &current);
    queried("timer_gettime", base_ns, nanos(&current.it_value));
    wait_for(SIGUSR1);
    done("timer_settime", base_ns, 0);

    /* An absolute expiry, one base duration past what the timer's own clock shows the program. */
    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = spec_of(faked(CLOCK_MONOTONIC) + base_ns);
    begin();
    timer_settime(steady, TIMER_ABSTIME, &new_value, NULL);
    wait_for(SIGUSR1);
    done("timer_absolute", base_ns, 0);
    timer_delete(steady);

    /* A timer created on the wall clock, which is what `timer_create` recorded for this arm. */
    if (timer_create(CLOCK_REALTIME, &event, &wall) != 0) {
        skipped("timer_realtime");
        return;
    }
    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = spec_of(base_ns);
    begin();
    timer_settime(wall, 0, &new_value, NULL);
    wait_for(SIGUSR1);
    done("timer_realtime", base_ns, 0);
    timer_delete(wall);
}
#endif

/* ---- the timerfds, which only Linux has ---- */

#ifdef __linux__
/* Blocks until the timerfd fires, which is a read of the count of expiries. */
static long long expired(int fd) {
    uint64_t ticks = 0;
    if (read(fd, &ticks, sizeof ticks) != (ssize_t)sizeof ticks) {
        return -1;
    }
    return (long long)ticks;
}

static void timerfds(void) {
    struct itimerspec new_value, current;
    int steady, wall;

    steady = timerfd_create(CLOCK_MONOTONIC, 0);
    if (steady < 0) {
        skipped("timerfd_relative");
        skipped("timerfd_absolute");
        skipped("timerfd_gettime");
        skipped("timerfd_first");
        skipped("timerfd_interval");
        skipped("timerfd_realtime");
        return;
    }

    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = spec_of(base_ns);
    begin();
    timerfd_settime(steady, 0, &new_value, NULL);
    memset(&current, 0, sizeof current);
    timerfd_gettime(steady, &current);
    queried("timerfd_gettime", base_ns, nanos(&current.it_value));
    done("timerfd_relative", base_ns, expired(steady));

    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = spec_of(faked(CLOCK_MONOTONIC) + base_ns);
    begin();
    timerfd_settime(steady, TFD_TIMER_ABSTIME, &new_value, NULL);
    done("timerfd_absolute", base_ns, expired(steady));

    /* A repeating timer: the interval converts when the timer is armed, like the expiry, so the
       gap between the first firing and the second is the same fake duration. */
    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = spec_of(base_ns);
    new_value.it_interval = spec_of(base_ns);
    begin();
    timerfd_settime(steady, 0, &new_value, NULL);
    done("timerfd_first", base_ns, expired(steady));
    begin();
    done("timerfd_interval", base_ns, expired(steady));
    close(steady);

    /* A timerfd created on the wall clock, which is what `timerfd_create` recorded for this arm. */
    wall = timerfd_create(CLOCK_REALTIME, 0);
    if (wall < 0) {
        skipped("timerfd_realtime");
        return;
    }
    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = spec_of(base_ns);
    begin();
    timerfd_settime(wall, 0, &new_value, NULL);
    done("timerfd_realtime", base_ns, expired(wall));
    close(wall);
}
#endif

/* A sleep and a timer armed for the same fake duration at the same moment, which the design says
   must still fire together. A timerfd where there is one, a POSIX timer where there is not, and
   the wall-clock interval timer on macOS, which has neither. */
static void sleep_and_timer(void) {
    struct timespec request = spec_of(base_ns);
    long long start, woke, fired;
#if defined(__APPLE__)
    /* The one timer this platform has: the wall-clock interval timer, firing a signal. */
    struct itimerval new_value;

    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = val_of(base_ns);

    start = genuine_mono();
    setitimer(ITIMER_REAL, &new_value, NULL);
    nanosleep(&request, NULL);
    woke = genuine_mono() - start;
    wait_for(SIGALRM);
    fired = genuine_mono() - start;
#elif defined(__linux__)
    struct itimerspec new_value;
    int fd = timerfd_create(CLOCK_MONOTONIC, 0);

    if (fd < 0) {
        skipped("together");
        return;
    }
    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = spec_of(base_ns);

    start = genuine_mono();
    timerfd_settime(fd, 0, &new_value, NULL);
    nanosleep(&request, NULL);
    woke = genuine_mono() - start;
    if (expired(fd) < 0) {
        skipped("together");
        close(fd);
        return;
    }
    fired = genuine_mono() - start;
    close(fd);
#else
    struct sigevent event;
    struct itimerspec new_value;
    timer_t together;

    memset(&event, 0, sizeof event);
    event.sigev_notify = SIGEV_SIGNAL;
    event.sigev_signo = SIGUSR2;
    if (timer_create(CLOCK_MONOTONIC, &event, &together) != 0) {
        skipped("together");
        return;
    }
    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = spec_of(base_ns);

    start = genuine_mono();
    timer_settime(together, 0, &new_value, NULL);
    nanosleep(&request, NULL);
    woke = genuine_mono() - start;
    wait_for(SIGUSR2);
    fired = genuine_mono() - start;
    timer_delete(together);
#endif

    say("together.available=1\n");
    say("together.fake=%lld\n", base_ns);
    say("together.sleep=%lld\n", woke);
    say("together.timer=%lld\n", fired);
}

int main(int argc, char **argv) {
    const char *shared;
    sigset_t blocked;

    if (argc < 2) {
        return 2;
    }
    base_ns = atoll(argv[1]) * 1000000LL;

    /* Every signal a timer here fires with, blocked so it stays pending rather than reaching a
       handler. */
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGALRM);
    sigaddset(&blocked, SIGUSR1);
    sigaddset(&blocked, SIGUSR2);
    sigaddset(&blocked, SIGVTALRM);
    sigaddset(&blocked, SIGPROF);
    sigprocmask(SIG_BLOCK, &blocked, NULL);

    interval_timers();
    alarms();
#ifndef __APPLE__
    posix_timers();
#endif
#ifdef __linux__
    timerfds();
#endif
    sleep_and_timer();
    cpu_timers(argc > 2 && strcmp(argv[2], "cpu") == 0);

    shared = getenv("LIBTIMEWARP__SHARED_FILE");
    say("shared=%s\n", shared ? shared : "");
    return 0;
}
