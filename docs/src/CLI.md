# The Command-Line Tool

`libtimewarp` reads and changes a running session. [Installation](INSTALL.md#the-command-line-tool)
sets it up.

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

Families are `realtime`, `monotonic`, `boottime`, and `cpu`. Durations and
dates are written as in [Configuration](CONFIGURATION.md#values). For a
monotonic family, `set-time` takes a duration since boot, such as `3d`.

`freeze` and `thaw` act on Realtime, Monotonic, and Boottime together, or on
the families you name:

```sh
libtimewarp freeze realtime
libtimewarp thaw realtime
```

`freeze` stores the rate to return to under `$XDG_STATE_HOME/libtimewarp`,
or `~/.local/state/libtimewarp`, and `LIBTIMEWARP_STATE_DIR` names another
directory. Freezing a family twice keeps the rate from the first freeze.
Remember that [thawing leaves sleeping threads asleep](COVERAGE.md#sleeps-timeouts-and-timers).

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

A refused command prints the reason to standard error and exits with
status 1:

```console
$ libtimewarp shift monotonic -1d
libtimewarp: would_go_backwards: Monotonic refuses a move to a value it has already shown
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
