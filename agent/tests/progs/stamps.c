/* Every way a program can read a file's timestamps and every way it can set them.

   argv[1] is a directory to work in, and argv[2] the whole second, in the faked wall clock, that
   every setter here asks for.

   The file is written first, so what every reader reports is the modification time of a file
   written just now. `now.before` and `now.after` bracket those reads on the faked wall clock, so a
   case can ask whether the timestamps agree with the clock without knowing what the clock was set
   to. Each reader reports `<key>.atime`, `<key>.mtime` and `<key>.ctime` in nanoseconds. The birth
   time comes from `statx` where that is the only buffer with room for it, and from every `stat`
   buffer where the platform keeps one there; `birth.in_stat` says which. The `64` spellings are
   reported only where the libc has them, and on x86_64 macOS every reader here is compiled to the
   `$INODE64` spelling of its own name, which fills the buffer this file declares.

   Each setter then asks for `argv[2]` plus `FRACTION`, as far as the call can spell it, on both the
   access and the modification time, and the file is read back with `stat`. `<key>.asked` is the
   instant the setter named, `<key>.mtime` the round trip through both conversions, and `<key>.rc`
   what the setter itself answered. `lutimes_link` is `lutimes` on a link to the file, which sets
   the link itself, read back with `lstat`.

   Then come the two `utimensat` flags that name no instant: `UTIME_OMIT` on the modification time,
   which must leave the round trip where it was, and `UTIME_NOW` on the access time, which must
   land at the clock's own now.

   Last, every setter that takes a fraction is handed one out of its range, on the file and on a
   path that does not exist. `range.<case>.rc` and `range.<case>.errno` are the answer, and where
   the call succeeded `range.<case>.mtime` is what the file then shows. */
#define _GNU_SOURCE
#define _LARGEFILE64_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>
#include <utime.h>

#define NS_PER_S 1000000000LL

/* What every setter asks for past the whole second: a nanosecond count no microsecond holds, so a
   setter that spells microseconds asks for 123456 of them and one that spells nanoseconds for all
   of it. */
#define FRACTION 123456789LL

/* Whether a `stat` buffer here has room for the birth time, which the BSDs' has and Linux's does
   not: there it takes `statx` to read one. */
#if defined(__FreeBSD__) || defined(__APPLE__)
#define BIRTH_IN_STAT 1
#else
#define BIRTH_IN_STAT 0
#endif

/* Where each timestamp sits in the buffer. macOS spells the four as `st_*timespec` and defines no
   POSIX `st_*tim` beside them. */
#ifdef __APPLE__
#define ATIME_OF(st) ((st)->st_atimespec)
#define MTIME_OF(st) ((st)->st_mtimespec)
#define CTIME_OF(st) ((st)->st_ctimespec)
#define BTIME_OF(st) ((st)->st_birthtimespec)
#else
#define ATIME_OF(st) ((st)->st_atim)
#define MTIME_OF(st) ((st)->st_mtim)
#define CTIME_OF(st) ((st)->st_ctim)
#define BTIME_OF(st) ((st)->st_birthtim)
#endif

/* The file every reader and every setter works on. */
static char path[4096];

/* A link to it, for the setter that sets a link rather than what the link names. */
static char link_path[4096];

/* An open descriptor on it, for the three functions that take one. */
static int fd = -1;

/* The whole second, in the faked wall clock, every setter asks for. */
static long long target_s;

static long long nanos(long long seconds, long long ns) { return seconds * NS_PER_S + ns; }

/* The faked wall clock, which is what every timestamp here is compared against. */
static long long fake_now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return nanos(ts.tv_sec, ts.tv_nsec);
}

static void report_stat(const char *key, const struct stat *st) {
    printf("%s.atime=%lld\n", key, nanos(ATIME_OF(st).tv_sec, ATIME_OF(st).tv_nsec));
    printf("%s.mtime=%lld\n", key, nanos(MTIME_OF(st).tv_sec, MTIME_OF(st).tv_nsec));
    printf("%s.ctime=%lld\n", key, nanos(CTIME_OF(st).tv_sec, CTIME_OF(st).tv_nsec));
#if BIRTH_IN_STAT
    printf("%s.btime=%lld\n", key, nanos(BTIME_OF(st).tv_sec, BTIME_OF(st).tv_nsec));
#endif
}

#ifdef __linux__
static void report_stat64(const char *key, const struct stat64 *st) {
    printf("%s.atime=%lld\n", key, nanos(st->st_atim.tv_sec, st->st_atim.tv_nsec));
    printf("%s.mtime=%lld\n", key, nanos(st->st_mtim.tv_sec, st->st_mtim.tv_nsec));
    printf("%s.ctime=%lld\n", key, nanos(st->st_ctim.tv_sec, st->st_ctim.tv_nsec));
}
#endif

/* Every reader of the group, each on the same file written a moment ago. */
static void read_them_all(void) {
    struct stat st;

    if (stat(path, &st) == 0) {
        report_stat("stat", &st);
    }
    if (fstat(fd, &st) == 0) {
        report_stat("fstat", &st);
    }
    if (lstat(path, &st) == 0) {
        report_stat("lstat", &st);
    }
    if (fstatat(AT_FDCWD, path, &st, 0) == 0) {
        report_stat("fstatat", &st);
    }
#ifdef __linux__
    {
        struct stat64 st64;
        if (stat64(path, &st64) == 0) {
            report_stat64("stat64", &st64);
        }
        if (fstat64(fd, &st64) == 0) {
            report_stat64("fstat64", &st64);
        }
        if (lstat64(path, &st64) == 0) {
            report_stat64("lstat64", &st64);
        }
        if (fstatat64(AT_FDCWD, path, &st64, 0) == 0) {
            report_stat64("fstatat64", &st64);
        }
    }
#endif
    /* Where the birth time sits in the `stat` buffer, every reader above has already reported it. */
    printf("birth.in_stat=%d\n", BIRTH_IN_STAT);
}

/* Linux's own reader, the one that carries a birth time. */
static void read_statx(void) {
#ifdef STATX_BASIC_STATS
    struct statx sx;
    if (statx(AT_FDCWD, path, 0, STATX_ALL, &sx) != 0) {
        printf("statx.available=0\n");
        return;
    }
    printf("statx.available=1\n");
    printf("statx.atime=%lld\n", nanos(sx.stx_atime.tv_sec, sx.stx_atime.tv_nsec));
    printf("statx.mtime=%lld\n", nanos(sx.stx_mtime.tv_sec, sx.stx_mtime.tv_nsec));
    printf("statx.ctime=%lld\n", nanos(sx.stx_ctime.tv_sec, sx.stx_ctime.tv_nsec));
    printf("statx.has_btime=%d\n", (sx.stx_mask & STATX_BTIME) ? 1 : 0);
    if (sx.stx_mask & STATX_BTIME) {
        printf("statx.btime=%lld\n", nanos(sx.stx_btime.tv_sec, sx.stx_btime.tv_nsec));
    }
#else
    printf("statx.available=0\n");
#endif
}

/* What the file says after a setter ran: the round trip through both conversions. */
static void report_round_trip(const char *key, int set) {
    struct stat st;
    printf("%s.rc=%d\n", key, set);
    if (set == 0 && stat(path, &st) == 0) {
        report_stat(key, &st);
    }
}

/* The same for the link itself. */
static void report_link_round_trip(const char *key, int set) {
    struct stat st;
    printf("%s.rc=%d\n", key, set);
    if (set == 0 && lstat(link_path, &st) == 0) {
        report_stat(key, &st);
    }
}

/* Every setter of the group, each asking for the same instant on both timestamps, to the
   precision it has. */
static void set_them_all(void) {
    struct utimbuf buf;
    struct timeval tv[2];
    struct timespec ts[2];
    long long whole = target_s * NS_PER_S;
    long long micros = whole + FRACTION / 1000 * 1000;
    long long all = whole + FRACTION;

    buf.actime = (time_t)target_s;
    buf.modtime = (time_t)target_s;
    printf("utime.asked=%lld\n", whole);
    report_round_trip("utime", utime(path, &buf));

    tv[0].tv_sec = (time_t)target_s;
    tv[0].tv_usec = FRACTION / 1000;
    tv[1] = tv[0];
    printf("utimes.asked=%lld\nfutimes.asked=%lld\n", micros, micros);
    printf("lutimes.asked=%lld\nlutimes_link.asked=%lld\n", micros, micros);
    report_round_trip("utimes", utimes(path, tv));
    report_round_trip("futimes", futimes(fd, tv));
    report_round_trip("lutimes", lutimes(path, tv));
    report_link_round_trip("lutimes_link", lutimes(link_path, tv));

    ts[0].tv_sec = (time_t)target_s;
    ts[0].tv_nsec = FRACTION;
    ts[1] = ts[0];
    printf("utimensat.asked=%lld\nfutimens.asked=%lld\n", all, all);
    report_round_trip("utimensat", utimensat(AT_FDCWD, path, ts, 0));
    report_round_trip("futimens", futimens(fd, ts));
}

/* One out-of-range case: what the call answered, and on a success what the file shows. */
static void report_range(const char *key, int set, int error) {
    struct stat st;
    printf("range.%s.rc=%d\n", key, set);
    printf("range.%s.errno=%d\n", key, set == 0 ? 0 : error);
    if (set == 0 && stat(path, &st) == 0) {
        printf("range.%s.mtime=%lld\n", key, nanos(MTIME_OF(&st).tv_sec, MTIME_OF(&st).tv_nsec));
    }
}

/* The nanoseconds a `timespec` setter is handed in each case, the first on the access time and the
   second on the modification time. -5 is no flag anywhere: Linux spells the two near 2^30, and
   the BSDs as -1 and -2. */
static const struct {
    const char *name;
    long first;
    long second;
} nsec_cases[] = {
    {"second", 1000000000L, 0},
    {"seconds", 2000000000L, 0},
    {"negative", -5, 0},
    {"omit_beside", UTIME_OMIT, -5},
    {"now_beside", UTIME_NOW, 2000000000L},
};

/* The same for the microseconds of a `timeval` setter. */
static const struct {
    const char *name;
    long first;
    long second;
} usec_cases[] = {
    {"second", 1000000L, 0},
    {"negative", -1, 0},
    {"modified", 0, 1000001L},
};

/* Every setter that takes a fraction, handed one out of its range. Where a platform refuses it
   the call fails, and how is the platform's own business: a case compares against a run without
   the library. */
static void set_out_of_range(void) {
    char key[64];
    size_t i;
    int set;
    const char *missing = "/nonexistent/written";

    for (i = 0; i < sizeof(nsec_cases) / sizeof(nsec_cases[0]); i++) {
        struct timespec ts[2];
        ts[0].tv_sec = (time_t)target_s;
        ts[0].tv_nsec = nsec_cases[i].first;
        ts[1].tv_sec = (time_t)target_s;
        ts[1].tv_nsec = nsec_cases[i].second;

        snprintf(key, sizeof(key), "utimensat.%s", nsec_cases[i].name);
        errno = 0;
        set = utimensat(AT_FDCWD, path, ts, 0);
        report_range(key, set, errno);

        snprintf(key, sizeof(key), "futimens.%s", nsec_cases[i].name);
        errno = 0;
        set = futimens(fd, ts);
        report_range(key, set, errno);

        snprintf(key, sizeof(key), "utimensat_missing.%s", nsec_cases[i].name);
        errno = 0;
        set = utimensat(AT_FDCWD, missing, ts, 0);
        report_range(key, set, errno);
    }

    for (i = 0; i < sizeof(usec_cases) / sizeof(usec_cases[0]); i++) {
        struct timeval tv[2];
        tv[0].tv_sec = (time_t)target_s;
        tv[0].tv_usec = usec_cases[i].first;
        tv[1].tv_sec = (time_t)target_s;
        tv[1].tv_usec = usec_cases[i].second;

        snprintf(key, sizeof(key), "utimes.%s", usec_cases[i].name);
        errno = 0;
        set = utimes(path, tv);
        report_range(key, set, errno);

        snprintf(key, sizeof(key), "futimes.%s", usec_cases[i].name);
        errno = 0;
        set = futimes(fd, tv);
        report_range(key, set, errno);

        snprintf(key, sizeof(key), "lutimes.%s", usec_cases[i].name);
        errno = 0;
        set = lutimes(path, tv);
        report_range(key, set, errno);

        snprintf(key, sizeof(key), "utimes_missing.%s", usec_cases[i].name);
        errno = 0;
        set = utimes(missing, tv);
        report_range(key, set, errno);
    }
}

/* The two flags that name no instant, on a file whose timestamps are the round trip above. */
static void set_the_flags(void) {
    struct timespec ts[2];
    ts[0].tv_sec = 0;
    ts[0].tv_nsec = UTIME_NOW;
    ts[1].tv_sec = 0;
    ts[1].tv_nsec = UTIME_OMIT;
    printf("flags.before=%lld\n", fake_now());
    report_round_trip("flags", utimensat(AT_FDCWD, path, ts, 0));
    printf("flags.after=%lld\n", fake_now());
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: stamps <dir> <target seconds>\n");
        return 2;
    }
    snprintf(path, sizeof(path), "%s/written", argv[1]);
    snprintf(link_path, sizeof(link_path), "%s/link", argv[1]);
    target_s = atoll(argv[2]);
    printf("target=%lld\n", target_s * NS_PER_S);

    fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0600);
    if (fd < 0) {
        perror("open");
        return 1;
    }
    if (write(fd, "x", 1) != 1) {
        perror("write");
        return 1;
    }
    if (symlink("written", link_path) != 0) {
        perror("symlink");
        return 1;
    }

    printf("now.before=%lld\n", fake_now());
    read_them_all();
    read_statx();
    printf("now.after=%lld\n", fake_now());

    set_them_all();
    set_the_flags();
    set_out_of_range();

    close(fd);
    fflush(stdout);
    return 0;
}
