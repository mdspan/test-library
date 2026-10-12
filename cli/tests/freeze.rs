//! `freeze` and `thaw`: the rate a freeze returns to, kept in a state file of the tool's own.
#![expect(
    clippy::expect_used,
    dead_code,
    reason = "test support: a failed assumption should fail the test loudly, and not every helper \
              is used by every case"
)]

mod fixture;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use common::session::layout::Keeper;
use controller::Controller;

use fixture::{Fixture, Run, Tool, one_at_a_time};

/// A session and a state directory of the case's own.
struct Case {
    session: Fixture,
    state: tempfile::TempDir,
}

impl Case {
    fn on(keeper: Keeper) -> Self {
        Self {
            session: Fixture::on(keeper),
            state: tempfile::tempdir().expect("a state directory"),
        }
    }

    /// The tool with this case's session directory and state directory.
    fn tool(&self) -> Tool {
        self.session
            .tool()
            .env("LIBTIMEWARP_STATE_DIR", self.state.path())
    }

    /// Runs the tool with `--session` naming the case's file, then `args`.
    fn run(&self, args: &[&str]) -> Run {
        let mut all = vec![OsStr::new("--session"), self.session.path().as_os_str()];
        all.extend(args.iter().map(OsStr::new));
        self.tool().run(&all)
    }

    /// Where the rates of this case's session are remembered.
    fn state_file(&self) -> PathBuf {
        self.state.path().join("frozen").join(
            self.session
                .path()
                .file_name()
                .expect("a session file has a name"),
        )
    }

    fn rate(&self, label: &str) -> String {
        self.run(&["info"])
            .ok()
            .get(&format!("{label}.rate"))
            .to_owned()
    }
}

#[track_caller]
fn assert_empty(dir: &Path) {
    let found: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|found| found.map(|entry| entry.expect("an entry").path()).collect())
        .unwrap_or_default();
    assert!(found.is_empty(), "left in {}: {found:?}", dir.display());
}

#[test]
fn the_tool_remembers_the_rate_a_freeze_comes_back_to() {
    let _one = one_at_a_time();
    let case = Case::on(Keeper::Affine);

    case.run(&["set-rate", "realtime", "7"]).ok();
    let frozen = case.run(&["freeze", "realtime"]).ok();
    assert_eq!(frozen.get("realtime.rate"), "0.000000000");
    assert_eq!(frozen.get("realtime.remembered"), "7.000000000");
    assert_eq!(
        case.rate("realtime"),
        "0.000000000",
        "the freeze did not land"
    );

    let thawed = case.run(&["thaw", "realtime"]).ok();
    assert_eq!(thawed.get("realtime.rate"), "7.000000000");

    case.run(&["thaw", "realtime"])
        .refused(1, "no remembered rate");
}

#[test]
fn a_freeze_with_no_family_named_stops_the_three_that_have_a_line() {
    let _one = one_at_a_time();
    let case = Case::on(Keeper::Affine);

    let frozen = case.run(&["freeze"]).ok();
    assert_eq!(
        frozen.keys(),
        [
            "realtime.rate",
            "realtime.remembered",
            "monotonic.rate",
            "monotonic.remembered",
            "boottime.rate",
            "boottime.remembered",
        ]
    );
    for label in ["realtime", "monotonic", "boottime"] {
        assert_eq!(case.rate(label), "0.000000000", "{label}");
        assert_eq!(frozen.get(&format!("{label}.remembered")), "1.000000000");
    }
    assert_eq!(case.rate("cpu"), "1.000000000");

    let thawed = case.run(&["thaw"]).ok();
    assert_eq!(
        thawed.keys(),
        ["realtime.rate", "monotonic.rate", "boottime.rate"]
    );
    for label in ["realtime", "monotonic", "boottime"] {
        assert_eq!(case.rate(label), "1.000000000", "{label}");
    }
}

#[test]
fn freezing_twice_keeps_the_first_rate() {
    let _one = one_at_a_time();
    let case = Case::on(Keeper::Affine);

    case.run(&["set-rate", "realtime", "7"]).ok();
    case.run(&["freeze", "realtime"]).ok();
    let again = case.run(&["freeze", "realtime"]).ok();
    assert_eq!(again.get("realtime.remembered"), "7.000000000");

    let thawed = case.run(&["thaw", "realtime"]).ok();
    assert_eq!(thawed.get("realtime.rate"), "7.000000000");
}

#[test]
fn a_thaw_with_nothing_remembered_leaves_the_rates_alone() {
    let _one = one_at_a_time();
    let case = Case::on(Keeper::Affine);

    case.run(&["set-rate", "realtime", "3"]).ok();
    let before = case.run(&["info"]).ok().stdout;
    let refused = case
        .run(&["thaw", "realtime", "monotonic"])
        .refused(1, "no remembered rate for realtime, monotonic");
    assert!(refused.stderr.contains("set-rate"), "{}", refused.stderr);
    assert_eq!(refused.stdout, "");
    assert_eq!(case.run(&["info"]).ok().stdout, before);
}

#[test]
fn a_thaw_needs_every_family_it_names_remembered() {
    let _one = one_at_a_time();
    let case = Case::on(Keeper::Affine);

    case.run(&["freeze", "realtime"]).ok();
    case.run(&["thaw", "realtime", "monotonic"])
        .refused(1, "no remembered rate for monotonic");
    assert_eq!(case.rate("realtime"), "0.000000000");
    assert!(case.state_file().exists());
}

#[test]
fn a_thaw_forgets_what_it_put_back_and_the_file_goes_with_the_last_entry() {
    let _one = one_at_a_time();
    let case = Case::on(Keeper::Affine);

    case.run(&["set-rate", "monotonic", "0.25"]).ok();
    case.run(&["freeze", "realtime", "monotonic"]).ok();
    let text = std::fs::read_to_string(case.state_file()).expect("the state file");
    assert_eq!(text, "realtime=1\nmonotonic=0.25\n");

    case.run(&["thaw", "monotonic"]).ok();
    let text = std::fs::read_to_string(case.state_file()).expect("the state file");
    assert_eq!(text, "realtime=1\n");
    assert_eq!(case.rate("monotonic"), "0.250000000");

    case.run(&["thaw", "realtime"]).ok();
    assert!(!case.state_file().exists());
    assert_empty(&case.state.path().join("frozen"));
}

#[test]
fn a_freeze_by_relative_path_is_thawed_through_newest() {
    let _one = one_at_a_time();
    let case = Case::on(Keeper::Affine);
    let name = case
        .session
        .path()
        .file_name()
        .expect("a session file has a name");

    case.run(&["set-rate", "realtime", "5"]).ok();
    let relative = Path::new(".").join(name);
    case.tool()
        .within(&case.session.session_dir())
        .run(&[
            OsStr::new("--session"),
            relative.as_os_str(),
            OsStr::new("freeze"),
            OsStr::new("realtime"),
        ])
        .ok();
    assert_eq!(case.rate("realtime"), "0.000000000");

    let thawed = case.tool().run(&["--newest", "thaw", "realtime"]).ok();
    assert_eq!(thawed.get("realtime.rate"), "5.000000000");
}

#[test]
fn a_thaw_puts_back_the_exact_rate() {
    let _one = one_at_a_time();
    let case = Case::on(Keeper::Affine);
    let rate = || {
        Controller::attach(case.session.path())
            .expect("the session attaches")
            .info()
            .realtime
            .rate
    };

    case.run(&["set-rate", "realtime", "0.1"]).ok();
    let before = rate();
    case.run(&["freeze"]).ok();
    case.run(&["thaw"]).ok();
    assert_eq!(rate(), before);
}

#[test]
fn a_freeze_on_a_null_session_is_refused_and_remembers_nothing() {
    let _one = one_at_a_time();
    let case = Case::on(Keeper::Null);

    case.run(&["freeze"]).refused(1, "unsupported_for_keeper");
    assert_empty(case.state.path());
}

#[test]
fn a_freeze_of_cpu_is_refused_and_remembers_nothing() {
    let _one = one_at_a_time();
    let case = Case::on(Keeper::Affine);

    let refused = case
        .run(&["freeze", "realtime", "cpu"])
        .refused(1, "bad_argument");
    assert_eq!(refused.stdout, "");
    assert_eq!(case.rate("realtime"), "1.000000000");
    assert!(!case.state_file().exists());
}

#[test]
fn an_unknown_family_is_refused_before_the_session_is_touched() {
    let _one = one_at_a_time();
    let case = Case::on(Keeper::Affine);

    case.run(&["freeze", "wallclock"])
        .refused(1, "no family called");
    assert_eq!(case.rate("realtime"), "1.000000000");
    assert_empty(case.state.path());
}

#[test]
fn a_state_file_that_does_not_parse_is_refused() {
    let _one = one_at_a_time();
    let case = Case::on(Keeper::Affine);
    std::fs::create_dir_all(case.state.path().join("frozen")).expect("the frozen directory");
    std::fs::write(case.state_file(), "realtime=fast\n").expect("a broken state file");

    case.run(&["freeze", "realtime"]).refused(1, "line 1");
    assert_eq!(case.rate("realtime"), "1.000000000");
}
