//! The Python tool over the C library: finding a session, reading it, changing it, and the freeze
//! it remembers a rate for, which is the tool's job rather than the library's.
//!
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "test support: a failed assumption should fail the test loudly"
)]
#![expect(
    clippy::print_stderr,
    reason = "a machine with no interpreter says the tool went unexercised, rather than passing \
              quietly"
)]

mod fixture;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use common::platform;
use common::session::layout::Keeper;
use common::time::{Family, Nanos};

use fixture::{Fixture, library, one_at_a_time, repository};

const HOUR: i64 = 3_600 * Nanos::SECOND;

/// Whether this machine has an interpreter to run the tool with.
fn python() -> Option<PathBuf> {
    let found = Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match found {
        Ok(status) if status.success() => Some(PathBuf::from("python3")),
        _ => {
            // A machine with no interpreter checks nothing here rather than failing the suite.
            eprintln!("no python3 on this machine, so the Python tool went unexercised");
            None
        }
    }
}

/// One run of the tool: the arguments after `python3 -m libtimewarp`, in a state directory of the
/// case's own so a freeze remembers nothing across cases.
struct Tool {
    state: PathBuf,
    session_dir_parent: PathBuf,
}

impl Tool {
    fn run(&self, args: &[&str]) -> Run {
        let Some(python) = python() else {
            return Run::skipped();
        };
        let output = Command::new(python)
            .args(["-m", "libtimewarp"])
            .args(args)
            .env("PYTHONPATH", repository().join("python"))
            .env("LIBTIMEWARP_CONTROLLER_LIB", library())
            .env("LIBTIMEWARP_STATE_DIR", &self.state)
            .env("LIBTIMEWARP_SESSION_DIR", &self.session_dir_parent)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .output()
            .expect("the Python tool ran");
        Run {
            facts: String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.split_once('='))
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            status: output.status.code().unwrap_or(-1),
            skipped: false,
        }
    }
}

struct Run {
    facts: BTreeMap<String, String>,
    stdout: String,
    stderr: String,
    status: i32,
    skipped: bool,
}

impl Run {
    const fn skipped() -> Self {
        Self {
            facts: BTreeMap::new(),
            stdout: String::new(),
            stderr: String::new(),
            status: 0,
            skipped: true,
        }
    }

    #[track_caller]
    fn ok(self) -> Self {
        assert_eq!(
            self.status, 0,
            "the tool exited {}\nstdout:\n{}\nstderr:\n{}",
            self.status, self.stdout, self.stderr
        );
        self
    }

    #[track_caller]
    fn get(&self, key: &str) -> &str {
        self.facts
            .get(key)
            .unwrap_or_else(|| panic!("no `{key}` in the tool's output:\n{}", self.stdout))
    }

    #[track_caller]
    fn number(&self, key: &str) -> i64 {
        self.get(key)
            .parse()
            .unwrap_or_else(|_| panic!("`{key}` is not a number: {}", self.get(key)))
    }
}

/// A child carrying the path of a session in its environment, the way every process of a faked
/// tree does. Killed when the case is done with it.
struct Member(Child);

impl Member {
    fn of(path: &Path) -> Self {
        Self(
            Command::new(fixture::compile("member", &[]))
                .env("LIBTIMEWARP__SHARED_FILE", path)
                .stdout(Stdio::null())
                .spawn()
                .expect("a process to read the environment of"),
        )
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Member {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

/// The name the family table gives a family's default timebase here: the first of the names its id
/// goes by, since FreeBSD spells some ids twice.
fn timebase_name(family: Family) -> &'static str {
    let id = platform::default_timebase(family).expect("a family with a timebase");
    platform::clock::named()
        .iter()
        .find(|&&(_, known)| known == id)
        .map(|&(name, _)| name)
        .expect("a name for every default timebase")
}

fn tool(session: &Fixture) -> Tool {
    let state = session.dir.path().join("tool-state");
    Tool {
        state,
        session_dir_parent: session.dir.path().to_path_buf(),
    }
}

#[test]
fn the_tool_finds_a_session_through_a_process_of_the_tree() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Affine);
    let tool = tool(&session);
    let member = Member::of(session.path());

    let run = tool.run(&["--pid", &member.pid().to_string(), "info"]);
    if run.skipped {
        return;
    }
    let run = run.ok();
    assert_eq!(run.get("session"), session.path().to_string_lossy());
    assert_eq!(run.get("keeper"), "affine");
    assert_eq!(run.number("creator-pid"), i64::from(std::process::id()));
}

#[test]
fn the_tool_finds_a_session_by_reading_the_directory() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Affine);
    let tool = tool(&session);

    let listed = tool.run(&["list"]);
    if listed.skipped {
        return;
    }
    let listed = listed.ok();
    assert_eq!(listed.number("count"), 1);
    assert_eq!(
        listed.get("directory"),
        session.session_dir().to_string_lossy()
    );
    // Telling one file from another is the creator pid and the creation time the header carries.
    assert_eq!(
        listed.number("session.0.creator-pid"),
        i64::from(std::process::id())
    );
    assert!(listed.number("session.0.created-ns") > 0);
    assert_eq!(listed.get("session.0.keeper"), "affine");
    assert_eq!(listed.get("session.0.fault"), "");

    // With one session in the directory, a command with no selection acts on it; naming the
    // creator picks the same one.
    let chosen = tool.run(&["info"]).ok();
    assert_eq!(chosen.get("session"), session.path().to_string_lossy());
    let by_creator = tool
        .run(&["--creator", &std::process::id().to_string(), "info"])
        .ok();
    assert_eq!(by_creator.get("session"), session.path().to_string_lossy());
}

#[test]
fn the_tool_reads_and_changes_a_session() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Affine);
    let tool = tool(&session);

    let started = Instant::now();
    let before = tool.run(&["get", "realtime"]);
    if before.skipped {
        return;
    }
    let before = before.ok().number("realtime.ns");

    tool.run(&["shift", "realtime", "-2h"]).ok();
    let after = tool.run(&["get", "realtime"]).ok().number("realtime.ns");
    // The wall clock runs on between the two reads, by no more than the three runs took.
    let between = i64::try_from(started.elapsed().as_nanos()).unwrap();
    let moved = before - after;
    assert!(
        (2 * HOUR - between - Nanos::SECOND..=2 * HOUR + Nanos::SECOND).contains(&moved),
        "the shift moved the wall clock by {moved} in {between} ns rather than two hours back",
    );

    tool.run(&["set-rate", "monotonic", "4"]).ok();
    let info = tool.run(&["info"]).ok();
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

    // Refused where the chapter says, with the code named on stderr.
    let refused = tool.run(&["shift", "monotonic", "-1h"]);
    assert_eq!(refused.status, 1);
    assert!(
        refused.stderr.contains("would_go_backwards"),
        "stderr was {}",
        refused.stderr
    );
    let cpu = tool.run(&["get", "cpu"]);
    assert_eq!(cpu.status, 1);
    assert!(
        cpu.stderr.contains("bad_argument"),
        "stderr was {}",
        cpu.stderr
    );
}

#[test]
fn the_tool_remembers_the_rate_a_freeze_comes_back_to() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Affine);
    let tool = tool(&session);

    let set = tool.run(&["set-rate", "realtime", "7"]);
    if set.skipped {
        return;
    }
    set.ok();

    let frozen = tool.run(&["freeze", "realtime"]).ok();
    assert_eq!(frozen.get("realtime.rate"), "0.000000000");
    assert_eq!(frozen.get("realtime.remembered"), "7.000000000");
    assert_eq!(
        tool.run(&["info"]).ok().get("realtime.rate"),
        "0.000000000",
        "the freeze did not land"
    );

    // The library has no memory of its own; the rate to come back to lives in the tool's state.
    let thawed = tool.run(&["thaw", "realtime"]).ok();
    assert_eq!(thawed.get("realtime.rate"), "7.000000000");

    // And a thaw with nothing remembered is a refusal rather than a guess.
    let again = tool.run(&["thaw", "realtime"]);
    assert_eq!(again.status, 1);
    assert!(
        again.stderr.contains("no remembered rate"),
        "stderr was {}",
        again.stderr
    );
}

#[test]
fn the_tool_opens_a_null_session_and_refuses_every_change_on_it() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Null);
    let tool = tool(&session);

    let info = tool.run(&["info"]);
    if info.skipped {
        return;
    }
    let info = info.ok();
    assert_eq!(info.get("keeper"), "null");
    assert_eq!(info.get("fault"), "");
    // Reads work on the null keeper, and answer the genuine clock.
    assert!(tool.run(&["get", "realtime"]).ok().number("realtime.ns") > 0);

    let refused = tool.run(&["set-rate", "realtime", "2"]);
    assert_eq!(refused.status, 1);
    assert!(
        refused.stderr.contains("unsupported_for_keeper"),
        "stderr was {}",
        refused.stderr
    );
}
