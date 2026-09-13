//! The C header over the crate, driven from C: the reads, the transaction calls, and the error
//! codes, through the shared object a program links against.
//!
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test support: a failed assumption should fail the test loudly"
)]

mod fixture;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use common::platform::default_timebase;
use common::session::layout::Keeper;
use common::time::{Family, Nanos};

use fixture::{Fixture, library, one_at_a_time, repository};

const HOUR: i64 = 3_600 * Nanos::SECOND;

/// The codes `enum libtimewarp_error` declares, by the names the chapter gives them.
const OK: i64 = 0;
const NO_SESSION: i64 = 1;
const BAD_ARGUMENT: i64 = 3;
const WOULD_GO_BACKWARDS: i64 = 4;
const UNSUPPORTED_FOR_KEEPER: i64 = 5;

/// Compiles `tests/c/surface.c` against the shipped header and the shared object beside it.
fn program() -> PathBuf {
    let library = library();
    fixture::compile(
        "surface",
        &[
            format!("-I{}", repository().join("include").display()),
            library.to_string_lossy().into_owned(),
            format!("-Wl,-rpath,{}", library.parent().unwrap().display()),
        ],
    )
}

/// What the C program printed, as the `key=value` lines it prints them in.
struct Facts(BTreeMap<String, String>);

impl Facts {
    fn get(&self, key: &str) -> &str {
        self.0
            .get(key)
            .unwrap_or_else(|| panic!("no `{key}` in the program's output: {:?}", self.0))
    }

    fn number(&self, key: &str) -> i64 {
        self.get(key)
            .parse()
            .unwrap_or_else(|_| panic!("`{key}` is not a number: {}", self.get(key)))
    }

    fn rate(&self, key: &str) -> f64 {
        self.get(key)
            .parse()
            .unwrap_or_else(|_| panic!("`{key}` is not a rate: {}", self.get(key)))
    }

    /// Asserts a code, and that a message came back beside it: empty for success, and something
    /// for every refusal.
    #[track_caller]
    fn code(&self, key: &str, expected: i64) {
        assert_eq!(self.number(key), expected, "{key}");
        let message = self.get(&format!("{key}.message"));
        if expected == OK {
            assert_eq!(message, "", "{key} succeeded and still left a message");
        } else {
            assert!(!message.is_empty(), "{key} left no message");
        }
    }
}

fn run() -> Facts {
    let affine = Fixture::on(Keeper::Affine);
    let null = Fixture::on(Keeper::Null);
    let missing = affine.session_dir().join("pid-1-nothing.session");

    let output = Command::new(program())
        .arg(affine.path())
        .arg(null.path())
        .arg(&missing)
        .output()
        .expect("the C program ran");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "the C program exited {:?}\nstdout:\n{stdout}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    Facts(
        stdout
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
    )
}

#[test]
fn the_c_header_carries_the_whole_surface() {
    let _one = one_at_a_time();
    let facts = run();
    assert_eq!(facts.get("done"), "1", "the program ran to the end");
    the_struct_the_header_declares(&facts);
    attaching(&facts);
    the_reads(&facts);
    the_transaction_calls(&facts);
    the_null_keeper(&facts);
}

/// The struct the header declares is the struct the library fills in: a field that drifted would
/// otherwise read the wrong bytes and say nothing about it.
#[track_caller]
fn the_struct_the_header_declares(facts: &Facts) {
    assert_eq!(facts.number("layout.sizeof"), 80 + 4096);
    for (key, offset) in [
        ("layout.keeper", 0),
        ("layout.emulate_vdso", 4),
        ("layout.fail_closed", 8),
        ("layout.creator_pid", 12),
        ("layout.creation_time", 16),
        ("layout.fault_code", 24),
        ("layout.fault_pid", 28),
        ("layout.rate", 32),
        ("layout.timebase", 64),
        ("layout.hook_path", 80),
    ] {
        assert_eq!(facts.number(key), offset, "{key}");
    }
}

/// A path holding nothing, a pointer where a path should be, and a session.
#[track_caller]
fn attaching(facts: &Facts) {
    facts.code("missing.status", NO_SESSION);
    facts.code("null_path.status", BAD_ARGUMENT);
    facts.code("attach.status", OK);
}

#[track_caller]
fn the_reads(facts: &Facts) {
    facts.code("info.status", OK);
    assert_eq!(
        facts.number("info.keeper"),
        i64::from(Keeper::Affine as u32)
    );
    assert_eq!(
        facts.number("info.creator_pid"),
        i64::from(std::process::id())
    );
    assert_eq!(facts.number("info.fault_code"), 0);
    assert_eq!(facts.get("info.hook_path"), "");
    for family in 0..4 {
        assert!((facts.rate(&format!("info.rate.{family}")) - 1.0).abs() < 1e-9);
    }
    // Each family's first clock, asked for rather than spelled: FreeBSD picks between two columns
    // of the family table by the running kernel's version.
    for (index, family) in [Family::Realtime, Family::Monotonic, Family::Boottime]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            facts.number(&format!("info.timebase.{index}")),
            i64::from(default_timebase(family).expect("every family but Cpu tracks a clock"))
        );
    }
    // Cpu tracks no timebase at all, and its slot is zero.
    assert_eq!(facts.number("info.timebase.3"), 0);

    for family in ["realtime", "monotonic", "boottime"] {
        facts.code(&format!("get.{family}.status"), OK);
        assert!(facts.number(&format!("get.{family}")) > 0, "{family}");
    }
    facts.code("get.cpu.status", BAD_ARGUMENT);
    facts.code("get.nofamily.status", BAD_ARGUMENT);
    facts.code("get.null_out.status", BAD_ARGUMENT);
}

/// Staging writes nothing, the commit publishes the whole of it, an abort publishes none of it,
/// and each refusal lands where the chapter says.
#[track_caller]
fn the_transaction_calls(facts: &Facts) {
    facts.code("begin.status", OK);
    facts.code("shift.status", OK);
    facts.code("set_rate.status", OK);
    facts.code("commit.status", OK);
    let staged = facts.number("staged.realtime");
    let committed = facts.number("committed.realtime");
    assert!(
        (committed - staged - HOUR).abs() < Nanos::SECOND,
        "the commit moved Realtime by {} rather than an hour",
        committed - staged,
    );
    assert!((facts.rate("committed.rate.monotonic") - 2.0).abs() < 1e-9);

    facts.code("begin2.status", OK);
    facts.code("set_time.status", OK);
    let aborted = facts.number("aborted.realtime");
    assert!(
        (aborted - committed).abs() < Nanos::SECOND,
        "the abort published something",
    );

    facts.code("begin3.status", OK);
    facts.code("cpu_rate.status", BAD_ARGUMENT);
    facts.code("negative_rate.status", BAD_ARGUMENT);
    facts.code("begin4.status", OK);
    facts.code("backwards.status", OK);
    facts.code("backwards.commit.status", WOULD_GO_BACKWARDS);
}

/// A session on the null keeper opens, reads, reports, and refuses every change.
#[track_caller]
fn the_null_keeper(facts: &Facts) {
    facts.code("null.attach.status", OK);
    facts.code("null.info.status", OK);
    assert_eq!(
        facts.number("null.info.keeper"),
        i64::from(Keeper::Null as u32)
    );
    facts.code("null.get.status", OK);
    assert!(facts.number("null.get.realtime") > 0);
    facts.code("null.begin.status", UNSUPPORTED_FOR_KEEPER);
}
