# The Libraries

The [controller](CONTROL.md) is available to your own programs in Python, C,
and Rust. All three attach to a session by the path of its session file;
[Sessions](SESSIONS.md#where-they-live) says where those live, and the
Python package can find them for you.

## Python

The `libtimewarp` package from [Installation](INSTALL.md#the-command-line-tool)
is also a library. It needs `libtimewarp_controller`, found the same way the
command-line tool finds it.

```python
import sys
from libtimewarp import Controller, Error, Family, select, session_dir

path = select(session_dir(), newest=True).path

with Controller(path) as session:
    print(session.get(Family.REALTIME))          # nanoseconds since the epoch

    with session.transaction() as change:        # commits when the block ends
        change.shift(Family.REALTIME, -2 * 3600 * 10**9)
        change.set_rate(Family.REALTIME, 10.0)

    info = session.info()
    print(info.families[Family.REALTIME].rate)   # 10.0

    try:
        with session.transaction() as change:
            change.shift(Family.MONOTONIC, -10**9)
    except Error as refused:
        print(refused.code.name, refused.message)
```

- `Controller(path)` attaches. As a context manager it closes on leaving the
  block.
- `get(family)` returns nanoseconds: since the epoch for Realtime, since boot
  for Monotonic and Boottime.
- `transaction()` starts a change. As a context manager it commits when the
  block ends and aborts if the block raises. Outside a `with`, call
  `commit()` or `abort()` yourself.
- `info()` returns the keeper, the hook, the creator pid and creation time,
  `families[family].rate` and `.timebase`, and `fault`.
- A refusal raises `Error`, whose `code` is one of the
  [error codes](CONTROL.md#errors) and whose `message` explains it.

Finding sessions:

| Function | Returns |
| --- | --- |
| `session_dir()` | The directory sessions are created in, computed the way the library computes it. |
| `sessions(directory)` | Every session there, newest first, each with `path`, `creator_pid`, and `creation_time`. |
| `select(directory, creator_pid=None, newest=False)` | The one session the arguments pick, or `LookupError`. |
| `shared_file_of(pid)` | The session file of a process started by a faked program. |

## C

Include `libtimewarp_controller.h` from `include/` and link against
`libtimewarp_controller`:

```sh
cc -I include -o mytool mytool.c -L target/release -ltimewarp_controller
```

```c
#include <stdio.h>
#include <libtimewarp_controller.h>

int main(int argc, char **argv) {
    struct libtimewarp_controller *session;
    struct libtimewarp_transaction *change;
    int64_t now;

    if (argc < 2 || libtimewarp_attach(argv[1], &session) != LIBTIMEWARP_OK) {
        fprintf(stderr, "attach: %s\n", libtimewarp_last_error());
        return 1;
    }
    if (libtimewarp_get(session, LIBTIMEWARP_REALTIME, &now) == LIBTIMEWARP_OK)
        printf("realtime %lld\n", (long long)now);

    if (libtimewarp_begin(session, &change) != LIBTIMEWARP_OK) {
        fprintf(stderr, "begin: %s\n", libtimewarp_last_error());
        libtimewarp_close(session);
        return 1;
    }
    if (libtimewarp_shift(change, LIBTIMEWARP_REALTIME, -7200LL * 1000000000LL) != LIBTIMEWARP_OK ||
        libtimewarp_set_rate(change, LIBTIMEWARP_REALTIME, 0.0) != LIBTIMEWARP_OK) {
        fprintf(stderr, "stage: %s\n", libtimewarp_last_error());
        libtimewarp_abort(change);
    } else if (libtimewarp_commit(change) != LIBTIMEWARP_OK) {
        fprintf(stderr, "commit: %s\n", libtimewarp_last_error());
    }
    libtimewarp_close(session);
    return 0;
}
```

- Every call returns a code from `enum libtimewarp_error`, `LIBTIMEWARP_OK`
  being success. `libtimewarp_last_error()` returns the message for the last
  call on the calling thread.
- `libtimewarp_commit` and `libtimewarp_abort` both end the transaction,
  whatever they return. A refused commit applies nothing.
- `libtimewarp_info` fills a `struct libtimewarp_info`: rates and timebases
  indexed by family, the hook path, the creator, and the fault.
- Use a session handle from one thread at a time. Separate handles are
  independent.

The header documents every call.

## Rust

The controller is the `controller` crate in this repository, with its types
from the `common` crate beside it. Neither is on crates.io, so depend on them
by path or by git:

```toml
[dependencies]
common = { git = "https://github.com/mdspan/libtimewarp" }
controller = { git = "https://github.com/mdspan/libtimewarp" }
```

```rust
use std::path::Path;

use common::time::{Family, Nanos};
use controller::{Controller, Error};

fn main() -> Result<(), Error> {
    let path = std::env::args().nth(1).expect("the path of a session file");
    let session = Controller::attach(Path::new(&path))?;

    let now = session.get(Family::Realtime)?;
    println!("realtime {}", now.0);

    let mut change = session.begin()?;
    change.shift(Family::Realtime, Nanos(-2 * 3600 * 1_000_000_000))?;
    change.set_rate(Family::Realtime, 10.0)?;
    change.commit()?;

    println!("rate {}", session.info().realtime.rate.as_f64());
    Ok(())
}
```

`Controller::attach` opens a session, `get` and `info` read it, and `begin`
returns a `Transaction` whose `commit` or `abort` consumes it. Errors are
`controller::Error`, and `Error::code` gives the stable
[code](CONTROL.md#errors).
