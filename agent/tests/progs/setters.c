/* Every way a program can set the wall clock, and what each one answered.

   argv[1] is the instant to ask for, in nanoseconds. argv[2] is `nulls` to make the calls that
   pass a null pointer as well, and anything else to leave them out; `nulls.available` says which.
   Those calls are the library's to answer: it refuses them the way the kernel would, and a run
   that reaches the platform's own calls instead gets whatever that platform makes of a null, which
   on macOS is libc reading the pointer before the kernel ever sees it.

   `tai.available` says whether this platform has a TAI clock to name at all; FreeBSD grew one at
   15.0 and has none below that. `coarse.available` says the same for the cheap wall clock, which
   macOS does not have. */
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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

/* A delta no platform's own adjtime takes: a second more than the widest whose nanoseconds still
   fit a signed 64-bit count, which is the bound where the call is a system call taking the timeval
   as it comes, and far past the bound where libc folds the delta into a narrower field. */
#define BAD_DELTA_S (9223372036854775807LL / NS_PER_S + 1)

/* How far ahead of the instant asked for the last setter puts the clock, in seconds. */
#define LAST_S 7200

int main(int argc, char **argv) {
    long long target = argc > 1 ? atoll(argv[1]) : 0;
    time_t seconds = (time_t)(target / NS_PER_S);
    const struct timespec *nowhere = NULL;
    int nulls = argc > 2 && strcmp(argv[2], "nulls") == 0;

    say("target", target);

    /* Clocks the kernel will not set, however privileged the caller is. */
    set_clock("monotonic", CLOCK_MONOTONIC, seconds, 0);
#ifdef CLOCK_BOOTTIME
    set_clock("boottime", CLOCK_BOOTTIME, seconds, 0);
#else
    /* The name this platform gives the clock that counts while the machine is asleep. */
    set_clock("boottime", CLOCK_UPTIME_RAW, seconds, 0);
#endif
#ifdef CLOCK_TAI
    printf("tai.available=1\n");
    set_clock("tai", CLOCK_TAI, seconds, 0);
#else
    printf("tai.available=0\n");
#endif
#ifdef CLOCK_REALTIME_COARSE
    printf("coarse.available=1\n");
    set_clock("coarse", CLOCK_REALTIME_COARSE, seconds, 0);
#else
    printf("coarse.available=0\n");
#endif
    set_clock("bogus", (clockid_t)4242, seconds, 0);

    /* Fields outside what the call may carry. */
    set_clock("bad_nanos", CLOCK_REALTIME, seconds, NS_PER_S);
    set_clock("before_epoch", CLOCK_REALTIME, -1, 0);
    set_day("bad_micros", seconds, 1000000);
    adjust("bad_delta", (time_t)BAD_DELTA_S, 0);

    /* Pointers the call cannot read, and the question a null delta asks. A clock that cannot be
       set is found before the pointer is. The null goes through a variable so the compiler leaves a
       call it has been told takes no null alone. */
    printf("nulls.available=%d\n", nulls);
    if (nulls) {
        errno = 0;
        answered("null_realtime", clock_settime(CLOCK_REALTIME, nowhere));
        errno = 0;
        answered("null_monotonic", clock_settime(CLOCK_MONOTONIC, nowhere));
        errno = 0;
        answered("no_delta", adjtime(NULL, NULL));
    }

    /* A question rather than a set: whether this process may set the clock at all. */
    errno = 0;
    answered("probe", settimeofday(NULL, NULL));

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
