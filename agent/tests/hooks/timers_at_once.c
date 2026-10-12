/* Answers every timer arm with a genuine duration of nothing, which asks the timer to fire at once,
   and declines everything else. */
#include <stdint.h>

#include "libtimewarp_hook.h"

int32_t libtimewarp_hook(const struct libtimewarp_call *call, int64_t *out) {
    if (call->kind != LIBTIMEWARP_DURATION_TO_GENUINE) {
        return LIBTIMEWARP_DECLINED;
    }
    switch (call->func) {
    case LIBTIMEWARP_FUNC_SETITIMER:
    case LIBTIMEWARP_FUNC_ALARM:
    case LIBTIMEWARP_FUNC_TIMER_SETTIME:
    case LIBTIMEWARP_FUNC_TIMERFD_SETTIME:
        *out = 0;
        return LIBTIMEWARP_HANDLED;
    default:
        return LIBTIMEWARP_DECLINED;
    }
}
