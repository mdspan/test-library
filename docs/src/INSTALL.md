# Installation

libtimewarp is built from source with Cargo.

## Requirements

- A 64-bit Linux, macOS, or FreeBSD system, on `x86_64` or `aarch64`.
- [rustup](https://rustup.rs). The repository pins its Rust version, and
  rustup installs that version the first time you build.
- Python 3.11 or later, for the command-line tool.

## Building

From the repository root:

```sh
cargo build --release
```

This builds two libraries into `target/release`:

| File | What it is |
| --- | --- |
| `libtimewarp.so` (`libtimewarp.dylib` on macOS) | The library you load into programs to fake their clock. |
| `libtimewarp_controller.so` (`.dylib` on macOS) | The library the command-line tool and your own programs use to change the clock live. |

The headers for C are in `include/`:

| Header | For |
| --- | --- |
| `libtimewarp_controller.h` | Programs that change a running session; see [The Libraries](API.md). |
| `libtimewarp_hook.h` | Writing a [hook](HOOKS.md). |

Put the libraries anywhere you like. The examples in this guide use the
build directory:

```sh
export TIMEWARP=$PWD/target/release/libtimewarp.so     # libtimewarp.dylib on macOS
```

Use an absolute path. Programs that change directory, and the programs they
start, still need to find the library.

## Leaving Out Go Support

Faking Go programs on Linux and FreeBSD takes code that the rest of the
library does without. To build without it:

```sh
cargo build --release -p agent --no-default-features
```

Everything else works the same. Go programs then see the real time. The
same effect is available for a single run through
[`LIBTIMEWARP_EMULATE_VDSO=0`](CONFIGURATION.md#go-and-programs-that-bypass-libc).

## The Command-Line Tool

The tool is a Python package in `python/`:

```sh
pip install ./python
```

It installs the `libtimewarp` command. The command finds
`libtimewarp_controller` through, in order, the `LIBTIMEWARP_CONTROLLER_LIB`
variable, the package's own directory, and the system's library search path.
While the library is still in the build directory, point the variable at it:

```sh
export LIBTIMEWARP_CONTROLLER_LIB=$PWD/target/release/libtimewarp_controller.so
```

`python3 -m libtimewarp` runs the same tool without installing it, from
inside `python/`.
