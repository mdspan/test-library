/*
 * The worked example: a simulator keeps its own time in the arena, and the wall clock reads it.
 *
 * `init` puts the simulation's starting instant in the arena, once, before any process of the
 * session can see it. Every wall-clock read then answers with the word in the arena, whichever
 * process asks, and everything else is declined so the timekeeper answers it as usual.
 */
#include <stdint.h>
#include <string.h>
#include <time.h>

#include "libtimewarp_hook.h"

/* 2020-01-01T00:00:00Z, where the simulation starts. */
#define START_NS 1577836800000000000LL

/* 2000-01-01T00:00:00Z, answered when the session was given no arena to read. */
#define NO_ARENA_NS 946684800000000000LL

int32_t libtimewarp_hook_init(void *arena, uint64_t size) {
    int64_t start = START_NS;

    if (arena != NULL && size >= sizeof start) {
        memcpy(arena, &start, sizeof start);
    }
    return 0;
}

int32_t libtimewarp_hook(const struct libtimewarp_call *call, int64_t *out) {
    /* The shape is enough to recognise a clock read; the function name is not needed. */
    if (call->kind != LIBTIMEWARP_INSTANT_TO_FAKE || call->clock != CLOCK_REALTIME) {
        return LIBTIMEWARP_DECLINED;
    }
    if (call->arena == NULL || call->size < sizeof *out) {
        *out = NO_ARENA_NS;
        return LIBTIMEWARP_HANDLED;
    }
    memcpy(out, call->arena, sizeof *out);
    return LIBTIMEWARP_HANDLED;
}
