# Changing Time Live

A running session can be read and changed from outside, while its programs
run. The piece that does this is the **controller**:
[the command-line tool](CLI.md), `libtimewarp`.

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
transaction with three operations. Several operations go in one transaction
through a [sequence](CLI.md#several-operations-at-once) on one command line.

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
  every change is refused with `unsupported_for_keeper`.

A controller that crashes in the middle of a change leaves the session as
it was.

## Errors

A refused change or read reports one of these codes, and
[the tool](CLI.md#exit-status) prints it:

| Code | Meaning |
| --- | --- |
| `no_session` | There is no session file at that path. |
| `bad_session_file` | The file is not a session this version can read. |
| `bad_argument` | A value out of range: a negative rate, a date outside the range, or an operation the family doesn't take. |
| `would_go_backwards` | A backwards move on Monotonic or Boottime. |
| `unsupported_for_keeper` | A change to a session on the `null` keeper. |
| `not_ours` | The file or its directory belongs to another user, or is the wrong kind of file. |
| `io` | The operating system refused something else. |
