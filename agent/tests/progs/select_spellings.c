/* Calls each spelling of `select` and `pselect` that macOS keeps on this architecture, by its own
   name, with one descriptor more than FD_SETSIZE and a zero timeout, and reports what each
   answered.

   The headers pick the spelling a program calls. The Darwin extensions give the `$DARWIN_EXTSN`
   pair, which takes sets of any width. Otherwise `x86_64` gives the conforming `$1050` pair and
   aarch64 the plain names, which are the conforming ones there; both refuse more than FD_SETSIZE
   descriptors. The plain names on `x86_64` are the ones of 10.4, which refuse no count. Each is
   named here with an asm label instead, so one program reaches all of them.

   Each set is two `fd_set`s wide, so a spelling that takes sets of any width reads and writes inside
   the program's buffers. Each spelling reports `<key>.rc` and `<key>.errno`, under `conforming`,
   `extsn` and, on `x86_64`, `legacy`. */
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/select.h>
#include <sys/time.h>
#include <time.h>

typedef int select_fn(int, fd_set *, fd_set *, fd_set *, struct timeval *);
typedef int pselect_fn(int, fd_set *, fd_set *, fd_set *, const struct timespec *,
                       const sigset_t *);

#ifdef __x86_64__
extern select_fn conforming_select __asm("_select$1050");
extern pselect_fn conforming_pselect __asm("_pselect$1050");
extern select_fn legacy_select __asm("_select");
extern pselect_fn legacy_pselect __asm("_pselect");
#else
extern select_fn conforming_select __asm("_select");
extern pselect_fn conforming_pselect __asm("_pselect");
#endif
extern select_fn extsn_select __asm("_select$DARWIN_EXTSN");
extern pselect_fn extsn_pselect __asm("_pselect$DARWIN_EXTSN");

/* Read, write and exception sets, each wide enough for FD_SETSIZE + 1 descriptors. */
static fd_set sets[3][2];

static void report(const char *key, int rc, int error) {
    printf("%s.rc=%d\n", key, rc);
    printf("%s.errno=%d\n", key, rc == 0 ? 0 : error);
}

static void try_select(const char *key, select_fn *call) {
    struct timeval timeout = {0, 0};
    int rc;

    memset(sets, 0, sizeof sets);
    errno = 0;
    rc = call(FD_SETSIZE + 1, sets[0], sets[1], sets[2], &timeout);
    report(key, rc, errno);
}

static void try_pselect(const char *key, pselect_fn *call) {
    struct timespec timeout = {0, 0};
    int rc;

    memset(sets, 0, sizeof sets);
    errno = 0;
    rc = call(FD_SETSIZE + 1, sets[0], sets[1], sets[2], &timeout, NULL);
    report(key, rc, errno);
}

int main(void) {
    try_select("select.conforming", conforming_select);
    try_select("select.extsn", extsn_select);
    try_pselect("pselect.conforming", conforming_pselect);
    try_pselect("pselect.extsn", extsn_pselect);
#ifdef __x86_64__
    try_select("select.legacy", legacy_select);
    try_pselect("pselect.legacy", legacy_pselect);
#endif
    fflush(stdout);
    return 0;
}
