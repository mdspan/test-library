/* A library that loads but answers nothing: it exports no `libtimewarp_hook`. */
#include <stdint.h>

#include "libtimewarp_hook.h"

int32_t libtimewarp_hook_init(void *arena, uint64_t size) {
    (void)arena;
    (void)size;
    return 0;
}
