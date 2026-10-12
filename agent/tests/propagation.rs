//! The session reaching the programs a target starts with the environment the target started
//! with, rather than the one libc keeps: every child of a Go program, whose runtime copies the
//! array the kernel laid out, and a C program's exec with `main`'s third argument, which is that
//! array on musl, FreeBSD, and macOS.
//!
//! A machine with no Go toolchain builds no Go target, and those cases say so rather than failing.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "test support: a failed assumption should fail the test loudly, and not every helper \
              is used by every case"
)]

mod harness;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::time::{Family, Nanos};
use controller::Controller;

use harness::{HOUR, PRELOAD, Run, log_of, session_files};

const TIME: &str = "2020-01-01T00:00:00Z";

/// [`TIME`] in Unix seconds, and the day after it.
const START: i64 = 1_577_836_800;
const DAY_AFTER: i64 = START + 86_400;

/// The variables a case starts the creator with, the wall clock frozen at [`TIME`].
const FROZEN: [(&str, &str); 2] = [("LIBTIMEWARP_TIME", TIME), ("LIBTIMEWARP_RATE", "0")];

/// What one run of `go_spawner` printed, its own lines and its child's, by key.
struct Printed(BTreeMap<String, String>);

impl Printed {
    fn get(&self, key: &str) -> &str {
        self.0
            .get(key)
            .unwrap_or_else(|| panic!("no `{key}` in what the run printed: {:#?}", self.0))
    }

    /// Every variable the Go program's own environment held, by name.
    fn go_environment(&self) -> BTreeMap<&str, &str> {
        self.0
            .iter()
            .filter_map(|(key, value)| Some((key.strip_prefix("go.env.")?, value.as_str())))
            .collect()
    }

    fn add(&mut self, line: &str) {
        if let Some((key, value)) = line.split_once('=') {
            self.0.insert(key.to_owned(), value.to_owned());
        }
    }
}

/// One run of a `go_spawner` build under the library, with its sessions in `dir`.
struct Spawn<'a> {
    program: &'a Path,
    dir: &'a Path,
    library: PathBuf,
    current_dir: Option<&'a Path>,
    env: Vec<(&'a str, &'a str)>,
    /// Where the child runs: `.` for the Go program's own directory.
    child_dir: &'a str,
    child: Vec<String>,
}

impl<'a> Spawn<'a> {
    /// A run that starts [`shell_running_the_probe`] where the Go program runs.
    fn new(program: &'a Path, dir: &'a Path) -> Self {
        Self {
            program,
            dir,
            library: harness::library(),
            current_dir: None,
            env: Vec::new(),
            child_dir: ".",
            child: shell_running_the_probe(),
        }
    }

    fn env(mut self, env: &[(&'a str, &'a str)]) -> Self {
        self.env.extend_from_slice(env);
        self
    }

    /// Runs the Go program, and `between` once it has reported, before it starts the child.
    fn run(self, between: impl FnOnce()) -> Printed {
        let mut command = Command::new(self.program);
        command
            .arg(self.child_dir)
            .args(&self.child)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env(PRELOAD, &self.library)
            .env("LIBTIMEWARP_SESSION_DIR", self.dir)
            .envs(self.env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(current) = self.current_dir {
            command.current_dir(current);
        }
        let mut running = command.spawn().expect("the Go program starts");
        let mut stdout = BufReader::new(running.stdout.take().unwrap());
        let mut stderr = running.stderr.take().unwrap();
        let mut printed = Printed(BTreeMap::new());

        loop {
            let mut line = String::new();
            if stdout.read_line(&mut line).unwrap() == 0 {
                let mut said = String::new();
                stderr.read_to_string(&mut said).unwrap();
                panic!(
                    "the Go program ended before starting its child: {}\n{said}",
                    running.wait().unwrap()
                );
            }
            let line = line.trim_end();
            if line == "go.ready=1" {
                break;
            }
            printed.add(line);
        }
        between();
        running.stdin.take().unwrap().write_all(b"\n").unwrap();

        let mut rest = String::new();
        stdout.read_to_string(&mut rest).unwrap();
        rest.lines().for_each(|line| printed.add(line));
        let mut said = String::new();
        stderr.read_to_string(&mut said).unwrap();
        let status = running.wait().unwrap();
        assert!(
            status.success(),
            "the Go program ended with {status}\n{said}"
        );
        printed
    }
}

/// The child every case but one has the Go program start: a shell that runs `probe`, the way a
/// program runs a command. macOS strips the library from its own shell, so there the probe is
/// started directly.
fn shell_running_the_probe() -> Vec<String> {
    let probe = harness::program("probe").to_string_lossy().into_owned();
    if cfg!(target_os = "macos") {
        vec![probe, "child".to_owned()]
    } else {
        ["/bin/sh", "-c", "\"$0\" child", probe.as_str()]
            .map(str::to_owned)
            .to_vec()
    }
}

/// The one session file in `dir`, where it physically is.
fn the_session(dir: &Path) -> PathBuf {
    let files = session_files(dir);
    let [path] = files.as_slice() else {
        panic!("wanted one session, found {files:?}");
    };
    path.canonicalize().unwrap()
}

/// Moves the wall clock of the one session in `dir` a day ahead.
fn move_a_day(dir: &Path) {
    let controller = Controller::attach(&the_session(dir)).unwrap();
    let mut transaction = controller.begin().unwrap();
    transaction
        .shift(Family::Realtime, Nanos(24 * HOUR))
        .unwrap();
    transaction.commit().unwrap();
}

/// Asserts that the probe that ran as `tag` read the wall clock frozen at `seconds`.
#[track_caller]
fn assert_frozen_at(printed: &Printed, tag: &str, seconds: i64) {
    assert_eq!(printed.get(&format!("{tag}.time")), seconds.to_string());
    assert_eq!(
        printed.get(&format!("{tag}.clock_gettime")),
        (seconds * 1_000_000_000).to_string()
    );
}

fn canonical(path: &str) -> PathBuf {
    assert!(Path::new(path).is_absolute(), "{path} is not absolute");
    Path::new(path).canonicalize().unwrap()
}

/// A Go program under `library` that creates a session, has it moved, and then starts a shell:
/// the shell and what it runs join that session, see the move, and leave no session of their own.
/// The Go program's own environment names the session in place of one setup variable and keeps
/// everything else.
fn a_go_program_s_children_join_its_session(program: &Path, library: &Path) {
    let dir = tempfile::tempdir().unwrap();
    let mut spawn = Spawn::new(program, dir.path()).env(&FROZEN);
    spawn.library = library.to_path_buf();
    let printed = spawn.run(|| move_a_day(dir.path()));

    let session = the_session(dir.path());
    assert_eq!(canonical(printed.get("child.shared")), session);
    assert_frozen_at(&printed, "child", DAY_AFTER);

    assert_eq!(canonical(printed.get("go.shared")), session);
    let environment = printed.go_environment();
    let kept = FROZEN
        .iter()
        .filter(|&&(name, value)| environment.get(name) == Some(&value))
        .count();
    assert_eq!(
        kept, 1,
        "the session's name takes the place of one setup variable: {environment:#?}"
    );
    let library = library.to_string_lossy().into_owned();
    let dir_text = dir.path().to_string_lossy().into_owned();
    for (name, value) in [
        ("PATH", "/usr/bin:/bin"),
        (PRELOAD, library.as_str()),
        ("LIBTIMEWARP_SESSION_DIR", dir_text.as_str()),
    ] {
        assert_eq!(environment.get(name), Some(&value), "{environment:#?}");
    }
    assert_eq!(environment.len(), 5, "{environment:#?}");
}

#[test]
fn a_go_program_on_the_cgo_runtime_passes_its_session_to_its_children() {
    let Some(program) = harness::go_program_with_cgo("go_spawner") else {
        return;
    };
    a_go_program_s_children_join_its_session(&program, &harness::library());
}

#[test]
fn a_go_program_linked_without_the_cgo_runtime_passes_its_session_to_its_children() {
    let Some(program) = harness::go_program("go_spawner") else {
        return;
    };
    a_go_program_s_children_join_its_session(&program, &harness::library());
}

/// A build that cannot fake the Go program's own clock still finds the environment it started with.
#[test]
fn a_build_without_the_vdso_feature_passes_a_go_program_s_session_to_its_children() {
    let Some(program) = harness::go_program("go_spawner") else {
        return;
    };
    a_go_program_s_children_join_its_session(&program, &harness::library_without_vdso());
}

#[test]
fn an_empty_session_variable_carries_the_session_and_spares_the_setup_variables() {
    let Some(program) = harness::go_program("go_spawner") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let printed = Spawn::new(&program, dir.path())
        .env(&FROZEN)
        .env(&[("LIBTIMEWARP__SHARED_FILE", "")])
        .run(|| move_a_day(dir.path()));

    let session = the_session(dir.path());
    assert_eq!(canonical(printed.get("child.shared")), session);
    assert_frozen_at(&printed, "child", DAY_AFTER);
    assert_eq!(canonical(printed.get("go.shared")), session);
    let environment = printed.go_environment();
    for (name, value) in FROZEN {
        assert_eq!(environment.get(name), Some(&value), "{environment:#?}");
    }
}

#[test]
fn a_relative_inherited_path_reaches_a_go_program_s_children_whole() {
    let Some(program) = harness::go_program("go_spawner") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut first = Run::prog("probe").session_dir(dir.path());
    for (name, value) in FROZEN {
        first = first.env(name, value);
    }
    let first = first.run();
    let created = Path::new(first.get("probe.shared"));
    let relative = created.file_name().unwrap().to_string_lossy().into_owned();

    let mut spawn =
        Spawn::new(&program, dir.path()).env(&[("LIBTIMEWARP__SHARED_FILE", &relative)]);
    spawn.current_dir = created.parent();
    // The child runs from the root, where the relative path names nothing.
    spawn.child_dir = "/";
    spawn.child = vec![
        harness::program("probe").to_string_lossy().into_owned(),
        "child".to_owned(),
    ];
    let printed = spawn.run(|| {});

    let session = the_session(dir.path());
    assert_eq!(canonical(printed.get("go.shared")), session);
    assert_eq!(canonical(printed.get("child.shared")), session);
    assert_frozen_at(&printed, "child", START);
}

#[test]
fn with_nothing_to_take_the_place_of_a_go_program_keeps_its_environment_and_says_so() {
    let Some(program) = harness::go_program("go_spawner") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("timewarp.log");
    let log_text = log.to_string_lossy().into_owned();
    let printed = Spawn::new(&program, dir.path())
        .env(&[("LIBTIMEWARP_LOG", &log_text)])
        .run(|| {});

    assert_eq!(printed.get("go.shared"), "");
    let library = harness::library().to_string_lossy().into_owned();
    let dir_text = dir.path().to_string_lossy().into_owned();
    let started: BTreeMap<&str, &str> = [
        ("PATH", "/usr/bin:/bin"),
        (PRELOAD, library.as_str()),
        ("LIBTIMEWARP_SESSION_DIR", dir_text.as_str()),
        ("LIBTIMEWARP_LOG", log_text.as_str()),
    ]
    .into_iter()
    .collect();
    assert_eq!(printed.go_environment(), started);

    let written = log_of(&log);
    let warning = format!("libtimewarp[{}] warn: ", printed.get("go.pid"));
    assert!(
        written
            .lines()
            .any(|line| line.starts_with(&warning) && line.contains("LIBTIMEWARP__SHARED_FILE=")),
        "{written}"
    );
    // Which is what the line warns of: the shell found no session to join.
    assert_eq!(session_files(dir.path()).len(), 2);
}

#[test]
fn a_c_program_that_execs_with_main_s_environment_keeps_the_new_program_in_its_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut run = Run::prog("envp_execer")
        .arg(harness::program("probe").to_string_lossy())
        .session_dir(dir.path());
    for (name, value) in FROZEN {
        run = run.env(name, value);
    }
    let report = run.run();

    assert_eq!(report.get("after.shared"), report.get("before.shared"));
    assert_eq!(report.get("after.time"), START.to_string());
    assert_eq!(session_files(dir.path()).len(), 1);
}
