//! The affine keeper's arithmetic.

use super::{Nanos, Rate};

/// One family's line over its timebase:
/// `fake = fake_anchor + rate * max(0, timebase_now - timebase_anchor)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    /// What the family shows at `timebase_anchor`.
    pub fake_anchor: Nanos,
    /// The genuine reading the anchor was taken against.
    pub timebase_anchor: Nanos,
    /// Fake nanoseconds per nanosecond of the timebase.
    pub rate: Rate,
}

impl Line {
    /// What the family shows for a genuine timebase reading.
    ///
    /// The clamp covers a reading older than the anchor it is paired with: a drifted sample, a
    /// preempted reader, a skewed or stepped timebase. Such a reading contributes nothing rather
    /// than pulling the clock backwards.
    #[must_use]
    pub fn project(&self, timebase_now: Nanos) -> Nanos {
        let elapsed = timebase_now.saturating_sub(self.timebase_anchor).0.max(0);
        self.fake_anchor
            .saturating_add(Nanos(self.rate.scale(elapsed)))
    }

    /// What the family shows for `instant`, a genuine reading of a clock that need not be the
    /// timebase, against `timebase_now` and `clock_now`: readings of the timebase and of that clock
    /// taken together.
    ///
    /// The instant lies `instant - clock_now` past the pair, which [`Line::project`] places the
    /// elapsed time past the anchor, and the whole distance from the anchor is scaled in one step,
    /// rounding toward negative infinity. With a single rounding the answer depends on the
    /// instant alone for as long as the two clocks advance together, so it is the same at every
    /// moment it is asked, and [`Line::earliest_showing`] inverts it exactly.
    #[must_use]
    pub fn show_instant(&self, timebase_now: Nanos, clock_now: Nanos, instant: Nanos) -> Nanos {
        let along = self.elapsed(timebase_now) + i128::from(instant.0) - i128::from(clock_now.0);
        // Inside `i64`, the product with a 64-bit rate fits in `i128`.
        let along = along.clamp(i128::from(i64::MIN), i128::from(i64::MAX));
        let carried = (along * i128::from(self.rate.0)) >> Rate::FRACTION_BITS;
        saturated(i128::from(self.fake_anchor.0) + carried)
    }

    /// The earliest genuine instant [`Line::show_instant`] shows at `fake` or past it, against the
    /// same two readings, or `None` on a frozen line, which shows its anchor for every instant.
    ///
    /// At a rate of one or below every fake nanosecond is shown for some genuine one, so the
    /// instant shows `fake` itself. A faster line skips fake nanoseconds, and the instant shows the
    /// first one it reaches, less than a rate's worth of them past `fake`.
    #[must_use]
    pub fn earliest_showing(
        &self,
        timebase_now: Nanos,
        clock_now: Nanos,
        fake: Nanos,
    ) -> Option<Nanos> {
        if self.rate == Rate::FROZEN {
            return None;
        }
        let rate = i128::from(self.rate.0);
        let scaled = (i128::from(fake.0) - i128::from(self.fake_anchor.0)) << Rate::FRACTION_BITS;
        let along = scaled.div_euclid(rate) + i128::from(scaled.rem_euclid(rate) != 0);
        Some(saturated(
            i128::from(clock_now.0) - self.elapsed(timebase_now) + along,
        ))
    }

    /// How far past the anchor a timebase reading lies, clamped the way [`Line::project`] clamps
    /// it.
    fn elapsed(&self, timebase_now: Nanos) -> i128 {
        i128::from(timebase_now.saturating_sub(self.timebase_anchor).0.max(0))
    }
}

/// The nearest instant there is to `value`.
fn saturated(value: i128) -> Nanos {
    Nanos(i64::try_from(value).unwrap_or(if value < 0 { i64::MIN } else { i64::MAX }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Nanos = Nanos(1_700_000_000_000_000_000);

    /// A line that shows the timebase itself, shifted: what a family starts as at creation.
    fn anchored(timebase_now: Nanos, shift: Nanos) -> Line {
        Line {
            fake_anchor: timebase_now.saturating_add(shift),
            timebase_anchor: timebase_now,
            rate: Rate::UNITY,
        }
    }

    #[test]
    fn an_unshifted_line_shows_the_timebase() {
        let line = anchored(NOW, Nanos::ZERO);
        assert_eq!(line.project(NOW), NOW);
        assert_eq!(
            line.project(Nanos(NOW.0 + 10 * Nanos::SECOND)),
            Nanos(NOW.0 + 10 * Nanos::SECOND)
        );
    }

    #[test]
    fn a_shift_moves_the_whole_line() {
        let line = anchored(NOW, Nanos(-7_200 * Nanos::SECOND));
        assert_eq!(line.project(NOW), Nanos(NOW.0 - 7_200 * Nanos::SECOND));
        assert_eq!(
            line.project(Nanos(NOW.0 + Nanos::SECOND)),
            Nanos(NOW.0 - 7_200 * Nanos::SECOND + Nanos::SECOND)
        );
    }

    #[test]
    fn the_rate_scales_elapsed_time_alone() {
        let line = Line {
            rate: Rate::from_f64(10.0).unwrap(),
            ..anchored(NOW, Nanos::ZERO)
        };
        assert_eq!(line.project(NOW), NOW);
        assert_eq!(
            line.project(Nanos(NOW.0 + Nanos::SECOND)),
            Nanos(NOW.0 + 10 * Nanos::SECOND)
        );
    }

    #[test]
    fn rate_zero_freezes_the_family() {
        let line = Line {
            rate: Rate::FROZEN,
            ..anchored(NOW, Nanos::ZERO)
        };
        assert_eq!(line.project(Nanos(NOW.0 + 3600 * Nanos::SECOND)), NOW);
    }

    #[test]
    fn a_timebase_behind_the_anchor_shows_the_anchor() {
        let line = anchored(NOW, Nanos::ZERO);
        assert_eq!(line.project(Nanos(NOW.0 - Nanos::SECOND)), NOW);
        assert_eq!(line.project(Nanos(i64::MIN)), NOW);
    }

    #[test]
    fn projection_saturates_instead_of_wrapping() {
        let line = Line {
            fake_anchor: Nanos(i64::MAX - 1),
            timebase_anchor: Nanos::ZERO,
            rate: Rate::UNITY,
        };
        assert_eq!(line.project(Nanos(i64::MAX)), Nanos(i64::MAX));
    }

    /// Rates at, below and above one, the dyadic ones exact in the fixed point and the others not.
    fn rates() -> Vec<Rate> {
        [1.0, 0.25, 0.3, 1.0 / 3.0, 1e-6, 1.5, 2.7, 10.0, 1000.0]
            .into_iter()
            .map(|rate| Rate::from_f64(rate).unwrap())
            .collect()
    }

    /// A line as a set clock leaves it: anchored at an instant with nanoseconds of its own, so no
    /// distance from it is a whole second.
    fn set_at(rate: Rate) -> Line {
        Line {
            fake_anchor: Nanos(946_684_800_123_456_789),
            timebase_anchor: NOW,
            rate,
        }
    }

    #[test]
    fn an_instant_shows_the_same_at_every_moment_it_is_asked() {
        for rate in rates() {
            let line = set_at(rate);
            for instant in [
                NOW.0 - 86_400 * Nanos::SECOND - 7,
                NOW.0 + 1,
                NOW.0 + 999_999,
            ] {
                let instant = Nanos(instant);
                let first = line.show_instant(NOW, NOW, instant);
                for later in 1..200 {
                    let now = Nanos(NOW.0 + later * 7_919);
                    assert_eq!(
                        line.show_instant(now, now, instant),
                        first,
                        "{rate:?} of {instant:?} at {now:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_instant_of_the_reading_shows_what_the_clock_shows() {
        for rate in rates() {
            let line = set_at(rate);
            for later in [0, 1, 999, 3_600 * Nanos::SECOND + 17] {
                let now = Nanos(NOW.0 + later);
                assert_eq!(line.show_instant(now, now, now), line.project(now));
            }
        }
    }

    #[test]
    fn a_stored_instant_shows_what_was_asked_for() {
        for rate in rates() {
            let line = set_at(rate);
            let past_one = rate > Rate::UNITY;
            // A whole second, a microsecond and a nanosecond, either side of the anchor.
            for fake in [
                946_684_800 * Nanos::SECOND,
                946_684_800_123_456_000,
                946_684_801_987_654_321,
                946_684_740_000_000_001,
                946_688_400 * Nanos::SECOND,
            ] {
                let fake = Nanos(fake);
                let stored = line.earliest_showing(NOW, NOW, fake).unwrap();
                // Asked again a while later, as the `stat` after a set is.
                let now = Nanos(NOW.0 + 12_345_678);
                let shown = line.show_instant(now, now, stored);
                if past_one {
                    let step = i64::try_from(rate.0 >> Rate::FRACTION_BITS).unwrap() + 1;
                    assert!(
                        (fake.0..fake.0 + step).contains(&shown.0),
                        "{rate:?} of {fake:?} shows {shown:?}"
                    );
                } else {
                    assert_eq!(shown, fake, "{rate:?}");
                }
                // And it is the earliest: the nanosecond before it shows something earlier.
                let before = line.show_instant(now, now, Nanos(stored.0 - 1));
                assert!(before < fake, "{rate:?} of {fake:?}: {before:?}");
            }
        }
    }

    #[test]
    fn a_stored_instant_shows_what_was_asked_for_against_another_timebase() {
        // The timebase reads on a scale of its own, so the pair is two unrelated numbers.
        let line = set_at(Rate::from_f64(0.3).unwrap());
        let timebase_now = Nanos(NOW.0 + 5 * Nanos::SECOND);
        let clock_now = Nanos(1_234_567_890_123);
        let fake = Nanos(946_684_801_987_654_321);
        let stored = line
            .earliest_showing(timebase_now, clock_now, fake)
            .unwrap();
        assert_eq!(line.show_instant(timebase_now, clock_now, stored), fake);
    }

    #[test]
    fn a_frozen_line_stores_nothing_and_shows_its_anchor() {
        let line = set_at(Rate::FROZEN);
        assert_eq!(line.earliest_showing(NOW, NOW, NOW), None);
        assert_eq!(
            line.show_instant(NOW, NOW, Nanos(NOW.0 - Nanos::SECOND)),
            line.fake_anchor
        );
    }

    #[test]
    fn instants_at_the_ends_saturate_instead_of_wrapping() {
        let line = Line {
            rate: Rate::MAX,
            ..set_at(Rate::UNITY)
        };
        assert_eq!(
            line.show_instant(NOW, NOW, Nanos(i64::MAX)),
            Nanos(i64::MAX)
        );
        assert_eq!(
            line.show_instant(NOW, Nanos(i64::MAX), Nanos(i64::MIN)),
            Nanos(i64::MIN)
        );
        let slowest = set_at(Rate(1));
        assert_eq!(
            slowest.earliest_showing(NOW, NOW, Nanos(i64::MAX)),
            Some(Nanos(i64::MAX))
        );
        assert_eq!(
            slowest.earliest_showing(NOW, NOW, Nanos(i64::MIN)),
            Some(Nanos(i64::MIN))
        );
    }
}
