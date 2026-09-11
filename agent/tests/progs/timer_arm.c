/* Arms one timer for a fake duration and blocks until it fires, reporting the genuine time it
   took, so a suite case can change the session while the timer is already armed, or hold it under
   a frozen clock and see that nothing releases it.

   Linked against the library rather than preloading it, so the genuine clock comes from the
   library's own symbol. argv[1] is the fake duration in milliseconds. `disarmed=` goes out first,
   carrying what a timer disarmed on this session reports it has left, and `armed=1` after the
   conversion, so a case knows exactly when to make its change. */
#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

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

static struct timespec spec_of(long long ns) {
    struct timespec ts;
    ts.tv_sec = (time_t)(ns / NS_PER_S);
    ts.tv_nsec = (long)(ns % NS_PER_S);
    return ts;
}

int main(int argc, char **argv) {
    struct itimerspec new_value, current;
    long long fake, started;
    uint64_t ticks = 0;
    int fd;

    if (argc < 2) {
        return 2;
    }
    fake = atoll(argv[1]) * 1000000LL;

    fd = timerfd_create(CLOCK_MONOTONIC, 0);
    if (fd < 0) {
        return 3;
    }

    /* Armed and then disarmed: a timer with nothing left reports nothing left, whatever the rate,
       so a freeze cannot turn a disarm into the longest duration there is. */
    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = spec_of(fake);
    timerfd_settime(fd, 0, &new_value, NULL);
    memset(&new_value, 0, sizeof new_value);
    timerfd_settime(fd, 0, &new_value, NULL);
    memset(&current, 0, sizeof current);
    timerfd_gettime(fd, &current);
    printf("disarmed=%lld\n", nanos(&current.it_value));

    memset(&new_value, 0, sizeof new_value);
    new_value.it_value = spec_of(fake);
    printf("fake=%lld\n", fake);
    timerfd_settime(fd, 0, &new_value, NULL);
    printf("armed=1\n");
    fflush(stdout);

    started = genuine_mono();
    if (read(fd, &ticks, sizeof ticks) != (ssize_t)sizeof ticks) {
        return 4;
    }
    printf("genuine=%lld\n", genuine_mono() - started);
    fflush(stdout);
    return 0;
}
