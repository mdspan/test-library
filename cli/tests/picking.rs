//! Which session the tool acts on when no `--session` names it: the session directory, `list`,
//! `--dir`, `--creator`, and `--newest`.
#![expect(
    dead_code,
    reason = "not every helper of the fixture is used by every case"
)]

mod fixture;

use std::fs;
use std::os::unix::fs::MetadataExt;

use common::session::layout::Keeper;

use fixture::{Fixture, Tool, one_at_a_time, restamp};

const SECOND: i64 = 1_000_000_000;

fn me() -> String {
    std::process::id().to_string()
}

#[test]
fn the_tool_finds_a_session_by_reading_the_directory() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Affine);
    let tool = session.tool();

    let listed = tool.run(&["list"]).ok();
    assert_eq!(listed.number("count"), 1);
    assert_eq!(
        listed.get("directory"),
        session.session_dir().to_string_lossy()
    );
    assert_eq!(
        listed.get("session.0.path"),
        session.path().to_string_lossy()
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
    let by_creator = tool.run(&["--creator", &me(), "info"]).ok();
    assert_eq!(by_creator.get("session"), session.path().to_string_lossy());
}

#[test]
fn list_prints_every_key_of_each_session_newest_first() {
    let _one = one_at_a_time();
    let mut session = Fixture::on(Keeper::Affine);
    let newer = session.another(Keeper::Null);
    restamp(session.path(), 100, 5 * SECOND);
    restamp(&newer, 200, 7 * SECOND);
    fs::write(session.session_dir().join("notes.txt"), "not a session").expect("a stray file");
    fs::write(session.session_dir().join("junk.session"), "not a session").expect("a bad file");

    let listed = session.tool().run(&["list"]).ok();
    assert_eq!(
        listed.keys(),
        [
            "directory",
            "session.0.path",
            "session.0.creator-pid",
            "session.0.created",
            "session.0.created-ns",
            "session.0.keeper",
            "session.0.fault",
            "session.1.path",
            "session.1.creator-pid",
            "session.1.created",
            "session.1.created-ns",
            "session.1.keeper",
            "session.1.fault",
            "count",
        ]
    );
    assert_eq!(listed.get("session.0.path"), newer.to_string_lossy());
    assert_eq!(listed.number("session.0.creator-pid"), 200);
    assert_eq!(
        listed.get("session.0.created"),
        "1970-01-01T00:00:07.000000000Z"
    );
    assert_eq!(listed.number("session.0.created-ns"), 7 * SECOND);
    assert_eq!(listed.get("session.0.keeper"), "null");
    assert_eq!(
        listed.get("session.1.path"),
        session.path().to_string_lossy()
    );
    assert_eq!(listed.number("session.1.creator-pid"), 100);
    assert_eq!(listed.get("session.1.keeper"), "affine");
    assert_eq!(listed.number("count"), 2);
}

#[test]
fn list_on_an_empty_directory_counts_none() {
    let dir = tempfile::tempdir().expect("a temporary directory");

    let listed = Tool::new()
        .run(&["--dir".as_ref(), dir.path().as_os_str(), "list".as_ref()])
        .ok();
    assert_eq!(listed.get("directory"), dir.path().to_string_lossy());
    assert_eq!(listed.number("count"), 0);
    assert_eq!(listed.keys(), ["directory", "count"]);
}

#[test]
fn the_temp_directory_is_the_one_the_environment_names() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let euid = fs::metadata(dir.path())
        .expect("the directory exists")
        .uid();

    let listed = Tool::new().env("TMPDIR", dir.path()).run(&["list"]).ok();
    assert_eq!(
        listed.get("directory"),
        dir.path()
            .join(format!("libtimewarp-{euid}"))
            .to_string_lossy()
    );
}

#[test]
fn dir_replaces_the_computed_directory() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Affine);
    let elsewhere = tempfile::tempdir().expect("a temporary directory");

    let listed = Tool::new()
        .env("LIBTIMEWARP_SESSION_DIR", elsewhere.path())
        .run(&[
            "--dir".as_ref(),
            session.session_dir().as_os_str(),
            "list".as_ref(),
        ])
        .ok();
    assert_eq!(listed.number("count"), 1);
    assert_eq!(
        listed.get("session.0.path"),
        session.path().to_string_lossy()
    );
}

#[test]
fn the_session_directory_comes_from_the_configuration_file_under_the_environment() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Affine);
    let config = session.dir().join("config.toml");
    fs::write(
        &config,
        format!(
            "session_dir = {:?}\n",
            session.dir().to_str().expect("a UTF-8 path")
        ),
    )
    .expect("the configuration file is written");
    let elsewhere = tempfile::tempdir().expect("a temporary directory");

    let from_file = Tool::new().env("LIBTIMEWARP_CONFIG", &config);
    assert_eq!(
        from_file.run(&["list"]).ok().get("directory"),
        session.session_dir().to_string_lossy()
    );
    assert_eq!(
        from_file.run(&["info"]).ok().get("session"),
        session.path().to_string_lossy()
    );

    let hidden = from_file.env("LIBTIMEWARP_SESSION_DIR", elsewhere.path());
    let listed = hidden.run(&["list"]).ok();
    assert_eq!(
        listed.get("directory"),
        elsewhere.path().join("libtimewarp").to_string_lossy()
    );
    assert_eq!(listed.number("count"), 0);
}

#[test]
fn a_refused_configuration_file_leaves_the_environment_to_say() {
    let _one = one_at_a_time();
    let session = Fixture::on(Keeper::Affine);
    let config = session.dir().join("config.toml");
    fs::write(&config, "session_dir = [").expect("the configuration file is written");

    let listed = session
        .tool()
        .env("LIBTIMEWARP_CONFIG", &config)
        .run(&["list"])
        .ok();
    assert_eq!(
        listed.get("directory"),
        session.session_dir().to_string_lossy()
    );
    assert_eq!(listed.number("count"), 1);
}

#[test]
fn no_session_in_the_directory_names_the_directory() {
    let dir = tempfile::tempdir().expect("a temporary directory");

    let run = Tool::new()
        .env("LIBTIMEWARP_SESSION_DIR", dir.path())
        .run(&["info"])
        .refused(1, "libtimewarp: no session in ");
    assert!(
        run.stderr
            .contains(&*dir.path().join("libtimewarp").to_string_lossy()),
        "{}",
        run.stderr
    );
    assert_eq!(run.stdout, "");
}

#[test]
fn several_sessions_are_listed_rather_than_guessed_at() {
    let _one = one_at_a_time();
    let mut session = Fixture::on(Keeper::Affine);
    let other = session.another(Keeper::Affine);
    restamp(session.path(), 100, 5 * SECOND);
    restamp(&other, 200, 7 * SECOND);

    let run = session.tool().run(&["info"]).refused(1, "2 sessions in ");
    for needle in [
        "--session",
        "--creator",
        "--newest",
        &format!(
            "{} (creator pid 200, created {} ns)",
            other.display(),
            7 * SECOND
        ),
        &format!(
            "{} (creator pid 100, created {} ns)",
            session.path().display(),
            5 * SECOND
        ),
    ] {
        assert!(
            run.stderr.contains(needle),
            "no `{needle}` in:\n{}",
            run.stderr
        );
    }
    assert_eq!(run.stdout, "");
}

#[test]
fn creator_and_newest_narrow_the_directory_down() {
    let _one = one_at_a_time();
    let mut session = Fixture::on(Keeper::Affine);
    let newest = session.another(Keeper::Affine);
    let middle = session.another(Keeper::Affine);
    restamp(session.path(), 100, 5 * SECOND);
    restamp(&middle, 100, 6 * SECOND);
    restamp(&newest, 200, 7 * SECOND);
    let tool = session.tool();
    let picked = |args: &[&str]| tool.run(args).ok().get("session").to_owned();

    assert_eq!(picked(&["--newest", "info"]), newest.to_string_lossy());
    assert_eq!(
        picked(&["--creator", "200", "info"]),
        newest.to_string_lossy()
    );
    assert_eq!(
        picked(&["--creator", "100", "--newest", "info"]),
        middle.to_string_lossy()
    );
    tool.run(&["--creator", "100", "info"])
        .refused(1, "2 sessions in ");
    tool.run(&["--creator", "300", "--newest", "info"])
        .refused(1, "no session in ");
}

#[test]
fn session_wins_over_the_other_picking_options() {
    let _one = one_at_a_time();
    let mut session = Fixture::on(Keeper::Affine);
    let newest = session.another(Keeper::Affine);
    restamp(session.path(), 100, 5 * SECOND);
    restamp(&newest, 200, 7 * SECOND);
    let elsewhere = tempfile::tempdir().expect("a temporary directory");

    let run = session
        .tool()
        .run(&[
            "--newest".as_ref(),
            "--creator".as_ref(),
            "300".as_ref(),
            "--dir".as_ref(),
            elsewhere.path().as_os_str(),
            "--session".as_ref(),
            session.path().as_os_str(),
            "info".as_ref(),
        ])
        .ok();
    assert_eq!(run.get("session"), session.path().to_string_lossy());
}

#[test]
fn a_creator_that_is_not_a_pid_is_a_usage_error() {
    Tool::new()
        .run(&["--creator", "me", "info"])
        .refused(2, "--creator");
    Tool::new().run(&["--creator"]).refused(2, "--creator");
    Tool::new()
        .run(&["--newest=yes", "info"])
        .refused(2, "--newest");
}
