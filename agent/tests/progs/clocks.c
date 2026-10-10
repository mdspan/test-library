/* Every function of the clock group, faked and genuine, in one process.

   Linked against the library rather than preloading it, so the genuine reading of a clock comes
   from the library's own symbol and the two can be compared here. The genuine `getrusage` comes
   through `syscall()`, which hands that number to the kernel untouched on every platform. The
   genuine `times` does too where the kernel has a call for it, and elsewhere is built out of the
   genuine `getrusage` and a genuine clock read, the way that libc builds it.

   What only Linux has is behind `__linux__`: glibc's internal aliases, the numbers a program can
   ask `syscall()` for, dynamic clock ids, and `adjtimex`. `ftime`, which FreeBSD has not, is behind
   `__linux__` or `__APPLE__`. What macOS has not is behind `__APPLE__`: `ntp_adjtime`, the ids
   `clock_getcpuclockid` hands out, and a `getrusage` that can be asked about one thread.

   argv[1], when given, names a file of two pages whose second page this maps through `syscall()`,
   so a system call using all six argument registers proves they are forwarded. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/times.h>
#include <time.h>
#include <unistd.h>
#ifndef __APPLE__
#include <sys/timex.h>
#endif

extern int libtimewarp_genuine_clock_gettime(clockid_t clock, struct timespec *ts);

#ifdef __APPLE__
/* macOS keeps both the header and the function, obsolete as they are. */
#include <sys/timeb.h>
#endif

#ifdef __linux__
/* glibc dropped <sys/timeb.h> at 2.33 and kept the symbol, so the shape is declared here. */
struct timeb {
    time_t time;
    unsigned short millitm;
    short timezone;
    short dstflag;
};

extern int ftime(struct timeb *tp);
extern int __ftime(struct timeb *tp);
extern int __clock_gettime(clockid_t clock, struct timespec *ts);
extern int __gettimeofday(struct timeval *tv, void *tz);
extern time_t __time(time_t *t);

/* A dynamic clock id, the way the kernel encodes one from a descriptor. */
#define FD_TO_CLOCKID(fd) ((clockid_t)((~(unsigned int)(fd) << 3) | 3u))
#endif

/* libSystem declares `syscall()` deprecated, and still makes the call. */
#ifdef __APPLE__
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
#endif
static int genuine_getrusage(int who, struct rusage *usage) {
    return (int)syscall(SYS_getrusage, who, usage);
}
#ifdef __APPLE__
#pragma clang diagnostic pop
#endif

#define NS_PER_S 1000000000LL

static long long nanos(const struct timespec *ts) {
    return (long long)ts->tv_sec * NS_PER_S + ts->tv_nsec;
}

static long long genuine(clockid_t id) {
    struct timespec ts;
    if (libtimewarp_genuine_clock_gettime(id, &ts) != 0) {
        return -1;
    }
    return nanos(&ts);
}

static void say(const char *key, long long value) { printf("%s=%lld\n", key, value); }

/* Spends `ns` of genuine time consuming CPU, bounded by a genuine reading so a faked rate cannot
   change how long it runs for. */
static void burn(long long ns) {
    long long start = genuine(CLOCK_MONOTONIC);
    volatile unsigned long sink = 0;
    while (genuine(CLOCK_MONOTONIC) - start < ns) {
        for (int i = 0; i < 10000; i++) {
            sink += (unsigned long)i;
        }
    }
}

/* Every way of asking the wall clock, in the units each of them answers in, scaled to nanoseconds. */
static void wall_clocks(void) {
    struct timespec ts;
    struct timeval tv;
    time_t seconds;

    clock_gettime(CLOCK_REALTIME, &ts);
    say("clock_gettime", nanos(&ts));
    gettimeofday(&tv, NULL);
    say("gettimeofday", (long long)tv.tv_sec * NS_PER_S + tv.tv_usec * 1000LL);
    seconds = time(NULL);
    say("time", (long long)seconds * NS_PER_S);
    ts.tv_sec = 0;
    ts.tv_nsec = 0;
    say("timespec_get.base", timespec_get(&ts, TIME_UTC));
    say("timespec_get", nanos(&ts));
    say("timespec_get.bad_base", timespec_get(&ts, 99));
#if defined(__linux__) || defined(__APPLE__)
    {
        struct timeb tb;
        ftime(&tb);
        say("ftime", (long long)tb.time * NS_PER_S + (long long)tb.millitm * 1000000LL);
        say("ftime.timezone", tb.timezone);
        say("ftime.dstflag", tb.dstflag);
    }
#endif
#ifdef __linux__
    {
        struct timeb tb;

        __clock_gettime(CLOCK_REALTIME, &ts);
        say("__clock_gettime", nanos(&ts));
        __gettimeofday(&tv, NULL);
        say("__gettimeofday", (long long)tv.tv_sec * NS_PER_S + tv.tv_usec * 1000LL);
        say("__time", (long long)__time(NULL) * NS_PER_S);
        __ftime(&tb);
        say("__ftime", (long long)tb.time * NS_PER_S + (long long)tb.millitm * 1000000LL);
    }

    ts.tv_sec = 0;
    ts.tv_nsec = 0;
    syscall(SYS_clock_gettime, CLOCK_REALTIME, &ts);
    say("sys_clock_gettime", nanos(&ts));
    syscall(SYS_gettimeofday, &tv, NULL);
    say("sys_gettimeofday", (long long)tv.tv_sec * NS_PER_S + tv.tv_usec * 1000LL);
#ifdef SYS_time
    say("has_sys_time", 1);
    say("sys_time", (long long)syscall(SYS_time, NULL) * NS_PER_S);
#else
    say("has_sys_time", 0);
#endif
#endif

    say("realtime.genuine", genuine(CLOCK_REALTIME));
    clock_gettime(CLOCK_MONOTONIC, &ts);
    say("monotonic", nanos(&ts));
    say("monotonic.genuine", genuine(CLOCK_MONOTONIC));
}

#if !defined(SYS_times)
/* The clock the elapsed time of `times` counts on, read genuinely: libSystem's `times` reads the
   wall clock and FreeBSD's the monotonic one. */
static int genuine_elapsed(struct timespec *ts) {
#ifdef __APPLE__
    return libtimewarp_genuine_clock_gettime(CLOCK_REALTIME, ts);
#else
    return (int)syscall(SYS_clock_gettime, CLOCK_MONOTONIC, ts);
#endif
}
#endif

/* The genuine `times`. Linux has a system call for it. FreeBSD and macOS have none: their libc
   builds the answer out of two `getrusage` calls and one clock read, and so does this, out of the
   genuine ones. */
static clock_t genuine_times(struct tms *out) {
#if defined(SYS_times)
    return (clock_t)syscall(SYS_times, out);
#else
    long hz = sysconf(_SC_CLK_TCK);
    struct rusage mine, theirs;
    struct timespec ts;

    if (genuine_getrusage(RUSAGE_SELF, &mine) != 0
        || genuine_getrusage(RUSAGE_CHILDREN, &theirs) != 0 || genuine_elapsed(&ts) != 0) {
        return (clock_t)-1;
    }
    out->tms_utime = (clock_t)(mine.ru_utime.tv_sec * hz + mine.ru_utime.tv_usec / (1000000 / hz));
    out->tms_stime = (clock_t)(mine.ru_stime.tv_sec * hz + mine.ru_stime.tv_usec / (1000000 / hz));
    out->tms_cutime =
        (clock_t)(theirs.ru_utime.tv_sec * hz + theirs.ru_utime.tv_usec / (1000000 / hz));
    out->tms_cstime =
        (clock_t)(theirs.ru_stime.tv_sec * hz + theirs.ru_stime.tv_usec / (1000000 / hz));
    return (clock_t)(ts.tv_sec * hz + ts.tv_nsec / (1000000000 / hz));
#endif
}

/* `clock`, `times` and `getrusage`: consumption follows the Cpu family, and `times`' elapsed return
   follows Monotonic. The genuine side of each comes from a system call the library passes through,
   or is built out of genuine readings. */
static void cpu_group(const char *tag) {
    struct tms faked_tms, real_tms;
    struct rusage faked_ru, real_ru, faked_thread, real_thread;
    char key[64];
    clock_t elapsed, real_elapsed;

    snprintf(key, sizeof key, "%s.clock", tag);
    say(key, (long long)clock());
    snprintf(key, sizeof key, "%s.clocks_per_sec", tag);
    say(key, (long long)CLOCKS_PER_SEC);
    snprintf(key, sizeof key, "%s.process_cpu.genuine", tag);
    say(key, genuine(CLOCK_PROCESS_CPUTIME_ID));
    snprintf(key, sizeof key, "%s.thread_cpu.genuine", tag);
    say(key, genuine(CLOCK_THREAD_CPUTIME_ID));

    memset(&faked_tms, 0, sizeof faked_tms);
    memset(&real_tms, 0, sizeof real_tms);
    elapsed = times(&faked_tms);
    real_elapsed = genuine_times(&real_tms);
    snprintf(key, sizeof key, "%s.times.elapsed", tag);
    say(key, (long long)elapsed);
    snprintf(key, sizeof key, "%s.times.elapsed.genuine", tag);
    say(key, (long long)real_elapsed);
    snprintf(key, sizeof key, "%s.times.cpu", tag);
    say(key, (long long)(faked_tms.tms_utime + faked_tms.tms_stime));
    snprintf(key, sizeof key, "%s.times.cpu.genuine", tag);
    say(key, (long long)(real_tms.tms_utime + real_tms.tms_stime));
    snprintf(key, sizeof key, "%s.times.children", tag);
    say(key, (long long)(faked_tms.tms_cutime + faked_tms.tms_cstime));
    snprintf(key, sizeof key, "%s.ticks_per_second", tag);
    say(key, (long long)sysconf(_SC_CLK_TCK));

    memset(&faked_ru, 0, sizeof faked_ru);
    memset(&real_ru, 0, sizeof real_ru);
    getrusage(RUSAGE_SELF, &faked_ru);
    genuine_getrusage(RUSAGE_SELF, &real_ru);
    snprintf(key, sizeof key, "%s.rusage.cpu", tag);
    say(key, (long long)faked_ru.ru_utime.tv_sec * NS_PER_S + faked_ru.ru_utime.tv_usec * 1000LL
                 + (long long)faked_ru.ru_stime.tv_sec * NS_PER_S + faked_ru.ru_stime.tv_usec * 1000LL);
    snprintf(key, sizeof key, "%s.rusage.cpu.genuine", tag);
    say(key, (long long)real_ru.ru_utime.tv_sec * NS_PER_S + real_ru.ru_utime.tv_usec * 1000LL
                 + (long long)real_ru.ru_stime.tv_sec * NS_PER_S + real_ru.ru_stime.tv_usec * 1000LL);
    snprintf(key, sizeof key, "%s.rusage.maxrss", tag);
    say(key, (long long)faked_ru.ru_maxrss);
    snprintf(key, sizeof key, "%s.rusage.maxrss.genuine", tag);
    say(key, (long long)real_ru.ru_maxrss);
    snprintf(key, sizeof key, "%s.rusage.minflt", tag);
    say(key, (long long)faked_ru.ru_minflt);
    snprintf(key, sizeof key, "%s.rusage.minflt.genuine", tag);
    say(key, (long long)real_ru.ru_minflt);

#ifndef __APPLE__
    memset(&faked_thread, 0, sizeof faked_thread);
    memset(&real_thread, 0, sizeof real_thread);
    getrusage(RUSAGE_THREAD, &faked_thread);
    genuine_getrusage(RUSAGE_THREAD, &real_thread);
    snprintf(key, sizeof key, "%s.thread_rusage.cpu", tag);
    say(key, (long long)faked_thread.ru_utime.tv_sec * NS_PER_S + faked_thread.ru_utime.tv_usec * 1000LL
                 + (long long)faked_thread.ru_stime.tv_sec * NS_PER_S + faked_thread.ru_stime.tv_usec * 1000LL);
    snprintf(key, sizeof key, "%s.thread_rusage.cpu.genuine", tag);
    say(key, (long long)real_thread.ru_utime.tv_sec * NS_PER_S + real_thread.ru_utime.tv_usec * 1000LL
                 + (long long)real_thread.ru_stime.tv_sec * NS_PER_S + real_thread.ru_stime.tv_usec * 1000LL);
#else
    /* macOS has no `RUSAGE_THREAD`: a thread's own consumption is asked for through Mach there. */
    (void)faked_thread;
    (void)real_thread;
#endif
}

/* The list that reaches the genuine function untouched. */
static void passed_through(void) {
    struct timespec res;
    struct timespec ts;
    (void)ts;

    clock_getres(CLOCK_REALTIME, &res);
    say("getres.realtime", nanos(&res));
    clock_getres(CLOCK_MONOTONIC, &res);
    say("getres.monotonic", nanos(&res));
    clock_getres(CLOCK_PROCESS_CPUTIME_ID, &res);
    say("getres.process_cpu", nanos(&res));

#ifndef __APPLE__
    /* `ntp_adjtime` reads the kernel's own clock, which our shift never reaches. macOS has no such
       call, and no ids from `clock_getcpuclockid` either: a CPU clock is asked for through Mach
       there rather than named by an id a program can hand to `clock_gettime`. */
    {
        struct timex tx;
        clockid_t own = 0;

        memset(&tx, 0, sizeof tx);
        say("ntp_adjtime.rc", ntp_adjtime(&tx));
#ifdef __linux__
        say("ntp_adjtime.time", (long long)tx.time.tv_sec * NS_PER_S);
        memset(&tx, 0, sizeof tx);
        say("adjtimex.rc", adjtimex(&tx));
        say("adjtimex.time", (long long)tx.time.tv_sec * NS_PER_S);
#endif

        /* An id naming this process's CPU clock, and one naming this thread's, are readings about
           somebody rather than clocks this process consumes, so they stay genuine. */
        if (clock_getcpuclockid(getpid(), &own) == 0) {
            clock_gettime(own, &ts);
            say("own_cpuclockid", nanos(&ts));
            say("own_cpuclockid.genuine", genuine(own));
        }
        if (pthread_getcpuclockid(pthread_self(), &own) == 0) {
            clock_gettime(own, &ts);
            say("pthread_cpuclockid", nanos(&ts));
            say("pthread_cpuclockid.genuine", genuine(own));
        }
    }
#endif

#ifdef __linux__
    /* A dynamic clock id names a hardware device. Ours names a descriptor that is no such device,
       so the kernel refuses it, faked and genuine alike. */
    errno = 0;
    say("dynamic.rc", clock_gettime(FD_TO_CLOCKID(3), &ts));
    say("dynamic.errno", errno);
    errno = 0;
    say("dynamic.genuine_rc", libtimewarp_genuine_clock_gettime(FD_TO_CLOCKID(3), &ts));
    say("dynamic.genuine_errno", errno);

    /* Numbers `syscall()` must hand to the kernel exactly as they came. Only Linux takes the
       function over, so only there is there anything to prove. */
    say("sys_getpid", (long long)syscall(SYS_getpid));
    say("getpid", (long long)getpid());
    memset(&res, 0, sizeof res);
    say("sys_clock_getres.rc", (long long)syscall(SYS_clock_getres, CLOCK_REALTIME, &res));
    say("sys_clock_getres", nanos(&res));
    errno = 0;
    say("sys_unknown.rc", (long long)syscall(-1L));
    say("sys_unknown.errno", errno);
#endif
}

#ifdef __linux__
/* A system call that uses all six argument registers: the second page of `path`, mapped through
   `syscall()`, whose last argument is the offset. */
static void six_arguments(const char *path) {
    long page = sysconf(_SC_PAGESIZE);
    int fd = open(path, O_RDONLY);
    long mapped;
    if (fd < 0) {
        say("mmap.byte", -1);
        return;
    }
    mapped = (long)syscall(SYS_mmap, (long)NULL, page, PROT_READ, MAP_PRIVATE, (long)fd, page);
    if (mapped == -1) {
        say("mmap.byte", -2);
    } else {
        say("mmap.byte", *(const unsigned char *)(void *)mapped);
        syscall(SYS_munmap, mapped, page);
    }
    close(fd);
}
#endif

int main(int argc, char **argv) {
    const char *shared;

    wall_clocks();
    burn(300000000);
    cpu_group("a");

    /* Time passing between the two rounds of the Cpu group. */
    {
        struct timespec pause = {0, 200000000};
        nanosleep(&pause, NULL);
    }
    burn(300000000);
    cpu_group("b");

    passed_through();
#ifdef __linux__
    if (argc > 1) {
        six_arguments(argv[1]);
    }
#else
    (void)argc;
    (void)argv;
#endif

    shared = getenv("LIBTIMEWARP__SHARED_FILE");
    printf("shared=%s\n", shared ? shared : "");
    fflush(stdout);
    return 0;
}
