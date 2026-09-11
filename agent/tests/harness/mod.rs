//! Builds the library, runs a target program under preload with a given environment, and hands
//! back what the target printed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

/// Where cargo put the artifacts of this test run.
fn target_dir() -> PathBuf {
    let mut dir = std::env::current_exe().expect("test executable has a path");
    dir.pop(); // deps
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir
}

static LIBRARY: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn library() -> PathBuf {
    LIBRARY
        .get_or_init(|| {
            let dir = target_dir();
            let mut cargo =
                Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
            cargo
                .args(["build", "-p", "agent"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .env_remove("CARGO_MAKEFLAGS");
            if dir.ends_with("release") {
                cargo.arg("--release");
            }
            let status = cargo.status().expect("cargo builds the library under test");
            assert!(status.success(), "building the library under test");

            let path = dir.join(format!(
                "{}timewarp{}",
                std::env::consts::DLL_PREFIX,
                std::env::consts::DLL_SUFFIX
            ));
            assert!(path.exists(), "{} was not built", path.display());
            path
        })
        .clone()
}

static COMPILE: Mutex<()> = Mutex::new(());

/// Compiles `agent/tests/progs/<name>.c`, once per test run.
pub(crate) fn program(name: &str) -> PathBuf {
    compile(name, &[])
}

/// The same, linked against the library instead of preloading it. That is how a program reaches
/// the library's own public symbols, and it interposes just the same: our `DT_NEEDED` entry comes
/// before libc's, so the loader finds our definitions first.
pub(crate) fn program_linked_to_library(name: &str) -> PathBuf {
    let library = library();
    compile(
        name,
        &[
            library.to_string_lossy().into_owned(),
            format!("-Wl,-rpath,{}", library.parent().unwrap().display()),
        ],
    )
}

/// A copy of `source`'s binary under a different basename, so a test can name what
/// `LIBTIMEWARP_SKIP_CMDS` or `LIBTIMEWARP_ONLY_CMDS` sees the running process as.
pub(crate) fn copy_named(source: &Path, name: &str) -> PathBuf {
    let out_dir = target_dir().join("timewarp-tests");
    std::fs::create_dir_all(&out_dir).unwrap();
    let dest = out_dir.join(name);
    std::fs::copy(source, &dest).unwrap();
    dest
}

/// Compiles `agent/tests/hooks/<name>.c` into a shared library a session can load, against the
/// header the library ships for hook authors.
pub(crate) fn hook_library(name: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/hooks")
        .join(format!("{name}.c"));
    build(
        &source,
        &target_dir()
            .join("timewarp-tests")
            .join(format!("{name}.so")),
        &[
            "-shared".to_owned(),
            "-fPIC".to_owned(),
            "-Wall".to_owned(),
            "-Wextra".to_owned(),
            "-Werror".to_owned(),
            format!("-I{}", include_dir().display()),
        ],
    )
}

/// Where the C header a hook author includes lives.
pub(crate) fn include_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../include")
        .canonicalize()
        .expect("the shipped header's directory")
}

fn compile(name: &str, extra: &[String]) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/progs")
        .join(format!("{name}.c"));
    build(
        &source,
        &target_dir().join("timewarp-tests").join(name),
        extra,
    )
}

/// Compiles `source` into `binary`, once per test run, unless the binary is already newer.
fn build(source: &Path, binary: &Path, extra: &[String]) -> PathBuf {
    let out_dir = binary.parent().expect("a binary sits in a directory");

    let _guard = COMPILE.lock().unwrap();
    std::fs::create_dir_all(out_dir).unwrap();
    let fresh = std::fs::metadata(binary)
        .and_then(|b| Ok(b.modified()? >= std::fs::metadata(source)?.modified()?))
        .unwrap_or(false);
    if !fresh {
        let status = Command::new(std::env::var("CC").as_deref().unwrap_or("cc"))
            .arg("-O0")
            .arg("-o")
            .arg(binary)
            .arg(source)
            .args(extra)
            .status()
            .expect("a C compiler");
        assert!(status.success(), "compiling {}", source.display());
    }
    binary.to_path_buf()
}

/// Whether a name in the session directory is a session file. The lock file beside each one is
/// not: it carries no data, and what the cases counting these want is how many sessions a run
/// made.
pub(crate) fn is_session_file(name: &std::ffi::OsStr) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|suffix| suffix == "session")
}

/// Every session file a run left in `dir`, which is the directory a `Run` was given.
pub(crate) fn session_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir.join("libtimewarp")) else {
        return Vec::new();
    };
    entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.file_name().is_some_and(is_session_file))
        .collect()
}

/// The one session a run left behind, mapped the way a controller would map it.
pub(crate) fn only_session(dir: &Path) -> common::session::Session {
    let files = session_files(dir);
    let [path] = files.as_slice() else {
        panic!("wanted the one session file a run leaves behind, found {files:?}");
    };
    common::session::attach(path).unwrap()
}

/// Whatever a run wrote to the log file it was given, or nothing when it wrote none.
pub(crate) fn log_of(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// One run of a target program under the library.
pub(crate) struct Run {
    program: PathBuf,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    preload: bool,
    session_dir: Option<PathBuf>,
}

impl Run {
    pub(crate) fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            preload: true,
            session_dir: None,
        }
    }

    /// The named target program, compiled from `tests/progs`.
    pub(crate) fn prog(name: &str) -> Self {
        Self::new(program(name))
    }

    pub(crate) fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub(crate) fn env(mut self, name: &str, value: impl AsRef<str>) -> Self {
        self.env.insert(name.to_owned(), value.as_ref().to_owned());
        self
    }

    /// Where sessions this run creates should land.
    pub(crate) fn session_dir(mut self, dir: &Path) -> Self {
        self.session_dir = Some(dir.to_path_buf());
        self
    }

    /// Runs without the library, for a genuine reading to compare against.
    pub(crate) const fn without_library(mut self) -> Self {
        self.preload = false;
        self
    }

    /// Runs the target and asserts it succeeded, which is what every case but a deliberate
    /// failure wants.
    pub(crate) fn run(self) -> Report {
        let program = self.program.clone();
        let report = self.run_unchecked();
        assert!(
            report.status.success(),
            "{} exited {:?}\nstdout:\n{}\nstderr:\n{}",
            program.display(),
            report.status.code(),
            report.stdout,
            report.stderr,
        );
        report
    }

    /// The same, handing back whatever the target did, for a case where exiting is the point.
    pub(crate) fn run_unchecked(self) -> Report {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        if self.preload {
            command.env("LD_PRELOAD", library());
        }
        if let Some(dir) = &self.session_dir {
            command.env("LIBTIMEWARP_SESSION_DIR", dir);
        }
        for (name, value) in &self.env {
            command.env(name, value);
        }

        let before = genuine_now();
        let output = command.output().expect("target program ran");
        let after = genuine_now();

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        Report {
            facts: parse(&stdout),
            stdout,
            stderr,
            status: output.status,
            before,
            after,
        }
    }
}

fn parse(stdout: &str) -> BTreeMap<String, String> {
    stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// What a target printed, plus the genuine window the run happened in.
pub(crate) struct Report {
    facts: BTreeMap<String, String>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    /// How the target left, for a case that expects it not to have run at all.
    pub(crate) status: std::process::ExitStatus,
    /// Genuine `CLOCK_REALTIME` in nanoseconds, immediately before and after the run.
    pub(crate) before: i64,
    pub(crate) after: i64,
}

impl Report {
    pub(crate) fn get(&self, key: &str) -> &str {
        self.facts
            .get(key)
            .unwrap_or_else(|| panic!("no `{key}` in target output:\n{}", self.stdout))
    }

    pub(crate) fn nanos(&self, key: &str) -> i64 {
        self.get(key)
            .parse()
            .unwrap_or_else(|_| panic!("`{key}` is not a number: {}", self.get(key)))
    }

    /// Asserts that every wall-clock reading under `tag` is the genuine clock plus `shift`.
    ///
    /// The bound is the run's own genuine window widened by a second, which is what lets the same
    /// assertion serve a shift of hours and a shift of nothing.
    pub(crate) fn assert_wall_clock(&self, tag: &str, shift_nanos: i64) {
        const SLACK: i64 = 1_000_000_000;
        let low = self.before + shift_nanos - SLACK;
        let high = self.after + shift_nanos + SLACK;

        for (key, scale) in [
            (format!("{tag}.clock_gettime"), 1),
            (format!("{tag}.gettimeofday"), 1),
            (format!("{tag}.time"), 1_000_000_000),
        ] {
            let value = self.nanos(&key) * scale;
            assert!(
                (low..=high).contains(&value),
                "{key} = {value} is not in {low}..={high} (off by {} s)",
                (value - low).min(high - value) / 1_000_000_000,
            );
        }
    }
}

pub(crate) fn genuine_now() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Safety: `ts` is a live buffer, and this test process has no library preloaded.
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &raw mut ts) };
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

pub(crate) const HOUR: i64 = 3_600 * 1_000_000_000;
