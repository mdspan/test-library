/* Reads the clock the way a runtime that skips libc does: through the object the auxiliary vector
   names, found and called by hand.

   The walk below is the one the Go runtime makes, step for step: the program headers for the load
   segment and the dynamic array, the dynamic array for the symbol table, the strings and the hash
   table, the version definitions for the version the entries are defined under, and then the hash
   chain for the name. A symbol found any other way would prove nothing about Go.

   With no arguments it reports one reading through each entry, and one of each through libc beside
   it, so a case can compare the two. With arguments it is argv[1] a wall-clock value in nanoseconds
   to stop at and argv[2] the most iterations to spend reaching it, printing both readings each
   time, so a controller changing the session underneath can be watched from outside. */
#define _GNU_SOURCE
#include <elf.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/time.h>
#include <time.h>

#include "report.h"

#define NS_PER_S 1000000000LL

#if defined(__x86_64__)
#define VDSO_VERSION "LINUX_2.6"
#define VDSO_CLOCK_GETTIME "__vdso_clock_gettime"
#define VDSO_GETTIMEOFDAY "__vdso_gettimeofday"
#define VDSO_TIME "__vdso_time"
#define VDSO_CLOCK_GETRES "__vdso_clock_getres"
#define VDSO_GETCPU "__vdso_getcpu"
/* A name the kernel's own object carries and ours does not, which is what says which of the two
   the auxiliary vector points at. Here the kernel defines the whole group twice, versioned and
   bare, and ours defines the versioned names alone. */
#define VDSO_KERNELS_OWN "clock_gettime"
#elif defined(__aarch64__)
#define VDSO_VERSION "LINUX_2.6.39"
#define VDSO_CLOCK_GETTIME "__kernel_clock_gettime"
#define VDSO_GETTIMEOFDAY "__kernel_gettimeofday"
#define VDSO_TIME "__kernel_time"
#define VDSO_CLOCK_GETRES "__kernel_clock_getres"
#define VDSO_GETCPU "__kernel_getcpu"
/* The same, where the kernel defines no bare name to ask for: it hides everything but the four
   `__kernel_` names, and the signal trampoline among them is the one no synthetic object has a
   reason to carry. It is `SYM_CODE_START` in the kernel, so it carries no function type, which is
   why the probe below asks for a name rather than for an entry to call. */
#define VDSO_KERNELS_OWN "__kernel_rt_sigreturn"
#else
#error "this program supports x86_64 and aarch64"
#endif

/* The entries answer in the kernel's convention: zero, or the negative of the refusal. */
typedef long (*clock_gettime_fn)(clockid_t, struct timespec *);
typedef long (*gettimeofday_fn)(struct timeval *, void *);
typedef time_t (*time_fn)(time_t *);
typedef long (*clock_getres_fn)(clockid_t, struct timespec *);
typedef long (*getcpu_fn)(unsigned *, unsigned *, void *);

struct vdso {
    size_t base; /* what a symbol's value is counted from */
    size_t addr; /* where the object itself is mapped */
    Elf64_Sym *sym;
    const char *str;
    Elf64_Word *hash;
    Elf64_Half *versym;
    Elf64_Verdef *verdef;
};

static unsigned elf_hash(const char *name) {
    unsigned value = 0, high;
    for (; *name; name++) {
        value = (value << 4) + (unsigned char)*name;
        if ((high = value & 0xf0000000)) {
            value ^= high >> 24;
        }
        value &= ~high;
    }
    return value;
}

static int vdso_open(struct vdso *v) {
    size_t addr = (size_t)getauxval(AT_SYSINFO_EHDR);
    Elf64_Ehdr *eh;
    Elf64_Phdr *ph;
    Elf64_Dyn *dyn = 0;
    int loaded = 0;
    int i;

    memset(v, 0, sizeof(*v));
    if (!addr) {
        return 0;
    }
    v->addr = addr;
    eh = (Elf64_Ehdr *)addr;
    if (memcmp(eh->e_ident, ELFMAG, SELFMAG) != 0) {
        return 0;
    }
    ph = (Elf64_Phdr *)(addr + eh->e_phoff);
    for (i = 0; i < eh->e_phnum; i++) {
        Elf64_Phdr *p = (Elf64_Phdr *)((char *)ph + (size_t)i * eh->e_phentsize);
        if (p->p_type == PT_LOAD && !loaded) {
            v->base = addr + p->p_offset - p->p_vaddr;
            loaded = 1;
        } else if (p->p_type == PT_DYNAMIC) {
            dyn = (Elf64_Dyn *)(addr + p->p_offset);
        }
    }
    if (!loaded || !dyn) {
        return 0;
    }
    for (; dyn->d_tag != DT_NULL; dyn++) {
        size_t at = v->base + dyn->d_un.d_ptr;
        switch (dyn->d_tag) {
        case DT_STRTAB: v->str = (const char *)at; break;
        case DT_SYMTAB: v->sym = (Elf64_Sym *)at; break;
        case DT_HASH: v->hash = (Elf64_Word *)at; break;
        case DT_VERSYM: v->versym = (Elf64_Half *)at; break;
        case DT_VERDEF: v->verdef = (Elf64_Verdef *)at; break;
        default: break;
        }
    }
    return v->str && v->sym && v->hash;
}

/* The index the version's own definition carries, or -1 when the object defines no such version. */
static int vdso_version(const struct vdso *v, const char *want) {
    Elf64_Verdef *d = v->verdef;
    unsigned wanted = elf_hash(want);

    if (!d) {
        return -1;
    }
    for (;;) {
        if (d->vd_version != 1) {
            return -1;
        }
        if (d->vd_hash == wanted) {
            Elf64_Verdaux *aux = (Elf64_Verdaux *)((char *)d + d->vd_aux);
            if (strcmp(v->str + aux->vda_name, want) == 0) {
                return (int)(d->vd_ndx & 0x7fff);
            }
        }
        if (!d->vd_next) {
            return -1;
        }
        d = (Elf64_Verdef *)((char *)d + d->vd_next);
    }
}

/* Walks the hash chain for `name`. A `version` below zero asks for the name under any version,
   which is how the aliases are found, and a `callable` of zero takes a name of any type, which is
   how a name that is there to be found rather than called is. */
static void *lookup(const struct vdso *v, const char *name, int version, int callable) {
    unsigned nbucket, *bucket, *chain, index;

    nbucket = v->hash[0];
    bucket = (unsigned *)(v->hash + 2);
    chain = bucket + nbucket;
    for (index = bucket[elf_hash(name) % nbucket]; index; index = chain[index]) {
        Elf64_Sym *s = &v->sym[index];
        if (s->st_shndx == SHN_UNDEF || !s->st_value) {
            continue;
        }
        if (callable && ELF64_ST_TYPE(s->st_info) != STT_FUNC) {
            continue;
        }
        if (strcmp(v->str + s->st_name, name) != 0) {
            continue;
        }
        if (version >= 0 && v->versym && (v->versym[index] & 0x7fff) != version) {
            continue;
        }
        return (void *)(v->base + s->st_value);
    }
    return 0;
}

/* A name under the version the entries are defined by, which is what a runtime asks for. */
static void *vdso_sym(const struct vdso *v, const char *name) {
    int version = vdso_version(v, VDSO_VERSION);

    return version < 0 ? 0 : lookup(v, name, version, 1);
}

/* Whether the object is the kernel's own rather than ours, by a name only the kernel's carries. */
static int is_kernels_own(const struct vdso *v) {
    return lookup(v, VDSO_KERNELS_OWN, -1, 0) != 0;
}

static long long nanos(const struct timespec *ts) {
    return (long long)ts->tv_sec * NS_PER_S + ts->tv_nsec;
}

/* The wall clock through the object, or -1 when it has no entry to read it with. */
static long long vdso_wall(const struct vdso *v) {
    clock_gettime_fn entry = (clock_gettime_fn)vdso_sym(v, VDSO_CLOCK_GETTIME);
    struct timespec ts;

    if (!entry || entry(CLOCK_REALTIME, &ts) != 0) {
        return -1;
    }
    return nanos(&ts);
}

static void report_vdso(const struct vdso *v) {
    clock_gettime_fn read_clock = (clock_gettime_fn)vdso_sym(v, VDSO_CLOCK_GETTIME);
    gettimeofday_fn read_day = (gettimeofday_fn)vdso_sym(v, VDSO_GETTIMEOFDAY);
    time_fn read_seconds = (time_fn)vdso_sym(v, VDSO_TIME);
    clock_getres_fn read_res = (clock_getres_fn)vdso_sym(v, VDSO_CLOCK_GETRES);
    getcpu_fn read_cpu = (getcpu_fn)vdso_sym(v, VDSO_GETCPU);
    struct timespec ts;
    struct timeval tv;
    time_t seconds;

    printf("vdso.base=%llu\n", (unsigned long long)v->addr);
    printf("vdso.kernels_own=%d\n", is_kernels_own(v));
    printf("vdso.entries=%d\n", read_clock && read_day && read_seconds ? 1 : 0);
    if (read_clock && read_clock(CLOCK_REALTIME, &ts) == 0) {
        printf("vdso.clock_gettime=%lld\n", nanos(&ts));
    }
    if (read_clock && read_clock(CLOCK_MONOTONIC, &ts) == 0) {
        printf("vdso.monotonic=%lld\n", nanos(&ts));
    }
    if (read_day && read_day(&tv, NULL) == 0) {
        printf("vdso.gettimeofday=%lld\n",
               (long long)tv.tv_sec * NS_PER_S + (long long)tv.tv_usec * 1000LL);
    }
    if (read_seconds) {
        seconds = read_seconds(NULL);
        printf("vdso.time=%lld\n", (long long)seconds);
    }

    /* The two that stay the genuine object's. */
    printf("vdso.getres.found=%d\n", read_res ? 1 : 0);
    if (read_res && read_res(CLOCK_REALTIME, &ts) == 0) {
        printf("vdso.getres=%lld\n", nanos(&ts));
    }
    printf("vdso.getcpu.found=%d\n", read_cpu ? 1 : 0);
    if (read_cpu) {
        unsigned cpu = 0xffffffffu, node = 0xffffffffu;
        long rc = read_cpu(&cpu, &node, NULL);
        printf("vdso.getcpu=%ld\n", rc);
        printf("vdso.getcpu.cpu=%u\n", cpu);
    }
}

/* Both readings, over and over, until the wall clock passes `deadline` or `budget` runs out. */
static int tick(const struct vdso *v, long long deadline, long long budget) {
    long long i;

    for (i = 0; i < budget; i++) {
        struct timespec libc_ts;
        long long through_vdso = vdso_wall(v);
        long long through_libc;
        struct timespec pause = {0, 200000};

        clock_gettime(CLOCK_REALTIME, &libc_ts);
        through_libc = nanos(&libc_ts);
        printf("tick=%lld %lld\n", through_vdso, through_libc);
        if (through_vdso >= deadline && through_libc >= deadline) {
            fflush(stdout);
            return 0;
        }
        /* A pause between readings, converted like any other sleep, so a fast clock only makes
           the stream denser. */
        nanosleep(&pause, NULL);
    }
    fflush(stdout);
    return 1;
}

int main(int argc, char **argv) {
    struct vdso v;
    int opened = vdso_open(&v);

    printf("vdso.found=%d\n", opened);
    if (!opened) {
        fflush(stdout);
        return 2;
    }
    if (argc > 2) {
        return tick(&v, atoll(argv[1]), atoll(argv[2]));
    }
    report_vdso(&v);
    report("libc");
    return 0;
}
