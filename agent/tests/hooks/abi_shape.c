/*
 * Nothing runs this: it is here so the compiler checks that the shipped header is a header a hook
 * author can include and build against, and that the call struct is the shape the library reads.
 */
#include <stddef.h>
#include <stdint.h>

#include "libtimewarp_hook.h"

#define CHECK(what, offset) \
    _Static_assert(offsetof(struct libtimewarp_call, what) == (offset), #what " moved")

CHECK(func, 0);
CHECK(kind, 4);
CHECK(clock, 8);
CHECK(value, 16);
CHECK(real_ns, 24);
CHECK(keeper, 32);
CHECK(arena, 40);
CHECK(size, 48);
_Static_assert(sizeof(struct libtimewarp_call) == 56, "the call struct changed size");

/* Declining is zero, so a hook that falls through its switch leaves `*out` alone. */
_Static_assert(LIBTIMEWARP_DECLINED == 0, "declining is not zero");

int32_t libtimewarp_hook(const struct libtimewarp_call *call, int64_t *out) {
    switch (call->kind) {
    case LIBTIMEWARP_INSTANT_TO_FAKE:
    case LIBTIMEWARP_INSTANT_TO_GENUINE:
    case LIBTIMEWARP_DURATION_TO_GENUINE:
    case LIBTIMEWARP_DURATION_TO_FAKE:
        *out = call->keeper;
        return LIBTIMEWARP_HANDLED;
    default:
        return LIBTIMEWARP_DECLINED;
    }
}
