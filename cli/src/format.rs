//! How values are written on the tool's output.

use common::platform::clock;
use common::session::layout::{Fault, FaultCode, Keeper};
use common::time::{Nanos, Rate};

const SECONDS_PER_DAY: i64 = 86_400;

/// An instant as RFC 3339 in UTC, with nine fraction digits and `Z`.
pub(crate) fn instant(at: Nanos) -> String {
    let seconds = at.0.div_euclid(Nanos::SECOND);
    let fraction = at.0.rem_euclid(Nanos::SECOND);
    let (year, month, day) = civil_from_days(seconds.div_euclid(SECONDS_PER_DAY));
    let of_day = seconds.rem_euclid(SECONDS_PER_DAY);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{fraction:09}Z",
        of_day / 3_600,
        of_day % 3_600 / 60,
        of_day % 60,
    )
}

/// A duration as `[-]<seconds>.<nine digits>s`.
pub(crate) fn elapsed(by: Nanos) -> String {
    let sign = if by.0 < 0 { "-" } else { "" };
    let magnitude = by.0.unsigned_abs();
    let second = Nanos::SECOND.unsigned_abs();
    format!("{sign}{}.{:09}s", magnitude / second, magnitude % second)
}

/// A rate with nine fraction digits.
pub(crate) fn rate(rate: Rate) -> String {
    fraction(rate.as_f64())
}

/// A rate as a `double`, with nine fraction digits.
pub(crate) fn fraction(rate: f64) -> String {
    format!("{rate:.9}")
}

/// A timebase by the first name the platform's clock table gives its id, else the number. Empty
/// for none.
pub(crate) fn timebase(id: Option<libc::clockid_t>) -> String {
    let Some(id) = id else {
        return String::new();
    };
    clock::named()
        .iter()
        .find(|&&(_, known)| known == id)
        .map_or_else(|| id.to_string(), |&(name, _)| name.to_owned())
}

/// A keeper's lowercase label.
pub(crate) const fn keeper(keeper: Keeper) -> &'static str {
    keeper.as_str()
}

/// A fault's lowercase label, or empty when there is none.
pub(crate) const fn fault(fault: Option<Fault>) -> &'static str {
    match fault {
        None => "",
        Some(Fault {
            code: FaultCode::BadValue,
            ..
        }) => "bad_value",
    }
}

/// The pid that hit a fault, or empty when there is none.
pub(crate) fn fault_pid(fault: Option<Fault>) -> String {
    fault.map_or_else(String::new, |fault| fault.pid.to_string())
}

/// The proleptic Gregorian date `days` after the epoch, by the days-to-civil algorithm: count from
/// 0000-03-01 so the leap day ends the year, then split into 400-year eras.
const fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = era * 400 + year_of_era + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use common::config::parse_timestamp;

    use super::*;

    #[test]
    fn instants_read_back_through_the_library() {
        for nanos in [
            0,
            1,
            -1,
            1_577_836_800_123_456_789,
            951_782_400 * Nanos::SECOND,
            -2_208_988_800 * Nanos::SECOND + 5,
            i64::MAX,
            i64::MIN + Nanos::SECOND,
        ] {
            let text = instant(Nanos(nanos));
            assert_eq!(parse_timestamp(&text).ok(), Some(Nanos(nanos)), "{text}");
        }
    }

    #[test]
    fn instants_before_the_epoch_count_back_from_it() {
        assert_eq!(instant(Nanos(-1)), "1969-12-31T23:59:59.999999999Z");
        assert_eq!(instant(Nanos(0)), "1970-01-01T00:00:00.000000000Z");
        assert_eq!(instant(Nanos(i64::MIN)), "1677-09-21T00:12:43.145224192Z");
        assert_eq!(
            instant(Nanos(951_782_400 * Nanos::SECOND)),
            "2000-02-29T00:00:00.000000000Z"
        );
    }

    #[test]
    fn elapsed_keeps_the_sign_outside_the_digits() {
        assert_eq!(elapsed(Nanos(0)), "0.000000000s");
        assert_eq!(elapsed(Nanos(1_500_000_000)), "1.500000000s");
        assert_eq!(elapsed(Nanos(-1)), "-0.000000001s");
        assert_eq!(elapsed(Nanos(i64::MIN)), "-9223372036.854775808s");
    }

    #[test]
    fn timebases_go_by_name_or_number() {
        assert_eq!(timebase(None), "");
        assert_eq!(timebase(Some(clock::REALTIME)), "CLOCK_REALTIME");
        assert_eq!(timebase(Some(12_345)), "12345");
    }
}
