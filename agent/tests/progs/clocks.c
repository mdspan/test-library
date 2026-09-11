/* Every function of the clock group, faked and genuine, in one process.

   Linked against the library rather than preloading it, so the genuine reading of a clock comes
   from the library's own symbol and the two can be compared here. The genuine `times` and
   `getrusage` come through `syscall()`, whose other numbers the library passes through untouched,
   which is the same claim from the other side.

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
#include <sys/timex.h>
#include <sys/times.h>
#include <time.h>
#include <unistd.h>

extern int libtimewarp_genuine_clock_gettime(clockid_t clock, struct timespec *ts);

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
    struct timeb tb;
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
    ftime(&tb);
    say("ftime", (long long)tb.time * NS_PER_S + (long long)tb.millitm * 1000000LL);
    say("ftime.timezone", tb.timezone);
    say("ftime.dstflag", tb.dstflag);

    __clock_gettime(CLOCK_REALTIME, &ts);
    say("__clock_gettime", nanos(&ts));
    __gettimeofday(&tv, NULL);
    say("__gettimeofday", (long long)tv.tv_sec * NS_PER_S + tv.tv_usec * 1000LL);
    say("__time", (long long)__time(NULL) * NS_PER_S);
    __ftime(&tb);
    say("__ftime", (long long)tb.time * NS_PER_S + (long long)tb.millitm * 1000000LL);

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

    say("realtime.genuine", genuine(CLOCK_REALTIME));
    clock_gettime(CLOCK_MONOTONIC, &ts);
    say("monotonic", nanos(&ts));
    say("monotonic.genuine", genuine(CLOCK_MONOTONIC));
}

/* `clock`, `times` and `getrusage`: consumption follows the Cpu family, and `times`' elapsed return
   follows Monotonic. The genuine side of each comes from a pass-through system call. */
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
    real_elapsed = (clock_t)syscall(SYS_times, &real_tms);
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
    syscall(SYS_getrusage, RUSAGE_SELF, &real_ru);
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

    memset(&faked_thread, 0, sizeof faked_thread);
    memset(&real_thread, 0, sizeof real_thread);
    getrusage(RUSAGE_THREAD, &faked_thread);
    syscall(SYS_getrusage, RUSAGE_THREAD, &real_thread);
    snprintf(key, sizeof key, "%s.thread_rusage.cpu", tag);
    say(key, (long long)faked_thread.ru_utime.tv_sec * NS_PER_S + faked_thread.ru_utime.tv_usec * 1000LL
                 + (long long)faked_thread.ru_stime.tv_sec * NS_PER_S + faked_thread.ru_stime.tv_usec * 1000LL);
    snprintf(key, sizeof key, "%s.thread_rusage.cpu.genuine", tag);
    say(key, (long long)real_thread.ru_utime.tv_sec * NS_PER_S + real_thread.ru_utime.tv_usec * 1000LL
                 + (long long)real_thread.ru_stime.tv_sec * NS_PER_S + real_thread.ru_stime.tv_usec * 1000LL);
}

/* The list that reaches the genuine function untouched. */
static void passed_through(void) {
    struct timespec res;
    struct timex tx;
    clockid_t own = 0;
    struct timespec ts;

    clock_getres(CLOCK_REALTIME, &res);
    say("getres.realtime", nanos(&res));
    clock_getres(CLOCK_MONOTONIC, &res);
    say("getres.monotonic", nanos(&res));
    clock_getres(CLOCK_PROCESS_CPUTIME_ID, &res);
    say("getres.process_cpu", nanos(&res));

    memset(&tx, 0, sizeof tx);
    say("adjtimex.rc", adjtimex(&tx));
    say("adjtimex.time", (long long)tx.time.tv_sec * NS_PER_S);
    memset(&tx, 0, sizeof tx);
    say("ntp_adjtime.rc", ntp_adjtime(&tx));
    say("ntp_adjtime.time", (long long)tx.time.tv_sec * NS_PER_S);

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

    /* A dynamic clock id names a hardware device. Ours names a descriptor that is no such device,
       so the kernel refuses it, faked and genuine alike. */
    errno = 0;
    say("dynamic.rc", clock_gettime(FD_TO_CLOCKID(3), &ts));
    say("dynamic.errno", errno);
    errno = 0;
    say("dynamic.genuine_rc", libtimewarp_genuine_clock_gettime(FD_TO_CLOCKID(3), &ts));
    say("dynamic.genuine_errno", errno);

    /* Numbers `syscall()` must hand to the kernel exactly as they came. */
    say("sys_getpid", (long long)syscall(SYS_getpid));
    say("getpid", (long long)getpid());
    memset(&res, 0, sizeof res);
    say("sys_clock_getres.rc", (long long)syscall(SYS_clock_getres, CLOCK_REALTIME, &res));
    say("sys_clock_getres", nanos(&res));
    errno = 0;
    say("sys_unknown.rc", (long long)syscall(-1L));
    say("sys_unknown.errno", errno);
}

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
    if (argc > 1) {
        six_arguments(argv[1]);
    }

    shared = getenv("LIBTIMEWARP__SHARED_FILE");
    printf("shared=%s\n", shared ? shared : "");
    fflush(stdout);
    return 0;
}
