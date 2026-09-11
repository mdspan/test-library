/* Every way a program can set the wall clock, and what each one answered.

   argv[1] is the instant to ask for, in nanoseconds. The calls with null pointers mean this must
   run under the library: without us libc dereferences them where the kernel would answer EFAULT. */
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/time.h>
#include <time.h>

#define NS_PER_S 1000000000LL

static void say(const char *key, long long value) { printf("%s=%lld\n", key, value); }

/* The result of a call, and the errno beside it, which is only meaningful on a failure. */
static void answered(const char *tag, int result) {
    printf("%s.result=%d\n", tag, result);
    printf("%s.errno=%d\n", tag, result == 0 ? 0 : errno);
}

static long long wall_clock(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (long long)ts.tv_sec * NS_PER_S + ts.tv_nsec;
}

static void set_clock(const char *tag, clockid_t id, time_t seconds, long nanos) {
    struct timespec ts;
    ts.tv_sec = seconds;
    ts.tv_nsec = nanos;
    errno = 0;
    answered(tag, clock_settime(id, &ts));
}

static void set_day(const char *tag, time_t seconds, long micros) {
    struct timeval tv;
    tv.tv_sec = seconds;
    tv.tv_usec = micros;
    errno = 0;
    answered(tag, settimeofday(&tv, NULL));
}

static void adjust(const char *tag, time_t seconds, long micros) {
    struct timeval delta, old;
    delta.tv_sec = seconds;
    delta.tv_usec = micros;
    old.tv_sec = -1;
    old.tv_usec = -1;
    errno = 0;
    answered(tag, adjtime(&delta, &old));
    printf("%s.old_sec=%lld\n", tag, (long long)old.tv_sec);
    printf("%s.old_usec=%lld\n", tag, (long long)old.tv_usec);
}

/* How far the wall clock is asked to move by the one adjustment that is meant to land, in seconds.
   Inside what libc's own adjtime takes, which is about half an hour. */
#define ADJUST_S 600

/* How far ahead of the instant asked for the last setter puts the clock, in seconds. */
#define LAST_S 7200

int main(int argc, char **argv) {
    long long target = argc > 1 ? atoll(argv[1]) : 0;
    time_t seconds = (time_t)(target / NS_PER_S);
    const struct timespec *nowhere = NULL;

    say("target", target);

    /* Clocks the kernel will not set, however privileged the caller is. */
    set_clock("monotonic", CLOCK_MONOTONIC, seconds, 0);
    set_clock("boottime", CLOCK_BOOTTIME, seconds, 0);
    set_clock("tai", CLOCK_TAI, seconds, 0);
    set_clock("coarse", CLOCK_REALTIME_COARSE, seconds, 0);
    set_clock("bogus", (clockid_t)4242, seconds, 0);

    /* Fields outside what the call may carry. */
    set_clock("bad_nanos", CLOCK_REALTIME, seconds, NS_PER_S);
    set_clock("before_epoch", CLOCK_REALTIME, -1, 0);
    set_day("bad_micros", seconds, 1000000);
    adjust("bad_delta", 100000, 0);

    /* Pointers the call cannot read. A clock that cannot be set is found first. The null goes
       through a variable so the compiler leaves a call it has been told takes no null alone. */
    errno = 0;
    answered("null_realtime", clock_settime(CLOCK_REALTIME, nowhere));
    errno = 0;
    answered("null_monotonic", clock_settime(CLOCK_MONOTONIC, nowhere));

    /* A question rather than a set: whether this process may set the clock at all. */
    errno = 0;
    answered("probe", settimeofday(NULL, NULL));
    errno = 0;
    answered("no_delta", adjtime(NULL, NULL));

    /* And the three that move the clock, each read back afterwards. */
    say("before", wall_clock());
    set_clock("clock_settime", CLOCK_REALTIME, seconds, 0);
    say("after_clock_settime", wall_clock());
    adjust("adjtime", ADJUST_S, 0);
    say("after_adjtime", wall_clock());
    set_day("settimeofday", seconds + LAST_S, 0);
    say("after_settimeofday", wall_clock());

    say("adjust_s", ADJUST_S);
    say("last_s", LAST_S);
    return 0;
}
