/*
 * Records every call it is handed and declines it, so a suite case can read back which function
 * reached the hook, in which shape, naming which clock, while the program's own readings stay the
 * keeper's.
 *
 * The arena is the log: the first word counts the triples that follow, and each triple is the
 * function, the kind, and the clock id.
 */
#include <stdint.h>

#include "libtimewarp_hook.h"

int32_t libtimewarp_hook(const struct libtimewarp_call *call, int64_t *out) {
    int64_t *log = (int64_t *)call->arena;
    uint64_t capacity;

    (void)out;
    if (log == NULL || call->size < 4 * sizeof(int64_t)) {
        return LIBTIMEWARP_DECLINED;
    }
    capacity = (call->size / sizeof(int64_t) - 1) / 3;
    if (log[0] >= 0 && (uint64_t)log[0] < capacity) {
        int64_t *slot = log + 1 + log[0] * 3;
        slot[0] = (int64_t)call->func;
        slot[1] = (int64_t)call->kind;
        slot[2] = (int64_t)call->clock;
        log[0] += 1;
    }
    return LIBTIMEWARP_DECLINED;
}
