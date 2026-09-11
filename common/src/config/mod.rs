//! Environment variable names and the parsers for their values.

mod timestamp;

use std::ffi::OsString;

pub use timestamp::parse_timestamp;

use crate::time::{Family, Nanos, Rate};

/// Set by the library for its descendants, never by a user. The inherited path wins over
/// everything else in the environment.
pub const ENV_SHARED_FILE: &str = "LIBTIMEWARP__SHARED_FILE";

/// Set by whoever spawns an updater, so the constructor of the process it execs knows it is one
/// and returns at once rather than attaching as a target would.
pub const ENV_UPDATER: &str = "LIBTIMEWARP__UPDATER";

/// The loader's variable for a library to put ahead of libc. Ours is the value a target runs
/// under, and the one place a controller inside a faked tree can read our own path from.
pub const ENV_PRELOAD: &str = "LD_PRELOAD";

/// Overrides the directory session files are created in.
pub const ENV_SESSION_DIR: &str = "LIBTIMEWARP_SESSION_DIR";

/// The freedesktop runtime directory, used when `LIBTIMEWARP_SESSION_DIR` is unset.
pub const ENV_XDG_RUNTIME_DIR: &str = "XDG_RUNTIME_DIR";

/// Which timekeeper the session runs: `affine`, the default, or `null` for genuine time
/// underneath. Read at creation and recorded in the header, like every choice a session makes.
pub const ENV_KEEPER: &str = "LIBTIMEWARP_KEEPER";

/// Names a hook library and the size of its arena: `path/to/lib.so@arena_size`. Read at creation
/// and recorded in the header, so an attacher loads the same library without the variable.
pub const ENV_HOOK: &str = "LIBTIMEWARP_HOOK";

/// How far the Realtime family is moved from the genuine clock, as a duration. A bare shift moves
/// Realtime alone: applying it to the monotonic clocks would drive them negative most of the time.
pub const ENV_SHIFT: &str = "LIBTIMEWARP_SHIFT";

/// An absolute wall-clock time, in place of a shift. RFC 3339, or `@` and Unix seconds.
pub const ENV_TIME: &str = "LIBTIMEWARP_TIME";

/// How fast the Realtime, Monotonic, and Boottime families run. Cpu is left at unity.
pub const ENV_RATE: &str = "LIBTIMEWARP_RATE";

/// How fast the Cpu family runs. The family's one setting, fixed for the life of the session.
pub const ENV_CPU_RATE: &str = "LIBTIMEWARP_CPU_RATE";

/// How far the Realtime family is moved. Wins over [`ENV_SHIFT`].
pub const ENV_REALTIME_SHIFT: &str = "LIBTIMEWARP_REALTIME_SHIFT";

/// How far the Monotonic family is moved. Wins over [`ENV_SHIFT`], which never reaches it.
pub const ENV_MONOTONIC_SHIFT: &str = "LIBTIMEWARP_MONOTONIC_SHIFT";

/// How far the Boottime family is moved. Wins over [`ENV_SHIFT`], which never reaches it.
pub const ENV_BOOTTIME_SHIFT: &str = "LIBTIMEWARP_BOOTTIME_SHIFT";

/// How fast the Realtime family runs. Wins over [`ENV_RATE`].
pub const ENV_REALTIME_RATE: &str = "LIBTIMEWARP_REALTIME_RATE";

/// How fast the Monotonic family runs. Wins over [`ENV_RATE`].
pub const ENV_MONOTONIC_RATE: &str = "LIBTIMEWARP_MONOTONIC_RATE";

/// How fast the Boottime family runs. Wins over [`ENV_RATE`].
pub const ENV_BOOTTIME_RATE: &str = "LIBTIMEWARP_BOOTTIME_RATE";

/// Which genuine clock the Realtime family tracks passage against, by name.
pub const ENV_REALTIME_TIMEBASE: &str = "LIBTIMEWARP_REALTIME_TIMEBASE";

/// Which genuine clock the Monotonic family tracks passage against, by name.
pub const ENV_MONOTONIC_TIMEBASE: &str = "LIBTIMEWARP_MONOTONIC_TIMEBASE";

/// Which genuine clock the Boottime family tracks passage against, by name.
pub const ENV_BOOTTIME_TIMEBASE: &str = "LIBTIMEWARP_BOOTTIME_TIMEBASE";

/// The variables that configure one family. Cpu has a rate alone: it takes no timebase, and an
/// offset there would say "this process has already burned N seconds", which no test wants.
#[derive(Debug, Clone, Copy)]
pub struct FamilyVars {
    /// Moves the family, or `None` when the family takes no shift.
    pub shift: Option<&'static str>,
    /// How fast the family runs.
    pub rate: &'static str,
    /// The genuine clock the family tracks passage against, or `None` when it has no timebase.
    pub timebase: Option<&'static str>,
}

/// The variables that configure `family`.
#[must_use]
pub const fn family_vars(family: Family) -> FamilyVars {
    match family {
        Family::Realtime => FamilyVars {
            shift: Some(ENV_REALTIME_SHIFT),
            rate: ENV_REALTIME_RATE,
            timebase: Some(ENV_REALTIME_TIMEBASE),
        },
        Family::Monotonic => FamilyVars {
            shift: Some(ENV_MONOTONIC_SHIFT),
            rate: ENV_MONOTONIC_RATE,
            timebase: Some(ENV_MONOTONIC_TIMEBASE),
        },
        Family::Boottime => FamilyVars {
            shift: Some(ENV_BOOTTIME_SHIFT),
            rate: ENV_BOOTTIME_RATE,
            timebase: Some(ENV_BOOTTIME_TIMEBASE),
        },
        Family::Cpu => FamilyVars {
            shift: None,
            rate: ENV_CPU_RATE,
            timebase: None,
        },
    }
}

/// Where log lines are appended. Read by every process at every exec, not only by a creator: it
/// says where this process writes, not how the session behaves.
pub const ENV_LOG: &str = "LIBTIMEWARP_LOG";

/// An already-open descriptor to write log lines to. Wins over [`ENV_LOG`].
pub const ENV_LOG_FD: &str = "LIBTIMEWARP_LOG_FD";

/// How much of the log a destination keeps, by level name.
pub const ENV_LOG_LEVEL: &str = "LIBTIMEWARP_LOG_LEVEL";

/// Whether a target that reads the clock through the vDSO is caught. A flag.
pub const ENV_EMULATE_VDSO: &str = "LIBTIMEWARP_EMULATE_VDSO";

/// Whether a target whose setup failed refuses to run rather than showing genuine time. A flag.
pub const ENV_FAIL_CLOSED: &str = "LIBTIMEWARP_FAIL_CLOSED";

/// How often the FreeBSD timekeep page is republished, as a duration.
pub const ENV_TIMEKEEP_INTERVAL: &str = "LIBTIMEWARP_TIMEKEEP_INTERVAL";

/// Basenames to leave on genuine time, comma-separated. Read by every process at every exec,
/// attachers included: the decision is per process, not per session.
pub const ENV_SKIP_CMDS: &str = "LIBTIMEWARP_SKIP_CMDS";

/// Basenames to fake alone, comma-separated; every other command is left genuine. Read the same
/// way as [`ENV_SKIP_CMDS`], and setting both is a bad value.
pub const ENV_ONLY_CMDS: &str = "LIBTIMEWARP_ONLY_CMDS";

/// Looks a variable up. Taken as an argument so the rules that read the environment can be tested
/// without mutating the process's own.
pub trait EnvLookup {
    /// The variable's value, or `None` when it is unset.
    fn get(&self, name: &str) -> Option<OsString>;

    /// An empty value counts as unset everywhere the library reads the environment.
    fn nonempty(&self, name: &str) -> Option<OsString> {
        self.get(name).filter(|value| !value.is_empty())
    }
}

/// The process's own environment.
#[derive(Debug, Clone, Copy)]
pub struct ProcessEnv;

impl EnvLookup for ProcessEnv {
    fn get(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }
}

impl<F> EnvLookup for F
where
    F: Fn(&str) -> Option<OsString>,
{
    fn get(&self, name: &str) -> Option<OsString> {
        self(name)
    }
}

/// A variable's value was not in the shape its parser accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError;

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not a valid value")
    }
}

impl std::error::Error for ParseError {}

/// Why the library could not set up what a variable asked for. Every one of these is a bad value:
/// it is logged, and by default the process runs on genuine time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Not in the shape the variable's parser accepts.
    Parse,
    /// A clock name this platform does not have.
    UnknownClock,
    /// A clock the family may not track passage against.
    ForbiddenTimebase,
    /// A clock the kernel refused to read.
    UnreadableClock,
    /// Another variable already says this, and saying it twice cannot be honoured once.
    SaidTwice(&'static str),
    /// A wall-clock value outside the range an instant can hold.
    OutOfRange,
    /// A move that would put a nondecreasing family below zero.
    BelowZero,
    /// A timekeeper this library does not have.
    UnknownKeeper,
    /// Something that cannot run while the timekeep page is on, which is the hook: the page
    /// carries numbers and a hook answers with a function.
    NeedsPageOff,
    /// A hook library the library could not put in front of the keeper.
    Hook(crate::hook::HookError),
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse => f.write_str("not a valid value"),
            Self::UnknownClock => f.write_str("not a clock this platform has"),
            Self::ForbiddenTimebase => {
                f.write_str("not a clock this family may track passage against")
            }
            Self::UnreadableClock => f.write_str("the kernel refused to read that clock"),
            Self::SaidTwice(other) => write!(f, "says the same thing as {other}"),
            Self::OutOfRange => f.write_str("outside the range an instant can hold"),
            Self::BelowZero => f.write_str("would put a nondecreasing family below zero"),
            Self::UnknownKeeper => f.write_str("not a timekeeper this library has"),
            Self::NeedsPageOff => write!(
                f,
                "cannot be used while the timekeep page is on; set {ENV_EMULATE_VDSO}=0"
            ),
            Self::Hook(why) => write!(f, "{why}"),
        }
    }
}

impl From<ParseError> for Reason {
    fn from(_: ParseError) -> Self {
        Self::Parse
    }
}

/// A variable whose value the library could not use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadValue {
    /// The variable's name.
    pub variable: &'static str,
    /// What went wrong with its value.
    pub reason: Reason,
}

impl BadValue {
    /// A bad value for `variable`.
    #[must_use]
    pub const fn new(variable: &'static str, reason: Reason) -> Self {
        Self { variable, reason }
    }
}

impl std::fmt::Display for BadValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.variable, self.reason)
    }
}

impl std::error::Error for BadValue {}

/// The duration a variable holds, or `None` when it is unset.
///
/// # Errors
/// [`BadValue`] naming `name` when the text is not a duration.
pub fn duration_var(env: &impl EnvLookup, name: &'static str) -> Result<Option<Nanos>, BadValue> {
    parse_var(env, name, |text| parse_duration(text).map(Nanos))
}

/// The rate a variable holds, or `None` when it is unset.
///
/// # Errors
/// [`BadValue`] naming `name` when the text is not a rate the fixed point can hold.
pub fn rate_var(env: &impl EnvLookup, name: &'static str) -> Result<Option<Rate>, BadValue> {
    parse_var(env, name, parse_rate)
}

/// The instant a variable holds, or `None` when it is unset.
///
/// # Errors
/// [`BadValue`] naming `name` when the text is not a timestamp inside the range of an instant.
pub fn timestamp_var(env: &impl EnvLookup, name: &'static str) -> Result<Option<Nanos>, BadValue> {
    parse_var(env, name, parse_timestamp)
}

/// The flag a variable holds, or `default` when it is unset.
pub fn flag_var(env: &impl EnvLookup, name: &str, default: bool) -> bool {
    env.nonempty(name)
        .map_or(default, |value| parse_flag(&value.to_string_lossy()))
}

/// Whether [`ENV_SKIP_CMDS`] or [`ENV_ONLY_CMDS`] excludes a process from faking, given the
/// basename of its own command.
///
/// `command` is `None` when a process could not learn its own name; such a process matches
/// neither list, so it stays off [`ENV_ONLY_CMDS`] and clear of [`ENV_SKIP_CMDS`] alike.
///
/// # Errors
/// [`BadValue`] when both are set: "fake only these" and "fake everything but these" cannot both
/// be honoured.
pub fn excluded(env: &impl EnvLookup, command: Option<&str>) -> Result<bool, BadValue> {
    let skip = env.nonempty(ENV_SKIP_CMDS);
    let only = env.nonempty(ENV_ONLY_CMDS);
    match (skip, only) {
        (Some(_), Some(_)) => Err(BadValue::new(
            ENV_SKIP_CMDS,
            Reason::SaidTwice(ENV_ONLY_CMDS),
        )),
        (Some(list), None) => Ok(in_list(&list.to_string_lossy(), command)),
        (None, Some(list)) => Ok(!in_list(&list.to_string_lossy(), command)),
        (None, None) => Ok(false),
    }
}

/// Whether `command` is one of the comma-separated basenames in `list`.
fn in_list(list: &str, command: Option<&str>) -> bool {
    let Some(command) = command else {
        return false;
    };
    list.split(',')
        .map(str::trim)
        .any(|entry| !entry.is_empty() && entry == command)
}

fn parse_var<T>(
    env: &impl EnvLookup,
    name: &'static str,
    parse: impl Fn(&str) -> Result<T, ParseError>,
) -> Result<Option<T>, BadValue> {
    env.nonempty(name).map_or(Ok(None), |value| {
        parse(&value.to_string_lossy())
            .map(Some)
            .map_err(|error| BadValue::new(name, error.into()))
    })
}

const NS: i128 = 1;
const US: i128 = 1_000;
const MS: i128 = 1_000_000;
const S: i128 = 1_000_000_000;
const M: i128 = 60 * S;
const H: i128 = 60 * M;
const D: i128 = 24 * H;

/// Parses a duration into nanoseconds: one number, an optional fraction after a dot, and one unit
/// out of `ns`, `us`, `ms`, `s`, `m`, `h`, `d`. A bare number means seconds.
///
/// # Errors
/// [`ParseError`] when the text is not that shape, or when the value does not fit an `i64`.
pub fn parse_duration(text: &str) -> Result<i64, ParseError> {
    let text = text.trim();
    let negative = text.starts_with('-');
    let rest = text.strip_prefix(['-', '+']).unwrap_or(text);

    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(rest.len());
    let (number, unit) = rest.split_at(digits_end);

    let multiplier = match unit.trim() {
        "ns" => NS,
        "us" => US,
        "ms" => MS,
        "s" | "" => S,
        "m" => M,
        "h" => H,
        "d" => D,
        _ => return Err(ParseError),
    };

    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty() && fraction.is_empty() {
        return Err(ParseError);
    }
    if fraction.contains('.') {
        return Err(ParseError);
    }

    let whole: i128 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| ParseError)?
    };
    let mut nanos = whole.checked_mul(multiplier).ok_or(ParseError)?;

    if !fraction.is_empty() {
        let scale = u32::try_from(fraction.len())
            .ok()
            .and_then(|digits| 10i128.checked_pow(digits))
            .ok_or(ParseError)?;
        let numerator: i128 = fraction.parse().map_err(|_| ParseError)?;
        nanos = nanos
            .checked_add(numerator.checked_mul(multiplier).ok_or(ParseError)? / scale)
            .ok_or(ParseError)?;
    }

    if negative {
        nanos = -nanos;
    }
    i64::try_from(nanos).map_err(|_| ParseError)
}

/// Parses a rate: how many fake seconds pass in one second of the timebase. `10` is ten times real
/// speed and `0` freezes.
///
/// The decimal goes through a `double`, the same width the controller takes at its own edge, and
/// lands in the library's 32.32 fixed point from there.
///
/// # Errors
/// [`ParseError`] when the text is not a number, or when it is negative, not finite, or at or past
/// the ceiling the fixed point can hold.
pub fn parse_rate(text: &str) -> Result<Rate, ParseError> {
    let value: f64 = text.trim().parse().map_err(|_| ParseError)?;
    Rate::from_f64(value).ok_or(ParseError)
}

/// Parses a flag whose only meaning is on or off. `0` is off, anything else is on.
#[must_use]
pub fn parse_flag(text: &str) -> bool {
    text.trim() != "0"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_convert_to_nanoseconds() {
        assert_eq!(parse_duration("1ns"), Ok(1));
        assert_eq!(parse_duration("1us"), Ok(1_000));
        assert_eq!(parse_duration("500ms"), Ok(500_000_000));
        assert_eq!(parse_duration("90s"), Ok(90_000_000_000));
        assert_eq!(parse_duration("2m"), Ok(120_000_000_000));
        assert_eq!(parse_duration("-2h"), Ok(-7_200_000_000_000));
        assert_eq!(parse_duration("365d"), Ok(365 * 86_400 * 1_000_000_000));
    }

    #[test]
    fn a_bare_number_means_seconds() {
        assert_eq!(parse_duration("-7200"), parse_duration("-2h"));
        assert_eq!(parse_duration("0"), Ok(0));
    }

    #[test]
    fn fractions_are_allowed() {
        assert_eq!(parse_duration("1.5h"), Ok(5_400_000_000_000));
        assert_eq!(parse_duration("0.5s"), Ok(500_000_000));
        assert_eq!(parse_duration("-1.25s"), Ok(-1_250_000_000));
        assert_eq!(parse_duration(".5s"), Ok(500_000_000));
    }

    #[test]
    fn junk_is_refused() {
        for bad in ["", "h", "2x", "1.2.3", "1e9", "two", "1 000s", "--1s"] {
            assert_eq!(
                parse_duration(bad),
                Err(ParseError),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn out_of_range_is_refused() {
        assert_eq!(parse_duration("1000000000d"), Err(ParseError));
    }

    #[test]
    fn rates_parse_as_decimals() {
        assert_eq!(parse_rate("1"), Ok(Rate::UNITY));
        assert_eq!(parse_rate("0"), Ok(Rate::FROZEN));
        assert_eq!(parse_rate("10"), Rate::from_f64(10.0).ok_or(ParseError));
        assert_eq!(parse_rate(" 0.5 "), Rate::from_f64(0.5).ok_or(ParseError));
    }

    #[test]
    fn a_rate_the_fixed_point_cannot_hold_is_refused() {
        for bad in ["-1", "fast", "", "nan", "inf", "4.3e9"] {
            assert_eq!(parse_rate(bad), Err(ParseError), "{bad} should be refused");
        }
    }

    #[test]
    fn every_family_names_the_variables_that_configure_it() {
        assert_eq!(
            family_vars(Family::Realtime).shift,
            Some(ENV_REALTIME_SHIFT)
        );
        assert_eq!(family_vars(Family::Boottime).rate, ENV_BOOTTIME_RATE);
        // Cpu takes no timebase and no shift; the rate is the family's one setting.
        assert_eq!(family_vars(Family::Cpu).rate, ENV_CPU_RATE);
        assert_eq!(family_vars(Family::Cpu).shift, None);
        assert_eq!(family_vars(Family::Cpu).timebase, None);
    }

    #[test]
    fn a_bad_variable_names_itself() {
        let env = |name: &str| (name == ENV_RATE).then(|| "-1".into());
        let bad = rate_var(&env, ENV_RATE).unwrap_err();
        assert_eq!(bad, BadValue::new(ENV_RATE, Reason::Parse));
        assert_eq!(bad.to_string(), "LIBTIMEWARP_RATE: not a valid value");
    }

    #[test]
    fn an_empty_variable_reads_as_unset() {
        let env = |name: &str| (name == ENV_SHIFT).then(|| "".into());
        assert_eq!(duration_var(&env, ENV_SHIFT), Ok(None));
        assert!(flag_var(&env, ENV_SHIFT, true));
    }

    #[test]
    fn only_zero_turns_a_flag_off() {
        assert!(!parse_flag("0"));
        assert!(parse_flag("1"));
        assert!(parse_flag("yes"));
    }

    #[test]
    fn with_neither_list_set_nothing_is_excluded() {
        let env = |_: &str| None;
        assert_eq!(excluded(&env, Some("curl")), Ok(false));
        assert_eq!(excluded(&env, None), Ok(false));
    }

    #[test]
    fn a_command_on_the_skip_list_is_excluded() {
        let env = |name: &str| (name == ENV_SKIP_CMDS).then(|| "curl,git".into());
        assert_eq!(excluded(&env, Some("curl")), Ok(true));
        assert_eq!(excluded(&env, Some("git")), Ok(true));
        assert_eq!(excluded(&env, Some("sh")), Ok(false));
    }

    #[test]
    fn a_command_off_the_only_list_is_excluded() {
        let env = |name: &str| (name == ENV_ONLY_CMDS).then(|| "curl,git".into());
        assert_eq!(excluded(&env, Some("curl")), Ok(false));
        assert_eq!(excluded(&env, Some("sh")), Ok(true));
    }

    #[test]
    fn entries_are_trimmed_basenames() {
        let env = |name: &str| (name == ENV_SKIP_CMDS).then(|| " curl , git ,".into());
        assert_eq!(excluded(&env, Some("curl")), Ok(true));
        assert_eq!(excluded(&env, Some("git")), Ok(true));
        assert_eq!(
            excluded(&env, Some("")),
            Ok(false),
            "an empty entry matches nothing"
        );
    }

    #[test]
    fn a_process_that_cannot_learn_its_own_name_matches_neither_list() {
        let skip = |name: &str| (name == ENV_SKIP_CMDS).then(|| "curl".into());
        let only = |name: &str| (name == ENV_ONLY_CMDS).then(|| "curl".into());
        assert_eq!(excluded(&skip, None), Ok(false));
        assert_eq!(excluded(&only, None), Ok(true));
    }

    #[test]
    fn setting_both_lists_is_a_bad_value() {
        let env = |name: &str| match name {
            n if n == ENV_SKIP_CMDS => Some("curl".into()),
            n if n == ENV_ONLY_CMDS => Some("git".into()),
            _ => None,
        };
        let bad = excluded(&env, Some("curl")).unwrap_err();
        assert_eq!(
            bad,
            BadValue::new(ENV_SKIP_CMDS, Reason::SaidTwice(ENV_ONLY_CMDS))
        );
    }
}
