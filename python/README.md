# libtimewarp, the Python tool

A command-line tool and a small library over `libtimewarp_controller`, the C library that reads
and changes a live libtimewarp session.

## Installing

```sh
cargo build -p capi --release           # builds libtimewarp_controller
pip install ./python
```

The tool finds the shared object through `LIBTIMEWARP_CONTROLLER_LIB`, then beside the package,
then on the loader's own search path. Point the variable at the build when it is not installed:

```sh
export LIBTIMEWARP_CONTROLLER_LIB=target/release/libtimewarp_controller.so
```

## Picking the session

Session names are generated, so there is no name to type. Three ways in:

```sh
libtimewarp --pid 4321 info      # the session that process of the tree is in
libtimewarp --session PATH info  # the path outright
libtimewarp list                 # every session in the directory
```

With one session in the directory, a command with none of those acts on it. With several, pick by
creator pid with `--creator`, by creation time with `--newest`, or name the path.

## Commands

```sh
libtimewarp info                      # keeper, hook, creator, each family's rate and timebase
libtimewarp get realtime              # what the family shows now
libtimewarp shift realtime -2h
libtimewarp set-time realtime 2020-01-01T00:00:00Z
libtimewarp set-rate realtime 10
libtimewarp freeze                    # remember each rate, then set it to zero
libtimewarp thaw                      # put the remembered rates back
```

`freeze` and `thaw` are the tool's, not the library's: the library offers primitives, and the rate
to come back to is remembered here, under `$XDG_STATE_HOME/libtimewarp`.

Output is `key=value` lines, one fact each.
