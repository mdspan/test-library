//! Environment variable names and the parsers for their values.

use std::ffi::OsString;

/// Set by the library for its descendants, never by a user. The inherited path wins over
/// everything else in the environment.
pub const ENV_SHARED_FILE: &str = "LIBTIMEWARP__SHARED_FILE";

/// Overrides the directory session files are created in.
pub const ENV_SESSION_DIR: &str = "LIBTIMEWARP_SESSION_DIR";

/// The freedesktop runtime directory, used when `LIBTIMEWARP_SESSION_DIR` is unset.
pub const ENV_XDG_RUNTIME_DIR: &str = "XDG_RUNTIME_DIR";

/// How far the Realtime family is moved from the genuine clock, as a duration.
pub const ENV_SHIFT: &str = "LIBTIMEWARP_SHIFT";

/// Whether a target that reads the clock through the vDSO is caught. A flag.
pub const ENV_EMULATE_VDSO: &str = "LIBTIMEWARP_EMULATE_VDSO";

/// Whether a target whose setup failed refuses to run rather than showing genuine time. A flag.
pub const ENV_FAIL_CLOSED: &str = "LIBTIMEWARP_FAIL_CLOSED";

/// How often the FreeBSD timekeep page is republished, as a duration.
pub const ENV_TIMEKEEP_INTERVAL: &str = "LIBTIMEWARP_TIMEKEEP_INTERVAL";

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
    fn only_zero_turns_a_flag_off() {
        assert!(!parse_flag("0"));
        assert!(parse_flag("1"));
        assert!(parse_flag("yes"));
    }
}
