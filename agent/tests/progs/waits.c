/* Every sleep and every timeout the library takes over, each asked to wait one fake duration and
   each measured against the genuine clock it actually blocked in.

   The groups differ by platform: `epoll` and the two `clockwait` calls are Linux's, `kevent` is
   the BSDs' and its wider form macOS's, and a call by number is Linux's way of asking for one.
   macOS has none of the timed locks, timed semaphores, message queues or C11 threads calls, and no
   way to build a condition variable on a clock other than the wall clock. A wait a platform does
   not have is not reported at all, and the suite's own list for that platform does not name it.

   Linked against the library rather than preloading it, so the genuine clock comes from the
   library's own symbol and the measurement is not the one under test.

   argv[1] is the fake duration, in milliseconds, every wait is asked for. `sleep` takes whole
   seconds and so always asks for one.

   Each wait reports three facts: `<key>.fake`, the nanoseconds of faketime it asked for,
   `<key>.genuine`, the nanoseconds of genuine time it blocked, and `<key>.rc`, what it answered. A
   wait this system does not have reports `<key>.available=0` instead. */
#define _GNU_SOURCE
#include <aio.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/time.h>
#ifdef __linux__
#include <sys/epoll.h>
#include <sys/syscall.h>
#endif
#ifdef __FreeBSD__
/* `pthread_timedjoin_np` is a non-portable extension here, and lives in a header of its own. */
#include <pthread_np.h>
#endif
#if defined(__FreeBSD__) || defined(__APPLE__)
#include <sys/event.h>
#endif
#ifndef __APPLE__
/* The timed semaphores, the message queues and the C11 threads calls, none of which this platform
   has. */
#include <mqueue.h>
#include <semaphore.h>
#include <threads.h>
#endif
#include <time.h>
#include <unistd.h>

extern int libtimewarp_genuine_clock_gettime(clockid_t clock, struct timespec *ts);

#define NS_PER_S 1000000000LL

/* The fake duration every wait is asked for, from argv[1]. */
static long long base_ns;

/* How much genuine time the interrupter lets a sleep have before it cuts it short. */
#define INTERRUPT_AFTER_NS 20000000LL

static long long nanos(const struct timespec *ts) {
    return (long long)ts->tv_sec * NS_PER_S + ts->tv_nsec;
}

/* The genuine monotonic clock, which is what every measurement here is made against. */
static long long genuine_mono(void) {
    struct timespec ts;
    if (libtimewarp_genuine_clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return nanos(&ts);
}

/* A clock as the program sees it, which is where an absolute deadline is measured from. */
static long long faked(clockid_t id) {
    struct timespec ts;
    if (clock_gettime(id, &ts) != 0) {
        return 0;
    }
    return nanos(&ts);
}

static struct timespec spec_of(long long ns) {
    struct timespec ts;
    ts.tv_sec = (time_t)(ns / NS_PER_S);
    ts.tv_nsec = (long)(ns % NS_PER_S);
    return ts;
}

/* The deadline one base duration past what `id` shows the program now. */
static struct timespec deadline_on(clockid_t id) { return spec_of(faked(id) + base_ns); }

static long long started;

static void begin(void) { started = genuine_mono(); }

static void done(const char *key, long long fake, long long rc) {
    long long elapsed = genuine_mono() - started;
    printf("%s.available=1\n", key);
    printf("%s.fake=%lld\n", key, fake);
    printf("%s.genuine=%lld\n", key, elapsed);
    printf("%s.rc=%lld\n", key, rc);
    fflush(stdout);
}

static void skipped(const char *key) {
    printf("%s.available=0\n", key);
    fflush(stdout);
}

/* Burns `ns` of genuine time, for a helper that must pace itself against the clock the faking
   never reaches. */
static void burn(long long ns) {
    long long until = genuine_mono() + ns;
    while (genuine_mono() < until) {
        sched_yield();
    }
}

/* ---- the sleeps ---- */

static void sleeps(void) {
    struct timespec request;
#ifndef __APPLE__
    struct timespec remaining;
#endif

    request = spec_of(base_ns);
    begin();
    done("nanosleep", base_ns, nanosleep(&request, NULL));

#ifndef __APPLE__
    request = spec_of(base_ns);
    begin();
    done("clock_nanosleep_relative", base_ns,
         clock_nanosleep(CLOCK_MONOTONIC, 0, &request, NULL));

    remaining = deadline_on(CLOCK_MONOTONIC);
    begin();
    done("clock_nanosleep_absolute", base_ns,
         clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &remaining, NULL));

    remaining = deadline_on(CLOCK_REALTIME);
    begin();
    done("clock_nanosleep_wall", base_ns,
         clock_nanosleep(CLOCK_REALTIME, TIMER_ABSTIME, &remaining, NULL));
#endif

#ifdef __linux__
    request = spec_of(base_ns);
    begin();
    done("sys_nanosleep", base_ns, (long long)syscall(SYS_nanosleep, &request, NULL));

    request = spec_of(base_ns);
    begin();
    done("sys_clock_nanosleep", base_ns,
         (long long)syscall(SYS_clock_nanosleep, CLOCK_MONOTONIC, 0, &request, NULL));
#endif

    /* Whole seconds is all this one takes, so it asks for one however short the base is. */
    begin();
    done("sleep", NS_PER_S, sleep(1));

    begin();
    done("usleep", base_ns, usleep((useconds_t)(base_ns / 1000)));

#ifndef __APPLE__
    request = spec_of(base_ns);
    begin();
    done("thrd_sleep", base_ns, thrd_sleep(&request, NULL));
#endif
}

/* ---- a sleep cut short, whose remainder must come back in faketime ---- */

static pthread_t sleeping_thread;

static void woken(int signal) { (void)signal; }

static void *interrupter(void *arg) {
    (void)arg;
    burn(INTERRUPT_AFTER_NS);
    pthread_kill(sleeping_thread, SIGUSR1);
    return NULL;
}

static void interrupted_sleep(void) {
    struct sigaction action;
    pthread_t helper;
    struct timespec request = spec_of(base_ns);
    struct timespec remaining = {0, 0};
    long long rc;

    memset(&action, 0, sizeof action);
    action.sa_handler = woken;
    sigaction(SIGUSR1, &action, NULL);

    sleeping_thread = pthread_self();
    if (pthread_create(&helper, NULL, interrupter, NULL) != 0) {
        skipped("nanosleep_interrupted");
        return;
    }
    begin();
    rc = nanosleep(&request, &remaining);
    done("nanosleep_interrupted", base_ns, rc);
    printf("nanosleep_interrupted.remaining=%lld\n", nanos(&remaining));
    printf("nanosleep_interrupted.errno=%d\n", errno);
    fflush(stdout);
    pthread_join(helper, NULL);
}

/* ---- the timeouts that name a relative duration ---- */

static void relative_timeouts(void) {
    struct timespec timeout;
    struct timeval tv;
#ifndef __APPLE__
    sigset_t waited;
#endif

    begin();
    done("poll", base_ns, poll(NULL, 0, (int)(base_ns / 1000000)));

#ifndef __APPLE__
    timeout = spec_of(base_ns);
    begin();
    done("ppoll", base_ns, ppoll(NULL, 0, &timeout, NULL));
#endif

    tv.tv_sec = (time_t)(base_ns / NS_PER_S);
    tv.tv_usec = (suseconds_t)((base_ns % NS_PER_S) / 1000);
    begin();
    done("select", base_ns, select(0, NULL, NULL, NULL, &tv));

    timeout = spec_of(base_ns);
    begin();
    done("pselect", base_ns, pselect(0, NULL, NULL, NULL, &timeout, NULL));

#ifdef __linux__
    {
        struct epoll_event events[1];
        int epoll_fd = epoll_create1(0);
        if (epoll_fd < 0) {
            skipped("epoll_wait");
            skipped("epoll_pwait");
            skipped("epoll_pwait2");
        } else {
            begin();
            done("epoll_wait", base_ns, epoll_wait(epoll_fd, events, 1, (int)(base_ns / 1000000)));

            begin();
            done("epoll_pwait", base_ns,
                 epoll_pwait(epoll_fd, events, 1, (int)(base_ns / 1000000), NULL));

            timeout = spec_of(base_ns);
            begin();
            errno = 0;
            {
                int rc = epoll_pwait2(epoll_fd, events, 1, &timeout, NULL);
                if (rc < 0 && errno == ENOSYS) {
                    skipped("epoll_pwait2");
                } else {
                    done("epoll_pwait2", base_ns, rc);
                }
            }
            close(epoll_fd);
        }
    }
#endif
#if defined(__FreeBSD__) || defined(__APPLE__)
    /* The platform's own wait on a set of events, which nothing ever posts to here, so it runs its
       timeout out. macOS carries a wider event beside it, which is a function of its own. */
    {
        struct kevent event;
        int kq = kqueue();
        if (kq < 0) {
            skipped("kevent");
#ifdef __APPLE__
            skipped("kevent64");
#endif
        } else {
            timeout = spec_of(base_ns);
            begin();
            done("kevent", base_ns, kevent(kq, NULL, 0, &event, 1, &timeout));
#ifdef __APPLE__
            {
                struct kevent64_s wide;
                timeout = spec_of(base_ns);
                begin();
                done("kevent64", base_ns, kevent64(kq, NULL, 0, &wide, 1, 0, &timeout));
            }
#endif
            close(kq);
        }
    }
#endif

#ifndef __APPLE__
    /* A signal that is blocked and never sent, so the wait runs its timeout out. */
    sigemptyset(&waited);
    sigaddset(&waited, SIGUSR2);
    pthread_sigmask(SIG_BLOCK, &waited, NULL);
    timeout = spec_of(base_ns);
    begin();
    done("sigtimedwait", base_ns, sigtimedwait(&waited, NULL, &timeout));
#endif
}

/* ---- a timeout over an asynchronous read that never completes ---- */

static void aio_timeout(void) {
    static char buffer[1];
    static struct aiocb request;
    const struct aiocb *list[1] = {&request};
    struct timespec timeout;
    int pipe_fds[2];

    if (pipe(pipe_fds) != 0) {
        skipped("aio_suspend");
        return;
    }
    memset(&request, 0, sizeof request);
    request.aio_fildes = pipe_fds[0];
    request.aio_buf = buffer;
    request.aio_nbytes = sizeof buffer;
    if (aio_read(&request) != 0) {
        skipped("aio_suspend");
        close(pipe_fds[0]);
        close(pipe_fds[1]);
        return;
    }
    timeout = spec_of(base_ns);
    begin();
    done("aio_suspend", base_ns, aio_suspend(list, 1, &timeout));
    /* The read is left where it is: closing the write end would complete it, and the process is
       about to leave anyway. */
}

/* ---- a timeout over a socket nothing useful arrives on ---- */

#ifdef __linux__
static int pacer_socket = -1;
static volatile int pacing = 1;

static void *pacer(void *arg) {
    char byte = 'x';
    (void)arg;
    while (pacing) {
        if (send(pacer_socket, &byte, 1, MSG_DONTWAIT) < 0 && errno != EAGAIN) {
            break;
        }
        burn(5000000LL);
    }
    return NULL;
}
#endif

#ifndef __APPLE__
static void recvmmsg_timeout(void) {
#ifdef __linux__
    enum { SLOTS = 2048 };
    static struct mmsghdr messages[SLOTS];
    static struct iovec vectors[SLOTS];
    static char buffers[SLOTS];
    struct timespec timeout;
    pthread_t helper;
    int pair[2];
    int index;

    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, pair) != 0) {
        skipped("recvmmsg");
        return;
    }
    pacer_socket = pair[1];
    if (pthread_create(&helper, NULL, pacer, NULL) != 0) {
        skipped("recvmmsg");
        close(pair[0]);
        close(pair[1]);
        return;
    }
    memset(messages, 0, sizeof messages);
    for (index = 0; index < SLOTS; index++) {
        vectors[index].iov_base = &buffers[index];
        vectors[index].iov_len = 1;
        messages[index].msg_hdr.msg_iov = &vectors[index];
        messages[index].msg_hdr.msg_iovlen = 1;
    }
    /* The kernel checks this timeout after each datagram, so the stream above is what lets it
       expire at all; the man page calls that out as a bug of the call itself. */
    timeout = spec_of(base_ns);
    begin();
    done("recvmmsg", base_ns, recvmmsg(pair[0], messages, SLOTS, 0, &timeout));
    pacing = 0;
    pthread_join(helper, NULL);
    close(pair[0]);
    close(pair[1]);
#else
    /* Here the call waits its timeout out before the first message, so an idle socket is the whole
       of the setup and one slot is enough. */
    struct mmsghdr message;
    struct iovec vector;
    char buffer;
    struct timespec timeout;
    int pair[2];

    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, pair) != 0) {
        skipped("recvmmsg");
        return;
    }
    memset(&message, 0, sizeof message);
    vector.iov_base = &buffer;
    vector.iov_len = sizeof buffer;
    message.msg_hdr.msg_iov = &vector;
    message.msg_hdr.msg_iovlen = 1;
    timeout = spec_of(base_ns);
    begin();
    done("recvmmsg", base_ns, recvmmsg(pair[0], &message, 1, 0, &timeout));
    close(pair[0]);
    close(pair[1]);
#endif
}
#endif

/* ---- the timeouts that name an absolute deadline ---- */

#ifndef __APPLE__
static pthread_mutex_t held_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_rwlock_t held_rwlock = PTHREAD_RWLOCK_INITIALIZER;
static mtx_t held_c11;
static sem_t holder_ready;
static sem_t holder_release;

/* Takes every lock the timed locks below are measured against, and holds them until released. It
   waits on a plain semaphore, which is not a call the library takes over. */
static void *holder(void *arg) {
    (void)arg;
    pthread_mutex_lock(&held_mutex);
    pthread_rwlock_wrlock(&held_rwlock);
    mtx_lock(&held_c11);
    sem_post(&holder_ready);
    sem_wait(&holder_release);
    mtx_unlock(&held_c11);
    pthread_rwlock_unlock(&held_rwlock);
    pthread_mutex_unlock(&held_mutex);
    return NULL;
}

static void timed_locks(void) {
    pthread_t thread;
    struct timespec deadline;

    if (mtx_init(&held_c11, mtx_plain) != thrd_success
        || sem_init(&holder_ready, 0, 0) != 0
        || sem_init(&holder_release, 0, 0) != 0
        || pthread_create(&thread, NULL, holder, NULL) != 0) {
        skipped("pthread_mutex_timedlock");
        skipped("pthread_rwlock_timedrdlock");
        skipped("pthread_rwlock_timedwrlock");
        skipped("pthread_timedjoin_np");
        skipped("mtx_timedlock");
        return;
    }
    sem_wait(&holder_ready);

    deadline = deadline_on(CLOCK_REALTIME);
    begin();
    done("pthread_mutex_timedlock", base_ns, pthread_mutex_timedlock(&held_mutex, &deadline));

    deadline = deadline_on(CLOCK_REALTIME);
    begin();
    done("pthread_rwlock_timedrdlock", base_ns,
         pthread_rwlock_timedrdlock(&held_rwlock, &deadline));

    deadline = deadline_on(CLOCK_REALTIME);
    begin();
    done("pthread_rwlock_timedwrlock", base_ns,
         pthread_rwlock_timedwrlock(&held_rwlock, &deadline));

    deadline = deadline_on(CLOCK_REALTIME);
    begin();
    done("mtx_timedlock", base_ns, mtx_timedlock(&held_c11, &deadline));

    /* The holder is still inside its own wait, so this one runs its deadline out too. */
    deadline = deadline_on(CLOCK_REALTIME);
    begin();
    done("pthread_timedjoin_np", base_ns, pthread_timedjoin_np(thread, NULL, &deadline));

    sem_post(&holder_release);
    pthread_join(thread, NULL);
}

static void timed_semaphores(void) {
    sem_t empty;
    struct timespec deadline;

    if (sem_init(&empty, 0, 0) != 0) {
        skipped("sem_timedwait");
#ifdef __linux__
        skipped("sem_clockwait");
#endif
        return;
    }
    deadline = deadline_on(CLOCK_REALTIME);
    begin();
    done("sem_timedwait", base_ns, sem_timedwait(&empty, &deadline));

#ifdef __linux__
    deadline = deadline_on(CLOCK_MONOTONIC);
    begin();
    errno = 0;
    {
        int rc = sem_clockwait(&empty, CLOCK_MONOTONIC, &deadline);
        if (rc < 0 && errno == ENOSYS) {
            skipped("sem_clockwait");
        } else {
            done("sem_clockwait", base_ns, rc);
        }
    }
#endif
    sem_destroy(&empty);
}
#endif

static void condition_variables(void) {
    pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
    pthread_cond_t wall = PTHREAD_COND_INITIALIZER;
    struct timespec deadline;
#ifndef __APPLE__
    pthread_cond_t steady;
    pthread_condattr_t attr;
    cnd_t c11_cond;
    mtx_t c11_mutex;
#endif

    /* Built with the default clock, so its deadline is read on the wall clock. This is the whole
       of the group on a platform with no attribute to name another one. */
    pthread_mutex_lock(&mutex);
    deadline = deadline_on(CLOCK_REALTIME);
    begin();
    done("pthread_cond_timedwait", base_ns, pthread_cond_timedwait(&wall, &mutex, &deadline));
    pthread_mutex_unlock(&mutex);
#ifndef __APPLE__
    pthread_mutex_lock(&mutex);

    /* Built with a clock of its own, which is what `pthread_cond_init` is interposed to learn. */
    pthread_condattr_init(&attr);
    pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
    pthread_cond_init(&steady, &attr);
    pthread_condattr_destroy(&attr);
    deadline = deadline_on(CLOCK_MONOTONIC);
    begin();
    done("pthread_cond_timedwait_monotonic", base_ns,
         pthread_cond_timedwait(&steady, &mutex, &deadline));

#ifdef __linux__
    deadline = deadline_on(CLOCK_MONOTONIC);
    begin();
    errno = 0;
    {
        int rc = pthread_cond_clockwait(&wall, &mutex, CLOCK_MONOTONIC, &deadline);
        if (rc == ENOSYS) {
            skipped("pthread_cond_clockwait");
        } else {
            done("pthread_cond_clockwait", base_ns, rc);
        }
    }
#endif
    pthread_mutex_unlock(&mutex);

    /* Destroying the variable makes the library forget the clock it was built with. Its memory is
       then reused by one built the way a static initialiser builds it, which never reaches
       `pthread_cond_init` at all, so its deadline is read on the wall clock. A library still
       holding the old clock would measure this against the wrong family. */
    pthread_cond_destroy(&steady);
    {
        pthread_cond_t reused = PTHREAD_COND_INITIALIZER;
        memcpy(&steady, &reused, sizeof steady);
    }
    pthread_mutex_lock(&mutex);
    deadline = deadline_on(CLOCK_REALTIME);
    begin();
    done("pthread_cond_timedwait_after_destroy", base_ns,
         pthread_cond_timedwait(&steady, &mutex, &deadline));
    pthread_mutex_unlock(&mutex);

    if (mtx_init(&c11_mutex, mtx_plain) != thrd_success
        || cnd_init(&c11_cond) != thrd_success) {
        skipped("cnd_timedwait");
        return;
    }
    mtx_lock(&c11_mutex);
    /* C11 reads this deadline on TIME_UTC, which is the wall clock. */
    deadline = deadline_on(CLOCK_REALTIME);
    begin();
    done("cnd_timedwait", base_ns, cnd_timedwait(&c11_cond, &c11_mutex, &deadline));
    mtx_unlock(&c11_mutex);
#endif
}

#ifndef __APPLE__
static void message_queues(void) {
    struct mq_attr attributes;
    struct timespec deadline;
    char name[64];
    char message[8] = "warp";
    char received[8];
    mqd_t queue;

    memset(&attributes, 0, sizeof attributes);
    attributes.mq_maxmsg = 1;
    attributes.mq_msgsize = sizeof message;
    snprintf(name, sizeof name, "/libtimewarp-waits-%d", (int)getpid());
    queue = mq_open(name, O_CREAT | O_RDWR, 0600, &attributes);
    if (queue == (mqd_t)-1) {
        skipped("mq_timedreceive");
        skipped("mq_timedsend");
        return;
    }

    deadline = deadline_on(CLOCK_REALTIME);
    begin();
    done("mq_timedreceive", base_ns,
         (long long)mq_timedreceive(queue, received, sizeof received, NULL, &deadline));

    /* One message fills the queue, so the next send has to wait its deadline out. */
    mq_send(queue, message, sizeof message, 0);
    deadline = deadline_on(CLOCK_REALTIME);
    begin();
    done("mq_timedsend", base_ns, mq_timedsend(queue, message, sizeof message, 0, &deadline));

    mq_close(queue);
    mq_unlink(name);
}
#endif

int main(int argc, char **argv) {
    const char *shared;

    if (argc < 2) {
        return 2;
    }
    base_ns = atoll(argv[1]) * 1000000LL;

    sleeps();
    interrupted_sleep();
    relative_timeouts();
    aio_timeout();
    condition_variables();
#ifndef __APPLE__
    recvmmsg_timeout();
    timed_locks();
    timed_semaphores();
    message_queues();
#endif

    shared = getenv("LIBTIMEWARP__SHARED_FILE");
    printf("shared=%s\n", shared ? shared : "");
    fflush(stdout);
    return 0;
}
