# Clocks and Families

A system has several clocks, and they answer different questions: what the
date is, how long something took, how much CPU a thread has used.
libtimewarp groups them into four **families**, and every setting applies
to a whole family at once.

| Family | Measures | Variables |
| --- | --- | --- |
| **Realtime** | The date and time of day. | `LIBTIMEWARP_TIME`, `LIBTIMEWARP_SHIFT`, `LIBTIMEWARP_REALTIME_*` |
| **Monotonic** | Time since boot, paused while the machine is suspended. | `LIBTIMEWARP_MONOTONIC_*` |
| **Boottime** | Time since boot, including suspend. | `LIBTIMEWARP_BOOTTIME_*` |
| **Cpu** | CPU time used by the process or a thread. | `LIBTIMEWARP_CPU_RATE` |

`LIBTIMEWARP_RATE` covers the first three.

Every clock in a family shows the family's value. On Linux, for example,
`CLOCK_MONOTONIC_RAW` shows the same time as a faked `CLOCK_MONOTONIC`.
`CLOCK_TAI` is the one clock with an offset of its own: it shows the
Realtime family plus the number of leap seconds TAI is ahead today. On
FreeBSD 15 the system only knows that number once a time daemon such as
`ntpd` has set it; a session started before then shows `CLOCK_TAI` equal
to the Realtime family.

## Which Clock Is in Which Family

| Family | Linux | macOS | FreeBSD 14 and earlier | FreeBSD 15 and later |
| --- | --- | --- | --- | --- |
| **Realtime** | `CLOCK_REALTIME`, `CLOCK_REALTIME_COARSE`, `CLOCK_REALTIME_ALARM`, `CLOCK_TAI` | `CLOCK_REALTIME` | `CLOCK_REALTIME`, `CLOCK_REALTIME_PRECISE`, `CLOCK_REALTIME_FAST`, `CLOCK_SECOND` | `CLOCK_REALTIME`, `CLOCK_REALTIME_PRECISE`, `CLOCK_REALTIME_FAST`, `CLOCK_REALTIME_COARSE`, `CLOCK_SECOND`, `CLOCK_TAI` |
| **Monotonic** | `CLOCK_MONOTONIC`, `CLOCK_MONOTONIC_COARSE`, `CLOCK_MONOTONIC_RAW` | `CLOCK_UPTIME_RAW`, `CLOCK_UPTIME_RAW_APPROX`, `mach_absolute_time`, `mach_approximate_time` | `CLOCK_MONOTONIC`, `CLOCK_MONOTONIC_PRECISE`, `CLOCK_MONOTONIC_FAST` | `CLOCK_UPTIME`, `CLOCK_UPTIME_PRECISE`, `CLOCK_UPTIME_FAST` |
| **Boottime** | `CLOCK_BOOTTIME`, `CLOCK_BOOTTIME_ALARM` | `CLOCK_MONOTONIC`, `CLOCK_MONOTONIC_RAW`, `CLOCK_MONOTONIC_RAW_APPROX`, `mach_continuous_time`, `mach_continuous_approximate_time` | `CLOCK_UPTIME`, `CLOCK_UPTIME_PRECISE`, `CLOCK_UPTIME_FAST`, `CLOCK_BOOTTIME` | `CLOCK_MONOTONIC`, `CLOCK_MONOTONIC_PRECISE`, `CLOCK_MONOTONIC_FAST`, `CLOCK_MONOTONIC_COARSE`, `CLOCK_BOOTTIME` |
| **Cpu** | `CLOCK_PROCESS_CPUTIME_ID`, `CLOCK_THREAD_CPUTIME_ID` | `CLOCK_PROCESS_CPUTIME_ID`, `CLOCK_THREAD_CPUTIME_ID` | `CLOCK_PROCESS_CPUTIME_ID`, `CLOCK_THREAD_CPUTIME_ID`, `CLOCK_VIRTUAL`, `CLOCK_PROF` | `CLOCK_PROCESS_CPUTIME_ID`, `CLOCK_THREAD_CPUTIME_ID`, `CLOCK_VIRTUAL`, `CLOCK_PROF` |

Some names mean different things on different systems:

- On macOS, `CLOCK_MONOTONIC` keeps counting while the machine sleeps, so it
  is in Boottime. `CLOCK_UPTIME_RAW` is the one that pauses.
- FreeBSD 15 changed the meaning of `CLOCK_MONOTONIC` and `CLOCK_BOOTTIME`
  to count suspended time, hence the two columns. On FreeBSD 14 and earlier
  no clock counts suspended time, so Boottime pauses across suspend there.

A few clocks are never faked, so they keep reporting real time:

- On Linux, clocks that belong to hardware devices, such as a PTP network
  card. Those exist to measure real time against the outside world.
- The CPU clocks of other processes and threads, from
  `clock_getcpuclockid` and `pthread_getcpuclockid`. The Cpu family changes
  how a program sees its own usage, and a measurement of somebody else is
  reported as it is.

A `clock_settime` of one of these goes to the system, which answers it as it
would without the library; see [Setting the Time](COVERAGE.md#setting-the-time).

## Timebases

Each of Realtime, Monotonic, and Boottime follows a real clock, called its
**timebase**, and runs at its rate relative to it. By default each family
follows the real clock of the same name:

| Family | Linux | macOS | FreeBSD |
| --- | --- | --- | --- |
| Realtime | `CLOCK_REALTIME` | `CLOCK_REALTIME` | `CLOCK_REALTIME` |
| Monotonic | `CLOCK_MONOTONIC` | `CLOCK_UPTIME_RAW` | the first clock of the family's column above |
| Boottime | `CLOCK_BOOTTIME` | `CLOCK_MONOTONIC` | the first clock of the family's column above |

The family starts at its default timebase's current reading, plus any
shift, whichever timebase you pick.

`LIBTIMEWARP_<FAMILY>_TIMEBASE` picks another. Two reasons to:

- **Keep the wall clock steady.** The real `CLOCK_REALTIME` jumps when NTP
  or an administrator sets the system time, and a faked wall clock that
  follows it jumps too, backwards included. A wall clock that follows
  `CLOCK_BOOTTIME` leaves those jumps out:

  ```sh
  LIBTIMEWARP_REALTIME_TIMEBASE=CLOCK_BOOTTIME LIBTIMEWARP_TIME=2020-01-01T00:00:00Z ...
  ```

- **Leave suspend out.** A wall clock that follows `CLOCK_MONOTONIC` on
  Linux stops while the machine is suspended.

Any clock of the Monotonic or Boottime family can be a timebase for any
family. `CLOCK_REALTIME` can be one for Realtime only, because its jumps
would break the monotonic families. Cpu has no timebase: each CPU clock is
scaled on its own.

## Choosing Settings

- **Pretend it is another date.** `LIBTIMEWARP_TIME` or `LIBTIMEWARP_SHIFT`.
  Timeouts, uptime, and benchmarks are untouched.
- **Speed a program up.** `LIBTIMEWARP_RATE=10` runs every elapsed-time
  clock ten times faster, and sleeps and timeouts finish ten times sooner.
- **Freeze the date and keep timeouts working.** `LIBTIMEWARP_REALTIME_RATE=0`
  freezes the wall clock alone. Programs that measure elapsed time with a
  monotonic clock keep running normally.
- **Freeze everything.** `LIBTIMEWARP_RATE=0`. Programs that sleep under
  it never wake; see
  [What Gets Faked](COVERAGE.md#sleeps-timeouts-and-timers).
