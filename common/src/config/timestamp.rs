//! The absolute wall-clock time `LIBTIMEWARP_TIME` names: RFC 3339, or `@` and Unix seconds.

use super::{ParseError, parse_duration};
use crate::time::Nanos;

/// Parses `LIBTIMEWARP_TIME`: an RFC 3339 timestamp such as `2020-01-01T00:00:00Z`, or `@`
/// followed by Unix seconds, `@1577836800`.
///
/// # Errors
/// [`ParseError`] when the text is neither shape, when a field is out of range, or when the
/// instant does not fit the nanoseconds an instant is counted in.
pub fn parse_timestamp(text: &str) -> Result<Nanos, ParseError> {
    let text = text.trim();
    // A bare number means seconds to the duration grammar, which is what the `@` form is.
    text.strip_prefix('@').map_or_else(
        || parse_rfc3339(text),
        |seconds| parse_duration(seconds).map(Nanos),
    )
}

/// One `date-time` of RFC 3339: a date, a separator, a time with an optional fraction, and an
/// offset. The offset is not optional; a timestamp without one names no instant.
fn parse_rfc3339(text: &str) -> Result<Nanos, ParseError> {
    let (date, rest) = text.split_at_checked(10).ok_or(ParseError)?;
    let (year, month, day) = parse_date(date)?;

    let separator = rest.chars().next().ok_or(ParseError)?;
    if !matches!(separator, 'T' | 't' | ' ') {
        return Err(ParseError);
    }
    let rest = rest.get(1..).ok_or(ParseError)?;

    let split = rest
        .find(['Z', 'z', '+'])
        .or_else(|| rest.rfind('-'))
        .ok_or(ParseError)?;
    let (time, zone) = rest.split_at_checked(split).ok_or(ParseError)?;
    let (hour, minute, second, nanos) = parse_time(time)?;
    let offset = parse_offset(zone)?;

    let days = days_from_civil(year, month, day)?;
    let seconds = days
        .checked_mul(86_400)
        .and_then(|s| s.checked_add(i64::from(hour) * 3_600))
        .and_then(|s| s.checked_add(i64::from(minute) * 60))
        .and_then(|s| s.checked_add(i64::from(second)))
        .and_then(|s| s.checked_sub(offset))
        .ok_or(ParseError)?;

    seconds
        .checked_mul(Nanos::SECOND)
        .and_then(|ns| ns.checked_add(i64::from(nanos)))
        .map(Nanos)
        .ok_or(ParseError)
}

/// `YYYY-MM-DD`, with the day checked against the month's own length.
fn parse_date(text: &str) -> Result<(i64, u32, u32), ParseError> {
    let mut fields = text.split('-');
    let year: i64 = field(fields.next(), 4)?.into();
    let month = field(fields.next(), 2)?;
    let day = field(fields.next(), 2)?;
    if fields.next().is_some()
        || !(1..=12).contains(&month)
        || day < 1
        || day > days_in(year, month)
    {
        return Err(ParseError);
    }
    Ok((year, month, day))
}

/// `HH:MM:SS` with an optional fraction. A second of 60 is the leap second RFC 3339 allows, and
/// lands on the same instant as the next minute's zero.
fn parse_time(text: &str) -> Result<(u32, u32, u32, u32), ParseError> {
    let (clock, fraction) = match text.split_once('.') {
        Some((clock, fraction)) if !fraction.is_empty() => (clock, Some(fraction)),
        Some(_) => return Err(ParseError),
        None => (text, None),
    };
    let mut fields = clock.split(':');
    let hour = field(fields.next(), 2)?;
    let minute = field(fields.next(), 2)?;
    let second = field(fields.next(), 2)?;
    if fields.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return Err(ParseError);
    }

    let nanos = match fraction {
        None => 0,
        Some(fraction) => {
            if !fraction.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ParseError);
            }
            // Nine digits is the resolution we count in: anything short of it is padded and
            // anything past it dropped, so `.5` is half a second, not five nanoseconds.
            let digits: String = fraction
                .chars()
                .chain(std::iter::repeat('0'))
                .take(9)
                .collect();
            digits.parse().map_err(|_| ParseError)?
        }
    };
    Ok((hour, minute, second, nanos))
}

/// `Z` or `±HH:MM`, as a count of seconds to subtract to reach UTC.
fn parse_offset(text: &str) -> Result<i64, ParseError> {
    if matches!(text, "Z" | "z") {
        return Ok(0);
    }
    let sign = match text.as_bytes().first() {
        Some(b'+') => 1,
        Some(b'-') => -1,
        _ => return Err(ParseError),
    };
    let (hours, minutes) = text
        .get(1..)
        .ok_or(ParseError)?
        .split_once(':')
        .ok_or(ParseError)?;
    let hours = field(Some(hours), 2)?;
    let minutes = field(Some(minutes), 2)?;
    if hours > 23 || minutes > 59 {
        return Err(ParseError);
    }
    Ok(sign * (i64::from(hours) * 3_600 + i64::from(minutes) * 60))
}

/// One fixed-width run of ASCII digits. RFC 3339 pads every field, so a short one is a typo.
fn field(text: Option<&str>, width: usize) -> Result<u32, ParseError> {
    let text = text.ok_or(ParseError)?;
    if text.len() != width || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ParseError);
    }
    text.parse().map_err(|_| ParseError)
}

const fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

const fn days_in(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days from the epoch to a proleptic Gregorian date, by the civil-from-days algorithm: shift the
/// year to start in March so the leap day is the last of it, then count eras of 400 years.
fn days_from_civil(year: i64, month: u32, day: u32) -> Result<i64, ParseError> {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era.checked_mul(146_097)
        .and_then(|days| days.checked_add(day_of_era - 719_468))
        .ok_or(ParseError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_is_zero() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Ok(Nanos::ZERO));
        assert_eq!(parse_timestamp("@0"), Ok(Nanos::ZERO));
    }

    #[test]
    fn the_two_spellings_agree() {
        assert_eq!(
            parse_timestamp("2020-01-01T00:00:00Z"),
            parse_timestamp("@1577836800")
        );
        assert_eq!(
            parse_timestamp("2020-01-01T00:00:00Z"),
            Ok(Nanos(1_577_836_800 * Nanos::SECOND))
        );
    }

    #[test]
    fn an_offset_moves_the_instant_to_utc() {
        let utc = parse_timestamp("2020-01-01T00:00:00Z").unwrap();
        assert_eq!(parse_timestamp("2020-01-01T01:00:00+01:00"), Ok(utc));
        assert_eq!(parse_timestamp("2019-12-31T19:30:00-04:30"), Ok(utc));
    }

    #[test]
    fn a_fraction_is_padded_to_nanoseconds() {
        assert_eq!(
            parse_timestamp("1970-01-01T00:00:00.5Z"),
            Ok(Nanos(500_000_000))
        );
        assert_eq!(
            parse_timestamp("1970-01-01T00:00:00.000000001Z"),
            Ok(Nanos(1))
        );
        assert_eq!(parse_timestamp("@0.25"), Ok(Nanos(250_000_000)));
    }

    #[test]
    fn leap_years_and_month_lengths_are_kept() {
        assert!(parse_timestamp("2020-02-29T00:00:00Z").is_ok());
        assert_eq!(parse_timestamp("2021-02-29T00:00:00Z"), Err(ParseError));
        assert_eq!(parse_timestamp("1900-02-29T00:00:00Z"), Err(ParseError));
        assert!(parse_timestamp("2000-02-29T00:00:00Z").is_ok());
        assert_eq!(parse_timestamp("2020-04-31T00:00:00Z"), Err(ParseError));
    }

    #[test]
    fn dates_before_the_epoch_go_backwards() {
        assert_eq!(
            parse_timestamp("1969-12-31T23:59:59Z"),
            Ok(Nanos(-Nanos::SECOND))
        );
        assert!(parse_timestamp("1900-01-01T00:00:00Z").unwrap() < Nanos::ZERO);
    }

    #[test]
    fn junk_is_refused() {
        for bad in [
            "",
            "2020-01-01",
            "2020-01-01T00:00:00",
            "20-01-01T00:00:00Z",
            "2020-1-1T00:00:00Z",
            "2020-13-01T00:00:00Z",
            "2020-01-01T24:00:00Z",
            "2020-01-01T00:60:00Z",
            "2020-01-01X00:00:00Z",
            "2020-01-01T00:00:00+0100",
            "2020-01-01T00:00:00Q",
            "@",
            "@ten",
            "yesterday",
        ] {
            assert_eq!(
                parse_timestamp(bad),
                Err(ParseError),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn an_instant_outside_the_range_is_refused() {
        assert_eq!(parse_timestamp("2300-01-01T00:00:00Z"), Err(ParseError));
        assert_eq!(parse_timestamp("1600-01-01T00:00:00Z"), Err(ParseError));
    }
}
