/* Shared by every target program: one line per fact, `<tag>.<key>=<value>`. */
#ifndef REPORT_H
#define REPORT_H

#include <stdio.h>
#include <stdlib.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static void report(const char *tag) {
    struct timespec ts, mono;
    struct timeval tv;
    time_t seconds;
    const char *shared;

    clock_gettime(CLOCK_REALTIME, &ts);
    gettimeofday(&tv, NULL);
    seconds = time(NULL);
    clock_gettime(CLOCK_MONOTONIC, &mono);
    shared = getenv("LIBTIMEWARP__SHARED_FILE");

    printf("%s.pid=%d\n", tag, (int)getpid());
    printf("%s.clock_gettime=%lld\n", tag, (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec);
    printf("%s.gettimeofday=%lld\n", tag, (long long)tv.tv_sec * 1000000000LL + tv.tv_usec * 1000LL);
    printf("%s.time=%lld\n", tag, (long long)seconds);
    printf("%s.monotonic=%lld\n", tag, (long long)mono.tv_sec * 1000000000LL + mono.tv_nsec);
    printf("%s.shared=%s\n", tag, shared ? shared : "");
    fflush(stdout);
}

#endif
