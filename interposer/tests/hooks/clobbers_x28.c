/*
 * A hook that declines every call after spinning with x28 overwritten, as compiled C may leave
 * it at any instruction. The Go runtime keeps its goroutine pointer there on aarch64, and its
 * signal handler takes the pointer from that register, so a signal handled while the spin runs on
 * a Go thread sees the value below in its place. On other architectures the hook only spins.
 */
#include <stdint.h>

#include "libtimewarp_hook.h"

/* How long each call holds the register: a few microseconds on hardware. */
#define SPINS 4000u

int32_t libtimewarp_hook(const struct libtimewarp_call *call, int64_t *out) {
    uint64_t spins = SPINS;

    (void)call;
    (void)out;
#if defined(__aarch64__)
    __asm__ volatile("mov x28, #0x2800\n"
                     "0:\n"
                     "subs %[spins], %[spins], #1\n"
                     "b.ne 0b\n"
                     : [spins] "+r"(spins)
                     :
                     : "x28", "cc");
#else
    while (spins != 0) {
        __asm__ volatile("" : "+r"(spins));
        spins--;
    }
#endif
    return LIBTIMEWARP_DECLINED;
}
