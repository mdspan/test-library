# First Steps

This chapter fakes a few programs and then changes a running one. It assumes
`TIMEWARP` holds the path of the library, as set up in
[Installation](INSTALL.md).

## Loading the Library

The library goes into a program through the dynamic loader:

| Platform | Variable |
| --- | --- |
| Linux, FreeBSD | `LD_PRELOAD` |
| macOS | `DYLD_INSERT_LIBRARIES` |

Everything else is set with `LIBTIMEWARP_` variables in the same
environment. The examples below use `LD_PRELOAD`; on macOS, write
`DYLD_INSERT_LIBRARIES` in its place.

## A Fixed Date

```console
$ LIBTIMEWARP_TIME=2020-01-01T00:00:00Z LD_PRELOAD=$TIMEWARP date -u
Wed Jan  1 00:00:00 UTC 2020
```

The clock starts at the date you gave and runs forward from there at the
normal speed.

## An Offset

```console
$ date -u
Sat Sep 26 03:36:39 UTC 2026
$ LIBTIMEWARP_SHIFT=-2h LD_PRELOAD=$TIMEWARP date -u
Sat Sep 26 01:36:39 UTC 2026
```

A duration is a number and a unit: `ns`, `us`, `ms`, `s`, `m`, `h`, or `d`.
`90s`, `-2h`, `1.5h`, and `365d` all work.

## Frozen Time

```console
$ LIBTIMEWARP_TIME=@1577836800 LIBTIMEWARP_RATE=0 LD_PRELOAD=$TIMEWARP \
    sh -c 'date -u +%s.%N; date -u +%s.%N'
1577836800.000000000
1577836800.000000000
```

`@` followed by a number is a Unix time in seconds. A rate of `0` stops the
clock. Sleeps stop with it: a program that sleeps under a frozen clock never
wakes, which [What Gets Faked](COVERAGE.md#sleeps-timeouts-and-timers)
explains.

## Faster Time

```console
$ LIBTIMEWARP_RATE=10 LD_PRELOAD=$TIMEWARP sh -c 'date +%s; sleep 10; date +%s'
1790394413
1790394423
```

The program sees ten seconds pass, and the `sleep 10` takes one real
second. Rates below 1 slow the clock down.

## A Whole Shell

Everything a faked program starts is faked the same way, on the same clock:

```sh
LIBTIMEWARP_SHIFT=-30d LD_PRELOAD=$TIMEWARP bash
```

Every command in that shell, and every command those commands run, sees the
date thirty days ago. The shell and its commands form one
[session](SESSIONS.md): they share one clock, and a change to it reaches all
of them.

## Changing the Clock While It Runs

Start a program under the library in one terminal:

```sh
LIBTIMEWARP_TIME=2020-01-01T00:00:00Z LD_PRELOAD=$TIMEWARP bash
```

In a second terminal, with the [command-line tool](CLI.md) installed:

```console
$ libtimewarp --newest get realtime
realtime.ns=1577836800659693134
realtime.iso=2020-01-01T00:00:00.659693134Z
$ libtimewarp --newest shift realtime 1h
realtime.ns=1577840400716066458
realtime.iso=2020-01-01T01:00:00.716066458Z
$ libtimewarp --newest set-rate realtime 2
realtime.rate=2.000000000
```

Run `date` in the first terminal after each command and it shows the change
straight away.

`--newest` picks the session created last. Every faked run leaves its
session file behind, so after the examples above there are several to pick
from; [Picking the Session](CLI.md#picking-the-session) covers the other
ways to choose, and [Cleaning Up](SESSIONS.md#cleaning-up) covers removing
old ones.

## Where to Go Next

- [Configuration](CONFIGURATION.md) lists every variable.
- [Clocks and Families](CLOCKS.md) explains why `LIBTIMEWARP_SHIFT` moves the
  date and leaves uptime alone, and how to change that.
- [Platform Notes](PLATFORMS.md) covers Go and the programs macOS will not
  let the library into.
