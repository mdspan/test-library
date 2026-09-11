//! The page's shape, its publishing protocol, and the clock a reader computes from it.
//!
//! Everything here runs wherever the suite runs. The page belongs to FreeBSD, but its layout is
//! fixed by the format and its arithmetic by the reader, and neither needs that kernel to check.

use std::mem::offset_of;

use super::*;
use crate::platform;
use crate::session::{Session, Setup, create};
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

/// A zeroed page on the heap, which is what the session file hands a republisher.
fn blank_page() -> Box<Timekeep> {
    // Safety: every field of the page is an integer, so an all-zero value is a valid one.
    let page: Box<Timekeep> = unsafe { Box::new(std::mem::zeroed()) };
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
    let page = blank_page();
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
    let page = blank_page();
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
    let page = blank_page();
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

    let hands = page.load().expect("the creator published the page");
    assert_eq!(hands.algo, basis.algo);
    assert_eq!(hands.counter_mask, basis.mask);

    // And what a reader computes from it is the session's own clock, which here is the genuine one.
    let through_page = page.reading(basis, true).expect("the page reads");
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
    let through_page = page.reading(basis, true).unwrap();
    let through_seqlock = crate::session::clock_now(session.header(), clock::REALTIME).unwrap();
    assert!(
        (through_page.0 - through_seqlock.0).abs() < Nanos::SECOND,
        "a page reader sees {through_page:?} and a libc reader {through_seqlock:?}",
    );
    // Two hours back, which is what the session was set up with.
    let genuine = platform::genuine_nanos(clock::REALTIME).unwrap();
    assert!(genuine.0 - through_page.0 > 3_600 * Nanos::SECOND);
}
