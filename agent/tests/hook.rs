//! A user's own library in front of the keeper: what it answers, what it declines, what it reads
//! from the arena, and what happens when it will not load.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "test support: a failed assumption should fail the test loudly, and not every helper \
              is used by every case"
)]

mod harness;

use std::path::{Path, PathBuf};

use common::session::layout::{self, Keeper};
use harness::{HOUR, Run, arena_words, log_of, only_session, session_files};

/// The instant `arena_clock`'s `init` writes into the arena: 2020-01-01T00:00:00Z.
const START_NS: i64 = 1_577_836_800_000_000_000;

/// What `arena_clock` answers when the session gave it no arena: 2000-01-01T00:00:00Z.
const NO_ARENA_NS: i64 = 946_684_800_000_000_000;

/// The `EX_CONFIG` of `sysexits.h`, which is what a process that fails closed leaves behind.
const EX_CONFIG: i32 = 78;

/// How close a reading has to be to count as the same instant: a second, the same slack the wall
/// clock assertions use.
const SLACK: i64 = 1_000_000_000;

/// `LIBTIMEWARP_HOOK` for a hook in the suite, with the arena size it should be given.
fn hook_value(name: &str, arena: Option<u64>) -> String {
    let library = harness::hook_library(name).display().to_string();
    arena.map_or_else(|| library.clone(), |size| format!("{library}@{size}"))
}

/// A copy of a hook library inside `dir`, so a test can take it away again afterwards.
fn hook_copy(name: &str, dir: &Path) -> PathBuf {
    let copy = dir.join(format!("{name}.so"));
    std::fs::copy(harness::hook_library(name), &copy).unwrap();
    copy
}

fn assert_near(what: &str, value: i64, wanted: i64) {
    assert!(
        (value - wanted).abs() <= SLACK,
        "{what} = {value} is not {wanted} (off by {} s)",
        (value - wanted) / 1_000_000_000,
    );
}

#[test]
fn the_hook_answers_the_wall_clock_from_the_arena_and_declines_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_HOOK", hook_value("arena_clock", Some(4096)))
        // The keeper still has settings of its own, for every call the hook declines.
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    // Every way of asking the wall clock reaches the hook, whichever function the program called.
    assert_eq!(report.nanos("probe.clock_gettime"), START_NS);
    assert_eq!(report.nanos("probe.gettimeofday"), START_NS);
    assert_eq!(report.nanos("probe.time"), START_NS / 1_000_000_000);
    // The monotonic clock is declined, so the keeper answers it: genuine, since a bare shift moves
    // the wall clock alone.
    assert!(report.nanos("probe.monotonic") < 10 * HOUR);

    let session = only_session(dir.path());
    let header = session.header();
    assert_eq!(
        header.keeper,
        Keeper::Affine as u32,
        "the keeper is still on"
    );
    assert_eq!(header.fault(), None, "nothing went wrong here");

    // The arena sits after the library's own structures, page-aligned, and the whole file covers it.
    let page = usize::try_from(header.page_size).unwrap();
    assert_eq!(
        usize::try_from(header.arena_offset).unwrap(),
        layout::arena_offset(page)
    );
    assert_eq!(u64::try_from(page).unwrap(), header.arena_size);
    assert_eq!(
        u64::from(header.file_size),
        header.arena_offset + header.arena_size
    );
    // Recorded absolute, so a child that changed directory still finds it.
    let recorded = Path::new(std::str::from_utf8(header.hook_path()).unwrap());
    assert!(recorded.is_absolute(), "{}", recorded.display());
    assert_eq!(recorded, harness::hook_library("arena_clock"));

    // And what `init` wrote is in the arena for every process of the session to read.
    assert_eq!(arena_words(&session, 1), vec![START_NS]);
}

#[test]
fn a_session_with_no_arena_hands_the_hook_a_null_one() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_HOOK", hook_value("arena_clock", None))
        .run();

    assert_eq!(report.nanos("probe.clock_gettime"), NO_ARENA_NS);

    let session = only_session(dir.path());
    let header = session.header();
    assert_eq!(
        (header.arena_offset, header.arena_size),
        (0, 0),
        "a session with no arena names neither an offset nor a size"
    );
    // The file ends where an arena would have started.
    let page = usize::try_from(header.page_size).unwrap();
    assert_eq!(
        usize::try_from(header.file_size).unwrap(),
        layout::arena_offset(page)
    );
}

#[test]
fn the_hook_reaches_every_process_of_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let probe = harness::program("probe");
    let report = Run::prog("execer")
        .arg(probe.to_string_lossy())
        .session_dir(dir.path())
        .env("LIBTIMEWARP_HOOK", hook_value("arena_clock", Some(4096)))
        .run();

    // The child loaded the library the session names, from the path in the header, and read the
    // arena the creator's `init` filled.
    assert_eq!(report.nanos("before.clock_gettime"), START_NS);
    assert_eq!(report.nanos("after.clock_gettime"), START_NS);
    assert_eq!(report.get("before.shared"), report.get("after.shared"));
    assert_eq!(session_files(dir.path()).len(), 1);
}

#[test]
fn an_attacher_loads_the_hook_the_session_names_with_no_variable_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let creator = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_HOOK", hook_value("arena_clock", Some(4096)))
        .run();

    // Nothing in this environment names a hook: the path and the arena come from the session.
    let joined = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP__SHARED_FILE", creator.get("probe.shared"))
        .run();

    assert_eq!(joined.nanos("probe.clock_gettime"), START_NS);
    assert_eq!(session_files(dir.path()).len(), 1);
}

#[test]
fn a_nested_clock_read_inside_the_hook_runs_the_keeper_alone() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_HOOK", hook_value("nested_read", Some(4096)))
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    // The hook answers the monotonic clock with what a nested read of the wall clock gave it, which
    // is the keeper's faked value: the nested read skipped the hook rather than recursing into it.
    let faked_wall_clock = report.nanos("probe.clock_gettime");
    report.assert_wall_clock("probe", -2 * HOUR);
    assert_near(
        "probe.monotonic",
        report.nanos("probe.monotonic"),
        faked_wall_clock,
    );

    // The hook left both readings in the arena: the faked one, and the one it took through the
    // genuine-clock symbol the library exports.
    let session = only_session(dir.path());
    let [nested, genuine] = arena_words(&session, 2)[..] else {
        panic!("the hook left two words in the arena");
    };
    assert_near("the nested reading", nested, report.before - 2 * HOUR);
    assert_near("the genuine reading", genuine, report.before);
}

#[test]
fn a_hook_that_hands_back_the_keepers_answer_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_HOOK", hook_value("abi_shape", None))
        .env("LIBTIMEWARP_SHIFT", "-2h")
        .run();

    // Handled, with the keeper's own number: the call carries what declining would have returned.
    report.assert_wall_clock("probe", -2 * HOUR);
    assert!(report.nanos("probe.monotonic") > 0);
}

/// Every way a hook can fail to be put in front of the keeper, with the value that does it.
fn each_broken_hook(dir: &Path) -> Vec<(&'static str, String)> {
    vec![
        (
            "a library that is not there",
            format!("{}/gone.so@4096", dir.display()),
        ),
        (
            "a library that answers nothing",
            hook_value("no_answer", Some(4096)),
        ),
        (
            "a library whose init refuses",
            hook_value("refusing_init", Some(4096)),
        ),
        (
            "an arena no session file could carry",
            hook_value("arena_clock", Some(1 << 32)),
        ),
    ]
}

#[test]
fn a_hook_the_library_cannot_use_is_a_bad_value() {
    for (what, value) in each_broken_hook(Path::new("/nonexistent")) {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("timewarp.log");
        let report = Run::prog("probe")
            .session_dir(dir.path())
            .env("LIBTIMEWARP_LOG", log.to_string_lossy())
            .env("LIBTIMEWARP_HOOK", &value)
            .env("LIBTIMEWARP_SHIFT", "-2h")
            .run();

        // Genuine, never half-faked: the shift the keeper was asked for is dropped with the hook.
        report.assert_wall_clock("probe", 0);

        let session = only_session(dir.path());
        let header = session.header();
        assert_eq!(header.keeper, Keeper::Null as u32, "{what}");
        assert_eq!(header.hook_path(), b"", "{what}");
        assert_eq!((header.arena_offset, header.arena_size), (0, 0), "{what}");
        assert!(header.fault().is_some(), "{what}");

        let written = log_of(&log);
        assert_eq!(written.lines().count(), 1, "{what}: {written}");
        assert!(written.contains("LIBTIMEWARP_HOOK: "), "{what}: {written}");
    }
}

#[test]
fn a_creator_that_gives_up_on_its_hook_leaves_the_directory_clean_for_what_it_publishes() {
    let dir = tempfile::tempdir().unwrap();
    Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_HOOK", hook_value("refusing_init", Some(4096)))
        .run();

    // One session and one lock file: the session it gave up on left nothing behind.
    let mut names: Vec<String> = std::fs::read_dir(dir.path().join("libtimewarp"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names.len(), 2, "{names:?}");
}

#[test]
fn a_hook_that_fails_closed_at_creation_publishes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_FAIL_CLOSED", "1")
        .env("LIBTIMEWARP_HOOK", hook_value("refusing_init", Some(4096)))
        .run_unchecked();

    assert_eq!(report.status.code(), Some(EX_CONFIG));
    assert_eq!(report.stdout, "", "the program ran anyway");
    assert!(
        report.stderr.contains("LIBTIMEWARP_HOOK: "),
        "{}",
        report.stderr
    );
    assert!(session_files(dir.path()).is_empty());
}

/// A session whose hook loaded for its creator and has been taken away since, which is the failure
/// an attacher meets with a session already in hand.
fn session_whose_hook_is_gone(dir: &Path, fail_closed: bool) -> (PathBuf, u32) {
    let library = hook_copy("arena_clock", dir);
    let report = Run::prog("probe")
        .session_dir(dir)
        .env(
            "LIBTIMEWARP_FAIL_CLOSED",
            if fail_closed { "1" } else { "0" },
        )
        .env("LIBTIMEWARP_HOOK", format!("{}@4096", library.display()))
        .run();
    assert_eq!(report.nanos("probe.clock_gettime"), START_NS);

    let session = PathBuf::from(report.get("probe.shared"));
    let creator = report.get("probe.pid").parse().unwrap();
    std::fs::remove_file(&library).unwrap();
    (session, creator)
}

#[test]
fn an_attacher_whose_hook_will_not_load_records_the_fault_and_runs_genuine() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("timewarp.log");
    let (session, creator) = session_whose_hook_is_gone(dir.path(), false);

    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP_LOG", log.to_string_lossy())
        .env("LIBTIMEWARP__SHARED_FILE", session.to_string_lossy())
        .run();

    // Genuine, and the session left as it was for whoever else attaches to it.
    report.assert_wall_clock("probe", 0);
    assert_eq!(session_files(dir.path()).len(), 1, "a session of its own");

    let fault = only_session(dir.path()).header().fault().expect("a fault");
    assert_eq!(
        fault.pid,
        report.get("probe.pid").parse::<u32>().unwrap(),
        "the process that hit it records it, not the creator {creator}"
    );
    let written = log_of(&log);
    assert_eq!(written.lines().count(), 1, "{written}");
    assert!(written.contains("LIBTIMEWARP_HOOK: "), "{written}");
}

#[test]
fn an_attacher_that_fails_closed_records_its_fault_before_it_exits() {
    let dir = tempfile::tempdir().unwrap();
    let (session, creator) = session_whose_hook_is_gone(dir.path(), true);

    let report = Run::prog("probe")
        .session_dir(dir.path())
        .env("LIBTIMEWARP__SHARED_FILE", session.to_string_lossy())
        // The choice belongs to the session, which was created with it set; an attacher reads none
        // of the setup variables, so its own environment says nothing here.
        .env("LIBTIMEWARP_FAIL_CLOSED", "0")
        .run_unchecked();

    assert_eq!(report.status.code(), Some(EX_CONFIG));
    assert_eq!(report.stdout, "", "the program ran anyway");
    assert!(
        report.stderr.contains("LIBTIMEWARP_HOOK: "),
        "{}",
        report.stderr
    );

    // The session outlives the process, so what killed it is readable afterwards.
    let fault = only_session(dir.path()).header().fault().expect("a fault");
    assert_ne!(
        fault.pid, creator,
        "the creator was fine; its child was not"
    );
}
