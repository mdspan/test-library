# Platform Notes

## Go

`time.Now`, `time.Since`, timers, and tickers follow the faked clock. On
macOS, Go calls the system library like any other program, and nothing
below applies but
[The Programs a Go Program Starts](#the-programs-a-go-program-starts). On
Linux and FreeBSD, the Go runtime reads the clock without calling the C
library, and libtimewarp fakes those reads under three conditions.

**The program has to be dynamically linked.** Go links statically by
default, and a statically linked program never loads the library. Build
with cgo enabled and as a position-independent executable:

```sh
CGO_ENABLED=1 go build -buildmode=pie -o myprog .
file myprog     # should say "dynamically linked"
```

The same program, printing `time.Now()`, built both ways:

```console
$ CGO_ENABLED=1 go build -buildmode=pie -o now-pie now.go
$ go build -o now-static now.go
$ LIBTIMEWARP_TIME=2020-01-01T00:00:00Z LD_PRELOAD=$TIMEWARP ./now-pie
2020-01-01T00:00:00Z
$ LIBTIMEWARP_TIME=2020-01-01T00:00:00Z LD_PRELOAD=$TIMEWARP ./now-static
2026-09-26T03:47:51Z
```

Go builds no position-independent executable on FreeBSD arm64. There, link
externally instead, which makes the program dynamically linked as well:

```sh
CGO_ENABLED=1 go build -ldflags=-linkmode=external -o myprog .
```

**`LIBTIMEWARP_EMULATE_VDSO` has to be on**, which is the default, and the
library has to be built with its default features; see
[Installation](INSTALL.md#leaving-out-go-support).

**On FreeBSD, Go 1.21 through 1.27 has to have built the program.** The
library finds the Go runtime's clock code there by its machine code, which
each Go release encodes its own way, and it carries the encodings of those
releases. A program built by another release may run on real time, and the
log then names the release at `warn` level; see
[Logging and Troubleshooting](TROUBLESHOOTING.md#what-gets-logged).

**Two things stay real**, because the Go runtime asks the kernel for them
directly:

- **File timestamps.** `os.Stat` shows real dates.
- **How long a sleep blocks when time runs fast.** A Go sleep or timer ends
  at the right faked instant as far as the program can tell, but under a
  rate above 1 it blocks for about the real length of the sleep: at rate 10,
  `time.Sleep(10 * time.Second)` takes about ten real seconds. Under a rate
  below 1 the timing is right. A freeze holds every Go timer, as it should.

### The Programs a Go Program Starts

On every platform, a Go program passes the programs it starts the
environment it was itself started with, never the one the C library keeps.
The library names the session there as well, so those programs join it.
That environment has no room for a new variable, so the session's name
takes the place of `LIBTIMEWARP__SHARED_FILE` when the program was started
with it, and otherwise of the first setting there that only a new session
reads, such as `LIBTIMEWARP_TIME`. The Go program's own `os.Getenv` then
reads that setting as empty, and the programs it starts do not get it;
having joined the session, they have no use for it.

Start the program with `LIBTIMEWARP__SHARED_FILE` set to empty to keep
every setting:

```sh
LIBTIMEWARP__SHARED_FILE= LIBTIMEWARP_TIME=2020-01-01T00:00:00Z LD_PRELOAD=$TIMEWARP ./myprog
```

A program started with neither `LIBTIMEWARP__SHARED_FILE` nor such a
setting logs a `warn` line, and the programs it starts begin sessions of
their own. See
[How Programs Find the Session](SESSIONS.md#how-programs-find-the-session).

## Linux

- Both glibc and musl work.
- Setting the clock from a faked program needs the `CAP_SYS_TIME`
  capability, the same as setting the real clock; see
  [Setting the Time](COVERAGE.md#setting-the-time).

## macOS

The library goes in through `DYLD_INSERT_LIBRARIES`:

```sh
DYLD_INSERT_LIBRARIES=$PWD/target/release/libtimewarp.dylib LIBTIMEWARP_SHIFT=-2h ./myprog
```

**Protected programs refuse the library.** System Integrity Protection
removes `DYLD_INSERT_LIBRARIES` from system programs, and from programs
built with the hardened runtime unless they carry the entitlement that
allows it. `/bin/sh`, `/bin/bash`, and `/usr/bin/env` are all protected, so:

- a faked program that runs a command through `system()`, `popen()`, or
  `sh -c` loses the library for that command and everything it starts;
- the system `bash` cannot be faked, so the shell example in
  [First Steps](FIRST_STEPS.md#a-whole-shell) needs a copy of the shell
  outside the protected directories.

Run programs directly, or copy the shell and the tools a program starts to
a directory outside the protected ones and use the copies. On a Mac with
System Integrity Protection turned off, the system programs pass the
library on like any other.

**Some calls stay real:**

- Timers armed through a kqueue, including `dispatch_after` and dispatch
  timer sources, fire on real time.
- `pthread_cond_timedwait_relative_np` waits for real time.
- A program that reads the hardware counter itself, without calling
  `mach_absolute_time`, reads the real counter. So do the system libraries,
  which read it that way.
- A clock port a system library obtained reads real time.
- On Intel Macs, programs built against SDKs old enough to use the 32-bit
  inode `stat` see real file dates.

**Timeouts are fewer.** macOS has no `clock_nanosleep`, `ppoll`, timed
mutex or semaphore waits, POSIX timers, or timerfds, so there is nothing of
those to fake. A condition variable always times out against the wall
clock there.

## FreeBSD

- Setting the clock from a faked program needs root, the same as setting
  the real clock.
- The two columns in [Which Clock Is in Which Family](CLOCKS.md#which-clock-is-in-which-family)
  matter: FreeBSD 15 moved `CLOCK_MONOTONIC` into Boottime.
