/* Linked against the library rather than preloading it, so it can call the public symbol. */
#include "report.h"

extern int libtimewarp_genuine_clock_gettime(clockid_t clock, struct timespec *ts);

int main(void) {
    struct timespec faked, genuine;

    clock_gettime(CLOCK_REALTIME, &faked);
    libtimewarp_genuine_clock_gettime(CLOCK_REALTIME, &genuine);

    printf("faked=%lld\n", (long long)faked.tv_sec * 1000000000LL + faked.tv_nsec);
    printf("genuine=%lld\n", (long long)genuine.tv_sec * 1000000000LL + genuine.tv_nsec);
    fflush(stdout);
    return 0;
}
