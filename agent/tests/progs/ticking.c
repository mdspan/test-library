/* Prints the three clocks a session carries a line for, over and over, so a controller changing
   them underneath can be watched from outside.

   argv[1] is a wall-clock value in nanoseconds to stop at and argv[2] the most iterations to spend
   reaching it. A run that ends because the wall clock passed the value exits 0; one that runs out
   of iterations exits 1, so a caller whose change never reached the program learns that instead of
   reading a short stream as a pass. */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

#define NS_PER_S 1000000000LL

static long long nanos(const struct timespec *ts) {
    return (long long)ts->tv_sec * NS_PER_S + ts->tv_nsec;
}

int main(int argc, char **argv) {
    long long deadline = argc > 1 ? atoll(argv[1]) : 0;
    long long budget = argc > 2 ? atoll(argv[2]) : 0;
    /* A pause between readings, converted like any other sleep, so a fast clock only makes the
       stream denser. */
    struct timespec pause = {0, 200000};
    long long i;

    for (i = 0; i < budget; i++) {
        struct timespec wall, mono, boot;
        clock_gettime(CLOCK_REALTIME, &wall);
        clock_gettime(CLOCK_MONOTONIC, &mono);
        clock_gettime(CLOCK_BOOTTIME, &boot);
        printf("tick=%lld %lld %lld\n", nanos(&wall), nanos(&mono), nanos(&boot));
        if (nanos(&wall) >= deadline) {
            fflush(stdout);
            return 0;
        }
        nanosleep(&pause, NULL);
    }
    fflush(stdout);
    return 1;
}
