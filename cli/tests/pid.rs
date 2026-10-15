//! `--pid`: the session a process of the faked tree carries the path of in its environment.
#![expect(
    dead_code,
    reason = "not every helper of the fixture is used by every case"
)]

mod fixture;

use common::session::layout::Keeper;

use fixture::{Fixture, Member};

#[test]
fn the_tool_finds_a_session_through_a_process_of_the_tree() {
    let session = Fixture::on(Keeper::Affine);
    let member = Member::of(session.path());

    let run = session
        .tool()
        .run(&["--pid", &member.pid().to_string(), "info"])
        .ok();
    assert_eq!(run.get("session"), session.path().to_string_lossy());
    assert_eq!(run.get("keeper"), "affine");
    assert_eq!(run.number("creator-pid"), i64::from(std::process::id()));
}

#[test]
fn a_session_path_with_a_space_in_it_is_read_whole() {
    let session = Fixture::spaced(Keeper::Affine);
    assert!(session.path().to_string_lossy().contains(' '));
    let member = Member::of(session.path());

    let run = session
        .tool()
        .run(&["--pid", &member.pid().to_string(), "info"])
        .ok();
    assert_eq!(run.get("session"), session.path().to_string_lossy());
}

#[test]
fn a_process_without_the_variable_is_in_no_session() {
    let session = Fixture::on(Keeper::Affine);
    let member = Member::outside();
    let pid = member.pid().to_string();

    session
        .tool()
        .run(&["--pid", &pid, "info"])
        .refused(1, &format!("pid {pid} carries no LIBTIMEWARP__SHARED_FILE"))
        .refused(1, "in no session");
}

#[test]
fn a_process_that_is_gone_names_the_pid_and_the_reason() {
    let session = Fixture::on(Keeper::Affine);
    let pid = {
        let member = Member::outside();
        member.pid().to_string()
    };

    let run = session
        .tool()
        .run(&["--pid", &pid, "info"])
        .refused(1, &format!("cannot read the environment of pid {pid}: "));
    assert!(run.stdout.is_empty());
}

#[test]
fn session_wins_over_pid() {
    let mut session = Fixture::on(Keeper::Affine);
    let other = session.another(Keeper::Affine);
    let member = Member::of(&other);

    let run = session
        .tool()
        .run(&[
            "--pid",
            &member.pid().to_string(),
            "--session",
            &session.path().to_string_lossy(),
            "info",
        ])
        .ok();
    assert_eq!(run.get("session"), session.path().to_string_lossy());
}

#[test]
fn pid_wins_over_the_directory() {
    let mut session = Fixture::on(Keeper::Affine);
    let other = session.another(Keeper::Affine);
    let member = Member::of(&other);

    let run = session
        .tool()
        .run(&["--newest", "--pid", &member.pid().to_string(), "info"])
        .ok();
    assert_eq!(run.get("session"), other.to_string_lossy());
}

#[test]
fn pid_takes_a_process_id() {
    fixture::run(&["--pid", "me", "info"]).refused(2, "--pid takes a process id, not me");
    fixture::run(&["--pid"]).refused(2, "--pid takes a value");
}
