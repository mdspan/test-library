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

   Each setter then asks for `argv[2]` on both the access and the modification time and the file is
   read back with `stat`, so `<key>.mtime` is the round trip through both conversions and
   `<key>.rc` is what the setter itself answered. `lutimes_link` is `lutimes` on a link to the file,
   which sets the link itself, read back with `lstat`.

   Last come the two `utimensat` flags that name no instant: `UTIME_OMIT` on the modification time,
   which must leave the round trip where it was, and `UTIME_NOW` on the access time, which must
   land at the clock's own now. */
#define _GNU_SOURCE
#define _LARGEFILE64_SOURCE
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

/* Every setter of the group, each asking for the same instant on both timestamps. */
static void set_them_all(void) {
    struct utimbuf buf;
    struct timeval tv[2];
    struct timespec ts[2];

    buf.actime = (time_t)target_s;
    buf.modtime = (time_t)target_s;
    report_round_trip("utime", utime(path, &buf));

    tv[0].tv_sec = (time_t)target_s;
    tv[0].tv_usec = 0;
    tv[1] = tv[0];
    report_round_trip("utimes", utimes(path, tv));
    report_round_trip("futimes", futimes(fd, tv));
    report_round_trip("lutimes", lutimes(path, tv));
    report_link_round_trip("lutimes_link", lutimes(link_path, tv));

    ts[0].tv_sec = (time_t)target_s;
    ts[0].tv_nsec = 0;
    ts[1] = ts[0];
    report_round_trip("utimensat", utimensat(AT_FDCWD, path, ts, 0));
    report_round_trip("futimens", futimens(fd, ts));
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

    close(fd);
    fflush(stdout);
    return 0;
}
