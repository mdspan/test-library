# Configuration

libtimewarp is configured with environment variables, read when a program
starts under the library. The same variables can also go in a
[configuration file](#the-configuration-file).

Most of them describe the clock a new [session](SESSIONS.md) starts with,
and apply only when a session is created: the first faked program you start
reads them, and the programs it starts join its session and ignore them.
To change a clock once it is running, use the [controller](CONTROL.md).
The exceptions are the variables under [Logging](#logging) and
[Excluding Commands](#excluding-commands), which every program reads for
itself.

## The Configuration File

| Variable | Default | Meaning |
| --- | --- | --- |
| `LIBTIMEWARP_CONFIG` | none | Read this configuration file in place of the defaults. |

Settings you use every time can go in a [TOML](https://toml.io) file.
Without `LIBTIMEWARP_CONFIG`, the library reads
`~/.config/libtimewarp/config.toml` if it exists (under `$XDG_CONFIG_HOME`
when that is set), and `/etc/libtimewarp/config.toml` otherwise. Only one
file is read.

```toml
# ~/.config/libtimewarp/config.toml
shift = "-400d"
skip_cmds = ["curl", "git"]

[realtime]
rate = 0

[log]
file = "/tmp/timewarp.log"
```

Each key stands for one variable on this page. Settings that name no
family go at the top level, a family's settings go in a table named after
it, and logging goes in `log`:

| Key | Variable | Value |
| --- | --- | --- |
| `time` | `LIBTIMEWARP_TIME` | a date-time with an offset, `2020-01-01T00:00:00Z`, or a string such as `"@1577836800"` |
| `shift` | `LIBTIMEWARP_SHIFT` | a duration string, `"-2h"`, or a number of seconds |
| `rate` | `LIBTIMEWARP_RATE` | a number |
| `realtime.shift`, `monotonic.shift`, `boottime.shift` | `LIBTIMEWARP_<FAMILY>_SHIFT` | a duration string, or a number of seconds |
| `realtime.rate`, `monotonic.rate`, `boottime.rate`, `cpu.rate` | `LIBTIMEWARP_<FAMILY>_RATE` | a number |
| `realtime.timebase`, `monotonic.timebase`, `boottime.timebase` | `LIBTIMEWARP_<FAMILY>_TIMEBASE` | a clock name string |
| `emulate_vdso` | `LIBTIMEWARP_EMULATE_VDSO` | `true` or `false` |
| `keeper` | `LIBTIMEWARP_KEEPER` | `"affine"` or `"null"` |
| `hook` | `LIBTIMEWARP_HOOK` | a string, `"/path/to/hook.so@size"` |
| `skip_cmds`, `only_cmds` | `LIBTIMEWARP_SKIP_CMDS`, `LIBTIMEWARP_ONLY_CMDS` | an array of command names |
| `log.file` | `LIBTIMEWARP_LOG` | a path string |
| `log.fd` | `LIBTIMEWARP_LOG_FD` | an integer |
| `log.level` | `LIBTIMEWARP_LOG_LEVEL` | a level string, `"debug"` |
| `session_dir` | `LIBTIMEWARP_SESSION_DIR` | a path string |
| `fail_closed` | `LIBTIMEWARP_FAIL_CLOSED` | `true` or `false` |

A rate may also be written as a string, `"0.5"`. Values follow the same
rules as in [Values](#values).

- A variable set in the environment wins over the file. Setting it to an
  empty value, `LIBTIMEWARP_SHIFT=`, clears the file's setting for that
  run.
- `LIBTIMEWARP_CONFIG=/dev/null` reads no file at all.

The file only describes how a session starts. Editing it does not change a
program that is already running; use the [controller](CONTROL.md) for that.

A file the library cannot read, that is not valid TOML, that has a key not
in the table above, or a value of the wrong type, leaves the program on
the real time. The log line names the file and the line:

```console
$ LIBTIMEWARP_LOG_FD=2 LD_PRELOAD=$TIMEWARP date -u
libtimewarp[26661] error: /home/me/.config/libtimewarp/config.toml: line 2: not a setting
Sat Sep 26 03:46:52 UTC 2026
```

The logging and fail-closed settings for that error come from the
environment alone, since the file could not be used. A value of the right
type that is still wrong, `rate = -1`, is reported by its variable's name,
as it would be in the environment.

## Setting the Clock

A setting applies to a whole [family](CLOCKS.md) of clocks. `<FAMILY>` below
is `REALTIME`, `MONOTONIC`, or `BOOTTIME`.

| Variable | Default | Meaning |
| --- | --- | --- |
| `LIBTIMEWARP_TIME` | none | Start the wall clock at this date, such as `2020-01-01T00:00:00Z` or `@1577836800`. |
| `LIBTIMEWARP_SHIFT` | none | Move the wall clock by a duration, such as `-2h`. |
| `LIBTIMEWARP_RATE` | `1` | How fast time passes for the wall clock and both monotonic families. `10` is ten times real speed, `0.5` half speed, `0` frozen. |
| `LIBTIMEWARP_<FAMILY>_SHIFT` | none | Move one family. `LIBTIMEWARP_REALTIME_SHIFT` wins over `LIBTIMEWARP_SHIFT`. |
| `LIBTIMEWARP_<FAMILY>_RATE` | `1` | How fast one family runs. Wins over `LIBTIMEWARP_RATE`. |
| `LIBTIMEWARP_CPU_RATE` | `1` | How fast CPU time accumulates, for the process and each thread. |
| `LIBTIMEWARP_<FAMILY>_TIMEBASE` | see [Timebases](CLOCKS.md#timebases) | Which real clock the family follows, by name, such as `CLOCK_BOOTTIME`. |

Some combinations are worth knowing:

- `LIBTIMEWARP_TIME` with `LIBTIMEWARP_REALTIME_RATE=0` is a frozen date.
  `LIBTIMEWARP_TIME` alone is a start date the clock runs on from.
- `LIBTIMEWARP_SHIFT` and `LIBTIMEWARP_TIME` move the wall clock only. The
  monotonic clocks count from boot, and "two hours ago" says nothing about
  them. Shift those with `LIBTIMEWARP_MONOTONIC_SHIFT` and
  `LIBTIMEWARP_BOOTTIME_SHIFT`.
- A monotonic shift may be negative, as long as it leaves the clock at or
  above zero.
- `LIBTIMEWARP_RATE` leaves CPU time alone. "Ten times faster" is about
  elapsed time, and CPU time measures work done. Set
  `LIBTIMEWARP_CPU_RATE` to scale it. The CPU rate is fixed for the life of
  the session, and no controller can change it.

### Values

- **Durations** are a number, optionally with a fraction, and one unit:
  `ns`, `us`, `ms`, `s`, `m`, `h`, or `d`, where a day is 86400 seconds.
  A bare number is seconds, so `-7200` and `-2h` are the same. Write a
  year in days, `365d`.
- **Dates** are RFC 3339 timestamps, `2020-01-01T00:00:00Z`, or `@`
  followed by Unix seconds, `@1577836800`. Any date between the years 1678
  and 2262 works.
- **Rates** are non-negative numbers. `0` freezes the family.
- **Clock names** are the C names, `CLOCK_MONOTONIC_RAW` for example.
  [Clocks and Families](CLOCKS.md) lists the ones each platform has.

## Go and Programs That Bypass libc

| Variable | Default | Meaning |
| --- | --- | --- |
| `LIBTIMEWARP_EMULATE_VDSO` | `1` | `0` leaves programs that read the clock without libc, Go programs among them, on real time. Every other program is faked as usual. |

## Hooks and Timekeepers

| Variable | Default | Meaning |
| --- | --- | --- |
| `LIBTIMEWARP_HOOK` | none | `/path/to/hook.so@size`: a [hook](HOOKS.md) library of your own that answers clock reads, and the size in bytes of the memory it shares across the session. `@size` is optional and defaults to `0`. |
| `LIBTIMEWARP_KEEPER` | `affine` | `null` turns off every setting in [Setting the Clock](#setting-the-clock), so a hook works on top of real time. |

## Excluding Commands

| Variable | Default | Meaning |
| --- | --- | --- |
| `LIBTIMEWARP_SKIP_CMDS` | none | Commands that see the real time, comma-separated. |
| `LIBTIMEWARP_ONLY_CMDS` | none | Fake only these commands, comma-separated. |

Some programs misbehave under a shifted date: `curl` and `git` reject every
certificate once the date moves outside their validity. Name such a
program by the file name of its executable alone, `curl` for
`/usr/bin/curl`:

```sh
LIBTIMEWARP_SHIFT=-400d LIBTIMEWARP_SKIP_CMDS=curl,git LD_PRELOAD=$TIMEWARP bash
```

An excluded program sees the real time, and the programs it starts are
faked as usual. Set one list or the other, never both.

## Logging

| Variable | Default | Meaning |
| --- | --- | --- |
| `LIBTIMEWARP_LOG` | none | Append log lines to this file. |
| `LIBTIMEWARP_LOG_FD` | none | Write log lines to this open file descriptor, `2` for standard error. Wins over `LIBTIMEWARP_LOG`. |
| `LIBTIMEWARP_LOG_LEVEL` | `info` | `error`, `warn`, `info`, `debug`, or `trace`. |

With neither `LIBTIMEWARP_LOG` nor `LIBTIMEWARP_LOG_FD` set, the library
writes nothing anywhere. [Logging and Troubleshooting](TROUBLESHOOTING.md)
lists what it logs.

## Where Sessions Live

| Variable | Default | Meaning |
| --- | --- | --- |
| `LIBTIMEWARP_SESSION_DIR` | see [Sessions](SESSIONS.md#where-they-live) | The directory session files go under. |

## When a Value Is Wrong

A value the library cannot use is logged, and by default the program runs
on the real time, with every setting ignored:

```console
$ LIBTIMEWARP_LOG_FD=2 LIBTIMEWARP_RATE=-1 LD_PRELOAD=$TIMEWARP date -u
libtimewarp[26663] error: LIBTIMEWARP_RATE: not a valid value
Sat Sep 26 03:46:53 UTC 2026
```

The library lives inside your program, so it never stops the program on
its own account, and it never fakes half of what you asked for. Where
there is a session to record it in, the problem is also recorded there, and
[`libtimewarp info`](CLI.md#commands) shows it in the `fault` line.

These are wrong values:

- a duration, date, rate, or clock name that does not parse;
- a negative rate;
- `LIBTIMEWARP_TIME` together with a wall-clock shift;
- a date outside 1678 to 2262;
- a monotonic shift that would take the clock below zero;
- a clock name that cannot be that family's
  [timebase](CLOCKS.md#timebases);
- a hook that fails to load, or whose `libtimewarp_hook_init` refuses;
- both `LIBTIMEWARP_SKIP_CMDS` and `LIBTIMEWARP_ONLY_CMDS`;
- a [configuration file](#the-configuration-file) that cannot be read, is
  not valid TOML, or has an unknown key or a value of the wrong type.

### Failing Closed

| Variable | Default | Meaning |
| --- | --- | --- |
| `LIBTIMEWARP_FAIL_CLOSED` | `0` | `1` makes a wrong value stop the program, with exit status 78. |

Real time is the safe answer inside a program you are only observing, and
the wrong one for a test: a test that asked for 2020 and got today may pass
for the wrong reason. With `LIBTIMEWARP_FAIL_CLOSED=1`, the program prints
the reason to standard error and exits with status 78 before its `main`
runs:

```console
$ LIBTIMEWARP_FAIL_CLOSED=1 LIBTIMEWARP_RATE=-1 LD_PRELOAD=$TIMEWARP date -u
libtimewarp[26664] error: LIBTIMEWARP_RATE: not a valid value
$ echo $?
78
```

The setting belongs to the session like the clock settings do, so a
program that joins the session later and hits a problem, a hook that fails
to load there for instance, exits too.

## Variables the Library Sets

`LIBTIMEWARP__SHARED_FILE`, with two underscores, is how the programs of a
session find it. The library sets it to the absolute path of the session
file; you never need to. Unset it to start
a fresh session from inside a faked shell; see
[Sessions](SESSIONS.md#one-clock-per-process-tree).

The library also sets it in the environment the program was started with,
which is what a Go program passes to the programs it starts. There it
takes the place of the variable's own entry, or else of the first clock,
Go, hook, or timekeeper setting, which the program then no longer sees in
that environment. Starting a program with `LIBTIMEWARP__SHARED_FILE` set to
empty keeps every other variable; see
[How Programs Find the Session](SESSIONS.md#how-programs-find-the-session).
