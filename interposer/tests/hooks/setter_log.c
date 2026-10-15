/*
 * A hook that takes every set into its own state, so the keeper never runs one, and records what
 * it was handed. Every other call is declined, which leaves the program's own clock readings the
 * keeper's.
 *
 * The arena is the log: the first word counts the setters that follow, and each entry is the
 * function, the kind, the clock id, and the instant the program asked for.
 *
 * It writes a number into *out that no clock could ever show, because a handled setter means the
 * program sees success and *out is ignored.
 */
#include <stdint.h>

#include "libtimewarp_hook.h"

/* The words each recorded setter takes up in the log. */
#define ENTRY 4

/* What a handled setter is answered with, which nothing reads. */
#define NONSENSE (-424242)

static int is_a_setter(uint32_t func) {
    return func == LIBTIMEWARP_FUNC_SETTIMEOFDAY || func == LIBTIMEWARP_FUNC_CLOCK_SETTIME ||
           func == LIBTIMEWARP_FUNC_ADJTIME;
}

int32_t libtimewarp_hook(const struct libtimewarp_call *call, int64_t *out) {
    int64_t *log = (int64_t *)call->arena;
    uint64_t capacity;

    if (!is_a_setter(call->func)) {
        return LIBTIMEWARP_DECLINED;
    }
    if (log != NULL && call->size >= (ENTRY + 1) * sizeof(int64_t)) {
        capacity = (call->size / sizeof(int64_t) - 1) / ENTRY;
        if (log[0] >= 0 && (uint64_t)log[0] < capacity) {
            int64_t *slot = log + 1 + log[0] * ENTRY;
            slot[0] = (int64_t)call->func;
            slot[1] = (int64_t)call->kind;
            slot[2] = (int64_t)call->clock;
            slot[3] = call->value;
            log[0] += 1;
        }
    }
    *out = NONSENSE;
    return LIBTIMEWARP_HANDLED;
}
