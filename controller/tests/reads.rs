//! What a controller reads out of a session: what each family shows now, and how the session was
//! set up.
//!
//! Creating a session reads a genuine clock and the system's randomness, so these cases need a
//! filled-in platform arm.
#![cfg(any(target_os = "linux", target_os = "freebsd"))]
#![expect(
    dead_code,
    reason = "test support: not every helper of the shared fixture is used by every case"
)]
#![expect(
    clippy::significant_drop_tightening,
    reason = "a transaction is consumed by `commit` or `abort`, which is where its lock is \
              released; the lint does not see through a method taking `self` and suggests a drop \
              that would not compile"
)]

mod fixture;

use common::platform::{clock, default_timebase};
use common::session::layout::{Fault, FaultCode, Keeper};
use common::session::{self, Setup};
use common::time::{Family, Nanos, Rate};
use controller::{Code, Controller, Error};

use fixture::{HOUR, SECOND, fixture, fixture_on, genuine, one_at_a_time, timebase_of};

/// How far apart two readings of the same clock taken a few instructions apart may be before the
/// answer is something other than "now". Generous: the budget is what a failure runs out of.
const A_MOMENT: i64 = SECOND;

#[track_caller]
fn near(value: Nanos, expected: Nanos) {
    assert!(
        (value.0 - expected.0).abs() < A_MOMENT,
        "{value:?} is not within a moment of {expected:?}",
    );
}

#[test]
fn get_answers_what_the_family_shows_now() {
    let _one = one_at_a_time();
    let fixture = fixture();

    for (family, clock) in [
        (Family::Realtime, clock::REALTIME),
        (Family::Monotonic, timebase_of(Family::Monotonic)),
        (Family::Boottime, timebase_of(Family::Boottime)),
    ] {
        let showing = fixture.controller.get(family).unwrap();
        near(showing, fixture.showing(clock));
        // A fresh session fakes nothing yet, so every family is still at its genuine clock.
        near(showing, genuine(clock));
    }
}

#[test]
fn get_follows_a_transaction() {
    let _one = one_at_a_time();
    let fixture = fixture();

    let mut transaction = fixture.controller.begin().unwrap();
    transaction.shift(Family::Realtime, Nanos(HOUR)).unwrap();
    transaction.shift(Family::Boottime, Nanos(HOUR)).unwrap();
    transaction.commit().unwrap();

    near(
        fixture.controller.get(Family::Realtime).unwrap(),
        Nanos(genuine(clock::REALTIME).0 + HOUR),
    );
    near(
        fixture.controller.get(Family::Boottime).unwrap(),
        Nanos(genuine(timebase_of(Family::Boottime)).0 + HOUR),
    );
    // Untouched, so still on its own genuine clock.
    near(
        fixture.controller.get(Family::Monotonic).unwrap(),
        genuine(timebase_of(Family::Monotonic)),
    );
}

#[test]
fn cpu_has_no_single_value_it_shows_now() {
    let _one = one_at_a_time();
    let fixture = fixture();

    let refused = fixture.controller.get(Family::Cpu).unwrap_err();
    assert_eq!(refused.code(), Code::BadArgument);
    assert!(
        refused.to_string().contains("Cpu"),
        "the message says which family: {refused}"
    );
}

#[test]
fn info_reports_how_the_session_was_set_up() {
    let _one = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    let setup = Setup {
        emulate_vdso: false,
        fail_closed: true,
        cpu_rate: Rate::from_f64(3.0).unwrap(),
        ..Setup::plain(dir.path().join("libtimewarp"))
    };
    let session = session::create(&setup).unwrap();
    let controller = Controller::attach(session.path()).unwrap();

    let info = controller.info();
    assert_eq!(info.keeper, Keeper::Affine);
    assert_eq!(info.hook, None, "this session was given no hook");
    assert_eq!(info.creator_pid, std::process::id());
    assert!(!info.emulate_vdso);
    assert!(info.fail_closed);
    assert_eq!(info.fault, None, "nothing went wrong here");
    near(info.creation_time, genuine(clock::REALTIME));

    for family in Family::WITH_A_TIMEBASE {
        let line = info.family(family);
        assert_eq!(line.rate, Rate::UNITY);
        assert_eq!(line.timebase, default_timebase(family));
    }
    // Cpu carries a rate of its own and tracks no timebase at all.
    assert_eq!(info.cpu.rate, Rate::from_f64(3.0).unwrap());
    assert_eq!(info.cpu.timebase, None);
}

#[test]
fn info_reports_the_rate_and_timebase_a_transaction_left() {
    let _one = one_at_a_time();
    let fixture = fixture();

    let mut transaction = fixture.controller.begin().unwrap();
    transaction.set_rate(Family::Realtime, 10.0).unwrap();
    transaction.set_rate(Family::Monotonic, 0.0).unwrap();
    transaction.commit().unwrap();

    let info = fixture.controller.info();
    assert_eq!(info.realtime.rate, Rate::from_f64(10.0).unwrap());
    assert_eq!(info.monotonic.rate, Rate::FROZEN);
    assert_eq!(info.boottime.rate, Rate::UNITY);
    // A rate change moves no timebase.
    assert_eq!(info.realtime.timebase, Some(clock::REALTIME));
    assert_eq!(
        info.monotonic.timebase,
        Some(timebase_of(Family::Monotonic))
    );
}

#[test]
fn a_null_session_reads_and_reports_and_takes_no_change() {
    let _one = one_at_a_time();
    let fixture = fixture_on(Keeper::Null);

    // Reads work, and answer the genuine clock, which is what the null keeper hands its own
    // processes.
    for (family, clock) in [
        (Family::Realtime, clock::REALTIME),
        (Family::Monotonic, timebase_of(Family::Monotonic)),
        (Family::Boottime, timebase_of(Family::Boottime)),
    ] {
        near(fixture.controller.get(family).unwrap(), genuine(clock));
    }
    assert_eq!(
        fixture.controller.get(Family::Cpu).unwrap_err().code(),
        Code::BadArgument
    );

    // So does info, and the fault slot with it.
    let info = fixture.controller.info();
    assert_eq!(info.keeper, Keeper::Null);
    assert_eq!(info.creator_pid, std::process::id());
    assert_eq!(info.fault, None);
    assert_eq!(info.realtime.rate, Rate::UNITY);
    assert_eq!(info.realtime.timebase, Some(clock::REALTIME));

    // Every change goes through a transaction, and there is no transaction to be had.
    let refused = fixture.controller.begin().unwrap_err();
    assert_eq!(refused.code(), Code::UnsupportedForKeeper);
    assert!(matches!(refused, Error::UnsupportedForKeeper));
}

#[test]
fn the_fault_slot_of_a_null_session_reads_through_info() {
    let _one = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    let fault = Fault::new(FaultCode::BadValue, 4321);
    let session = session::create_faulted(
        &Setup {
            keeper: Keeper::Null,
            ..Setup::plain(dir.path().join("libtimewarp"))
        },
        fault,
    )
    .unwrap();

    let info = Controller::attach(session.path()).unwrap().info();
    assert_eq!(info.keeper, Keeper::Null);
    assert_eq!(info.fault, Some(fault));
    assert_eq!(info.fault.unwrap().pid, 4321);
}
