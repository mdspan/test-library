/* The Mach clocks, each beside the `clock_gettime` id that reads the same counter.

   macOS only: nothing here exists on the other two platforms, and the suite case that runs this is
   named against this one.

   Preloaded rather than linked, which is how a program meets the library on this platform, so the
   genuine-clock symbol is asked of the loader at run time rather than named at link time. A run
   without the library finds none and says so.

   Every reading is printed in nanoseconds. The ticks of the four `mach_*_time` counters are
   converted here with `mach_timebase_info`, exactly as a program of the user's would convert them,
   so a case comparing one against its `clock_gettime` counterpart is comparing our round trip
   through the timebase against the program's own.

   argv[1], when given, is the fake duration in milliseconds that `mach_wait_until` is asked to
   sleep; without it that wait is skipped. */
#include <dlfcn.h>
#include <mach/clock.h>
#include <mach/clock_types.h>
#include <mach/mach.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

#define NS_PER_S 1000000000LL

/* The library's own way back to the genuine clock, or NULL in a run without the library. */
static int (*genuine_clock)(clockid_t, struct timespec *);

/* The ratio the counters convert with: 1:1 on Intel, 125:3 on Apple silicon. */
static mach_timebase_info_data_t timebase;

/* The two clock ports, or MACH_PORT_NULL where the kernel would not hand one out. */
static mach_port_t calendar_port = MACH_PORT_NULL;
static mach_port_t system_port = MACH_PORT_NULL;

static void say(const char *key, long long value) { printf("%s=%lld\n", key, value); }

static long long nanos(const struct timespec *ts) {
    return (long long)ts->tv_sec * NS_PER_S + ts->tv_nsec;
}

/* A count of ticks in nanoseconds, in the width the product needs. */
static long long ticks_ns(uint64_t ticks) {
    return (long long)((unsigned __int128)ticks * timebase.numer / timebase.denom);
}

/* The other direction, for the deadline `mach_wait_until` is given. */
static uint64_t ns_ticks(long long ns) {
    return (uint64_t)((unsigned __int128)ns * timebase.denom / timebase.numer);
}

static long long faked(clockid_t id) {
    struct timespec ts;
    if (clock_gettime(id, &ts) != 0) {
        return -1;
    }
    return nanos(&ts);
}

static long long genuine(clockid_t id) {
    struct timespec ts;
    if (genuine_clock == NULL || genuine_clock(id, &ts) != 0) {
        return -1;
    }
    return nanos(&ts);
}

/* A read on a clock port in nanoseconds, or -1 where there is no port to read. */
static long long port_time(mach_port_t port) {
    mach_timespec_t mts;
    if (port == MACH_PORT_NULL || clock_get_time(port, &mts) != KERN_SUCCESS) {
        return -1;
    }
    return (long long)mts.tv_sec * NS_PER_S + (long long)mts.tv_nsec;
}

/* One round of every reading, under `tag`. The Mach call comes first in each pair and the
   `clock_gettime` id second, so a pair that disagrees disagrees by more than the gap between two
   calls. */
static void round_of(const char *tag) {
    char key[64];

#define SAY(name, value)                                                                           \
    do {                                                                                           \
        snprintf(key, sizeof key, "%s.%s", tag, name);                                             \
        say(key, (value));                                                                         \
    } while (0)

    /* The genuine side of the three families first, so that the window between a reading and the
       genuine one it is measured against is as narrow as the two calls are apart. */
    SAY("uptime_raw.genuine", genuine(CLOCK_UPTIME_RAW));
    SAY("monotonic_raw.genuine", genuine(CLOCK_MONOTONIC_RAW));
    SAY("realtime.genuine", genuine(CLOCK_REALTIME));

    /* The counter that stops while the machine sleeps, which is the Monotonic family's. */
    SAY("absolute", ticks_ns(mach_absolute_time()));
    SAY("uptime_raw", faked(CLOCK_UPTIME_RAW));
    SAY("uptime_raw_nsec", (long long)clock_gettime_nsec_np(CLOCK_UPTIME_RAW));
    SAY("approximate", ticks_ns(mach_approximate_time()));
    SAY("uptime_raw_approx", faked(CLOCK_UPTIME_RAW_APPROX));

    /* The counter that keeps counting while it sleeps, which is Boottime's. */
    SAY("continuous", ticks_ns(mach_continuous_time()));
    SAY("monotonic_raw", faked(CLOCK_MONOTONIC_RAW));
    SAY("monotonic_raw_nsec", (long long)clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW));
    SAY("continuous_approximate", ticks_ns(mach_continuous_approximate_time()));
    SAY("monotonic_raw_approx", faked(CLOCK_MONOTONIC_RAW_APPROX));

    /* The wall clock, and the two ports a program asks the host for. */
    SAY("realtime", faked(CLOCK_REALTIME));
    SAY("realtime_nsec", (long long)clock_gettime_nsec_np(CLOCK_REALTIME));
    SAY("calendar", port_time(calendar_port));
    SAY("system", port_time(system_port));

#undef SAY
}

/* Spends `ns` of genuine time, so a round has something to advance over that no rate of the
   session's changes the length of. A run with no library to ask has no genuine clock either, and
   sleeps instead. */
static void burn(long long ns) {
    long long start;
    volatile unsigned long sink = 0;
    if (genuine_clock == NULL) {
        struct timespec pause = {0, 50000000};
        nanosleep(&pause, NULL);
        return;
    }
    start = genuine(CLOCK_UPTIME_RAW);
    while (genuine(CLOCK_UPTIME_RAW) - start < ns) {
        for (int i = 0; i < 10000; i++) {
            sink += (unsigned long)i;
        }
    }
}

/* A sleep to an absolute deadline in ticks, measured against the genuine clock it blocked on. */
static void wait_until(long long fake_ns) {
    uint64_t deadline = mach_absolute_time() + ns_ticks(fake_ns);
    long long started = genuine(CLOCK_UPTIME_RAW);
    kern_return_t result = mach_wait_until(deadline);
    say("wait.rc", result);
    say("wait.fake", fake_ns);
    say("wait.genuine", genuine(CLOCK_UPTIME_RAW) - started);
}

int main(int argc, char **argv) {
    const char *shared;

    genuine_clock = (int (*)(clockid_t, struct timespec *))dlsym(
        RTLD_DEFAULT, "libtimewarp_genuine_clock_gettime");
    say("genuine.available", genuine_clock != NULL);

    if (mach_timebase_info(&timebase) != KERN_SUCCESS) {
        fprintf(stderr, "no mach timebase\n");
        return 1;
    }
    say("timebase.numer", timebase.numer);
    say("timebase.denom", timebase.denom);

    /* The ports, which name the clock rather than read it. `SYSTEM_CLOCK` counts from boot without
       suspend and `CALENDAR_CLOCK` is the wall clock. */
    say("calendar.rc", host_get_clock_service(mach_host_self(), CALENDAR_CLOCK, &calendar_port));
    say("system.rc", host_get_clock_service(mach_host_self(), SYSTEM_CLOCK, &system_port));

    round_of("a");
    /* Genuine time passing between the two rounds, rather than a sleep: a sleep would be converted
       like any other, and a frozen family owes one the longest duration there is. */
    burn(50000000);
    round_of("b");

    if (argc > 1) {
        wait_until(atoll(argv[1]) * 1000000LL);
    }

    shared = getenv("LIBTIMEWARP__SHARED_FILE");
    printf("shared=%s\n", shared ? shared : "");
    fflush(stdout);
    return 0;
}
