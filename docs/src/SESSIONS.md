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

## Where They Live

A session is a file, `pid-<pid>-<id>.session`, where `<pid>` is the process
that created it and `<id>` is random. It sits in the first of these
directories that applies:

1. `$LIBTIMEWARP_SESSION_DIR/libtimewarp`, when `LIBTIMEWARP_SESSION_DIR` is set,
   in the environment or the [configuration file](CONFIGURATION.md#the-configuration-file).
2. `$XDG_RUNTIME_DIR/libtimewarp`, when `XDG_RUNTIME_DIR` is set.
3. `$TMPDIR/libtimewarp-<uid>`, or `/tmp/libtimewarp-<uid>` when `TMPDIR`
   is unset.

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
error and runs on real time, and the controller reports `BadSessionFile`.
Delete the old session file and start the program again.
