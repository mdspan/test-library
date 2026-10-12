//! The sessions a case points the tool at, and one run of the tool.
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "test support: a failed assumption should fail the test loudly"
)]

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::mem::offset_of;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};

use common::session::layout::{Fault, Header, Keeper};
use common::session::{self, Session, Setup};

/// A case that creates a session takes this: the lock file a reader probes is named once per
/// process, so two sessions at once would cross their probes.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

pub(crate) fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A directory of our own with one session in it, and any added later, kept mapped so the files
/// stay whole.
pub(crate) struct Fixture {
    dir: tempfile::TempDir,
    pub(crate) session: Session,
    others: Vec<Session>,
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
        let session = create(&dir.path().join("libtimewarp"), keeper, fault);
        Self {
            dir,
            session,
            others: Vec::new(),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        self.session.path()
    }

    /// The temporary directory: what `LIBTIMEWARP_SESSION_DIR` names to reach the sessions.
    pub(crate) fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// The directory the session files are in.
    pub(crate) fn session_dir(&self) -> PathBuf {
        self.dir().join("libtimewarp")
    }

    /// Creates one more session in the same directory and gives its path.
    pub(crate) fn another(&mut self, keeper: Keeper) -> PathBuf {
        let added = create(&self.session_dir(), keeper, None);
        let path = added.path().to_path_buf();
        self.others.push(added);
        path
    }

    /// The tool with `LIBTIMEWARP_SESSION_DIR` naming this fixture's directory.
    pub(crate) fn tool(&self) -> Tool {
        Tool::new().env("LIBTIMEWARP_SESSION_DIR", self.dir())
    }

    /// Runs the tool with `--session` naming this fixture's file, then `args`.
    pub(crate) fn run(&self, args: &[&str]) -> Run {
        let mut all = vec![OsStr::new("--session"), self.path().as_os_str()];
        all.extend(args.iter().map(OsStr::new));
        run(&all)
    }
}

fn create(dir: &Path, keeper: Keeper, fault: Option<Fault>) -> Session {
    // These cases are not about posing as the vDSO, so it is off.
    let setup = Setup {
        keeper,
        emulate_vdso: false,
        ..Setup::plain(dir.to_path_buf())
    };
    fault
        .map_or_else(
            || session::create(&setup),
            |fault| session::create_faulted(&setup, fault),
        )
        .expect("a session of our own")
}

/// Rewrites the creator pid and creation time in the header of the session file at `path`, so
/// sessions made by one process can stand for several.
pub(crate) fn restamp(path: &Path, creator_pid: u32, creation_time: i64) {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("the session file opens for writing");
    let at = |offset: usize| u64::try_from(offset).expect("an offset inside the header");
    file.write_all_at(
        &creator_pid.to_ne_bytes(),
        at(offset_of!(Header, creator_pid)),
    )
    .expect("the creator pid is written");
    file.write_all_at(
        &creation_time.to_ne_bytes(),
        at(offset_of!(Header, creation_time)),
    )
    .expect("the creation time is written");
}

/// The binary under test in an environment of the case's own: nothing inherited, so the session
/// directory and the configuration file are what the case sets.
pub(crate) struct Tool {
    env: Vec<(OsString, OsString)>,
}

impl Tool {
    /// An environment with `XDG_CONFIG_HOME` naming a directory with no configuration file in it.
    pub(crate) fn new() -> Self {
        Self { env: Vec::new() }.env("XDG_CONFIG_HOME", "/nonexistent")
    }

    /// The same environment with `name` set to `value`.
    pub(crate) fn env(mut self, name: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((name.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    /// Runs the tool with `args` after the program name.
    pub(crate) fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_libtimewarp"));
        command
            .env_clear()
            .envs(self.env.iter().cloned())
            .args(args);
        finished(&mut command)
    }
}

/// One run of the binary under test, with `args` after the program name.
pub(crate) fn run<S: AsRef<OsStr>>(args: &[S]) -> Run {
    finished(Command::new(env!("CARGO_BIN_EXE_libtimewarp")).args(args))
}

fn finished(command: &mut Command) -> Run {
    let output = command.output().expect("the tool ran");
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
