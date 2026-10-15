//! The `libtimewarp` binary, run against sessions of the suite's own.
#![expect(
    clippy::expect_used,
    dead_code,
    reason = "test support: a failed assumption should fail the test loudly, and not every helper \
              is used by every case"
)]

mod fixture;

use std::ffi::OsStr;
use std::process::{Command, Stdio};

use common::config::parse_timestamp;
use common::platform::{default_timebase, genuine_nanos};
use common::session::layout::{Fault, FaultCode, Keeper};
use common::time::{Family, Nanos};

use fixture::{Fixture, run, timebase_name};

fn genuine(family: Family) -> i64 {
    genuine_nanos(default_timebase(family).expect("a family with a timebase"))
        .expect("the genuine clock reads")
        .0
}

#[test]
fn info_reports_every_key_of_an_affine_session_in_order() {
    let session = Fixture::on(Keeper::Affine);

    let info = session.run(&["info"]).ok();
    assert_eq!(
        info.keys(),
        [
            "session",
            "keeper",
            "hook",
            "creator-pid",
            "created",
            "created-ns",
            "emulate-vdso",
            "fail-closed",
            "fault",
            "fault-pid",
            "realtime.rate",
            "realtime.timebase",
            "monotonic.rate",
            "monotonic.timebase",
            "boottime.rate",
            "boottime.timebase",
            "cpu.rate",
            "cpu.timebase",
        ]
    );
    assert_eq!(info.get("session"), session.path().to_string_lossy());
    assert_eq!(info.get("keeper"), "affine");
    assert_eq!(info.get("hook"), "");
    assert_eq!(info.number("creator-pid"), i64::from(std::process::id()));
    let created = info.number("created-ns");
    assert!(created > 0);
    assert_eq!(
        parse_timestamp(info.get("created")).ok(),
        Some(Nanos(created)),
        "`created` does not read back as `created-ns`"
    );
    assert_eq!(info.get("emulate-vdso"), "0");
    assert_eq!(info.get("fail-closed"), "0");
    assert_eq!(info.get("fault"), "");
    assert_eq!(info.get("fault-pid"), "");
    for (label, family) in [
        ("realtime", Family::Realtime),
        ("monotonic", Family::Monotonic),
        ("boottime", Family::Boottime),
    ] {
        assert_eq!(info.get(&format!("{label}.rate")), "1.000000000");
        assert_eq!(
            info.get(&format!("{label}.timebase")),
            timebase_name(family)
        );
    }
    assert_eq!(info.get("cpu.rate"), "1.000000000");
    assert_eq!(info.get("cpu.timebase"), "", "Cpu tracks no timebase");
}

#[test]
fn info_reports_the_fault_of_a_null_session() {
    let session = Fixture::faulted(Keeper::Null, Fault::new(FaultCode::BadValue, 4242));

    let info = session.run(&["info"]).ok();
    assert_eq!(info.get("keeper"), "null");
    assert_eq!(info.get("fault"), "bad_value");
    assert_eq!(info.number("fault-pid"), 4242);
}

#[test]
fn a_null_session_reads_the_genuine_clock_and_refuses_a_rate() {
    let session = Fixture::on(Keeper::Null);

    let info = session.run(&["info"]).ok();
    assert_eq!(info.get("keeper"), "null");
    assert_eq!(info.get("fault"), "");

    let before = genuine(Family::Realtime);
    let got = session.run(&["get", "realtime"]).ok().number("realtime.ns");
    let after = genuine(Family::Realtime);
    assert!(
        (before..=after).contains(&got),
        "realtime read {got}, outside [{before}, {after}]"
    );

    let refused = session
        .run(&["set-rate", "realtime", "2"])
        .refused(1, "libtimewarp: unsupported_for_keeper: ");
    assert_eq!(refused.stdout, "");
}

#[test]
fn get_answers_between_two_genuine_readings() {
    let session = Fixture::on(Keeper::Affine);

    for (label, family) in [
        ("realtime", Family::Realtime),
        ("monotonic", Family::Monotonic),
        ("boottime", Family::Boottime),
    ] {
        let before = genuine(family);
        let got = session.run(&["get", label]).ok();
        let after = genuine(family);

        assert_eq!(got.facts.len(), 2, "output was:\n{}", got.stdout);
        let nanos = got.number(&format!("{label}.ns"));
        assert!(
            (before..=after).contains(&nanos),
            "{label} read {nanos}, outside [{before}, {after}]"
        );
        if family == Family::Realtime {
            assert_eq!(
                parse_timestamp(got.get("realtime.iso")).ok(),
                Some(Nanos(nanos))
            );
        } else {
            let expected = format!("{}.{:09}s", nanos / Nanos::SECOND, nanos % Nanos::SECOND);
            assert_eq!(got.get(&format!("{label}.elapsed")), expected);
        }
    }
}

#[test]
fn get_cpu_is_the_controllers_refusal() {
    let session = Fixture::on(Keeper::Affine);

    let cpu = session
        .run(&["get", "cpu"])
        .refused(1, "libtimewarp: bad_argument: ");
    assert_eq!(cpu.stdout, "");
}

#[test]
fn an_unknown_family_names_the_four() {
    let session = Fixture::on(Keeper::Affine);

    let run = session
        .run(&["get", "wallclock"])
        .refused(1, "realtime, monotonic, boottime, cpu");
    assert!(run.stderr.starts_with("libtimewarp: "), "{}", run.stderr);
}

#[test]
fn a_path_with_no_session_is_no_session() {
    let session = Fixture::on(Keeper::Affine);
    let missing = session.path().with_file_name("nothing.session");

    run(&[
        OsStr::new("--session"),
        missing.as_os_str(),
        OsStr::new("info"),
    ])
    .refused(1, "libtimewarp: no_session: ");
}

#[test]
fn an_unknown_option_is_a_usage_error() {
    run(&["--frobnicate", "info"]).refused(2, "--frobnicate");
}

#[test]
fn a_missing_command_is_a_usage_error() {
    run::<&str>(&[]).refused(2, "usage:");
    let dir = tempfile::tempdir().expect("a temporary directory");
    run(&[OsStr::new("--session"), dir.path().as_os_str()]).refused(2, "usage:");
}

#[test]
fn an_unknown_command_and_extra_arguments_are_usage_errors() {
    run(&["--session", "x", "frobnicate"]).refused(2, "frobnicate");
    run(&["--session", "x", "get"]).refused(2, "usage:");
    run(&["--session", "x", "get", "realtime", "extra"]).refused(2, "extra");
    run(&["--session", "x", "info", "--help"]).refused(2, "--help");
    run(&["--session"]).refused(2, "--session");
}

#[test]
fn help_lists_the_commands_and_exits_zero() {
    for flag in ["--help", "-h"] {
        let help = run(&[flag]).ok();
        assert!(
            help.stdout.starts_with("usage: libtimewarp"),
            "{}",
            help.stdout
        );
        for word in [
            "list",
            "info",
            "get",
            "shift",
            "set-time",
            "set-rate",
            "freeze",
            "thaw",
            "--creator",
            "--newest",
            "--session",
            "--pid",
            "--dir",
        ] {
            assert!(
                help.stdout.contains(word),
                "no `{word}` in:\n{}",
                help.stdout
            );
        }
    }
}

#[test]
fn a_closed_stdout_ends_the_run_quietly() {
    let session = Fixture::on(Keeper::Affine);
    let (reader, writer) = std::io::pipe().expect("a pipe");
    drop(reader);

    let output = Command::new(env!("CARGO_BIN_EXE_libtimewarp"))
        .arg("--session")
        .arg(session.path())
        .arg("info")
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .expect("the tool ran");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "stderr was {stderr}");
    assert_eq!(output.status.code(), Some(0), "stderr was {stderr}");
}
