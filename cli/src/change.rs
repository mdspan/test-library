//! Changes to a session: operations parsed from their text, staged in order on one transaction,
//! and committed once.

use common::config::{parse_duration, parse_timestamp};
use common::time::{Family, Nanos};
use controller::{Controller, Transaction};

use crate::args::{Kind, Operation};
use crate::failure::Failure;
use crate::family;

/// One operation with its family and value checked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Step {
    pub(crate) family: Family,
    pub(crate) change: Change,
}

/// What a step does to its family.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Change {
    /// Move it by a duration.
    Shift(Nanos),
    /// Make it show a value now.
    SetTime(Nanos),
    /// Run it at a rate, handed to the controller as typed.
    SetRate(f64),
}

impl Step {
    /// The step `operation` spells.
    ///
    /// # Errors
    /// [`Failure::Other`] for an unknown family or a value its operation does not take.
    pub(crate) fn parse(operation: &Operation) -> Result<Self, Failure> {
        let family = family::named(&operation.family)?;
        let value = operation.value.as_str();
        let change = match operation.kind {
            Kind::Shift => Change::Shift(duration(value)?),
            Kind::SetTime => Change::SetTime(time(value)?),
            Kind::SetRate => Change::SetRate(rate(value)?),
        };
        Ok(Self { family, change })
    }

    /// Stages the step on `transaction`.
    ///
    /// # Errors
    /// The controller's refusal of the operation.
    pub(crate) fn stage(&self, transaction: &mut Transaction<'_>) -> Result<(), controller::Error> {
        match self.change {
            Change::Shift(by) => transaction.shift(self.family, by),
            Change::SetTime(to) => transaction.set_time(self.family, to),
            Change::SetRate(rate) => transaction.set_rate(self.family, rate),
        }
    }
}

/// Stages `steps` in order on one transaction of `session` and commits it. A refusal anywhere
/// applies none of them.
///
/// # Errors
/// The controller's refusal of `begin`, of a step, or of the commit.
pub(crate) fn apply(session: &Controller, steps: &[Step]) -> Result<(), Failure> {
    let mut transaction = session.begin()?;
    steps
        .iter()
        .try_for_each(|step| step.stage(&mut transaction))
        .and_then(|()| transaction.commit())
        .map_err(Failure::from)
}

/// What a run prints about one family it changed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Touched {
    pub(crate) family: Family,
    /// A `shift` or `set-time` touched it: print its reading.
    pub(crate) reading: bool,
    /// A `set-rate` touched it: print its rate.
    pub(crate) rate: bool,
}

/// Each family `steps` touch, once, in the order it first appears.
pub(crate) fn touched(steps: &[Step]) -> Vec<Touched> {
    let mut all: Vec<Touched> = Vec::new();
    for step in steps {
        let index = all
            .iter()
            .position(|one| one.family == step.family)
            .unwrap_or_else(|| {
                all.push(Touched {
                    family: step.family,
                    reading: false,
                    rate: false,
                });
                all.len() - 1
            });
        if let Some(one) = all.get_mut(index) {
            match step.change {
                Change::Shift(_) | Change::SetTime(_) => one.reading = true,
                Change::SetRate(_) => one.rate = true,
            }
        }
    }
    all
}

fn duration(text: &str) -> Result<Nanos, Failure> {
    parse_duration(text).map(Nanos).map_err(|_| {
        Failure::Other(format!(
            "{text:?} is not a duration: one number and one unit out of ns, us, ms, s, m, h, d"
        ))
    })
}

/// A timestamp for text starting with `@` or with a date, else a duration: time since the epoch
/// on Realtime, since boot on the monotonic families.
fn time(text: &str) -> Result<Nanos, Failure> {
    let trimmed = text.trim();
    let parsed = if trimmed.starts_with('@') || dated(trimmed) {
        parse_timestamp(trimmed)
    } else {
        parse_duration(trimmed).map(Nanos)
    };
    parsed.map_err(|_| {
        Failure::Other(format!(
            "{text:?} is not a time: an RFC 3339 timestamp with an offset such as \
             2020-01-01T00:00:00Z, @ and Unix seconds, or a duration"
        ))
    })
}

/// Whether `text` opens with the `YYYY-` of an RFC 3339 date.
fn dated(text: &str) -> bool {
    matches!(
        text.as_bytes(),
        [a, b, c, d, b'-', ..] if [a, b, c, d].iter().all(|digit| digit.is_ascii_digit())
    )
}

fn rate(text: &str) -> Result<f64, Failure> {
    text.trim().parse().map_err(|_| {
        Failure::Other(format!(
            "{text:?} is not a rate: fake seconds per second, such as 0.5 or 10"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(kind: Kind, family: &str, value: &str) -> Result<Step, Failure> {
        Step::parse(&Operation {
            kind,
            family: family.to_owned(),
            value: value.to_owned(),
        })
    }

    fn change(kind: Kind, value: &str) -> Option<Change> {
        step(kind, "realtime", value).ok().map(|step| step.change)
    }

    #[test]
    fn set_time_reads_timestamps_unix_seconds_and_durations() {
        let cases = [
            ("2020-01-01T00:00:00.123456789Z", 1_577_836_800_123_456_789),
            ("2020-01-01T01:00:00+01:00", 1_577_836_800 * Nanos::SECOND),
            ("@1577836800", 1_577_836_800 * Nanos::SECOND),
            ("@-1", -Nanos::SECOND),
            ("90s", 90 * Nanos::SECOND),
            (".5s", Nanos::SECOND / 2),
            ("-2h", -7_200 * Nanos::SECOND),
            ("1577836800", 1_577_836_800 * Nanos::SECOND),
        ];
        for (text, nanos) in cases {
            assert_eq!(
                change(Kind::SetTime, text),
                Some(Change::SetTime(Nanos(nanos))),
                "{text}"
            );
        }
    }

    #[test]
    fn set_time_refuses_what_neither_grammar_takes() {
        for text in ["2020-01-01T00:00:00", "2020-01-01", "noon", "", "@"] {
            assert!(change(Kind::SetTime, text).is_none(), "{text}");
        }
    }

    #[test]
    fn rates_pass_through_for_the_controller_to_judge() {
        assert_eq!(change(Kind::SetRate, "0.5"), Some(Change::SetRate(0.5)));
        assert_eq!(change(Kind::SetRate, "-1"), Some(Change::SetRate(-1.0)));
        assert!(matches!(
            change(Kind::SetRate, "NaN"),
            Some(Change::SetRate(rate)) if rate.is_nan()
        ));
        assert!(change(Kind::SetRate, "fast").is_none());
    }

    #[test]
    fn an_unknown_family_is_refused() {
        assert!(step(Kind::Shift, "wallclock", "1h").is_err());
    }

    #[test]
    fn each_family_is_touched_once_in_order_of_first_appearance() {
        let at = |family, change| Step { family, change };
        let steps = [
            at(Family::Monotonic, Change::SetRate(2.0)),
            at(Family::Realtime, Change::Shift(Nanos(1))),
            at(Family::Monotonic, Change::Shift(Nanos(1))),
            at(Family::Realtime, Change::SetTime(Nanos(1))),
            at(Family::Boottime, Change::SetRate(1.0)),
        ];
        let touched = |family, reading, rate| Touched {
            family,
            reading,
            rate,
        };
        assert_eq!(
            super::touched(&steps),
            [
                touched(Family::Monotonic, true, true),
                touched(Family::Realtime, true, false),
                touched(Family::Boottime, false, true),
            ]
        );
    }
}
