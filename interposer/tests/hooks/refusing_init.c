/* A hook whose own `init` refuses the arena it was given, which is a bad value. */
#include <stdint.h>

#include "libtimewarp_hook.h"

int32_t libtimewarp_hook_init(void *arena, uint64_t size) {
    (void)arena;
    (void)size;
    return 1;
}

int32_t libtimewarp_hook(const struct libtimewarp_call *call, int64_t *out) {
    (void)call;
    (void)out;
    return LIBTIMEWARP_DECLINED;
}
