# Sessions

A **session** is one faked clock and the programs that share it. The first
program you start under the library creates a session from its
environment, and every program it starts, and every program those start,
joins it. They all see the same time, and a change to the session reaches
all of them.

## One Clock per Process Tree

Children join their parent's session whatever their own environment says.
Setting `LIBTIMEWARP_SHIFT` differently for a command run from a faked
shell has no effect: that command joins the shell's session.

To start an independent session from inside a faked shell, unset the
variable that names the session:

```sh
env -u LIBTIMEWARP__SHARED_FILE LIBTIMEWARP_SHIFT=-1d LD_PRELOAD=$TIMEWARP ./other-program
```

## How Programs Find the Session

The library names the session in `LIBTIMEWARP__SHARED_FILE`, which every
program passes on to the programs it starts. It sets the variable in two
places: in the environment the C library keeps, which most programs pass
on, and in the environment the program was started with. The second is
the one a Go program passes to every program it starts, and the one a C
program passes when it hands `main`'s third argument to `exec` on musl,
FreeBSD, and macOS.

The environment a program was started with has no room for a new
variable, so the session's name takes the place of one already there:

- `LIBTIMEWARP__SHARED_FILE` itself, when the program was started with it,
  even empty;
- otherwise, the first variable that only a new session reads: one of
  those under [Setting the Clock](CONFIGURATION.md#setting-the-clock),
  [Go and Programs That Bypass libc](CONFIGURATION.md#go-and-programs-that-bypass-libc),
  and [Hooks and Timekeepers](CONFIGURATION.md#hooks-and-timekeepers).

The programs below join the session and never read those variables, so
they lose nothing. The program itself still sees every variable through
the C library, `getenv` included. What changes is the environment it
started with: a Go program started with
`LIBTIMEWARP_TIME=2020-01-01T00:00:00Z LIBTIMEWARP_RATE=0` finds one of
the two missing from `os.Getenv` and `os.Environ`, finds
`LIBTIMEWARP__SHARED_FILE` there instead, and passes that on. On musl,
FreeBSD, and macOS, `main`'s third argument shows the same. A program below
that unsets `LIBTIMEWARP__SHARED_FILE` to start a session of its own, as
above, starts it without the variable that gave way.

To keep every variable, start the program with `LIBTIMEWARP__SHARED_FILE`
set to empty. An empty value starts a new session, the same as no value,
and gives the session's name a place of its own:

```sh
LIBTIMEWARP__SHARED_FILE= LIBTIMEWARP_TIME=2020-01-01T00:00:00Z LD_PRELOAD=$TIMEWARP ./go-program
```

A program started with neither, for instance one that takes every setting
from the [configuration file](CONFIGURATION.md#the-configuration-file),
logs a `warn` line, and the programs a Go program like it starts begin
sessions of their own.

## Where They Live

A session is a file, `pid-<pid>-<id>.session`, where `<pid>` is the process
that created it and `<id>` is random. It sits in the first of these
directories that applies:

1. `$LIBTIMEWARP_SESSION_DIR/libtimewarp`, when `LIBTIMEWARP_SESSION_DIR` is set,
   in the environment or the [configuration file](CONFIGURATION.md#the-configuration-file).
2. `$XDG_RUNTIME_DIR/libtimewarp`, when `XDG_RUNTIME_DIR` is set.
3. `$TMPDIR/libtimewarp-<uid>`, or `/tmp/libtimewarp-<uid>` when `TMPDIR`
   is unset.

A relative directory is taken against the working directory of the program
that creates the session. The library passes the session on to child programs
by its absolute path, so a program that changes directory and then starts
another one keeps both on the same session. Symbolic links in the path are
kept as they are.

Beside each session file is a small lock file with the same name, ending in
`.lock`.

The directory is created private to you, mode `0700`, and the library
refuses a directory or a file owned by anyone else. A session is shared by
the programs of one user.

Programs in different containers can share a session as long as they run as
the same user and see the same directory. Set `LIBTIMEWARP_SESSION_DIR` to
a directory mounted in both.

Session names are generated, so you cannot pick one. The
[command-line tool](CLI.md#picking-the-session) finds sessions by the
process that created them, by any process in them, or by age.

## Cleaning Up

The library never deletes session files. Each run of a faked program leaves
one behind, so remove old ones when you like:

```sh
rm -r "${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}"/libtimewarp*
```

Remove only sessions whose programs have exited. A running program keeps
using its session, and the controller can no longer find a deleted one.

## After an Upgrade

A session file written by a different version of libtimewarp may have a
different layout. A program that tries to join such a session logs an
error and runs on real time, and the controller reports `bad_session_file`.
Delete the old session file and start the program again.
