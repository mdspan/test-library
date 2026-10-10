//! Setting the time from inside a target.
//!
//! A program that sets the clock becomes a writer for that one call: it runs a one-operation
//! transaction on the Realtime family, the way a controller would, with every maskable signal
//! blocked across the write section. The call succeeds only where the kernel would have let this
//! process set the clock genuinely, so a program that probes for privilege sees exactly what it
//! would have seen without us.
//!
//! Setters are the one group that never falls through to the genuine function. Once there is a
//! session, the hook answers the call or declines it, and a declined setter goes to the keeper:
//! the affine keeper runs the set, and the null keeper answers as an unprivileged process would
//! see. A process with no session has nothing of ours to move, so there the genuine function is
//! the whole of the answer.
//!
//! A refusal is the one the kernel would have made. A call can fail several of the kernel's checks
//! at once, a malformed value from a process without privilege for one, and each platform makes
//! them in its own order, so the errno is that of the first failure in [`platform::SET_ORDER`].

use common::hook::{Func, Status};
use common::platform::{self, SetCheck, clock};
use common::session::clock_now;
use common::session::layout::{Header, Keeper};
use common::time::{Family, Nanos};
use controller::Transaction;

use crate::state;

/// Microseconds in a second, the unit `settimeofday` and `adjtime` count their fractions in.
const MICROS_PER_SECOND: i64 = 1_000_000;

/// The first whole second a `timespec` may not name: past it the nanoseconds it stands for leave
/// the range an instant holds. The kernel refuses the same bound.
const SECONDS_CEILING: i64 = i64::MAX / Nanos::SECOND;

/// Why a setter was refused, under the `errno` the kernel would have refused it with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refused {
    /// A pointer the call could not read.
    Fault,
    /// A clock that cannot be set, or a field outside what it may hold.
    Invalid,
    /// A process the kernel would not have let set the clock.
    Denied,
}

impl Refused {
    /// The `errno` the program sees.
    const fn errno(self) -> libc::c_int {
        match self {
            Self::Fault => libc::EFAULT,
            Self::Invalid => libc::EINVAL,
            Self::Denied => libc::EPERM,
        }
    }
}

impl From<controller::Error> for Refused {
    /// The `errno` a refused transaction reports to the program.
    ///
    /// A value the family will not take is the kernel's `EINVAL`. Anything else is a refusal this
    /// process could do nothing about, which is what an unprivileged one sees.
    fn from(error: controller::Error) -> Self {
        match error {
            controller::Error::BadArgument(_) | controller::Error::WouldGoBackwards(_) => {
                Self::Invalid
            }
            _ => Self::Denied,
        }
    }
}

impl From<SetCheck> for Refused {
    /// The `errno` a failed check of the kernel's reports.
    fn from(check: SetCheck) -> Self {
        match check {
            SetCheck::Clock | SetCheck::Value => Self::Invalid,
            SetCheck::Pointer => Self::Fault,
            SetCheck::Privilege => Self::Denied,
        }
    }
}

/// What a set carries for the kernel to check, beside the clock it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Carried {
    /// A value the call may carry.
    Good,
    /// No value at all: a null pointer.
    Nothing,
    /// A value outside what the call may carry.
    Malformed,
}

/// The refusal a kernel that checks in `order` makes of a call failing the checks `fails` names:
/// the first of them, or `None` when the call fails none.
fn first_refusal(order: &[SetCheck], fails: impl Fn(SetCheck) -> bool) -> Option<Refused> {
    order
        .iter()
        .copied()
        .find(|&check| fails(check))
        .map(Refused::from)
}

/// The refusal this platform's kernel makes of a set of `clock` carrying `carried`, by this
/// process under this session, or `None` when it would let the set through.
fn refusal(header: &Header, clock: libc::clockid_t, carried: Carried) -> Option<Refused> {
    first_refusal(&platform::SET_ORDER, |check| match check {
        SetCheck::Clock => !platform::settable(clock),
        SetCheck::Pointer => carried == Carried::Nothing,
        SetCheck::Privilege => permitted(header).is_err(),
        SetCheck::Value => carried == Carried::Malformed,
    })
}

/// libc's convention for a setter: 0, or -1 with `errno` set to what refused it.
fn report(outcome: Result<(), Refused>) -> libc::c_int {
    match outcome {
        Ok(()) => 0,
        Err(why) => {
            platform::set_errno(why.errno());
            -1
        }
    }
}

/// What one setter asks of the family.
#[derive(Debug, Clone, Copy)]
enum Motion {
    /// Make the family show this value now, which is what `settimeofday` and `clock_settime` ask.
    To(Nanos),
    /// Move the family by this duration, applied at once rather than slewed, which is what
    /// `adjtime` asks.
    By(Nanos),
}

/// # Safety
/// `tv` must be null or valid for reads of a `timeval`, and `tz` null or valid for reads of a
/// `timezone`.
pub(crate) unsafe fn settimeofday(
    tv: *const libc::timeval,
    tz: *const libc::c_void,
) -> libc::c_int {
    let Some(header) = state::session() else {
        // Safety: both pointers are the caller's, passed on unchanged.
        return unsafe { platform::genuine_settimeofday(tv, tz) };
    };
    if tv.is_null() {
        // Nothing to move. The kernel makes this the privilege check and no more, whether the call
        // is a bare probe for privilege or a set of the obsolete timezone, which is a field we
        // already answer as zero wherever the program can read it.
        return report(permitted(header));
    }
    // Safety: `tv` is non-null, so the caller's guarantee makes it valid for this read.
    let Some(at) = instant_of(unsafe { &*tv }) else {
        let why = refusal(header, clock::REALTIME, Carried::Malformed);
        return report(Err(why.unwrap_or(Refused::Invalid)));
    };
    report(set(
        header,
        Func::Settimeofday,
        clock::REALTIME,
        Motion::To(at),
    ))
}

/// # Safety
/// `tp` must be null or valid for reads of a `timespec`.
pub(crate) unsafe fn clock_settime(
    clock: libc::clockid_t,
    tp: *const libc::timespec,
) -> libc::c_int {
    let Some(header) = state::session() else {
        // Safety: `tp` is the caller's, passed on unchanged.
        return unsafe { platform::genuine_clock_settime(clock, tp) };
    };
    if tp.is_null() {
        let why = refusal(header, clock, Carried::Nothing);
        return report(Err(why.unwrap_or(Refused::Fault)));
    }
    // Safety: `tp` is non-null, so the caller's guarantee makes it valid for this read.
    let ts = unsafe { &*tp };
    let Some(at) = whole(ts.tv_sec, ts.tv_nsec) else {
        let why = refusal(header, clock, Carried::Malformed);
        return report(Err(why.unwrap_or(Refused::Invalid)));
    };
    report(set(header, Func::ClockSettime, clock, Motion::To(at)))
}

/// # Safety
/// `delta` must be null or valid for reads of a `timeval`, and `olddelta` null or valid for writes
/// of one.
pub(crate) unsafe fn adjtime(
    delta: *const libc::timeval,
    olddelta: *mut libc::timeval,
) -> libc::c_int {
    let Some(header) = state::session() else {
        // Safety: both pointers are the caller's, passed on unchanged.
        return unsafe { platform::genuine_adjtime(delta, olddelta) };
    };
    let outcome = if delta.is_null() {
        if platform::ADJTIME_NULL_ASKS {
            // A question rather than a set, which needs no privilege: an adjustment here lands at
            // once rather than slewed, so there is never one still under way to report.
            Ok(())
        } else {
            let why = refusal(header, clock::REALTIME, Carried::Nothing);
            Err(why.unwrap_or(Refused::Fault))
        }
    } else {
        // Safety: `delta` is non-null, so the caller's guarantee makes it valid for this read.
        duration_of(unsafe { &*delta }).map_or_else(
            || {
                let why = refusal(header, clock::REALTIME, Carried::Malformed);
                Err(why.unwrap_or(Refused::Invalid))
            },
            |by| set(header, Func::Adjtime, clock::REALTIME, Motion::By(by)),
        )
    };
    if outcome.is_ok() && !olddelta.is_null() {
        // Safety: `olddelta` is non-null, so the caller's guarantee makes it valid for this write.
        unsafe {
            *olddelta = libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            };
        }
    }
    report(outcome)
}

/// One setter, from the arguments the program gave to the answer it sees.
///
/// The hook goes first and answers or declines. A declined setter goes to the keeper and never to
/// the genuine function: the affine keeper runs the transaction where the kernel would have let
/// this process set the clock, and refuses it the way the kernel would otherwise.
fn set(header: &Header, func: Func, clock: libc::clockid_t, motion: Motion) -> Result<(), Refused> {
    if let Some(hook) = state::hook() {
        // The hook wants the instant the program asked for, which a move by a duration names only
        // together with what the family shows now.
        let asked = match motion {
            Motion::To(at) => at,
            Motion::By(by) => showing(header, clock).saturating_add(by),
        };
        if hook.set(func, clock, asked) == Status::Handled {
            // The hook took the set into its own state, so the program sees success and nothing of
            // the keeper's moved.
            return Ok(());
        }
    }
    if let Some(why) = refusal(header, clock, Carried::Good) {
        return Err(why);
    }
    transact(header, motion)
}

/// Whether this process may set the clock at all, which is the whole answer for a setter with
/// nothing to move.
///
/// A session that fakes nothing may not move the machine's own clock either, so it answers as an
/// unprivileged process would see whatever this process actually holds.
fn permitted(header: &Header) -> Result<(), Refused> {
    if Keeper::from_tag(header.keeper) != Some(Keeper::Affine) {
        return Err(Refused::Denied);
    }
    if platform::may_set_the_clock() {
        Ok(())
    } else {
        Err(Refused::Denied)
    }
}

/// **W1** through **W12**: one transaction of one operation on the Realtime family, the same write
/// section a controller's commit runs.
#[expect(
    clippy::significant_drop_tightening,
    reason = "a transaction is consumed by `commit`, which is where its lock is released; the lint \
              does not see through a method taking `self` and suggests a drop that would not \
              compile"
)]
fn transact(header: &Header, motion: Motion) -> Result<(), Refused> {
    // Installing a session names its lock file in the same breath, so a session word without one
    // is a state nothing can leave behind.
    let lock_file = state::lock_file().ok_or(Refused::Denied)?;
    let mut transaction = Transaction::begin(header, lock_file)?;
    match motion {
        Motion::To(at) => transaction.set_time(Family::Realtime, at),
        Motion::By(by) => transaction.shift(Family::Realtime, by),
    }?;
    transaction.commit()?;
    Ok(())
}

/// The microseconds a `timeval`'s field holds, in whatever width the platform gives it:
/// `suseconds_t` is 64 bits on Linux and FreeBSD and 32 on macOS.
fn micros_of<T: Into<i64>>(micros: T) -> i64 {
    micros.into()
}

/// What the program would read from `clock` now: the session's value where it fakes it, and the
/// genuine reading where it does not.
fn showing(header: &Header, clock: libc::clockid_t) -> Nanos {
    clock_now(header, clock)
        .or_else(|| platform::genuine_nanos(clock))
        .unwrap_or(Nanos::ZERO)
}

/// The instant a `timeval` names, or `None` when the kernel would refuse the fields.
fn instant_of(tv: &libc::timeval) -> Option<Nanos> {
    let micros = micros_of(tv.tv_usec);
    if !(0..MICROS_PER_SECOND).contains(&micros) {
        return None;
    }
    whole(tv.tv_sec, micros.saturating_mul(1_000))
}

/// The duration a `timeval` names as a signed delta, or `None` when the platform's own `adjtime`
/// would refuse it.
///
/// A fraction wider than a second carries into the seconds rather than failing, the way libc
/// normalises the pair before it measures it, and the whole delta is bounded by what the platform
/// takes.
fn duration_of(delta: &libc::timeval) -> Option<Nanos> {
    let micros = micros_of(delta.tv_usec);
    let seconds = delta.tv_sec.saturating_add(micros / MICROS_PER_SECOND);
    if !(-platform::ADJTIME_SECONDS..=platform::ADJTIME_SECONDS).contains(&seconds) {
        return None;
    }
    Some(Nanos(seconds.saturating_mul(Nanos::SECOND).saturating_add(
        (micros % MICROS_PER_SECOND).saturating_mul(1_000),
    )))
}

/// The instant `seconds` and `nanos` name together, or `None` when the kernel would refuse the
/// pair: a date before 1970, a fraction that is not inside a second, or a value past the range an
/// instant holds.
fn whole(seconds: libc::time_t, nanos: i64) -> Option<Nanos> {
    if !(0..Nanos::SECOND).contains(&nanos) || !(0..SECONDS_CEILING).contains(&seconds) {
        return None;
    }
    Some(Nanos(
        seconds.saturating_mul(Nanos::SECOND).saturating_add(nanos),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeval(seconds: i64, micros: i64) -> libc::timeval {
        libc::timeval {
            tv_sec: seconds,
            // `suseconds_t` is 32 bits on macOS, and every value tested here fits it.
            tv_usec: libc::suseconds_t::try_from(micros).unwrap_or_default(),
        }
    }

    #[test]
    fn a_timeval_names_the_instant_its_two_fields_add_up_to() {
        assert_eq!(
            instant_of(&timeval(1_700_000_000, 123_456)),
            Some(Nanos(1_700_000_000_123_456_000))
        );
        assert_eq!(instant_of(&timeval(0, 0)), Some(Nanos::ZERO));
    }

    #[test]
    fn the_fields_the_kernel_refuses_name_no_instant() {
        // A fraction that is not inside a second, in either direction.
        assert_eq!(instant_of(&timeval(1, 1_000_000)), None);
        assert_eq!(instant_of(&timeval(1, -1)), None);
        // A date before 1970, and one past the range an instant holds.
        assert_eq!(instant_of(&timeval(-1, 0)), None);
        assert_eq!(instant_of(&timeval(SECONDS_CEILING, 0)), None);
        // And the widest pair it does name, which is the last instant short of the ceiling.
        assert_eq!(
            whole(SECONDS_CEILING - 1, Nanos::SECOND - 1),
            Some(Nanos(
                (SECONDS_CEILING - 1) * Nanos::SECOND + Nanos::SECOND - 1
            ))
        );
    }

    #[test]
    fn a_delta_may_be_negative_where_an_instant_may_not() {
        assert_eq!(
            duration_of(&timeval(-2, -500_000)),
            Some(Nanos(-2_500_000_000))
        );
        assert_eq!(
            duration_of(&timeval(600, 0)),
            Some(Nanos(600 * Nanos::SECOND))
        );
        // A fraction wider than a second carries rather than failing, which is what libc makes
        // of it, and the whole delta is bounded by what the platform's own `adjtime` takes.
        assert_eq!(
            duration_of(&timeval(1, 2_500_000)),
            Some(Nanos(3_500_000_000))
        );
        assert_eq!(
            duration_of(&timeval(platform::ADJTIME_SECONDS, 0)),
            Some(Nanos(platform::ADJTIME_SECONDS * Nanos::SECOND))
        );
        assert_eq!(
            duration_of(&timeval(platform::ADJTIME_SECONDS + 1, 0)),
            None
        );
        assert_eq!(
            duration_of(&timeval(-platform::ADJTIME_SECONDS - 1, 0)),
            None
        );
    }

    #[test]
    fn each_refusal_carries_the_errno_the_kernel_would_have_used() {
        assert_eq!(Refused::Fault.errno(), libc::EFAULT);
        assert_eq!(Refused::Invalid.errno(), libc::EINVAL);
        assert_eq!(Refused::Denied.errno(), libc::EPERM);
        assert_eq!(report(Ok(())), 0);
        assert_eq!(report(Err(Refused::Denied)), -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
    }

    #[test]
    fn a_call_failing_several_checks_is_refused_for_the_first_the_kernel_makes() {
        use SetCheck::{Clock, Pointer, Privilege, Value};

        // The orders Linux, macOS and FreeBSD check in.
        let linux = [Clock, Pointer, Value, Privilege];
        let macos = [Clock, Privilege, Pointer, Value];
        let freebsd = [Pointer, Privilege, Clock, Value];

        // A malformed value from a process without privilege: the value is looked at first on
        // Linux, and privilege on the other two.
        let malformed = |check| matches!(check, Privilege | Value);
        assert_eq!(first_refusal(&linux, malformed), Some(Refused::Invalid));
        assert_eq!(first_refusal(&macos, malformed), Some(Refused::Denied));
        assert_eq!(first_refusal(&freebsd, malformed), Some(Refused::Denied));
        // A clock that cannot be set, named by a process without privilege.
        let unsettable = |check| matches!(check, Clock | Privilege);
        assert_eq!(first_refusal(&linux, unsettable), Some(Refused::Invalid));
        assert_eq!(first_refusal(&macos, unsettable), Some(Refused::Invalid));
        assert_eq!(first_refusal(&freebsd, unsettable), Some(Refused::Denied));
        // A null pointer beside a clock that cannot be set.
        let unread = |check| matches!(check, Clock | Pointer);
        assert_eq!(first_refusal(&linux, unread), Some(Refused::Invalid));
        assert_eq!(first_refusal(&macos, unread), Some(Refused::Invalid));
        assert_eq!(first_refusal(&freebsd, unread), Some(Refused::Fault));
        // And a call that fails nothing is not refused at all.
        assert_eq!(first_refusal(&platform::SET_ORDER, |_| false), None);
    }

    #[test]
    fn this_platforms_order_makes_every_check_once() {
        for check in [
            SetCheck::Clock,
            SetCheck::Pointer,
            SetCheck::Privilege,
            SetCheck::Value,
        ] {
            assert_eq!(
                platform::SET_ORDER
                    .iter()
                    .filter(|made| **made == check)
                    .count(),
                1,
                "{check:?}"
            );
        }
    }

    #[test]
    fn a_refused_transaction_reports_a_bad_value_apart_from_a_refusal_we_could_not_help() {
        assert_eq!(
            Refused::from(controller::Error::BadArgument("")),
            Refused::Invalid
        );
        assert_eq!(
            Refused::from(controller::Error::WouldGoBackwards(Family::Monotonic)),
            Refused::Invalid
        );
        assert_eq!(
            Refused::from(controller::Error::UnsupportedForKeeper),
            Refused::Denied
        );
        assert_eq!(Refused::from(controller::Error::NoSession), Refused::Denied);
    }
}
