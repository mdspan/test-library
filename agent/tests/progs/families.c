/* Every clock of every family, faked and genuine, read twice with real time passing between.
   Linked against the library rather than preloading it, so the genuine reading of the same clock
   comes from the library's own symbol and the two can be compared inside one process. */
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

extern int libtimewarp_genuine_clock_gettime(clockid_t clock, struct timespec *ts);

/* Each platform's own column of the family table, under keys the cases name. Which family a clock
   belongs to is not spelled here: every id is reported beside its readings, and the case groups
   them by asking the library's own table, which on FreeBSD depends on the running kernel. */
static const struct {
    const char *name;
    clockid_t id;
} CLOCKS[] = {
#ifdef __FreeBSD__
    {"realtime", CLOCK_REALTIME},
    {"realtime_precise", CLOCK_REALTIME_PRECISE},
    {"realtime_fast", CLOCK_REALTIME_FAST},
    {"second", CLOCK_SECOND},
    {"monotonic", CLOCK_MONOTONIC},
    {"monotonic_precise", CLOCK_MONOTONIC_PRECISE},
    {"monotonic_fast", CLOCK_MONOTONIC_FAST},
    {"uptime", CLOCK_UPTIME},
    {"uptime_precise", CLOCK_UPTIME_PRECISE},
    {"uptime_fast", CLOCK_UPTIME_FAST},
    {"process_cpu", CLOCK_PROCESS_CPUTIME_ID},
    {"thread_cpu", CLOCK_THREAD_CPUTIME_ID},
    {"virtual", CLOCK_VIRTUAL},
    {"prof", CLOCK_PROF},
    /* 16 is CLOCK_TAI, which exists from 15.0 and which older headers do not name. */
    {"tai", 16},
#else
    {"realtime", CLOCK_REALTIME},
    {"realtime_coarse", CLOCK_REALTIME_COARSE},
    {"realtime_alarm", CLOCK_REALTIME_ALARM},
    {"tai", CLOCK_TAI},
    {"monotonic", CLOCK_MONOTONIC},
    {"monotonic_coarse", CLOCK_MONOTONIC_COARSE},
    {"monotonic_raw", CLOCK_MONOTONIC_RAW},
    {"boottime", CLOCK_BOOTTIME},
    {"boottime_alarm", CLOCK_BOOTTIME_ALARM},
    {"process_cpu", CLOCK_PROCESS_CPUTIME_ID},
    {"thread_cpu", CLOCK_THREAD_CPUTIME_ID},
#endif
};

static long long nanos(const struct timespec *ts) {
    return (long long)ts->tv_sec * 1000000000LL + ts->tv_nsec;
}

static long long genuine(clockid_t id) {
    struct timespec ts;
    if (libtimewarp_genuine_clock_gettime(id, &ts) != 0) {
        return -1;
    }
    return nanos(&ts);
}

static long long faked(clockid_t id) {
    struct timespec ts;
    if (clock_gettime(id, &ts) != 0) {
        return -1;
    }
    return nanos(&ts);
}

/* Spends `ns` of genuine time consuming CPU, so the Cpu family has something to scale. The bound
   is a genuine reading, or a faked rate would change how long this runs for. */
static void burn(long long ns) {
    long long start = genuine(CLOCK_MONOTONIC);
    volatile unsigned long sink = 0;
    while (genuine(CLOCK_MONOTONIC) - start < ns) {
        for (int i = 0; i < 10000; i++) {
            sink += (unsigned long)i;
        }
    }
}

static void round_of(const char *tag) {
    for (size_t i = 0; i < sizeof CLOCKS / sizeof CLOCKS[0]; i++) {
        printf("%s.%s=%lld\n", tag, CLOCKS[i].name, faked(CLOCKS[i].id));
        printf("%s.%s.genuine=%lld\n", tag, CLOCKS[i].name, genuine(CLOCKS[i].id));
    }
}

/* The id behind every name above, so a case can ask the library's table which family each is in. */
static void ids(void) {
    for (size_t i = 0; i < sizeof CLOCKS / sizeof CLOCKS[0]; i++) {
        printf("id.%s=%d\n", CLOCKS[i].name, (int)CLOCKS[i].id);
    }
}

int main(void) {
    struct timespec pause = {0, 200000000};
    clockid_t own;

    ids();
    burn(50000000);
    round_of("a");

    /* The library does not interpose sleeps yet, so this is genuine time passing. */
    nanosleep(&pause, NULL);
    burn(50000000);
    round_of("b");

    /* An id from `clock_getcpuclockid` is a reading about a process rather than a clock this
       process consumes, so it stays genuine even for our own pid. */
    if (clock_getcpuclockid(getpid(), &own) == 0) {
        printf("own_cpuclockid=%lld\n", faked(own));
        printf("own_cpuclockid.genuine=%lld\n", genuine(own));
    }

    {
        const char *shared = getenv("LIBTIMEWARP__SHARED_FILE");
        printf("shared=%s\n", shared ? shared : "");
    }
    fflush(stdout);
    return 0;
}
