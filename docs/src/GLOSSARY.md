# Glossary

### arena

Memory shared by every program of a session with a [hook](#hook), sized by
the number after `@` in `LIBTIMEWARP_HOOK`. See [Hooks](HOOKS.md#shared-memory).

### controller

What reads or changes a running [session](#session): the command-line
tool, `libtimewarp`. See [Changing Time Live](CONTROL.md).

### family

A group of clocks that share every setting: Realtime, Monotonic, Boottime,
and Cpu. See [Clocks and Families](CLOCKS.md).

### fault

A problem recorded in a session, shown by `libtimewarp info`. See
[Faults](TROUBLESHOOTING.md#faults).

### hook

A shared library of your own that answers clock reads in place of the
settings. See [Hooks](HOOKS.md).

### keeper

What computes the faked time when no [hook](#hook) answers. `affine`, the
default, applies the settings in [Configuration](CONFIGURATION.md). `null`
returns the real time.

### rate

How many faked seconds pass per real second. `1` is normal speed, `0` is
frozen.

### session

One faked clock and every program sharing it, stored in a session file. See
[Sessions](SESSIONS.md).

### timebase

The real clock a [family](#family) follows. See
[Timebases](CLOCKS.md#timebases).

### transaction

A group of changes a controller applies together, which every program sees
at once. See [Changing Time Live](CONTROL.md#changing).
