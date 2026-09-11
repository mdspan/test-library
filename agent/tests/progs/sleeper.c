/* Sleeps one fake duration and reports the genuine time it actually blocked, so a suite case can
   change the session while the sleep is already under way.

   Linked against the library rather than preloading it, so the genuine clock comes from the
   library's own symbol. argv[1] is the fake duration in milliseconds. `sleeping=1` goes out before
   the sleep, which is after the conversion, so a case knows exactly when to make its change. */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

extern int libtimewarp_genuine_clock_gettime(clockid_t clock, struct timespec *ts);

#define NS_PER_S 1000000000LL

static long long genuine_mono(void) {
    struct timespec ts;
    if (libtimewarp_genuine_clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return (long long)ts.tv_sec * NS_PER_S + ts.tv_nsec;
}

int main(int argc, char **argv) {
    struct timespec request;
    long long fake, started;

    if (argc < 2) {
        return 2;
    }
    fake = atoll(argv[1]) * 1000000LL;
    request.tv_sec = (time_t)(fake / NS_PER_S);
    request.tv_nsec = (long)(fake % NS_PER_S);

    printf("fake=%lld\n", fake);
    printf("sleeping=1\n");
    fflush(stdout);

    started = genuine_mono();
    nanosleep(&request, NULL);
    printf("genuine=%lld\n", genuine_mono() - started);
    fflush(stdout);
    return 0;
}
