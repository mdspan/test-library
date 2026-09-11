//! The page's shape, its publishing protocol, and the clock a reader computes from it.
//!
//! Everything here runs wherever the suite runs. The page belongs to FreeBSD, but its layout is
//! fixed by the format and its arithmetic by the reader, and neither needs that kernel to check.

use std::mem::offset_of;

use super::*;
use crate::platform;
use crate::session::layout::ParamsValue;
use crate::session::{Session, Setup, create, naming};
use crate::sync::Anchor;
use crate::time::Rate;

/// A basis over the counter this machine has, at a frequency of our own choosing, so a case can say
/// exactly how far a page should advance between two readings.
///
/// 2^32 units of scale per tick is 2^-32 of a second per tick, which is a counter of about four
/// gigahertz: a plausible one, and a power of two, so the arithmetic below is exact.
fn made_up_basis() -> Basis {
    Basis {
        algo: ALGO_NATIVE,
        flavour: 0,
        mask: u32::MAX,
        scale: 1 << 32,
    }
}

fn hands_at(count: u32, elapsed: Nanos, realtime: Nanos, scale: u64) -> Hands {
    let offset = Bintime::from_nanos(elapsed);
    Hands {
        algo: ALGO_NATIVE,
        flavour: 0,
        scale,
        offset_count: count,
        counter_mask: u32::MAX,
        offset,
        boottime: Bintime::from_nanos(realtime).minus(offset),
    }
}

/// A zeroed structure a reader follows, on the heap, for the cases about the publishing protocol
/// alone.
fn blank_timekeep() -> Box<Timekeep> {
    // Safety: every field of it is an integer, so an all-zero value is a valid one.
    let page: Box<Timekeep> = unsafe { Box::new(std::mem::zeroed()) };
    page.initialize();
    page
}

/// A zeroed page on the heap, which is what the session file hands a republisher: the structure a
/// reader follows and the republishers' own words behind it.
fn blank_page() -> Box<Page> {
    // Safety: every field of the page is an integer, so an all-zero value is a valid one.
    let page: Box<Page> = unsafe { Box::new(std::mem::zeroed()) };
    page.initialize();
    page
}

#[test]
fn the_page_is_the_shape_a_reader_has_compiled_in() {
    // `struct vdso_timehands`: four words, the scale, two more words, two bintimes, and a
    // machine-dependent tail of thirty-two bytes on both architectures we support.
    assert_eq!(size_of::<Timehands>(), 88);
    assert_eq!(offset_of!(Timehands, algo), 0);
    assert_eq!(offset_of!(Timehands, generation), 4);
    assert_eq!(offset_of!(Timehands, scale), 8);
    assert_eq!(offset_of!(Timehands, offset_count), 16);
    assert_eq!(offset_of!(Timehands, counter_mask), 20);
    assert_eq!(offset_of!(Timehands, offset_sec), 24);
    assert_eq!(offset_of!(Timehands, offset_frac), 32);
    assert_eq!(offset_of!(Timehands, boottime_sec), 40);
    assert_eq!(offset_of!(Timehands, boottime_frac), 48);
    assert_eq!(offset_of!(Timehands, md), 56);

    // `struct vdso_timekeep`: three words, the padding the alignment of a timehands imposes, and
    // then the sets themselves.
    assert_eq!(offset_of!(Timekeep, version), 0);
    assert_eq!(offset_of!(Timekeep, enabled), 4);
    assert_eq!(offset_of!(Timekeep, current), 8);
    assert_eq!(offset_of!(Timekeep, hands), 16);

    // And the whole of it fits the page the session file sets aside for it.
    assert!(size_of::<Timekeep>() <= 4096);
}

#[test]
fn a_bintime_carries_a_nanosecond_instant_back_and_forth() {
    for nanos in [
        0,
        1,
        999_999_999,
        Nanos::SECOND,
        1_700_000_000 * Nanos::SECOND + 123_456_789,
        -1,
        -(Nanos::SECOND) - 500,
    ] {
        let there_and_back = Bintime::from_nanos(Nanos(nanos)).to_nanos();
        assert!(
            (there_and_back.0 - nanos).abs() <= 1,
            "{nanos} came back as {}",
            there_and_back.0,
        );
    }
    // A fraction below a second stays a fraction: half a second is the top bit of the word.
    assert_eq!(Bintime::from_nanos(Nanos(500_000_000)).frac, 1 << 63);
    assert_eq!(Bintime::from_nanos(Nanos(Nanos::SECOND)).sec, 1);
}

#[test]
fn a_difference_of_two_instants_borrows_across_the_second() {
    let later = Bintime::from_nanos(Nanos(2 * Nanos::SECOND + 250_000_000));
    let earlier = Bintime::from_nanos(Nanos(Nanos::SECOND + 750_000_000));
    let gap = later.minus(earlier);
    assert_eq!(gap.to_nanos().0, 500_000_000);
    // And the sum of the two halves is the whole again, which is what a reader computes for an
    // absolute clock.
    assert_eq!(earlier.plus(gap), later);
}

#[test]
fn publishing_toggles_the_index_and_leaves_the_other_set_alone() {
    let page = blank_timekeep();
    assert_eq!(page.load(), None, "an unpublished page is followed by none");

    let first = hands_at(
        1_000,
        Nanos(42),
        Nanos(1_700_000_000 * Nanos::SECOND),
        1 << 32,
    );
    page.publish(&first);
    assert_eq!(page.current.load(Ordering::Relaxed), 1);
    assert_eq!(page.load(), Some(first));

    let second = hands_at(
        2_000,
        Nanos(43),
        Nanos(1_700_000_001 * Nanos::SECOND),
        1 << 33,
    );
    page.publish(&second);
    assert_eq!(page.current.load(Ordering::Relaxed), 0);
    assert_eq!(page.load(), Some(second));

    // Back where it started, and a third publish reuses the set the first one wrote.
    page.publish(&first);
    assert_eq!(page.current.load(Ordering::Relaxed), 1);
    assert_eq!(page.load(), Some(first));
}

#[test]
fn the_generation_climbs_and_never_lands_on_the_one_that_means_busy() {
    let page = blank_timekeep();
    let hands = hands_at(0, Nanos::ZERO, Nanos::ZERO, 1 << 32);
    let mut seen = Vec::new();
    for _ in 0..4 {
        page.publish(&hands);
        let current = page.current.load(Ordering::Relaxed) as usize;
        let generation = page.hands[current].generation.load(Ordering::Relaxed);
        assert_ne!(
            generation, 0,
            "a published set says it is still being written"
        );
        seen.push(generation);
    }
    assert_eq!(seen, vec![1, 2, 3, 4]);

    // The wrap is the one place the climb could land on zero, and it steps over it.
    page.hands[page.current.load(Ordering::Relaxed) as usize]
        .generation
        .store(u32::MAX, Ordering::Relaxed);
    page.publish(&hands);
    let current = page.current.load(Ordering::Relaxed) as usize;
    assert_eq!(page.hands[current].generation.load(Ordering::Relaxed), 1);
}

#[test]
fn a_page_a_reader_will_not_follow_answers_nothing() {
    let page = blank_timekeep();
    page.publish(&hands_at(0, Nanos::ZERO, Nanos::ZERO, 1 << 32));
    assert!(page.load().is_some());

    // A version nobody knows, which is what another library's page would carry.
    page.version.store(VERSION + 1, Ordering::Release);
    assert_eq!(page.load(), None);
    page.version.store(VERSION, Ordering::Release);

    // And a page that has been turned off, which is what a fresh one is until the first publish.
    page.enabled.store(0, Ordering::Release);
    assert_eq!(page.load(), None);
}

#[test]
fn a_reader_of_the_page_extrapolates_at_the_scale_it_carries() {
    let basis = made_up_basis();
    let elapsed = Nanos(3_600 * Nanos::SECOND);
    let realtime = Nanos(1_700_000_000 * Nanos::SECOND);
    let hands = hands_at(1_000_000, elapsed, realtime, basis.scale);

    // At the counter the parameters were taken at, the page shows exactly what went into it.
    assert_eq!(hands.showing(1_000_000, false).to_nanos(), elapsed);
    assert!((hands.showing(1_000_000, true).to_nanos().0 - realtime.0).abs() <= 1);

    // A scale of 2^32 is 2^-32 of a second per tick, so 2^32 ticks are one second.
    let a_second_on = hands
        .showing(1_000_000u32.wrapping_add(1 << 30), false)
        .to_nanos();
    assert!(
        (a_second_on.0 - elapsed.0 - Nanos::SECOND / 4).abs() < 1_000,
        "a quarter of a second of ticks moved the page by {} ns",
        a_second_on.0 - elapsed.0,
    );

    // Twice the scale is twice the speed, which is how a session's rate reaches a page reader.
    let faster = hands_at(1_000_000, elapsed, realtime, basis.scale * 2);
    let moved = faster
        .showing(1_000_000u32.wrapping_add(1 << 30), false)
        .to_nanos();
    assert!((moved.0 - elapsed.0 - Nanos::SECOND / 2).abs() < 1_000);
}

#[test]
fn the_counter_wraps_inside_its_mask_rather_than_running_backwards() {
    // A sixteen-bit counter, snapshotted sixteen ticks before it wraps, at a scale coarse enough
    // for a tick to be a number of nanoseconds rather than a fraction of one.
    let hands = Hands {
        counter_mask: 0xffff,
        ..hands_at(0xfff0, Nanos::ZERO, Nanos::ZERO, 1 << 58)
    };
    // Eight ticks on, the counter has not wrapped; sixteen on, it has, and the page must read it
    // as sixteen ticks rather than as an enormous number of them.
    let before = hands.showing(0xfff8, false).to_nanos();
    let after = hands.showing(0x0000, false).to_nanos();
    assert!(after > before, "the page ran backwards across a wrap");
    assert_eq!(after.0, 2 * before.0, "sixteen ticks is twice eight");
    // A reading outside the mask is masked before the difference is taken, so the bits above it
    // change nothing.
    assert_eq!(hands.showing(0xffff_0000, false).to_nanos(), after);
}

#[test]
fn the_scale_saturates_rather_than_wrapping_past_what_the_page_can_hold() {
    // A rate the page cannot encode publishes the fastest it can, which is what leaves the cadence
    // to make up the rest in forward steps.
    assert_eq!(scaled(1 << 40, Rate::MAX), u64::MAX);
    assert_eq!(scaled(1 << 32, Rate::UNITY), 1 << 32);
    assert_eq!(scaled(1 << 32, Rate::FROZEN), 0);
    // Half speed halves the scale, which is the whole of how a rate reaches the page.
    assert_eq!(scaled(1 << 32, Rate::from_f64(0.5).unwrap()), 1 << 31);
}

/// A session in a directory of its own, with the page on.
fn session_in(dir: &std::path::Path, apply: impl FnOnce(&mut Setup)) -> Session {
    let mut setup = Setup::plain(dir.join("libtimewarp"));
    setup.emulate_vdso = true;
    apply(&mut setup);
    create(&setup).unwrap()
}

#[test]
fn a_created_session_carries_a_page_a_reader_can_already_follow() {
    if !platform::HAS_TIMEKEEP_PAGE || counter::basis().is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let session = session_in(tmp.path(), |_| {});
    let page = session.timekeep().expect("the page is live");
    let basis = counter::basis().unwrap();

    let hands = page
        .timekeep
        .load()
        .expect("the creator published the page");
    assert_eq!(hands.algo, basis.algo);
    assert_eq!(hands.counter_mask, basis.mask);

    // And what a reader computes from it is the session's own clock, which here is the genuine one.
    let through_page = page.timekeep.reading(basis, true).expect("the page reads");
    let through_libc = platform::genuine_nanos(clock::REALTIME).unwrap();
    assert!(
        (through_page.0 - through_libc.0).abs() < Nanos::SECOND,
        "the page reads {through_page:?} and libc reads {through_libc:?}",
    );
}

#[test]
fn republishing_carries_the_session_shift_to_a_reader_of_the_page() {
    if !platform::HAS_TIMEKEEP_PAGE || counter::basis().is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let session = session_in(tmp.path(), |setup| {
        setup.realtime.shift = Nanos(-2 * 3_600 * Nanos::SECOND);
    });
    let page = session.timekeep().unwrap();
    let basis = counter::basis().unwrap();

    republish(session.header(), page, basis).expect("the page is republished");
    let through_page = page.timekeep.reading(basis, true).unwrap();
    let through_seqlock = crate::session::clock_now(session.header(), clock::REALTIME).unwrap();
    assert!(
        (through_page.0 - through_seqlock.0).abs() < Nanos::SECOND,
        "a page reader sees {through_page:?} and a libc reader {through_seqlock:?}",
    );
    // Two hours back, which is what the session was set up with.
    let genuine = platform::genuine_nanos(clock::REALTIME).unwrap();
    assert!(genuine.0 - through_page.0 > 3_600 * Nanos::SECOND);
}

// The continuity rule: a swap-published linear clock is continuous at its swap instant, and what
// that leaves drains forward. Every case below is the arithmetic alone, so it runs wherever the
// suite does.

/// The cadence a session republishes at by default, which is what the drain aims at.
const INTERVAL: Nanos = Nanos(1_000_000);

/// A session showing these two values, at this rate, having stepped this many times.
fn view_at(realtime: Nanos, elapsed: Nanos, rate: Rate, epoch: u64) -> View {
    View {
        realtime,
        elapsed,
        rate,
        epoch,
    }
}

/// What the page shows for both clocks at some instant.
const fn shown_at(realtime: Nanos, elapsed: Nanos) -> Shown {
    Shown { realtime, elapsed }
}

#[test]
fn a_page_nobody_has_published_takes_the_sessions_own_values() {
    let view = view_at(
        Nanos(1_700_000_000 * Nanos::SECOND),
        Nanos(60 * Nanos::SECOND),
        Rate::from_f64(4.0).unwrap(),
        0,
    );
    let next = continuous(&view, None, false, INTERVAL);
    assert_eq!(next.realtime, view.realtime);
    assert_eq!(next.elapsed, view.elapsed);
    // Nothing to be continuous with, so nothing to slew away either.
    assert_eq!(next.rate, view.rate);
}

#[test]
fn a_page_behind_the_session_snaps_forward_onto_its_line() {
    let view = view_at(
        Nanos(1_700_000_000 * Nanos::SECOND),
        Nanos(60 * Nanos::SECOND),
        Rate::UNITY,
        7,
    );
    let behind = shown_at(
        Nanos(view.realtime.0 - 5 * Nanos::SECOND),
        Nanos(view.elapsed.0 - 5 * Nanos::SECOND),
    );
    let next = continuous(&view, Some(behind), false, INTERVAL);
    assert_eq!(next.realtime, view.realtime, "the wall clock did not snap");
    assert_eq!(next.elapsed, view.elapsed, "the elapsed clock did not snap");
    // A forward step is allowed, so there is no lead to give back and the rate is the session's.
    assert_eq!(next.rate, view.rate);
}

#[test]
fn a_page_ahead_keeps_its_own_value_and_runs_under_the_sessions_rate() {
    let view = view_at(
        Nanos(1_700_000_000 * Nanos::SECOND),
        Nanos(60 * Nanos::SECOND),
        Rate::UNITY,
        7,
    );
    let lead = 4 * INTERVAL.0;
    let ahead = shown_at(Nanos(view.realtime.0 + lead), Nanos(view.elapsed.0 + lead));
    let next = continuous(&view, Some(ahead), false, INTERVAL);
    assert_eq!(next.realtime, ahead.realtime, "the page stepped back");
    assert_eq!(next.elapsed, ahead.elapsed, "the page stepped back");
    assert!(
        next.rate < view.rate,
        "a page ahead published the session's own rate, {:?}",
        next.rate,
    );
    // Under, but never backwards and never by more than the slew allows.
    assert!(next.rate.0 >= view.rate.0 - view.rate.0 / SLEW_DIVISOR);
}

#[test]
fn a_lead_no_slew_could_give_back_at_once_comes_back_at_the_rate_the_slew_allows() {
    let rate = Rate::from_f64(2.0).unwrap();
    let view = view_at(Nanos(Nanos::SECOND), Nanos(Nanos::SECOND), rate, 0);
    // An hour of lead, which no rate under the session's could give back inside one interval.
    let ahead = shown_at(Nanos(view.realtime.0 + 3_600 * Nanos::SECOND), view.elapsed);
    let next = continuous(&view, Some(ahead), false, INTERVAL);
    assert_eq!(next.rate.0, rate.0 - rate.0 / SLEW_DIVISOR);
    assert!(next.rate.0 > 0, "the page stopped instead of slewing");
}

#[test]
fn a_frozen_session_holds_the_lead_until_it_moves_forward_again() {
    let frozen = view_at(
        Nanos(1_700_000_000 * Nanos::SECOND),
        Nanos(60 * Nanos::SECOND),
        Rate::FROZEN,
        3,
    );
    let ahead = shown_at(
        Nanos(frozen.realtime.0 + Nanos::SECOND),
        Nanos(frozen.elapsed.0 + Nanos::SECOND),
    );
    let held = continuous(&frozen, Some(ahead), false, INTERVAL);
    assert_eq!(held.rate, Rate::FROZEN, "a frozen page moved");
    assert_eq!(held.realtime, ahead.realtime);
    assert_eq!(held.elapsed, ahead.elapsed);

    // And the lead is still there to be absorbed when the session moves again.
    let thawed = View {
        rate: Rate::UNITY,
        ..frozen
    };
    let draining = continuous(&thawed, Some(ahead), false, INTERVAL);
    assert!(draining.rate < Rate::UNITY);
}

#[test]
fn a_step_the_user_asked_for_is_followed_and_the_elapsed_clock_still_goes_forward_only() {
    let view = view_at(
        Nanos(1_600_000_000 * Nanos::SECOND),
        Nanos(60 * Nanos::SECOND),
        Rate::UNITY,
        8,
    );
    // The page is where the session was before a `set_time` moved the wall clock back a year.
    let ahead = shown_at(
        Nanos(1_700_000_000 * Nanos::SECOND),
        Nanos(view.elapsed.0 - Nanos::SECOND),
    );
    let followed = continuous(&view, Some(ahead), true, INTERVAL);
    assert_eq!(
        followed.realtime, view.realtime,
        "the wall clock refused the user's own step",
    );
    // The elapsed clock's own API promises it never goes back, so it snaps forward instead.
    assert_eq!(followed.elapsed, view.elapsed);

    // Without the bump the same values are a page ahead, and the rule holds it where it is.
    let held = continuous(&view, Some(ahead), false, INTERVAL);
    assert_eq!(held.realtime, ahead.realtime);

    // A step the elapsed clock is below is still forward only.
    let elapsed_ahead = shown_at(view.realtime, Nanos(view.elapsed.0 + Nanos::SECOND));
    let stepped = continuous(&view, Some(elapsed_ahead), true, INTERVAL);
    assert_eq!(stepped.elapsed, elapsed_ahead.elapsed);
}

/// One page and one session run forward tick by tick, which is what a heartbeat does to a lead.
///
/// The page advances at the rate it last published and the session at its own, so what comes out
/// is the series a reader of each would see: one pair per republish, the page's reading and the
/// session's own at the same instant.
fn drain(view: &mut View, mut shown: Shown, ticks: usize) -> Vec<(Shown, Shown)> {
    let mut readings = Vec::new();
    for _ in 0..ticks {
        let next = continuous(view, Some(shown), false, INTERVAL);
        readings.push((
            shown_at(next.realtime, next.elapsed),
            shown_at(view.realtime, view.elapsed),
        ));
        // A tick of the interval passes: the page moves at what it just published, and the
        // session at its own rate.
        shown = shown_at(
            Nanos(next.realtime.0 + next.rate.scale(INTERVAL.0)),
            Nanos(next.elapsed.0 + next.rate.scale(INTERVAL.0)),
        );
        view.realtime = Nanos(view.realtime.0 + view.rate.scale(INTERVAL.0));
        view.elapsed = Nanos(view.elapsed.0 + view.rate.scale(INTERVAL.0));
    }
    readings
}

#[test]
fn a_lead_drains_forward_and_the_page_never_steps_back_while_it_does() {
    let mut view = view_at(
        Nanos(1_700_000_000 * Nanos::SECOND),
        Nanos(60 * Nanos::SECOND),
        Rate::UNITY,
        0,
    );
    // What a rate of a hundred dropping to one leaves behind after one interval of lag.
    let lead = 99 * INTERVAL.0;
    let ahead = shown_at(Nanos(view.realtime.0 + lead), Nanos(view.elapsed.0 + lead));
    let readings = drain(&mut view, ahead, 2_000);

    for pair in readings.windows(2) {
        let [(before, _), (after, _)] = [pair[0], pair[1]];
        assert!(
            after.realtime >= before.realtime && after.elapsed >= before.elapsed,
            "the page stepped back from {before:?} to {after:?}",
        );
    }
    // And the lines rejoined: the last republish lands on the session's own value.
    let (page, session) = readings[readings.len() - 1];
    for gap in [
        page.realtime.0 - session.realtime.0,
        page.elapsed.0 - session.elapsed.0,
    ] {
        assert_eq!(gap, 0, "the page is still {gap} ns from the session's line");
    }
    // The lead came back forward, a piece at a time, rather than all at once at the end.
    let (early, session) = readings[1];
    assert!(early.realtime.0 - session.realtime.0 < lead);
}

#[test]
fn a_shorter_interval_leaves_a_smaller_lead_to_drain() {
    // The lag between a change landing in the seqlock and the page carrying it is a republish
    // interval, so what a rate change leaves behind is proportional to it.
    let lead_of = |interval: Nanos| {
        let from = Rate::from_f64(10.0).unwrap();
        Nanos(from.scale(interval.0) - Rate::UNITY.scale(interval.0))
    };
    let quick = lead_of(Nanos(1_000_000));
    let slow = lead_of(Nanos(20_000_000));
    assert!(slow.0 > 10 * quick.0, "{slow:?} against {quick:?}");
}

#[test]
fn a_rate_past_what_the_page_can_encode_is_made_up_in_forward_steps() {
    let basis = made_up_basis();
    // Fast enough that the page's own scale saturates, so the page cannot keep up however it is
    // published.
    let racing = Rate::MAX;
    assert_eq!(scaled(basis.scale, racing), u64::MAX);

    // The page therefore falls behind between republishes, and every republish snaps it forward
    // onto the session's line rather than leaving it there.
    let view = view_at(
        Nanos(1_700_000_000 * Nanos::SECOND),
        Nanos(60 * Nanos::SECOND),
        racing,
        0,
    );
    let behind = shown_at(
        Nanos(view.realtime.0 - Nanos::SECOND),
        Nanos(view.elapsed.0 - Nanos::SECOND),
    );
    let next = continuous(&view, Some(behind), false, INTERVAL);
    assert_eq!(next.realtime, view.realtime);
    assert_eq!(next.elapsed, view.elapsed);
    assert_eq!(scaled(basis.scale, next.rate), u64::MAX);
}

#[test]
fn one_republisher_holds_the_page_at_a_time() {
    let page = blank_page();
    let now = Nanos(10 * Nanos::SECOND);
    let held = page.take(now).expect("nobody holds a fresh page");
    // A second republisher stands aside: whoever holds it is publishing a value at least as fresh.
    assert!(page.take(now).is_none());
    drop(held);
    assert!(page.take(now).is_some(), "the page was never given back");
}

#[test]
fn a_page_held_far_longer_than_a_republish_takes_is_broken_and_taken() {
    let page = blank_page();
    // What a republisher that took the page and died leaves behind: the instant it took it, and
    // nobody to give it back.
    let taken_at = Nanos(10 * Nanos::SECOND);
    page.continuity
        .publishing
        .store(taken_at.0, Ordering::Relaxed);

    // Still within the bound, so the holder is taken to be alive and this republisher stands aside.
    let soon = Nanos(taken_at.0 + HELD_TOO_LONG.0 - 1);
    assert!(page.take(soon).is_none());

    // Past it, so whoever took it is gone: a republish is a handful of stores and nothing that
    // survived them would still be holding the page.
    let later = Nanos(taken_at.0 + HELD_TOO_LONG.0);
    assert!(page.take(later).is_some(), "a dead holder wedged the page");
}

#[test]
fn a_page_older_than_the_counter_takes_to_come_round_is_not_anchored_on() {
    let basis = made_up_basis();
    let page = blank_page();
    let now = Nanos(3_600 * Nanos::SECOND);
    page.timekeep
        .publish(&hands_at(0, Nanos::ZERO, Nanos::ZERO, basis.scale));

    // Nothing has said when the page was last published, which is what a page nobody has
    // republished carries.
    assert_eq!(page.showing(basis, 0, now), None);

    // A republish a moment ago leaves a value to anchor at.
    page.continuity
        .published_at
        .store(now.0 - 1_000_000, Ordering::Relaxed);
    assert!(page.showing(basis, 0, now).is_some());

    // One older than the counter's own wrap does not: what it shows comes from some lap of the
    // counter nobody can name.
    let period = wrap_period(basis);
    assert!(period.0 > 0, "the counter never comes round");
    page.continuity
        .published_at
        .store(now.0 - period.0, Ordering::Relaxed);
    assert_eq!(page.showing(basis, 0, now), None);
}

#[test]
fn the_counters_wrap_is_the_span_its_width_and_speed_give_it() {
    // 2^32 ticks at 2^-32 of a second each is one second.
    let a_second = wrap_period(made_up_basis());
    assert!(
        (a_second.0 - Nanos::SECOND).abs() < Nanos::SECOND / 100,
        "a four-gigahertz counter comes round in {a_second:?}",
    );
    // Half the width at the same speed is half the span.
    let narrow = wrap_period(Basis {
        mask: u32::MAX >> 1,
        ..made_up_basis()
    });
    assert!((narrow.0 - Nanos::SECOND / 2).abs() < Nanos::SECOND / 100);
}

/// The counter this machine really has, measured against its own clock.
///
/// The scale a page carries has to match the counter a reader will take, and only FreeBSD's own
/// page says what that is. Measuring it here is what lets the cases below publish a page whose
/// arithmetic a reader would agree with, wherever the suite runs. `None` on a machine whose
/// counter does not move.
fn measured_basis() -> Option<Basis> {
    let probe = Basis {
        algo: ALGO_NATIVE,
        flavour: 0,
        mask: u32::MAX,
        scale: 1 << 32,
    };
    let started = platform::genuine_nanos(clock::MONOTONIC)?;
    let from = probe.read();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let ended = platform::genuine_nanos(clock::MONOTONIC)?;
    let ticks = u128::from(probe.read().wrapping_sub(from));
    let nanos = u128::try_from(ended.0.checked_sub(started.0)?).ok()?;
    if ticks == 0 || nanos == 0 {
        return None;
    }
    // 2^64 genuine seconds per tick, which is the unit the page's scale is written in.
    let scale = u64::try_from((nanos << u64::BITS) / (ticks * SECOND_FRAC)).ok()?;
    (scale > 0).then_some(Basis { scale, ..probe })
}

/// What a rate change publishes: every family re-anchored at the **W5** instant, so the old and
/// new lines meet there and then run at the new rate.
fn at_rate(anchor: &Anchor, live: ParamsValue, rate: Rate) -> ParamsValue {
    let mut next = live;
    for family in next.lines_mut() {
        let Some(now) = anchor.of(family.timebase) else {
            continue;
        };
        family.line.fake_anchor = family.line.project(now);
        family.line.timebase_anchor = now;
        family.line.rate = rate;
    }
    next
}

/// How close two readings of the same clock, taken microseconds apart, have to be to count as the
/// same line.
const TOGETHER_NS: i64 = 500_000;

#[test]
fn a_rate_drop_the_heartbeat_alone_carries_never_steps_a_page_reader_back() {
    let Some(basis) = measured_basis() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = Setup::plain(tmp.path().join("libtimewarp"));
    let fast = Rate::from_f64(10.0).unwrap();
    for family in [
        &mut setup.realtime,
        &mut setup.monotonic,
        &mut setup.boottime,
    ] {
        family.rate = fast;
    }
    let session = create(&setup).unwrap();
    let header = session.header();
    let page = blank_page();
    republish(header, &page, basis).expect("the page is published");

    // A writer drops the rate and dies before its own republish, which is the widest window the
    // page has: from here only the heartbeat can carry the change.
    let lock = sync::begin(header, &naming::lock_path(session.path())).unwrap();
    lock.publish::<()>(header, None, |anchor, live| {
        Ok(at_rate(anchor, live, Rate::UNITY))
    })
    .unwrap();
    drop(lock);
    std::thread::sleep(std::time::Duration::from_millis(2));

    let mut last = page
        .timekeep
        .reading(basis, true)
        .expect("a reader follows it");
    let mut gaps = Vec::new();
    let mut rejoined = false;
    for _ in 0..4_000 {
        republish(header, &page, basis);
        let reading = page
            .timekeep
            .reading(basis, true)
            .expect("a reader follows it");
        assert!(
            reading >= last,
            "the page stepped back from {last:?} to {reading:?}",
        );
        last = reading;
        let through_seqlock = crate::session::clock_now(header, clock::REALTIME).unwrap();
        gaps.push(reading.0 - through_seqlock.0);
        if gaps[gaps.len() - 1] < TOGETHER_NS {
            rejoined = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    // The page really was left ahead: the lag times the rate change, which is milliseconds here.
    assert!(
        gaps[0] > 5 * 1_000_000,
        "the page was only {} ns ahead, so nothing was drained",
        gaps[0],
    );
    assert!(rejoined, "the page never came back to the session's line");
    // Forward all the way: the lead came back by running slower, never by stepping.
    for pair in gaps.windows(2) {
        assert!(pair[1] <= pair[0] + TOGETHER_NS, "the lead grew: {gaps:?}");
    }
}

#[test]
fn a_family_tracking_a_timebase_of_its_own_still_reaches_the_page() {
    let Some(basis) = measured_basis() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = Setup::plain(tmp.path().join("libtimewarp"));
    // The wall clock measures passage against the monotonic clock, which is what insulates it
    // from a step in the genuine one. The page has no room for that choice and needs none: the
    // republisher re-anchors each clock from its own line at every tick, so whichever genuine
    // clock a family tracks is absorbed there.
    setup.realtime.timebase = platform::default_timebase(Family::Monotonic).unwrap();
    let session = create(&setup).unwrap();
    let header = session.header();
    let page = blank_page();

    for _ in 0..3 {
        republish(header, &page, basis).expect("the page is republished");
        let through_page = page.timekeep.reading(basis, true).unwrap();
        let through_seqlock = crate::session::clock_now(header, clock::REALTIME).unwrap();
        assert!(
            (through_page.0 - through_seqlock.0).abs() < TOGETHER_NS,
            "a page reader sees {through_page:?} and a libc reader {through_seqlock:?}",
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
