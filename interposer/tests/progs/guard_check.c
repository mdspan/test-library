/* What a wrapper hands the process it runs, asked without the library: whether the kernel would
   let this process set the clock (`privileged`), and whether something in front of the kernel
   refuses every set before the kernel looks at it (`guarded`). Each call here is one the kernel
   would refuse or answer without moving any clock, so `guarded` holds only where each of them is
   refused with EPERM where a privileged process would have heard something else.

   Linux asks the privilege question the way the library does, with a settimeofday that names
   neither a time nor a timezone. macOS has the library ask for effective uid 0, and checks for a
   sandbox that refuses settimeofday and adjtime with nothing to set. FreeBSD has no guard. */
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>
#ifdef __linux__
#include <sys/syscall.h>
#include <sys/timex.h>
#endif

/* Whether a call answered -1 with EPERM. */
static int refused(long result) { return result == -1 && errno == EPERM; }

#ifdef __linux__
static int privileged(void) { return syscall(SYS_settimeofday, NULL, NULL) == 0; }

static int guarded(void) {
    struct timeval bad_micros = {0, -1};
    struct timex absurd;
    int all = 1;

    /* The kernel's own answers to a privileged caller: EFAULT for the null time, EINVAL for the
       fraction, and EINVAL for a tick of zero. */
    errno = 0;
    all &= refused(syscall(SYS_clock_settime, CLOCK_REALTIME, NULL));
    errno = 0;
    all &= refused(syscall(SYS_settimeofday, &bad_micros, NULL));
    memset(&absurd, 0, sizeof absurd);
    absurd.modes = ADJ_TICK;
    errno = 0;
    all &= refused(syscall(SYS_adjtimex, &absurd));
    memset(&absurd, 0, sizeof absurd);
    absurd.modes = ADJ_TICK;
    errno = 0;
    all &= refused(syscall(SYS_clock_adjtime, CLOCK_REALTIME, &absurd));
    return all;
}
#elif defined(__APPLE__)
static int privileged(void) { return geteuid() == 0; }

static int guarded(void) {
    int all = 1;

    /* The kernel's own answers to the superuser: success for a settimeofday with nothing to set,
       and EFAULT for an adjtime with no delta to read. */
    errno = 0;
    all &= refused(settimeofday(NULL, NULL));
    errno = 0;
    all &= refused(adjtime(NULL, NULL));
    return all;
}
#else
static int privileged(void) { return geteuid() == 0; }
static int guarded(void) { return 0; }
#endif

int main(void) {
    printf("privileged=%d\n", privileged());
    printf("guarded=%d\n", guarded());
    return 0;
}
