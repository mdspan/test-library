# Hooks

The settings in [Configuration](CONFIGURATION.md) describe a clock that runs
at a steady rate from a starting point. A **hook** replaces that with code
of your own: a clock that replays a recording, follows a simulator, jumps at
scripted moments, or advances a fixed step per read.

A hook is a shared library with one required function. libtimewarp loads it
into every program of the session and calls it on every call it fakes. The
hook answers the calls it cares about and declines the rest, and a declined
call gets the answer it would have had without the hook.

## An Example

Every wall-clock read in this session moves the clock forward exactly 1 ms,
starting at 2020-01-01. The read count lives in the session's shared memory,
so all the programs of the session share one sequence.

```c
/* Every wall-clock read in the session advances the clock by exactly 1 ms,
   starting at 2020-01-01T00:00:00Z. The count lives in the arena, so every
   process of the session shares one clock. */
#include <stdatomic.h>
#include <stdint.h>
#include <time.h>

#include "libtimewarp_hook.h"

#define START_NS 1577836800000000000LL
#define STEP_NS 1000000LL

int32_t libtimewarp_hook_init(void *arena, uint64_t size) {
    if (arena == NULL || size < sizeof(_Atomic int64_t)) {
        return 1; /* refuse: this hook needs an arena */
    }
    atomic_init((_Atomic int64_t *)arena, 0);
    return 0;
}

int32_t libtimewarp_hook(const struct libtimewarp_call *call, int64_t *out) {
    if (call->kind != LIBTIMEWARP_INSTANT_TO_FAKE || call->clock != CLOCK_REALTIME) {
        return LIBTIMEWARP_DECLINED;
    }
    int64_t reads = atomic_fetch_add((_Atomic int64_t *)call->arena, 1);
    *out = START_NS + reads * STEP_NS;
    return LIBTIMEWARP_HANDLED;
}
```

Build it against `include/libtimewarp_hook.h` and name it in
`LIBTIMEWARP_HOOK`, with the size of the shared memory after `@`:

```console
$ cc -shared -fPIC -I include -o tick.so tick.c
$ LIBTIMEWARP_HOOK=$PWD/tick.so@4096 LD_PRELOAD=$TIMEWARP \
    sh -c 'date -u +%T.%3N; date -u +%T.%3N; date -u +%T.%3N'
00:00:00.009
00:00:00.010
00:00:00.011
```

The first `date` shows `.009` because the shell and `date` read the wall
clock several times before printing.

## The Functions

```c
int32_t libtimewarp_hook(const struct libtimewarp_call *call, int64_t *out);
int32_t libtimewarp_hook_init(void *arena, uint64_t size);
```

**`libtimewarp_hook`** is required. It runs on every faked call, in every
program of the session. It returns `LIBTIMEWARP_HANDLED` after writing the
answer to `*out`, which the program then sees as is, or
`LIBTIMEWARP_DECLINED` to let the call be answered normally.

**`libtimewarp_hook_init`** is optional. It runs once, in the program that
creates the session, before any other program can see the shared memory, so
it is the place to set that memory up. Returning 0 lets the session start.
Anything else is a [wrong value](CONFIGURATION.md#when-a-value-is-wrong):
the error is logged and the programs run on real time.

## The Call

| Field | Meaning |
| --- | --- |
| `func` | Which function the program called, from `enum libtimewarp_func`: `LIBTIMEWARP_FUNC_CLOCK_GETTIME`, `LIBTIMEWARP_FUNC_NANOSLEEP`, `LIBTIMEWARP_FUNC_STAT`, and so on. Aliases report the name they alias. |
| `kind` | Which of the four conversions below this is. |
| `clock` | The clock id being read or waited on. Calls that name no clock carry `CLOCK_REALTIME`. |
| `value` | The number to convert, in nanoseconds. |
| `real_ns` | The real reading of `clock` right now. |
| `keeper` | The answer the call gets if the hook declines. |
| `arena`, `size` | The session's shared memory and its size, or `NULL` and 0. |

Every call is one of four conversions:

| `kind` | `value` is | `*out` is | Used by |
| --- | --- | --- | --- |
| `LIBTIMEWARP_INSTANT_TO_FAKE` | a real instant | the faked instant | clock reads, file timestamp reads |
| `LIBTIMEWARP_INSTANT_TO_GENUINE` | a faked instant | the real instant | file timestamp sets, setting the clock |
| `LIBTIMEWARP_DURATION_TO_GENUINE` | a faked duration | the real duration to wait | sleeps, timeouts, arming timers |
| `LIBTIMEWARP_DURATION_TO_FAKE` | a real remaining time | the faked remaining time | timer queries such as `getitimer` |

So a hook that can answer a clock read and scale a duration handles
everything, and a hook can still look at `func` for special cases. A wait
with an absolute deadline arrives as a clock read for the faked now,
followed by a duration.

A timer arm the hook answers with 0 or less fires at once, and an interval
answered that way repeats as often as the system allows: a timer the program
armed stays armed whatever the hook answers. An answer for `setitimer`,
`getitimer` or `alarm` rounds up to the next whole microsecond, the unit of
the interval timer underneath all three.

Calls that set the clock, `settimeofday` and its neighbours, reach the hook
as `LIBTIMEWARP_INSTANT_TO_GENUINE`, with the instant the program asked for.
Handling one tells the program it succeeded, and `*out` is ignored.
Declining it lets the call go ahead as it would without the hook, which
[What Gets Faked](COVERAGE.md#setting-the-time) describes.

## Shared Memory

The number after `@` in `LIBTIMEWARP_HOOK` is the size of the **arena**: a
block of memory every program of the session maps, at the same contents.
It is rounded up to whole pages, and `@0`, or no `@` at all, means no arena.
Use atomic operations on it: many programs and threads read and write it at
once.

## Reading Clocks From a Hook

- A clock read made from inside the hook skips the hook and returns the
  answer it would have had without it. That is how a hook reads the faked
  value of another clock.
- `libtimewarp_genuine_clock_gettime`, declared in the same header, returns
  the real value of any clock.

## Writing a Safe Hook

The hook runs wherever a program reads the clock, and programs read the
clock in signal handlers and, in Go, on small borrowed stacks. A hook that
has to hold up there:

- uses only async-signal-safe functions, and allocates nothing;
- keeps its stack frames small;
- returns normally, since a `longjmp` out of the hook leaves that thread
  bypassing the hook from then on.

On Linux arm64, a clock read the Go runtime makes reaches the hook with
every signal blocked. The runtime keeps its own state in a register that a
compiled hook is free to use, and a signal handled while the hook had that
register in use would find something else there and crash the program.
Signals that arrive meanwhile are delivered once the hook returns, so a hook
must not wait for one, and each of those reads costs two more system calls.

The [promises](README.md#what-it-promises) libtimewarp makes, monotonic
clocks never running backwards among them, hold for the calls the hook
declines. For the calls it handles they are the hook's to keep: a hook that
answers `CLOCK_MONOTONIC` reads has to return values that never decrease,
across every thread and every program of the session. libtimewarp checks
none of this. A program that never reads the clock from a signal handler
leaves the hook free to do more.

## Hooks and the Other Settings

A hook works on top of the normal clock. With the default keeper, a
declined call gets the faked clock from the settings in
[Configuration](CONFIGURATION.md), and the [controller](CONTROL.md) keeps
working. With `LIBTIMEWARP_KEEPER=null`, a declined call gets the real time.
