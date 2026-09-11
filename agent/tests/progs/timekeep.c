/* Reads the clock the way a runtime that skips libc does on FreeBSD: from the page of parameters
   the auxiliary vector names, extrapolated by hand.

   The arithmetic below is the kernel's own `binuptime`, step for step, and the structures are
   `struct vdso_timekeep` and `struct vdso_timehands` as a reader has them compiled in. A page read
   any other way would prove nothing about Go.

   With no arguments it reports one reading of each clock through the page, and one of each through
   libc beside it, so a case can compare the two. `fork` reports both again from a child, so a case
   can see that one page serves a whole tree. `tick <deadline> <budget>` prints both readings over
   and over until the wall clock passes the deadline, so a controller changing the session
   underneath can be watched from outside. */
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "report.h"

#define NS_PER_S 1000000000LL

/* The auxiliary vector entry naming where the timekeep page is mapped. */
#ifndef AT_TIMEKEEP
#define AT_TIMEKEEP 22
#endif

/* The one revision of the layout there has ever been, and the algorithm word for the counter each
   architecture reads in a single instruction. */
#define TK_VER_1 1
#define TH_ALGO_NATIVE 1

/* How many sets of parameters the kernel's own page carries. A reader follows the index, so ours
   using two of them changes nothing here. */
#define TH_NUM 4

struct bintime {
    int64_t sec;
    uint64_t frac;
};

struct timehands {
    uint32_t algo;
    uint32_t gen;
    uint64_t scale;
    uint32_t offset_count;
    uint32_t counter_mask;
    struct bintime offset;
    struct bintime boottime;
    uint32_t md[8]; /* th_x86_shift on x86_64, th_physical on aarch64, then reserved */
};

struct timekeep {
    uint32_t ver;
    uint32_t enabled;
    uint32_t current;
    uint32_t pad;
    struct timehands th[TH_NUM];
};

/* The counter the page names, taken the way the page says to take it. Zero when this reader cannot
   take that counter, which is what a kernel page on a counter of some other kind leaves. */
#if defined(__x86_64__)
static int counter_now(const struct timehands *th, uint32_t *out) {
    uint32_t low, high;

    if (th->algo != TH_ALGO_NATIVE) {
        return 0;
    }
    __asm__ __volatile__("lfence; rdtsc" : "=a"(low), "=d"(high));
    *out = (uint32_t)((((uint64_t)high << 32) | low) >> (th->md[0] & 63));
    return 1;
}
#elif defined(__aarch64__)
static int counter_now(const struct timehands *th, uint32_t *out) {
    uint64_t counted;

    if (th->algo != TH_ALGO_NATIVE) {
        return 0;
    }
    if (th->md[0]) {
        __asm__ __volatile__("isb; mrs %0, cntpct_el0" : "=r"(counted));
    } else {
        __asm__ __volatile__("isb; mrs %0, cntvct_el0" : "=r"(counted));
    }
    *out = (uint32_t)counted;
    return 1;
}
#else
#error "this program supports x86_64 and aarch64"
#endif

static void bintime_addx(struct bintime *bt, uint64_t by) {
    uint64_t was = bt->frac;

    bt->frac += by;
    if (was > bt->frac) {
        bt->sec++;
    }
}

static void bintime_add(struct bintime *bt, const struct bintime *by) {
    uint64_t was = bt->frac;

    bt->frac += by->frac;
    if (was > bt->frac) {
        bt->sec++;
    }
    bt->sec += by->sec;
}

static long long nanos_of(const struct bintime *bt) {
    return bt->sec * NS_PER_S + (long long)(((__uint128_t)bt->frac * NS_PER_S) >> 64);
}

/* Where the kernel, or we, left the page. */
static const struct timekeep *the_page(void) {
    void *at = NULL;

    if (elf_aux_info(AT_TIMEKEEP, &at, sizeof(at)) != 0) {
        return NULL;
    }
    return (const struct timekeep *)at;
}

/* The clock the page shows: the elapsed one, or the wall clock when `absolute`. Returns zero when
   the page carries nothing this reader can follow. */
static int page_read(const struct timekeep *tk, int absolute, long long *out) {
    int tries;

    if (!tk || tk->ver != TK_VER_1) {
        return 0;
    }
    for (tries = 0; tries < 64; tries++) {
        uint32_t current, gen, count, delta;
        const struct timehands *th;
        struct bintime shown;

        if (!__atomic_load_n(&tk->enabled, __ATOMIC_ACQUIRE)) {
            return 0;
        }
        current = __atomic_load_n(&tk->current, __ATOMIC_ACQUIRE) % TH_NUM;
        th = &tk->th[current];
        gen = __atomic_load_n(&th->gen, __ATOMIC_ACQUIRE);
        shown = th->offset;
        if (!counter_now(th, &count)) {
            return 0;
        }
        delta = (count - th->offset_count) & th->counter_mask;
        bintime_addx(&shown, th->scale * (uint64_t)delta);
        if (absolute) {
            bintime_add(&shown, &th->boottime);
        }
        __atomic_thread_fence(__ATOMIC_ACQUIRE);
        if (gen != 0 && gen == __atomic_load_n(&th->gen, __ATOMIC_RELAXED) &&
            current == __atomic_load_n(&tk->current, __ATOMIC_RELAXED) % TH_NUM) {
            *out = nanos_of(&shown);
            return 1;
        }
    }
    return 0;
}

static void report_page(const char *tag, const struct timekeep *tk) {
    long long wall = 0, elapsed = 0;
    int read_wall = page_read(tk, 1, &wall);
    int read_elapsed = page_read(tk, 0, &elapsed);
    int status = 0;

    /* Whether this process has a child it never started. The updater is spawned by a double fork
       whose intermediate is reaped on the spot, so a target that looks finds nothing. */
    printf("%s.children=%d\n", tag, waitpid(-1, &status, WNOHANG) > 0 ? 1 : 0);

    printf("%s.found=%d\n", tag, tk ? 1 : 0);
    printf("%s.enabled=%d\n", tag, tk && tk->enabled ? 1 : 0);
    printf("%s.ok=%d\n", tag, read_wall && read_elapsed ? 1 : 0);
    if (read_wall) {
        printf("%s.clock_gettime=%lld\n", tag, wall);
        printf("%s.time=%lld\n", tag, wall / NS_PER_S);
        printf("%s.gettimeofday=%lld\n", tag, wall);
    }
    if (read_elapsed) {
        printf("%s.monotonic=%lld\n", tag, elapsed);
    }
    fflush(stdout);
}

/* Both readings, over and over, until the wall clock passes `deadline` or `budget` runs out. */
static int tick(const struct timekeep *tk, long long deadline, long long budget) {
    long long index;

    for (index = 0; index < budget; index++) {
        struct timespec through_libc, pause = {0, 200000};
        long long through_page = 0;

        if (!page_read(tk, 1, &through_page)) {
            printf("tick.unreadable=1\n");
            fflush(stdout);
            return 1;
        }
        clock_gettime(CLOCK_REALTIME, &through_libc);
        printf("tick=%lld %lld\n", through_page,
               (long long)through_libc.tv_sec * NS_PER_S + through_libc.tv_nsec);
        if (through_page >= deadline &&
            (long long)through_libc.tv_sec * NS_PER_S + through_libc.tv_nsec >= deadline) {
            fflush(stdout);
            return 0;
        }
        /* Genuine time, since sleeps are not interposed here, so a faked rate cannot change the
           cadence. */
        nanosleep(&pause, NULL);
    }
    fflush(stdout);
    return 1;
}

int main(int argc, char **argv) {
    const struct timekeep *tk = the_page();
    const char *mode = argc > 1 ? argv[1] : "once";

    if (strcmp(mode, "tick") == 0) {
        if (argc < 4) {
            return 2;
        }
        return tick(tk, atoll(argv[2]), atoll(argv[3]));
    }

    report_page("page", tk);
    report("libc");

    if (strcmp(mode, "fork") == 0) {
        pid_t child = fork();
        int status = 0;

        if (child == 0) {
            /* The child inherited the address space, so it inherited the page and the entry that
               names it, with nothing of ours to do. */
            report_page("child_page", tk);
            report("child_libc");
            _exit(0);
        }
        if (child < 0) {
            return 1;
        }
        waitpid(child, &status, 0);
        return status == 0 ? 0 : 1;
    }
    return 0;
}
