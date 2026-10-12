//! The sessions a case points the tool at, and one run of the tool.
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "test support: a failed assumption should fail the test loudly"
)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};

use common::session::layout::{Fault, Keeper};
use common::session::{self, Session, Setup};

/// A case that creates a session takes this: the lock file a reader probes is named once per
/// process, so two sessions at once would cross their probes.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

pub(crate) fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A directory of our own with one session in it, kept mapped so the file stays whole.
pub(crate) struct Fixture {
    _dir: tempfile::TempDir,
    pub(crate) session: Session,
}

impl Fixture {
    pub(crate) fn on(keeper: Keeper) -> Self {
        Self::build(keeper, None)
    }

    /// A session whose creator recorded `fault` before publishing it.
    pub(crate) fn faulted(keeper: Keeper, fault: Fault) -> Self {
        Self::build(keeper, Some(fault))
    }

    fn build(keeper: Keeper, fault: Option<Fault>) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory");
        // These cases are not about posing as the vDSO, so it is off.
        let setup = Setup {
            keeper,
            emulate_vdso: false,
            ..Setup::plain(dir.path().join("libtimewarp"))
        };
        let session = fault
            .map_or_else(
                || session::create(&setup),
                |fault| session::create_faulted(&setup, fault),
            )
            .expect("a session of our own");
        Self { _dir: dir, session }
    }

    pub(crate) fn path(&self) -> &Path {
        self.session.path()
    }

    /// Runs the tool with `--session` naming this fixture's file, then `args`.
    pub(crate) fn run(&self, args: &[&str]) -> Run {
        let mut all = vec![OsStr::new("--session"), self.path().as_os_str()];
        all.extend(args.iter().map(OsStr::new));
        run(&all)
    }
}

/// One run of the binary under test, with `args` after the program name.
pub(crate) fn run<S: AsRef<OsStr>>(args: &[S]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_libtimewarp"))
        .args(args)
        .output()
        .expect("the tool ran");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    Run {
        facts: stdout
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
        stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status.code().unwrap_or(-1),
    }
}

/// What one run left behind.
pub(crate) struct Run {
    pub(crate) facts: BTreeMap<String, String>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) status: i32,
}

impl Run {
    #[track_caller]
    pub(crate) fn ok(self) -> Self {
        assert_eq!(
            self.status, 0,
            "the tool exited {}\nstdout:\n{}\nstderr:\n{}",
            self.status, self.stdout, self.stderr
        );
        self
    }

    /// Asserts the run exited `status` with `needle` in its stderr.
    #[track_caller]
    pub(crate) fn refused(self, status: i32, needle: &str) -> Self {
        assert_eq!(
            self.status, status,
            "the tool exited {}\nstdout:\n{}\nstderr:\n{}",
            self.status, self.stdout, self.stderr
        );
        assert!(
            self.stderr.contains(needle),
            "no `{needle}` in stderr:\n{}",
            self.stderr
        );
        self
    }

    #[track_caller]
    pub(crate) fn get(&self, key: &str) -> &str {
        self.facts
            .get(key)
            .unwrap_or_else(|| panic!("no `{key}` in the tool's output:\n{}", self.stdout))
    }

    #[track_caller]
    pub(crate) fn number(&self, key: &str) -> i64 {
        self.get(key)
            .parse()
            .unwrap_or_else(|_| panic!("`{key}` is not a number: {}", self.get(key)))
    }

    /// The keys in the order the tool printed them.
    pub(crate) fn keys(&self) -> Vec<&str> {
        self.stdout
            .lines()
            .filter_map(|line| line.split_once('=').map(|(key, _)| key))
            .collect()
    }
}
