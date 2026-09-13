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
        .get_or_init(|| build_library(&[], &target_dir()))
        .clone()
}

static LIBRARY_WITHOUT_VDSO: OnceLock<PathBuf> = OnceLock::new();

/// The same library, built without the `vdso` feature, so a case can run a target under a build
/// that cannot pose as the vDSO at all.
///
/// A target directory of its own, because the two builds differ only in their features and would
/// otherwise take turns overwriting one library.
pub(crate) fn library_without_vdso() -> PathBuf {
    LIBRARY_WITHOUT_VDSO
        .get_or_init(|| {
            let dir = target_dir().join("timewarp-tests/no-vdso");
            let profile = if target_dir().ends_with("release") {
                "release"
            } else {
                "debug"
            };
            build_library(
                &[
                    "--no-default-features",
                    "--target-dir",
                    &dir.to_string_lossy(),
                ],
                &dir.join(profile),
            )
        })
        .clone()
}

/// Builds the library under test into `into`, with `extra` on the cargo command line.
fn build_library(extra: &[&str], into: &Path) -> PathBuf {
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo
        .args(["build", "-p", "agent"])
        .args(extra)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("CARGO_MAKEFLAGS");
    if target_dir().ends_with("release") {
        cargo.arg("--release");
    }
    let status = cargo.status().expect("cargo builds the library under test");
    assert!(status.success(), "building the library under test");

    let path = into.join(format!(
        "{}timewarp{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    assert!(path.exists(), "{} was not built", path.display());
    path
}

/// The variable that loads the library ahead of the program: `DYLD_INSERT_LIBRARIES` on macOS,
/// where dyld reads our interpose table out of it, and `LD_PRELOAD` on the other two.
pub(crate) const PRELOAD: &str = if cfg!(target_os = "macos") {
    "DYLD_INSERT_LIBRARIES"
} else {
    "LD_PRELOAD"
};

static COMPILE: Mutex<()> = Mutex::new(());

/// Compiles `agent/tests/progs/<name>.c`, once per test run.
pub(crate) fn program(name: &str) -> PathBuf {
    compile(name, &[])
}

/// The same, linked against the library instead of preloading it. That is how a program reaches
/// the library's own public symbols, and it interposes just the same: our `DT_NEEDED` entry comes
/// before libc's, so the loader finds our definitions first, and on macOS dyld applies the
/// interpose table of every image loaded at launch, linked or inserted alike.
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

/// The same again, with the extra libraries a program needs beyond libc. glibc folds `librt` and
/// `libpthread` into libc and older systems do not, so a program using timed message queues,
/// asynchronous reads, or threads names both.
pub(crate) fn program_linked_with(name: &str, libraries: &[&str]) -> PathBuf {
    let library = library();
    let mut extra = vec![
        library.to_string_lossy().into_owned(),
        format!("-Wl,-rpath,{}", library.parent().unwrap().display()),
    ];
    extra.extend(libraries.iter().map(|flag| (*flag).to_owned()));
    compile(name, &extra)
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
///
/// A hook may call the library's genuine-clock symbol, which is the agent's and not there at link
/// time. A loader that binds lazily leaves such a name to the process it lands in; dyld wants to be
/// told, which is what the last flag does.
pub(crate) fn hook_library(name: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/hooks")
        .join(format!("{name}.c"));
    let mut flags = vec![
        "-shared".to_owned(),
        "-fPIC".to_owned(),
        "-Wall".to_owned(),
        "-Wextra".to_owned(),
        "-Werror".to_owned(),
        format!("-I{}", include_dir().display()),
    ];
    if cfg!(target_os = "macos") {
        flags.push("-Wl,-U,_libtimewarp_genuine_clock_gettime".to_owned());
    }
    build(
        &source,
        &target_dir()
            .join("timewarp-tests")
            .join(format!("{name}.so")),
        &flags,
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

/// Compiles `agent/tests/progs/<name>.go` into a dynamically linked Go program, or `None` when
/// this machine has no Go toolchain to build one with.
///
/// A position-independent executable with cgo enabled, because that is the Go binary the library
/// can reach at all: it has an interpreter, so the loader runs and our constructor with it. The
/// runtime inside still skips libc for every clock it reads, which is the point of running one.
#[expect(
    clippy::print_stderr,
    reason = "a machine with no Go says the cases went unexercised, rather than passing quietly"
)]
pub(crate) fn go_program(name: &str) -> Option<PathBuf> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/progs")
        .join(format!("{name}.go"));
    let binary = target_dir().join("timewarp-tests").join(name);
    let out_dir = binary.parent().expect("a binary sits in a directory");

    let _guard = COMPILE.lock().unwrap();
    std::fs::create_dir_all(out_dir).unwrap();
    let fresh = std::fs::metadata(&binary)
        .and_then(|b| Ok(b.modified()? >= std::fs::metadata(&source)?.modified()?))
        .unwrap_or(false);
    if fresh {
        return Some(binary);
    }
    let built = Command::new("go")
        .args(["build", "-buildmode=pie", "-o"])
        .arg(&binary)
        .arg(&source)
        .env("CGO_ENABLED", "1")
        .env("GOCACHE", out_dir.join("gocache"))
        .status();
    match built {
        Ok(status) if status.success() => Some(binary),
        // A machine with no Go checks nothing here rather than failing the suite.
        _ => {
            eprintln!("no working go on this machine, so the Go cases went unexercised");
            None
        }
    }
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
        let mut command = Command::new(std::env::var("CC").as_deref().unwrap_or("cc"));
        command.arg("-O0").arg("-o").arg(binary).arg(source);
        // macOS keeps part of its own surface behind this macro, the open-file-description locks
        // and the calls it kept from before POSIX withdrew them among them, so a C file of ours
        // asks for all of it rather than for whatever the default happens to be.
        if cfg!(target_os = "macos") {
            command.arg("-D_DARWIN_C_SOURCE");
        }
        let status = command.args(extra).status().expect("a C compiler");
        assert!(status.success(), "compiling {}", source.display());
    }
    binary.to_path_buf()
}

/// What a case sets beside its own environment when it gives one family a rate of its own.
///
/// The FreeBSD timekeep page carries one rate for the wall clock and the elapsed clock it serves,
/// so a session with the page on refuses a pair that differs, and a case about one family's line
/// runs there with the page off, which is what the design says such a case must do. Everywhere
/// else there is no such page and this is empty. A case that names such a rate in its environment
/// is noticed by `Run` without asking; this is for the ones that reach for the controller instead,
/// and for the ones that build their own command.
pub(crate) const fn separate_family_rates() -> &'static [(&'static str, &'static str)] {
    if cfg!(target_os = "freebsd") {
        &[("LIBTIMEWARP_EMULATE_VDSO", "0")]
    } else {
        &[]
    }
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

/// The first `count` words of a session's arena, as a hook left them.
pub(crate) fn arena_words(session: &common::session::Session, count: usize) -> Vec<i64> {
    let header = session.header();
    let offset = usize::try_from(header.arena_offset).unwrap();
    let bytes = std::fs::read(session.path()).unwrap();
    (0..count)
        .map(|index| {
            let at = offset + index * size_of::<i64>();
            let word = bytes
                .get(at..at + size_of::<i64>())
                .expect("inside the arena");
            i64::from_le_bytes(word.try_into().unwrap())
        })
        .collect()
}

/// What the `clock_group` hook recorded: one triple per call, in the order they arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Seen {
    pub(crate) func: i64,
    pub(crate) kind: i64,
    pub(crate) clock: i64,
}

/// The log `clock_group.c` left in the arena.
pub(crate) fn calls_seen(session: &common::session::Session) -> Vec<Seen> {
    let count = arena_words(session, 1).first().copied().unwrap_or(0);
    let words = arena_words(session, 1 + usize::try_from(count).unwrap() * 3);
    words
        .get(1..)
        .unwrap_or_default()
        .chunks_exact(3)
        .filter_map(|triple| match *triple {
            [func, kind, clock] => Some(Seen { func, kind, clock }),
            _ => None,
        })
        .collect()
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
    library: Option<PathBuf>,
    session_dir: Option<PathBuf>,
    through: Vec<String>,
}

impl Run {
    pub(crate) fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            preload: true,
            library: None,
            session_dir: None,
            through: Vec::new(),
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

    /// Runs under a build of the library other than the default one.
    pub(crate) fn under_library(mut self, library: PathBuf) -> Self {
        self.library = Some(library);
        self
    }

    /// Runs the target through a command that execs it, which is how a case gives the target
    /// something this process does not have to hand it directly.
    ///
    /// The wrapper is the first process under the library, so it is the one that creates the
    /// session and the target attaches to it by the inherited path, as any exec does.
    pub(crate) fn through(mut self, wrapper: &[String]) -> Self {
        self.through = wrapper.to_vec();
        self
    }

    /// Whether this run gives one family a rate of its own.
    ///
    /// The timekeep page carries one rate for the wall clock and the elapsed clock it serves, so a
    /// session with the page on refuses a pair that differs: the page is what rules the
    /// combination out. A bare `LIBTIMEWARP_RATE` covers every family at once and needs none of
    /// this, which is what leaves the cases about the page itself running with it on.
    fn names_one_familys_rate(&self) -> bool {
        [
            "LIBTIMEWARP_REALTIME_RATE",
            "LIBTIMEWARP_MONOTONIC_RATE",
            "LIBTIMEWARP_BOOTTIME_RATE",
        ]
        .iter()
        .any(|name| self.env.contains_key(*name))
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
        let mut command = match self.through.split_first() {
            Some((wrapper, rest)) => {
                let mut command = Command::new(wrapper);
                command.args(rest).arg(&self.program);
                command
            }
            None => Command::new(&self.program),
        };
        command
            .args(&self.args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        if self.preload {
            let library = self.library.clone().unwrap_or_else(library);
            command.env(PRELOAD, library);
        }
        if let Some(dir) = &self.session_dir {
            command.env("LIBTIMEWARP_SESSION_DIR", dir);
        }
        // On FreeBSD the timekeep page rules out two things a case may ask for, and a case that
        // asks for either runs with the page off, as the design says such a case must. A case that
        // says otherwise still wins, because its own settings go on below.
        if cfg!(target_os = "freebsd")
            && (self.env.contains_key("LIBTIMEWARP_HOOK") || self.names_one_familys_rate())
        {
            command.env("LIBTIMEWARP_EMULATE_VDSO", "0");
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
    /// Every fact whose key starts with `prefix`, under what is left of the key.
    pub(crate) fn under(&self, prefix: &str) -> Vec<(String, String)> {
        self.facts
            .iter()
            .filter_map(|(key, value)| Some((key.strip_prefix(prefix)?.to_owned(), value.clone())))
            .collect()
    }

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
