# Changing Time Live

A running session can be read and changed from outside, while its programs
run. The piece that does this is the **controller**, and it comes in three
forms:

- [The command-line tool](CLI.md), `libtimewarp`.
- [Libraries](API.md) for Python, C, and Rust.

Every form offers the same operations.

## Reading

- **get** a family: what it shows right now, the same value every program
  in the session is getting.
- **info**: how the session was set up, each family's rate and timebase, and
  whether something went wrong.

## Changing

A change is made of one or more operations on a family:

- **shift**: move the family by a duration.
- **set time**: make the family show a value now.
- **set rate**: change how fast the family runs. `0` freezes it.

Operations are grouped into a **transaction**, and a transaction reaches
every program at once: each program sees all of it or none of it. Freezing
the wall clock and both monotonic families together, for example, is one
transaction with three operations.

The rules:

- **Monotonic and Boottime never go backwards.** A backwards shift or set
  time on them is refused.
- **The wall clock accepts any date** between the years 1678 and 2262.
- **Cpu can't be changed.** Its rate is fixed when the session is created,
  and it has no single value to get.
- **A refused operation cancels the transaction.** Nothing in it is
  applied.
- **A frozen program stays asleep.** Setting a rate back to non-zero leaves
  threads that slept while frozen asleep; see
  [Freezing is one way](COVERAGE.md#sleeps-timeouts-and-timers).
- **Only one change at a time.** Two controllers changing the same session
  take turns. Programs keep reading the clock while a change is being
  prepared.
- **A session without a clock to change refuses changes.** A session on the
  `null` keeper, including one that fell back after a
  [wrong value](CONFIGURATION.md#when-a-value-is-wrong), can be read, and
  every change returns `UnsupportedForKeeper`.

A controller that crashes in the middle of a change leaves the session as
it was.

## Errors

| Code | Meaning |
| --- | --- |
| `NoSession` | There is no session file at that path. |
| `BadSessionFile` | The file is not a session this version can read. |
| `BadArgument` | A value out of range: a negative rate, a date outside the range, an unknown family, or an operation the family doesn't take. |
| `WouldGoBackwards` | A backwards move on Monotonic or Boottime. |
| `UnsupportedForKeeper` | A change to a session on the `null` keeper. |
| `NotOurs` | The file or its directory belongs to another user, or is the wrong kind of file. |
| `Io` | The operating system refused something else. |
