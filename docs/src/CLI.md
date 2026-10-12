# The Command-Line Tool

`libtimewarp` reads and changes a running session.

## Installing

`cargo build --release` from [Installation](INSTALL.md#building) builds the
tool as `target/release/libtimewarp`. Run it from there, or install it into
Cargo's `bin` directory (`~/.cargo/bin` by default):

```sh
cargo install --path cli
```

## Commands

```sh
libtimewarp list                               # every session in the directory
libtimewarp info                               # setup, rates, timebases, and faults
libtimewarp get realtime                       # what a family shows now
libtimewarp shift realtime -2h                 # move a family by a duration
libtimewarp set-time realtime 2020-01-01T00:00:00Z
libtimewarp set-rate realtime 10               # 0 freezes
libtimewarp freeze                             # remember each rate, then set it to 0
libtimewarp thaw                               # put the remembered rates back
```

`libtimewarp --help` lists them.

Families are `realtime`, `monotonic`, `boottime`, and `cpu`. Durations,
dates, and rates are written exactly as in
[Configuration](CONFIGURATION.md#values), so a date needs its offset:
`2020-01-01T00:00:00Z` works and `2020-01-01T00:00:00` is refused.

`set-time` takes a date, written as an RFC 3339 timestamp or as `@` and Unix
seconds, or a duration. For Realtime a duration is time since the epoch; for
a monotonic family it is time since boot, such as `3d`.

Every token after the command is an argument, so a negative duration such as
`-2h` needs no `--`.

## Several Operations at Once

`shift`, `set-time`, and `set-rate` take further operations after their own,
each written the same way as a command:

```sh
libtimewarp shift realtime -2h set-rate realtime 10 set-rate monotonic 10
```

The operations are applied in order as one
[transaction](CONTROL.md#changing): every program sees all of them at once,
and when one is refused, none is applied.

## Freezing

`freeze` and `thaw` act on Realtime, Monotonic, and Boottime together, or on
the families you name:

```sh
libtimewarp freeze realtime
libtimewarp thaw realtime
```

`freeze` remembers each family's rate before setting it to `0`, and `thaw`
sets the remembered rate back. Freezing a family twice keeps the rate from
the first freeze. Thawing a family with no remembered rate fails and changes
nothing; set its rate with `set-rate` instead. Remember that
[thawing leaves sleeping threads asleep](COVERAGE.md#sleeps-timeouts-and-timers).

The remembered rates are kept in a state file, one per session, at
`<state directory>/frozen/<session file name>`, with one `<family>=<rate>`
line per family:

```text
realtime=10
monotonic=1
```

The state directory is `LIBTIMEWARP_STATE_DIR`, else
`$XDG_STATE_HOME/libtimewarp`, else `~/.local/state/libtimewarp`. `thaw`
removes the lines it uses, and the file once it is empty.

## Output

Every command prints `key=value` lines, one fact per line, so a script can
read them:

```console
$ libtimewarp info
session=/run/user/1000/libtimewarp/pid-26692-41afb16a-74a8-43a9-be42-38bb269d14a2.session
keeper=affine
hook=
creator-pid=26692
created=2026-09-26T03:47:09.673452186Z
created-ns=1790394429673452186
emulate-vdso=1
fail-closed=0
fault=
fault-pid=
realtime.rate=1.000000000
realtime.timebase=CLOCK_REALTIME
monotonic.rate=1.000000000
monotonic.timebase=CLOCK_MONOTONIC
boottime.rate=1.000000000
boottime.timebase=CLOCK_BOOTTIME
cpu.rate=1.000000000
cpu.timebase=
```

A non-empty `fault` means a program of the session hit a
[wrong value](CONFIGURATION.md#when-a-value-is-wrong) and the session runs
on real time; `fault-pid` is that program.

A change prints each family it touched once, in the order the family first
appears on the command line: what the family shows after a `shift` or
`set-time`, then its rate after a `set-rate`:

```console
$ libtimewarp shift realtime -2h set-rate realtime 10 set-rate monotonic 10
realtime.ns=1791769112769441040
realtime.iso=2026-10-12T01:38:32.769441040Z
realtime.rate=10.000000000
monotonic.rate=10.000000000
```

`freeze` prints each family's new rate and the rate it remembers:

```console
$ libtimewarp freeze
realtime.rate=0.000000000
realtime.remembered=10.000000000
monotonic.rate=0.000000000
monotonic.remembered=10.000000000
boottime.rate=0.000000000
boottime.remembered=1.000000000
```

## Exit Status

| Status | Meaning |
| --- | --- |
| 0 | The command succeeded. |
| 1 | The command failed. The reason is on standard error. |
| 2 | The command line is wrong. The usage line follows the reason on standard error. |

A command the session refuses prints the [error code](CONTROL.md#errors)
and the reason:

```console
$ libtimewarp shift monotonic -1d
libtimewarp: would_go_backwards: Monotonic refuses a move to a value it has already shown
```

Any other failure, such as a value that does not parse, prints the reason
alone:

```console
$ libtimewarp set-time realtime 2020-01-01T00:00:00
libtimewarp: "2020-01-01T00:00:00" is not a time: an RFC 3339 timestamp with an offset such as 2020-01-01T00:00:00Z, @ and Unix seconds, or a duration
```

## Picking the Session

| Option | Picks |
| --- | --- |
| none | The only session in the directory. With several, the tool lists them and asks you to choose. |
| `--creator PID` | The session that process created. Use it for the program you started under the library. |
| `--pid PID` | The session a process started by a faked program belongs to. |
| `--newest` | The session created last. |
| `--session PATH` | The session file at that path. |
| `--dir DIR` | Look for sessions in `DIR` in place of the [usual directory](SESSIONS.md#where-they-live). |

The options go before the command:

```sh
libtimewarp --creator 4321 set-rate realtime 0
```

`--pid` works for the programs a faked program starts, and fails for the
program you started yourself: the tool reads the session's location from
the process's environment, and that process received its environment before
the library chose the session. `--creator` covers it.

The tool computes the session directory the same way the library does, so
run it with the same `LIBTIMEWARP_SESSION_DIR`, `XDG_RUNTIME_DIR`, and
`TMPDIR` as the faked programs, or pass `--dir`. It reads
`LIBTIMEWARP_SESSION_DIR` from the
[configuration file](CONFIGURATION.md#the-configuration-file) too, when
the environment leaves it unset.
