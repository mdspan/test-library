//! The sessions a case points the tool at, and one run of the tool.
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "test support: a failed assumption should fail the test loudly"
)]

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io::Read;
use std::mem::offset_of;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

use common::platform::{self, default_timebase};
use common::session::layout::{Fault, Header, Keeper};
use common::session::{self, Session, Setup};
use common::time::Family;

/// A directory of our own with one session in it, and any added later, kept mapped so the files
/// stay whole.
pub(crate) struct Fixture {
    dir: tempfile::TempDir,
    pub(crate) session: Session,
    others: Vec<Session>,
}

impl Fixture {
    pub(crate) fn on(keeper: Keeper) -> Self {
        Self::build(tempdir(""), keeper, None)
    }

    /// A session whose creator recorded `fault` before publishing it.
    pub(crate) fn faulted(keeper: Keeper, fault: Fault) -> Self {
        Self::build(tempdir(""), keeper, Some(fault))
    }

    /// A session in a directory whose path has a space in it.
    pub(crate) fn spaced(keeper: Keeper) -> Self {
        Self::build(tempdir("with space "), keeper, None)
    }

    fn build(dir: tempfile::TempDir, keeper: Keeper, fault: Option<Fault>) -> Self {
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

fn tempdir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("a temporary directory")
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

/// The name the family table gives a family's default timebase here: the first of the names its id
/// goes by, since FreeBSD spells some ids twice.
pub(crate) fn timebase_name(family: Family) -> &'static str {
    let id = default_timebase(family).expect("a family with a timebase");
    platform::clock::named()
        .iter()
        .find(|&&(_, known)| known == id)
        .map(|&(name, _)| name)
        .expect("a name for every default timebase")
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

/// Where cargo put the artifacts of this test run.
fn target_dir() -> PathBuf {
    let mut dir = std::env::current_exe().expect("test executable has a path");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir
}

/// Compiles `tests/c/<name>.c` into a program of the suite's own, with `extra` on the command line
/// beyond the source and the output. Each test binary builds to a name of its own and renames the
/// result into place, so two binaries compiling at once do not write one file.
pub(crate) fn compile(name: &str, extra: &[String]) -> PathBuf {
    let out = target_dir().join("timewarp-cli-tests");
    std::fs::create_dir_all(&out).expect("a directory for the programs under test");
    let binary = out.join(name);
    let building = out.join(format!("{name}.{}", std::process::id()));
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/c")
        .join(format!("{name}.c"));
    let status = Command::new(std::env::var("CC").as_deref().unwrap_or("cc"))
        .arg("-O0")
        .args(["-Wall", "-Wextra", "-Werror"])
        .arg("-o")
        .arg(&building)
        .arg(&source)
        .args(extra)
        .status()
        .expect("a C compiler");
    assert!(status.success(), "compiling {}", source.display());
    std::fs::rename(&building, &binary).expect("the program moves into place");
    binary
}

static MEMBER: OnceLock<PathBuf> = OnceLock::new();

/// A running process of the suite's own, `tests/c/member.c`, killed on drop.
pub(crate) struct Member(Child);

impl Member {
    /// A member whose environment carries `path` as its session file.
    pub(crate) fn of(path: &Path) -> Self {
        Self::with(Some(path.as_os_str()))
    }

    /// A member whose environment carries no session file.
    pub(crate) fn outside() -> Self {
        Self::with(None)
    }

    fn with(shared_file: Option<&OsStr>) -> Self {
        let program = MEMBER.get_or_init(|| compile("member", &[]));
        let mut command = Command::new(program);
        command.env_clear().stdout(Stdio::piped());
        if let Some(path) = shared_file {
            command.env(common::config::ENV_SHARED_FILE, path);
        }
        let mut child = command
            .spawn()
            .expect("a process to read the environment of");
        // `spawn` can return before exec has placed the environment, which reads as empty until
        // then; the member's "up" comes after.
        let mut line = [0; 3];
        let reported = child
            .stdout
            .as_mut()
            .expect("the member's stdout is piped")
            .read_exact(&mut line);
        assert!(
            reported.is_ok() && line == *b"up\n",
            "the member did not report in: {reported:?}, {line:?}, {:?}",
            child.wait()
        );
        Self(child)
    }

    pub(crate) fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Member {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

/// The binary under test in an environment of the case's own: nothing inherited, so the session
/// directory and the configuration file are what the case sets.
pub(crate) struct Tool {
    env: Vec<(OsString, OsString)>,
    cwd: Option<PathBuf>,
}

impl Tool {
    /// An environment with `XDG_CONFIG_HOME` naming a directory with no configuration file in it.
    pub(crate) fn new() -> Self {
        Self {
            env: Vec::new(),
            cwd: None,
        }
        .env("XDG_CONFIG_HOME", "/nonexistent")
    }

    /// The same environment with `name` set to `value`.
    pub(crate) fn env(mut self, name: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((name.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    /// The same environment, run from `dir`.
    pub(crate) fn within(mut self, dir: &Path) -> Self {
        self.cwd = Some(dir.to_path_buf());
        self
    }

    /// Runs the tool with `args` after the program name.
    pub(crate) fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_libtimewarp"));
        command
            .env_clear()
            .envs(self.env.iter().cloned())
            .args(args);
        if let Some(dir) = &self.cwd {
            command.current_dir(dir);
        }
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
