# Logging and Troubleshooting

## Turning On the Log

The library writes nothing unless you ask: a program's standard error
belongs to the program, and a line from the library could end up in its
output. Name a destination:

```sh
LIBTIMEWARP_LOG=/tmp/timewarp.log ...    # append to a file
LIBTIMEWARP_LOG_FD=2 ...                 # write to standard error
```

Every program of the session appends to the same file, one whole line at a
time. The file is created readable by you alone.

A line looks like this:

```text
libtimewarp[26663] error: LIBTIMEWARP_RATE: not a valid value
```

## What Gets Logged

| Level | Event |
| --- | --- |
| `error` | A [wrong value](CONFIGURATION.md#when-a-value-is-wrong), a hook that fails to load, or a session file the program cannot use. The program is running on real time. |
| `warn` | A program that was changing the clock appears to have died mid-change. The session recovers by itself. Or, on FreeBSD, a Go program built by a Go release the library does not match, named in the line; that program runs on real time. |
| `info` | A setting this build of the library ignores, such as asking for Go support from a build without it, or a program the library cannot fake Go in. |
| `debug` | On FreeBSD, a program that is not written in Go, so has no Go clock routines to take over. |

A healthy session logs nothing at `info` or above. Each problem is logged
once per program.

## Faults

Log lines can be lost with a closed file. So a problem that sends a program
to real time is also recorded in its session, where
[`libtimewarp info`](CLI.md#output) shows it long after the program is
gone:

```text
keeper=null
...
fault=bad_value
fault-pid=28964
```

`fault-pid` is the program that hit the problem. A problem in the program
that created the session sends the whole session to real time, which
`keeper=null` shows. A problem in a program that joined later, such as a
hook that fails to load there, sends only that program to real time, and the
rest of the session carries on.

The session keeps the first fault it gets. Later ones appear only in the
log.

## Common Problems

**Nothing is faked.**

- Check the library path in `LD_PRELOAD` or `DYLD_INSERT_LIBRARIES`. Use an
  absolute path. Run with `LIBTIMEWARP_LOG_FD=2` to see errors.
- A statically linked program never loads the library. `file ./program`
  says whether it is dynamically linked. Go programs are statically linked
  by default; see [Go](PLATFORMS.md#go).
- On FreeBSD, a Go program built by a Go release outside 1.21 through 1.27
  may run on real time. A `warn` line in the log names the release; see
  [Go](PLATFORMS.md#go).
- On macOS, system programs and programs started through `/bin/sh` refuse
  the library; see [macOS](PLATFORMS.md#macos).
- The program may be on `LIBTIMEWARP_SKIP_CMDS`, or missing from
  `LIBTIMEWARP_ONLY_CMDS`, perhaps in a
  [configuration file](CONFIGURATION.md#the-configuration-file).
- `libtimewarp info` may show a fault: an earlier wrong value sent the whole
  session to real time.

**A child program ignores its own settings.** It joined its parent's
session, as every child does. See
[One Clock per Process Tree](SESSIONS.md#one-clock-per-process-tree).

**A program hangs after a freeze and never recovers.** Threads that slept
under a frozen clock stay asleep after a thaw. Restart the program. See
[What Gets Faked](COVERAGE.md#sleeps-timeouts-and-timers).

**HTTPS or git fails under a shifted date.** Certificates look expired or
not yet valid on the faked date. Exclude those programs with
[`LIBTIMEWARP_SKIP_CMDS`](CONFIGURATION.md#excluding-commands).

**The command-line tool cannot find the session.** It looks in the
[session directory](SESSIONS.md#where-they-live) its own environment
computes. Run it with the same `LIBTIMEWARP_SESSION_DIR`, `XDG_RUNTIME_DIR`,
`TMPDIR`, and configuration file as the faked program, or pass `--dir` or `--session`. With
`--pid`, give a process started by the faked program; for the program you
started yourself, use `--creator`.

**A session file is refused after an upgrade.** Delete it and start the
program again; see [After an Upgrade](SESSIONS.md#after-an-upgrade).
