//! The Mach clocks: the counter `mach_absolute_time` reads, the sleep measured in its ticks, and
//! the clock ports `host_get_clock_service` hands out.
//!
//! A tick is not a nanosecond. `mach_timebase_info` gives the ratio a program converts with, one
//! nanosecond a tick on Intel and 125 nanoseconds every 3 ticks on Apple silicon, and we convert
//! with the same one in both directions: the ticks a call carries become nanoseconds, the keeper
//! answers in nanoseconds, and its answer becomes ticks again.
//!
//! [`Ratio`] is that conversion and nothing else, which is why it is built and tested wherever the
//! suite runs rather than on the one platform that has a counter to convert. Every product below
//! is taken in 128 bits: a count of ticks near the end of its range times a numerator of 125 is
//! past what 64 hold, and a wait forever is written as exactly such a count.
//!
//! The bodies beside it are macOS's, in [`entries`].

#[cfg(target_os = "macos")]
mod entries;

#[cfg(target_os = "macos")]
pub(crate) use entries::*;

use common::time::Nanos;

/// The ratio `mach_timebase_info` answers: `numer` nanoseconds for every `denom` ticks.
///
/// Both halves are kept widened, because every use of either is a factor of a 128-bit product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ratio {
    /// Nanoseconds per `denom` ticks.
    numer: u64,
    /// Ticks per `numer` nanoseconds.
    denom: u64,
}

impl Ratio {
    /// One nanosecond a tick, which is Intel's counter and the answer a kernel that says nothing
    /// is read as.
    pub(crate) const IDENTITY: Self = Self { numer: 1, denom: 1 };

    /// The ratio the two halves name, or [`Ratio::IDENTITY`] when either is zero.
    ///
    /// A zero half is a kernel that filled nothing, and a tick a nanosecond is the reading that is
    /// wrong by the least; a zero denominator is besides a division nothing may make.
    pub(crate) fn new(numer: u32, denom: u32) -> Self {
        if numer == 0 || denom == 0 {
            return Self::IDENTITY;
        }
        Self {
            numer: u64::from(numer),
            denom: u64::from(denom),
        }
    }

    /// The pair as one word, the numerator above the denominator, so a reader takes both halves in
    /// one load. Never zero, which is what leaves zero naming a pair nobody has asked for yet.
    pub(crate) const fn packed(self) -> u64 {
        (self.numer << 32) | self.denom
    }

    /// The pair such a word holds.
    pub(crate) fn unpack(word: u64) -> Self {
        Self::new(
            u32::try_from(word >> 32).unwrap_or(0),
            u32::try_from(word & u64::from(u32::MAX)).unwrap_or(0),
        )
    }

    /// The nanoseconds `ticks` of the counter stand for, truncated the way the program's own
    /// conversion truncates it.
    ///
    /// The library never makes this conversion: every count of ticks it hands out it wrote itself,
    /// from nanoseconds. It is here because it is what the program will make of one, so it is what
    /// the cases below hold the other three to, and it is what [`Ratio::nanos_at`] is the rounded
    /// form of.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the conversion the program makes of what we hand it, which the cases hold \
                      the other three against"
        )
    )]
    pub(crate) fn nanos(self, ticks: u64) -> Nanos {
        Self::instant(u128::from(ticks) * u128::from(self.numer) / u128::from(self.denom))
    }

    /// The first instant at which the counter reads `ticks`.
    ///
    /// A deadline in ticks is a promise about a reading, and a reading truncates, so the instant
    /// the promise comes due is the first nanosecond whose reading is `ticks` rather than the one
    /// `ticks` itself truncates from.
    pub(crate) fn nanos_at(self, ticks: u64) -> Nanos {
        Self::instant((u128::from(ticks) * u128::from(self.numer)).div_ceil(u128::from(self.denom)))
    }

    /// What the counter reads at `nanos`, truncated for the reason a counter's reading is.
    pub(crate) fn ticks(self, nanos: Nanos) -> u64 {
        Self::count(Self::elapsed(nanos) * u128::from(self.denom) / u128::from(self.numer))
    }

    /// The first tick at or past `nanos`, which is the deadline a wait until `nanos` blocks to.
    pub(crate) fn ticks_at(self, nanos: Nanos) -> u64 {
        Self::count(
            (Self::elapsed(nanos) * u128::from(self.denom)).div_ceil(u128::from(self.numer)),
        )
    }

    /// A product as the instant it names, clamped to the widest one there is.
    fn instant(value: u128) -> Nanos {
        Nanos(i64::try_from(value).unwrap_or(i64::MAX))
    }

    /// A product as the count of ticks it names, clamped to the widest the counter holds.
    fn count(value: u128) -> u64 {
        u64::try_from(value).unwrap_or(u64::MAX)
    }

    /// An instant as the quantity to convert, with anything below zero read as none of it: a
    /// counter starts at zero and never ran before it.
    fn elapsed(nanos: Nanos) -> u128 {
        u128::from(nanos.0.max(0).unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apple silicon's counter, which ticks at 24 MHz: 125 nanoseconds every 3 ticks.
    const APPLE: Ratio = Ratio {
        numer: 125,
        denom: 3,
    };

    /// Intel's, one nanosecond a tick.
    const INTEL: Ratio = Ratio::IDENTITY;

    /// A second, in the units each side counts in.
    const SECOND_NS: Nanos = Nanos(1_000_000_000);

    #[test]
    fn a_second_is_a_second_at_either_ratio() {
        assert_eq!(INTEL.nanos(1_000_000_000), SECOND_NS);
        assert_eq!(INTEL.ticks(SECOND_NS), 1_000_000_000);
        // 24 MHz, which is what 125:3 says the counter runs at.
        assert_eq!(APPLE.ticks(SECOND_NS), 24_000_000);
        assert_eq!(APPLE.nanos(24_000_000), SECOND_NS);
    }

    #[test]
    fn a_kernel_that_answers_nothing_leaves_a_tick_a_nanosecond() {
        assert_eq!(Ratio::new(0, 3), Ratio::IDENTITY);
        assert_eq!(Ratio::new(125, 0), Ratio::IDENTITY);
        assert_eq!(Ratio::new(0, 0), Ratio::IDENTITY);
        assert_eq!(Ratio::new(1, 1), Ratio::IDENTITY);
    }

    #[test]
    fn the_pair_survives_the_word_it_travels_in() {
        for ratio in [INTEL, APPLE, Ratio::new(1_000_000_000, 24_000_000)] {
            assert_eq!(Ratio::unpack(ratio.packed()), ratio);
            assert_ne!(ratio.packed(), 0, "a packed pair is never the unasked one");
        }
        // The widest pair either half holds, which is where a narrower word would have lost one.
        let widest = Ratio::new(u32::MAX, u32::MAX);
        assert_eq!(Ratio::unpack(widest.packed()), widest);
    }

    #[test]
    fn a_count_that_would_overflow_a_64_bit_product_still_converts() {
        // Two hundred million million ticks: times a numerator of 125 that is past what 64 bits
        // hold, and the answer is an instant well inside what one holds.
        let ticks = 200_000_000_000_000_000u64;
        assert_eq!(APPLE.nanos(ticks), Nanos(8_333_333_333_333_333_333));
        // And the other way: the widest instant times a denominator of 3 is past 64 bits too.
        assert_eq!(APPLE.ticks(Nanos(i64::MAX)), 221_360_928_884_514_619);
    }

    #[test]
    fn a_count_past_what_an_instant_holds_clamps_rather_than_wrapping() {
        // The tick a program writes for a wait that should never come due.
        assert_eq!(APPLE.nanos(u64::MAX), Nanos(i64::MAX));
        assert_eq!(APPLE.nanos_at(u64::MAX), Nanos(i64::MAX));
        assert_eq!(INTEL.nanos(u64::MAX), Nanos(i64::MAX));
        // And a counter that would have to read past its own end.
        assert_eq!(Ratio::new(1, 1_000).ticks(Nanos(i64::MAX)), u64::MAX);
        assert_eq!(Ratio::new(1, 1_000).ticks_at(Nanos(i64::MAX)), u64::MAX);
    }

    #[test]
    fn an_instant_before_the_counter_started_reads_as_no_ticks() {
        for ratio in [INTEL, APPLE] {
            assert_eq!(ratio.ticks(Nanos(-1)), 0);
            assert_eq!(ratio.ticks(Nanos(i64::MIN)), 0);
            assert_eq!(ratio.ticks_at(Nanos(-1)), 0);
            assert_eq!(ratio.nanos(0), Nanos::ZERO);
            assert_eq!(ratio.ticks(Nanos::ZERO), 0);
        }
    }

    #[test]
    fn the_instant_a_tick_comes_due_is_the_first_one_that_reads_it() {
        for ratio in [INTEL, APPLE] {
            for ticks in 0..2_000u64 {
                let at = ratio.nanos_at(ticks);
                assert_eq!(ratio.ticks(at), ticks, "{ratio:?} at {ticks}");
                assert!(
                    at == Nanos::ZERO || ratio.ticks(Nanos(at.0 - 1)) < ticks,
                    "{ratio:?} reads {ticks} a nanosecond before {at:?}"
                );
            }
        }
        // The one tick of Apple silicon that a truncating conversion would have lost: 125/3 is 41
        // nanoseconds and a third, so the counter reads it at 42 and not at 41.
        assert_eq!(APPLE.nanos(1), Nanos(41));
        assert_eq!(APPLE.nanos_at(1), Nanos(42));
        assert_eq!(APPLE.ticks(Nanos(41)), 0);
        assert_eq!(APPLE.ticks(Nanos(42)), 1);
    }

    #[test]
    fn the_first_tick_at_an_instant_is_the_first_one_at_or_past_it() {
        for ratio in [INTEL, APPLE] {
            for nanos in (0..100_000i64).step_by(7) {
                let at = ratio.ticks_at(Nanos(nanos));
                assert!(
                    ratio.nanos(at) >= Nanos(nanos),
                    "{ratio:?} reads {at} before {nanos}"
                );
                assert!(
                    at == 0 || ratio.nanos(at - 1) < Nanos(nanos),
                    "{ratio:?} already read {nanos} a tick before {at}"
                );
            }
        }
    }

    #[test]
    fn a_round_trip_never_runs_the_clock_forward() {
        // The truncating pair is the one a reading goes through, so neither direction may come
        // back larger than it went in: a clock that gained a tick per read would not be monotone
        // against itself.
        for ratio in [INTEL, APPLE, Ratio::new(1_000_000_000, 24_000_000)] {
            for uptime in [
                0i64,
                1,
                41,
                1_000_000_007,
                5 * 86_400 * 1_000_000_000,
                i64::MAX,
            ] {
                let there = ratio.ticks(Nanos(uptime));
                assert!(ratio.nanos(there) <= Nanos(uptime), "{ratio:?} at {uptime}");
            }
            for ticks in [0u64, 1, 24_000_000, 1_000_000_007, u64::MAX / 125] {
                assert!(
                    ratio.ticks(ratio.nanos(ticks)) <= ticks,
                    "{ratio:?} at {ticks}"
                );
            }
        }
    }

    #[test]
    fn a_days_uptime_agrees_with_itself_at_both_ratios() {
        // What a clock read actually converts: a family's nanoseconds out to ticks and back, at a
        // magnitude a machine really reaches. A day of it may lose the tick it truncates and
        // nothing more.
        let day = Nanos(86_400 * 1_000_000_000);
        for ratio in [INTEL, APPLE] {
            let there = ratio.nanos(ratio.ticks(day));
            assert!(
                (day.0 - there.0) < 42,
                "{ratio:?} lost {} ns of a day",
                day.0 - there.0
            );
        }
    }
}
