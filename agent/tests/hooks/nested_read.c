/*
 * A hook that builds its answer out of the two readings the library offers it.
 *
 * Asked for the monotonic clock, it reads the wall clock twice: once the ordinary way, which reaches
 * the timekeeper rather than this hook again, and once through the library's genuine-clock symbol.
 * It leaves both in the arena for whoever wants to read them afterwards, and answers with the faked
 * one. Every other call is declined.
 */
#include <stdint.h>
#include <string.h>
#include <time.h>

#include "libtimewarp_hook.h"

/* Where the two readings land in the arena. */
#define NESTED 0
#define GENUINE 1

static int64_t nanos(const struct timespec *ts) {
    return (int64_t)ts->tv_sec * 1000000000LL + (int64_t)ts->tv_nsec;
}

int32_t libtimewarp_hook(const struct libtimewarp_call *call, int64_t *out) {
    struct timespec faked, genuine;
    int64_t words[2];

    if (call->kind != LIBTIMEWARP_INSTANT_TO_FAKE || call->clock != CLOCK_MONOTONIC) {
        return LIBTIMEWARP_DECLINED;
    }

    /* The nested read skips this hook and takes the keeper's answer, so it cannot recurse. */
    clock_gettime(CLOCK_REALTIME, &faked);
    libtimewarp_genuine_clock_gettime(CLOCK_REALTIME, &genuine);
    words[NESTED] = nanos(&faked);
    words[GENUINE] = nanos(&genuine);

    if (call->arena != NULL && call->size >= sizeof words) {
        memcpy(call->arena, words, sizeof words);
    }
    *out = words[NESTED];
    return LIBTIMEWARP_HANDLED;
}
