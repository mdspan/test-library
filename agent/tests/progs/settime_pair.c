/* Two processes in one session: one sets the wall clock, the other watches it move on its next
   read. argv[1] is the instant the setter asks for, in nanoseconds.

   The child inherits the mapped session from the fork, so both processes compute from the same
   parameters. Two pipes order the three steps: the observer reads before the set, the setter sets,
   the observer reads again. */
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define NS_PER_S 1000000000LL

static void say(const char *key, long long value) { printf("%s=%lld\n", key, value); }

static long long wall_clock(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (long long)ts.tv_sec * NS_PER_S + ts.tv_nsec;
}

/* One byte down a pipe, and one byte off one: the whole of the ordering between the two. */
static void nudge(int fd) { (void)!write(fd, "x", 1); }
static void await(int fd) {
    char byte;
    (void)!read(fd, &byte, 1);
}

int main(int argc, char **argv) {
    long long target = argc > 1 ? atoll(argv[1]) : 0;
    int watching[2], moved[2];
    pid_t child;
    struct timeval tv;
    int result;

    if (pipe(watching) != 0 || pipe(moved) != 0) {
        return 1;
    }

    child = fork();
    if (child < 0) {
        return 1;
    }
    if (child == 0) {
        close(watching[0]);
        close(moved[1]);
        say("observer.before", wall_clock());
        nudge(watching[1]);
        await(moved[0]);
        say("observer.after", wall_clock());
        fflush(stdout);
        _exit(0);
    }

    close(watching[1]);
    close(moved[0]);
    await(watching[0]);

    tv.tv_sec = (time_t)(target / NS_PER_S);
    tv.tv_usec = (suseconds_t)((target % NS_PER_S) / 1000);
    errno = 0;
    result = settimeofday(&tv, NULL);
    say("setter.result", result);
    say("setter.errno", result == 0 ? 0 : errno);
    say("setter.after", wall_clock());
    fflush(stdout);

    nudge(moved[1]);
    waitpid(child, NULL, 0);
    say("target", target);
    return 0;
}
