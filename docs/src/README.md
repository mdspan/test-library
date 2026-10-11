# Introduction

libtimewarp runs programs on a clock you control. Load it into a program and
that program, along with everything it starts, sees the time you choose:
a fixed date, an offset from today, time passing ten times faster, or time
standing still. You can change the clock while the programs run, and every
one of them sees the change on its next read.

It works on Linux, macOS, and FreeBSD, on `x86_64` and `aarch64`, with no
changes to the programs themselves and no privileges.

## What It Can Do

- **Set the clock.** Start a program at `2020-01-01T00:00:00Z`, or two hours
  in the past.
- **Change how fast time passes.** Run a program's clock at ten times real
  speed, at half speed, or frozen.
- **Change the clock live.** Shift, set, speed up, or freeze a running
  program from another terminal, with a command-line tool or from Python, C,
  or Rust.
- **Keep a whole process tree in step.** A shell and every command it runs
  share one clock.
- **Treat clocks differently.** The wall clock, the monotonic clocks, and
  CPU time each take their own settings, so you can move the date without
  breaking every timeout.
- **Replace the clock entirely.** A [hook](HOOKS.md) is a small library of
  your own that answers clock reads, for a clock that follows a recording,
  a simulator, or a script.
- **Reach Go programs.** The Go runtime reads the clock without calling
  libc, and libtimewarp fakes it anyway; see
  [Platform Notes](PLATFORMS.md#go).

Sleeps, timeouts, timers, and file timestamps follow the faked clock too.
[What Gets Faked](COVERAGE.md) has the full list.

## What It Promises

- **Monotonic clocks never run backwards**, in any process, including
  while you change them.
- **The wall clock moves backwards only when you move it**, or when the
  system clock it follows steps back. [Clocks and Families](CLOCKS.md#timebases)
  shows how to shut the second case out.
- **A change reaches every process at once.** The next read in every
  process of the tree sees it.
- **A crash never breaks the clock.** A process that dies while changing
  the clock leaves the others reading a valid time.
- **Clock reads are safe in signal handlers.**

A [hook](HOOKS.md) returns whatever it returns, so for the calls a hook
answers, these promises are the hook's to keep.

## Compared With libfaketime

[libfaketime](https://github.com/wolfcw/libfaketime) does the same job, and
libtimewarp is built to fake every call libfaketime fakes. The differences:

- **Go programs.** libfaketime cannot fake dynamically linked Go programs.
- **Live changes.** libfaketime rereads a configuration file every so
  often. libtimewarp changes are immediate, through a controller.
- **Monotonic clocks stay monotonic** when the settings change mid-run.
- **Signal handlers.** A libfaketime clock read from a signal handler can
  deadlock. A libtimewarp read cannot.
- **Clocks are grouped.** libfaketime applies one transformation to every
  clock. libtimewarp gives the wall clock, the monotonic clocks, and CPU
  time settings of their own.

Two things behave the same in both: a sleep that has already started keeps
the length it was given when it started, and a program built without
dynamic linking cannot be faked.

## Where to Start

[Installation](INSTALL.md) builds the library, and [First Steps](FIRST_STEPS.md)
walks through faking a program and changing its clock while it runs.
