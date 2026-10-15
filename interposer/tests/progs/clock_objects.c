/* Objects whose clock is learned when they are made, made in the numbers and the ways that test
   whether it still is when they are armed or waited on: POSIX timers created and deleted over and
   over, more live timers and condition variables than the library keeps a clock for, and timerfds
   reached through numbers `timerfd_create` never handed out.

   Every arm and every wait is for argv[3] milliseconds of faketime, as an absolute deadline on the
   object's own clock unless its key says otherwise. Each reports `<key>.fired`, 1 when it fired or
   timed out and 0 when it had not after a guard many times as long, `<key>.genuine`, the genuine
   nanoseconds that took, and for a condition variable `<key>.rc`, what the wait answered.

   argv[1] names the case and argv[2] how many objects it makes before the one it arms:

   - `timer_churn`: creates and deletes that many monotonic POSIX timers, then arms a new one.
   - `timer_crowd`: keeps that many monotonic POSIX timers alive, or as many as the system allows,
     then arms a new monotonic one and a new wall-clock one.
   - `timerfd_numbers`: arms one monotonic timerfd through every way of giving it another number,
     after its first number is closed, and with a relative duration; then one wall-clock timerfd
     moved onto a number a monotonic one had. The count is unused.
   - `cond_crowd`: keeps that many monotonic condition variables alive, then waits on a new one and
     on one built by the static initialiser, which reads the wall clock.
   - `cond_reinit`: builds that many monotonic condition variables, initialises all but the last
     `REST` again with the wall clock, the last `REST` again with the monotonic clock and then the
     wall clock, each without destroying it first, as a program does that reuses the memory of a
     variable it never destroyed, and waits on each of the last for an eighth of argv[3] on the
     wall clock. In place of the keys above it reports `cond_reinit.waits`, how many it meant to
     wait on, and `cond_reinit.on_time`, how many timed out no sooner than half that before the
     first that did not.

   Linked against the library rather than preloading it, so the genuine clock comes from the
   library's own symbol. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#ifdef __linux__
#include <sys/timerfd.h>
#endif

extern int libtimewarp_genuine_clock_gettime(clockid_t clock, struct timespec *ts);

#define NS_PER_S 1000000000LL
#define NS_PER_MS 1000000LL

/* How many times the duration an arm or a wait asked for it is given before it counts as never. */
#define GUARD 20

/* The fake duration of every arm and wait, from argv[3]. */
static long long base_ns;

static long long nanos(const struct timespec *ts) {
    return (long long)ts->tv_sec * NS_PER_S + ts->tv_nsec;
}

static struct timespec spec_of(long long ns) {
    struct timespec ts;
    ts.tv_sec = (time_t)(ns / NS_PER_S);
    ts.tv_nsec = (long)(ns % NS_PER_S);
    return ts;
}

static long long genuine_mono(void) {
    struct timespec ts;
    if (libtimewarp_genuine_clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return nanos(&ts);
}

/* `clock` as the program sees it, plus `ahead_ns`. */
static struct timespec ahead_on(clockid_t clock, long long ahead_ns) {
    struct timespec ts;
    clock_gettime(clock, &ts);
    return spec_of(nanos(&ts) + ahead_ns);
}

/* `clock` as the program sees it, plus the duration every arm and wait asks for. */
static struct timespec deadline_on(clockid_t clock) { return ahead_on(clock, base_ns); }

static long long started;

static void begin(void) { started = genuine_mono(); }

static void done(const char *key, int fired) {
    printf("%s.fired=%d\n", key, fired);
    printf("%s.genuine=%lld\n", key, genuine_mono() - started);
    fflush(stdout);
}

/* The longest a case waits for something that should have come long before. */
static struct timespec guard(void) { return spec_of(GUARD * base_ns); }

/* The POSIX timers every timer case arms, each delivering this signal, which stays blocked so it
   is waited for rather than handled. */
#define FIRED SIGUSR1

static void block_fired(void) {
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, FIRED);
    sigprocmask(SIG_BLOCK, &set, NULL);
}

static int fired_within_guard(void) {
    sigset_t set;
    struct timespec patience = guard();
    sigemptyset(&set);
    sigaddset(&set, FIRED);
    return sigtimedwait(&set, NULL, &patience) == FIRED;
}

static int quiet_timer(clockid_t clock, timer_t *timer) {
    struct sigevent event;
    memset(&event, 0, sizeof event);
    event.sigev_notify = SIGEV_NONE;
    return timer_create(clock, &event, timer);
}

static int signal_timer(clockid_t clock, timer_t *timer) {
    struct sigevent event;
    memset(&event, 0, sizeof event);
    event.sigev_notify = SIGEV_SIGNAL;
    event.sigev_signo = FIRED;
    return timer_create(clock, &event, timer);
}

/* Arms `timer` for the deadline on `clock` and reports whether, and when, it fired. */
static void arm_timer(const char *key, timer_t timer, clockid_t clock) {
    struct itimerspec value;
    int fired = 0;
    memset(&value, 0, sizeof value);
    value.it_value = deadline_on(clock);
    begin();
    if (timer_settime(timer, TIMER_ABSTIME, &value, NULL) == 0) {
        fired = fired_within_guard();
    }
    done(key, fired);
}

static int timer_churn(long count) {
    timer_t timer;
    long made;

    block_fired();
    for (made = 0; made < count; made++) {
        if (quiet_timer(CLOCK_MONOTONIC, &timer) != 0) {
            return 3;
        }
        timer_delete(timer);
    }
    if (signal_timer(CLOCK_MONOTONIC, &timer) != 0) {
        return 3;
    }
    arm_timer("timer_churn", timer, CLOCK_MONOTONIC);
    return 0;
}

/* Makes a timer to arm once the crowd is in place, giving a timer of the crowd back for it where
   the system allows no more, which FreeBSD's 32 to a process soon does. */
static int crowded_timer(clockid_t clock, timer_t *crowd, long *made, timer_t *timer) {
    while (signal_timer(clock, timer) != 0) {
        if (*made == 0) {
            return -1;
        }
        *made -= 1;
        timer_delete(crowd[*made]);
    }
    return 0;
}

static int timer_crowd(long count) {
    timer_t *crowd = calloc((size_t)count, sizeof *crowd);
    timer_t steady, wall;
    long made;

    if (crowd == NULL) {
        return 3;
    }
    block_fired();
    for (made = 0; made < count; made++) {
        if (quiet_timer(CLOCK_MONOTONIC, &crowd[made]) != 0) {
            break;
        }
    }
    if (crowded_timer(CLOCK_MONOTONIC, crowd, &made, &steady) != 0
        || crowded_timer(CLOCK_REALTIME, crowd, &made, &wall) != 0) {
        return 3;
    }
    printf("timer_crowd.made=%ld\n", made);
    arm_timer("timer_crowd", steady, CLOCK_MONOTONIC);
    arm_timer("timer_crowd_wall", wall, CLOCK_REALTIME);
    return 0;
}

#ifdef __linux__
/* Waits for the timerfd behind `fd` to fire, and takes the count so it can fire again. */
static int timerfd_fired(int fd) {
    struct pollfd ready;
    uint64_t ticks;
    struct timespec patience = guard();
    ready.fd = fd;
    ready.events = POLLIN;
    ready.revents = 0;
    if (poll(&ready, 1, (int)(nanos(&patience) / NS_PER_MS)) != 1) {
        return 0;
    }
    return read(fd, &ticks, sizeof ticks) == (ssize_t)sizeof ticks;
}

/* Arms the timerfd under the number `fd` for the deadline on `clock`, or for the duration alone
   when `clock` is -1, and reports whether, and when, it fired. */
static void arm_timerfd(const char *key, int fd, clockid_t clock) {
    struct itimerspec value;
    int fired = 0, flags = 0;
    memset(&value, 0, sizeof value);
    if (clock == -1) {
        value.it_value = spec_of(base_ns);
    } else {
        value.it_value = deadline_on(clock);
        flags = TFD_TIMER_ABSTIME;
    }
    begin();
    if (fd >= 0 && timerfd_settime(fd, flags, &value, NULL) == 0) {
        fired = timerfd_fired(fd);
    }
    done(key, fired);
}

static int timerfd_numbers(void) {
    int made = timerfd_create(CLOCK_MONOTONIC, 0);
    int copy, wall, moved;

    if (made < 0) {
        return 3;
    }
    arm_timerfd("timerfd_dup", dup(made), CLOCK_MONOTONIC);
    arm_timerfd("timerfd_dup2", dup2(made, 900), CLOCK_MONOTONIC);
    arm_timerfd("timerfd_dup3", dup3(made, 901, O_CLOEXEC), CLOCK_MONOTONIC);
    arm_timerfd("timerfd_fcntl_dupfd", fcntl(made, F_DUPFD, 902), CLOCK_MONOTONIC);
    arm_timerfd("timerfd_fcntl_dupfd_cloexec", fcntl(made, F_DUPFD_CLOEXEC, 903),
                CLOCK_MONOTONIC);

    /* The number it was made under goes, and a copy still arms on the clock it was made on. */
    copy = dup(made);
    close(made);
    arm_timerfd("timerfd_after_close", copy, CLOCK_MONOTONIC);
    arm_timerfd("timerfd_relative", copy, -1);

    /* A number a monotonic timerfd was made under, closed, and handed to a wall-clock one. */
    moved = timerfd_create(CLOCK_MONOTONIC, 0);
    wall = timerfd_create(CLOCK_REALTIME, 0);
    if (moved < 0 || wall < 0) {
        return 3;
    }
    close(moved);
    arm_timerfd("timerfd_renumbered", dup2(wall, moved), CLOCK_REALTIME);
    return 0;
}
#endif

/* Wakes every waiter on a variable once the guard has passed, so a wait the library made too long
   ends with an answer rather than never, unless a byte on `stop` says the wait is over. */
struct watch {
    pthread_mutex_t *mutex;
    pthread_cond_t *cond;
    int stop;
};

static void *watchdog(void *arg) {
    struct watch *watch = arg;
    struct timespec patience = guard();
    struct pollfd stop;
    stop.fd = watch->stop;
    stop.events = POLLIN;
    stop.revents = 0;
    if (poll(&stop, 1, (int)(nanos(&patience) / NS_PER_MS)) == 0) {
        pthread_mutex_lock(watch->mutex);
        pthread_cond_broadcast(watch->cond);
        pthread_mutex_unlock(watch->mutex);
    }
    return NULL;
}

/* Waits on `cond` for `ahead_ns` past what `clock` shows and answers what the wait did, or -1
   when it could not wait. Reports what came of it under `key` unless that is null. */
static int watched_wait(const char *key, pthread_cond_t *cond, clockid_t clock,
                        long long ahead_ns) {
    pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
    struct watch watch;
    struct timespec deadline;
    pthread_t thread;
    int stop[2], rc;

    if (pipe(stop) != 0) {
        return -1;
    }
    watch.mutex = &mutex;
    watch.cond = cond;
    watch.stop = stop[0];
    pthread_mutex_lock(&mutex);
    if (pthread_create(&thread, NULL, watchdog, &watch) != 0) {
        pthread_mutex_unlock(&mutex);
        return -1;
    }
    deadline = ahead_on(clock, ahead_ns);
    begin();
    rc = pthread_cond_timedwait(cond, &mutex, &deadline);
    if (key != NULL) {
        printf("%s.rc=%d\n", key, rc);
        done(key, rc == ETIMEDOUT);
    }
    pthread_mutex_unlock(&mutex);
    if (write(stop[1], "", 1) == 1) {
        pthread_join(thread, NULL);
    }
    close(stop[0]);
    close(stop[1]);
    return rc;
}

/* Waits on `cond` for the deadline on `clock` and reports what came of it. */
static void wait_on(const char *key, pthread_cond_t *cond, clockid_t clock) {
    watched_wait(key, cond, clock, base_ns);
}

static int cond_crowd(long count) {
    pthread_cond_t *crowd = calloc((size_t)count + 1, sizeof *crowd);
    pthread_cond_t wall = PTHREAD_COND_INITIALIZER;
    pthread_condattr_t steady;
    long made;

    if (crowd == NULL) {
        return 3;
    }
    pthread_condattr_init(&steady);
    if (pthread_condattr_setclock(&steady, CLOCK_MONOTONIC) != 0) {
        return 3;
    }
    for (made = 0; made <= count; made++) {
        if (pthread_cond_init(&crowd[made], &steady) != 0) {
            return 3;
        }
    }
    wait_on("cond_crowd", &crowd[count], CLOCK_MONOTONIC);
    wait_on("cond_crowd_wall", &wall, CLOCK_REALTIME);
    return 0;
}

/* How many variables `cond_reinit` gives their clocks again and waits on. */
#define REST 32

static int cond_reinit(long count) {
    pthread_cond_t *conds = calloc((size_t)count, sizeof *conds);
    pthread_condattr_t steady;
    long long ahead = base_ns / 8;
    long made, waited;
    int rc;

    if (conds == NULL || count < REST) {
        return 3;
    }
    pthread_condattr_init(&steady);
    if (pthread_condattr_setclock(&steady, CLOCK_MONOTONIC) != 0) {
        return 3;
    }
    for (made = 0; made < count; made++) {
        if (pthread_cond_init(&conds[made], &steady) != 0) {
            return 3;
        }
    }
    /* Each of these frees a slot that may lie on the walk of one of the last, ahead of its own. */
    for (made = 0; made < count - REST; made++) {
        pthread_cond_init(&conds[made], NULL);
    }
    for (made = count - REST; made < count; made++) {
        pthread_cond_init(&conds[made], &steady);
    }
    for (made = count - REST; made < count; made++) {
        pthread_cond_init(&conds[made], NULL);
    }
    printf("cond_reinit.waits=%d\n", REST);
    for (waited = 0; waited < REST; waited++) {
        rc = watched_wait(NULL, &conds[count - REST + waited], CLOCK_REALTIME, ahead);
        if (rc != ETIMEDOUT || genuine_mono() - started < ahead / 2) {
            break;
        }
    }
    printf("cond_reinit.on_time=%ld\n", waited);
    return 0;
}

int main(int argc, char **argv) {
    long count;

    if (argc < 4) {
        return 2;
    }
    count = atol(argv[2]);
    base_ns = atoll(argv[3]) * NS_PER_MS;
    if (strcmp(argv[1], "timer_churn") == 0) {
        return timer_churn(count);
    }
    if (strcmp(argv[1], "timer_crowd") == 0) {
        return timer_crowd(count);
    }
#ifdef __linux__
    if (strcmp(argv[1], "timerfd_numbers") == 0) {
        return timerfd_numbers();
    }
#endif
    if (strcmp(argv[1], "cond_crowd") == 0) {
        return cond_crowd(count);
    }
    if (strcmp(argv[1], "cond_reinit") == 0) {
        return cond_reinit(count);
    }
    return 2;
}
