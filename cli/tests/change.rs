//! `shift`, `set-time`, `set-rate`, and several of them staged on one transaction.
#![expect(
    dead_code,
    reason = "not every helper of the fixture is used by every case"
)]

mod fixture;

use std::time::Instant;

use common::session::layout::Keeper;
use common::time::{Family, Nanos};

use fixture::{Fixture, Run, one_at_a_time, timebase_name};

const HOUR: i64 = 3_600 * Nanos::SECOND;

/// Every rate and Realtime's reading. Exact only while Realtime is frozen.
fn state(session: &Fixture) -> (Vec<String>, i64) {
    let info = session.run(&["info"]).ok();
    let rates = ["realtime", "monotonic", "boottime", "cpu"]
        .map(|label| info.get(&format!("{label}.rate")).to_owned())
        .to_vec();
    let realtime = session.run(&["get", "realtime"]).ok().number("realtime.ns");
    (rates, realtime)
}

/// A session with Realtime frozen, so its reading compares exactly.
fn frozen() -> Fixture {
    let session = Fixture::on(Keeper::Affine);
    session.run(&["set-rate", "realtime", "0"]).ok();
    session
}

#[track_caller]
fn unchanged_by(session: &Fixture, args: &[&str], status: i32, needle: &str) -> Run {
    let before = state(session);
    let run = session.run(args).refused(status, needle);
    assert_eq!(run.stdout, "", "a refused run printed");
    assert_eq!(state(session), before, "{args:?} changed the session");
    run
}

#[test]
fn the_tool_reads_and_changes_a_session() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Affine);

    let started = Instant::now();
    let before = session.run(&["get", "realtime"]).ok().number("realtime.ns");

    session.run(&["shift", "realtime", "-2h"]).ok();
    let after = session.run(&["get", "realtime"]).ok().number("realtime.ns");
    // The wall clock runs on between the two reads, by no more than the three runs took.
    let between = i64::try_from(started.elapsed().as_nanos()).expect("a short test");
    let moved = before - after;
    assert!(
        (2 * HOUR - between - Nanos::SECOND..=2 * HOUR + Nanos::SECOND).contains(&moved),
        "the shift moved the wall clock by {moved} in {between} ns rather than two hours back",
    );

    session.run(&["set-rate", "monotonic", "4"]).ok();
    let info = session.run(&["info"]).ok();
    assert_eq!(info.get("monotonic.rate"), "4.000000000");
    for (label, family) in [
        ("realtime", Family::Realtime),
        ("monotonic", Family::Monotonic),
        ("boottime", Family::Boottime),
    ] {
        assert_eq!(
            info.get(&format!("{label}.timebase")),
            timebase_name(family)
        );
    }
    assert_eq!(info.get("cpu.timebase"), "", "Cpu tracks no timebase");

    session
        .run(&["shift", "monotonic", "-1h"])
        .refused(1, "libtimewarp: would_go_backwards: ");
    session
        .run(&["get", "cpu"])
        .refused(1, "libtimewarp: bad_argument: ");
}

#[test]
fn one_operation_prints_what_it_touched() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Affine);

    let shifted = session.run(&["shift", "realtime", "-2h"]).ok();
    assert_eq!(shifted.keys(), ["realtime.ns", "realtime.iso"]);
    let shifted = session.run(&["shift", "boottime", "1s"]).ok();
    assert_eq!(shifted.keys(), ["boottime.ns", "boottime.elapsed"]);
    let set = session.run(&["set-time", "monotonic", "30d"]).ok();
    assert_eq!(set.keys(), ["monotonic.ns", "monotonic.elapsed"]);
    assert!(set.number("monotonic.ns") >= 30 * 24 * HOUR);
    let rated = session.run(&["set-rate", "boottime", "0.5"]).ok();
    assert_eq!(rated.keys(), ["boottime.rate"]);
    assert_eq!(rated.get("boottime.rate"), "0.500000000");
}

#[test]
fn a_set_time_keeps_every_nanosecond() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Affine);

    let set = session
        .run(&[
            "set-rate",
            "realtime",
            "0",
            "set-time",
            "realtime",
            "2020-01-01T00:00:00.123456789Z",
        ])
        .ok();
    assert_eq!(set.keys(), ["realtime.ns", "realtime.iso", "realtime.rate"]);
    assert_eq!(set.number("realtime.ns"), 1_577_836_800_123_456_789);
    assert_eq!(set.get("realtime.iso"), "2020-01-01T00:00:00.123456789Z");
    assert_eq!(set.get("realtime.rate"), "0.000000000");
    let got = session.run(&["get", "realtime"]).ok();
    assert_eq!(got.number("realtime.ns"), 1_577_836_800_123_456_789);
}

#[test]
fn set_time_takes_unix_seconds_and_durations() {
    let _one = one_at_a_time();
    let session = frozen();

    let at = session.run(&["set-time", "realtime", "@1577836800.5"]).ok();
    assert_eq!(at.number("realtime.ns"), 1_577_836_800_500_000_000);
    let since = session.run(&["set-time", "realtime", "1d"]).ok();
    assert_eq!(since.number("realtime.ns"), 24 * HOUR);
    assert_eq!(since.get("realtime.iso"), "1970-01-02T00:00:00.000000000Z");
}

#[test]
fn a_frozen_family_moves_by_exactly_the_shift() {
    let _one = one_at_a_time();
    let session = frozen();

    let before = session.run(&["get", "realtime"]).ok().number("realtime.ns");
    let shifted = session.run(&["shift", "realtime", ".5s"]).ok();
    assert_eq!(shifted.number("realtime.ns"), before + 500_000_000);
    let after = session.run(&["get", "realtime"]).ok().number("realtime.ns");
    assert_eq!(after, before + 500_000_000);
}

#[test]
fn a_timestamp_without_an_offset_is_refused() {
    let _one = one_at_a_time();
    let session = frozen();

    for value in ["2020-01-01T00:00:00", "2020-01-01"] {
        let run = unchanged_by(
            &session,
            &["set-time", "realtime", value],
            1,
            "libtimewarp: ",
        );
        for shape in ["RFC 3339", "@", "duration"] {
            assert!(run.stderr.contains(shape), "no `{shape}` in {}", run.stderr);
        }
    }
}

#[test]
fn values_that_do_not_parse_change_nothing() {
    let _one = one_at_a_time();
    let session = frozen();

    unchanged_by(&session, &["shift", "realtime", "2x"], 1, "duration");
    unchanged_by(&session, &["set-rate", "realtime", "fast"], 1, "rate");
    unchanged_by(
        &session,
        &["shift", "realtime", "1h", "set-rate", "monotonic", "fast"],
        1,
        "rate",
    );
    unchanged_by(
        &session,
        &["shift", "realtime", "1h", "shift", "wallclock", "1h"],
        1,
        "realtime, monotonic, boottime, cpu",
    );
}

#[test]
fn a_refused_operation_applies_none_of_the_sequence() {
    let _one = one_at_a_time();
    let session = frozen();

    unchanged_by(
        &session,
        &["shift", "realtime", "1h", "shift", "monotonic", "-1d"],
        1,
        "libtimewarp: would_go_backwards: ",
    );
    unchanged_by(
        &session,
        &["shift", "realtime", "1h", "shift", "cpu", "1h"],
        1,
        "libtimewarp: bad_argument: ",
    );
    for rate in ["-1", "NaN", "inf", "1e30"] {
        unchanged_by(
            &session,
            &["shift", "realtime", "1h", "set-rate", "monotonic", rate],
            1,
            "libtimewarp: bad_argument: ",
        );
    }
}

#[test]
fn rates_of_a_sequence_print_in_the_order_given() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Affine);

    let run = session
        .run(&["set-rate", "realtime", "2", "set-rate", "monotonic", "3"])
        .ok();
    assert_eq!(run.keys(), ["realtime.rate", "monotonic.rate"]);
    assert_eq!(run.get("realtime.rate"), "2.000000000");
    assert_eq!(run.get("monotonic.rate"), "3.000000000");
    let info = session.run(&["info"]).ok();
    assert_eq!(info.get("realtime.rate"), "2.000000000");
    assert_eq!(info.get("monotonic.rate"), "3.000000000");
}

#[test]
fn each_family_prints_once_where_it_first_appears() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Affine);

    let run = session
        .run(&[
            "set-rate",
            "monotonic",
            "2",
            "shift",
            "realtime",
            "-2h",
            "set-rate",
            "realtime",
            "10",
            "shift",
            "monotonic",
            "1s",
            "shift",
            "realtime",
            "1m",
        ])
        .ok();
    assert_eq!(
        run.keys(),
        [
            "monotonic.ns",
            "monotonic.elapsed",
            "monotonic.rate",
            "realtime.ns",
            "realtime.iso",
            "realtime.rate",
        ]
    );
    assert_eq!(run.get("realtime.rate"), "10.000000000");
    assert_eq!(run.get("monotonic.rate"), "2.000000000");
}

#[test]
fn a_malformed_sequence_is_a_usage_error() {
    let _one = one_at_a_time();
    let session = frozen();

    unchanged_by(&session, &["shift", "realtime"], 2, "usage:");
    unchanged_by(&session, &["set-rate"], 2, "usage:");
    unchanged_by(
        &session,
        &["shift", "realtime", "1h", "set-rate", "monotonic"],
        2,
        "set-rate",
    );
    unchanged_by(
        &session,
        &["shift", "realtime", "1h", "get", "monotonic", "1"],
        2,
        "get",
    );
    unchanged_by(
        &session,
        &[
            "shift",
            "realtime",
            "1h",
            "shift",
            "monotonic",
            "1h",
            "extra",
        ],
        2,
        "extra",
    );
}

#[test]
fn a_null_session_refuses_a_change() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Null);

    let run = session
        .run(&["shift", "realtime", "1h"])
        .refused(1, "libtimewarp: unsupported_for_keeper: ");
    assert_eq!(run.stdout, "");
}
