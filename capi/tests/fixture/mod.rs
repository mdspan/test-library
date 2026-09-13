//! Building the C library under test, and the sessions a case points it at.
#![expect(
    clippy::expect_used,
    reason = "test support: a failed assumption should fail the test loudly"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use common::session::layout::Keeper;
use common::session::{self, Session, Setup};

/// A case that creates a session takes this: the lock file a reader probes is named once per
/// process, so two sessions at once would cross their probes.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

pub(crate) fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where cargo put the artifacts of this test run.
pub(crate) fn target_dir() -> PathBuf {
    let mut dir = std::env::current_exe().expect("test executable has a path");
    dir.pop(); // deps
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir
}

/// The repository's root, which is where the shipped header and the Python tool live.
pub(crate) fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("the crate sits inside the repository")
}

static LIBRARY: OnceLock<PathBuf> = OnceLock::new();

/// The C library under test, built once per test run.
///
/// A test binary of this crate is linked against the rlib rather than the `cdylib`, so the shared
/// object a C caller or the Python tool loads has to be asked for by name.
pub(crate) fn library() -> PathBuf {
    LIBRARY
        .get_or_init(|| {
            let dir = target_dir();
            let mut cargo =
                Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
            cargo
                .args(["build", "-p", "capi"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .env_remove("CARGO_MAKEFLAGS");
            if dir.ends_with("release") {
                cargo.arg("--release");
            }
            let status = cargo.status().expect("cargo builds the library under test");
            assert!(status.success(), "building the library under test");

            let path = dir.join(format!(
                "{}timewarp_controller{}",
                std::env::consts::DLL_PREFIX,
                std::env::consts::DLL_SUFFIX
            ));
            assert!(path.exists(), "{} was not built", path.display());
            path
        })
        .clone()
}

/// Compiles `tests/c/<name>.c` into a program of the suite's own, with `extra` on the command line
/// beyond the source and the output.
pub(crate) fn compile(name: &str, extra: &[String]) -> PathBuf {
    let out = target_dir().join("timewarp-capi-tests");
    std::fs::create_dir_all(&out).expect("a directory for the programs under test");
    let binary = out.join(name);
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/c")
        .join(format!("{name}.c"));
    let status = Command::new(std::env::var("CC").as_deref().unwrap_or("cc"))
        .arg("-O0")
        .args(["-Wall", "-Wextra", "-Werror"])
        .arg("-o")
        .arg(&binary)
        .arg(&source)
        .args(extra)
        .status()
        .expect("a C compiler");
    assert!(status.success(), "compiling {}", source.display());
    binary
}

/// A directory of our own with one session in it, kept mapped so the file stays whole.
pub(crate) struct Fixture {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) session: Session,
}

impl Fixture {
    pub(crate) fn on(keeper: Keeper) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory");
        // With the timekeep page on, the wall clock and the elapsed clock it serves share a
        // rate, and the surface below gives one family a rate of its own. The page is not what
        // these cases are about, so it is off.
        let session = session::create(&Setup {
            keeper,
            emulate_vdso: false,
            ..Setup::plain(dir.path().join("libtimewarp"))
        })
        .expect("a session of our own");
        Self { dir, session }
    }

    /// Where a case points the tool: the directory holding session files, which is the one the
    /// naming rule computes under `LIBTIMEWARP_SESSION_DIR`.
    pub(crate) fn session_dir(&self) -> PathBuf {
        self.dir.path().join("libtimewarp")
    }

    pub(crate) fn path(&self) -> &Path {
        self.session.path()
    }
}
