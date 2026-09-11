//! The lease, the interval a respawned updater keeps, and the two ways an updater learns which
//! session it is for.
//!
//! The lease's own calls are the write lock's, so they work wherever the suite runs; the walk of
//! the processes of a uid is FreeBSD's alone and is left to the suite that runs there.

use super::*;
use crate::session::Setup;

fn lease_file(dir: &Path) -> PathBuf {
    let path = dir.join("pid-1-abc.lease");
    std::fs::write(&path, b"").unwrap();
    path
}

#[test]
fn a_lease_reads_released_until_somebody_takes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = lease_file(tmp.path());
    assert_eq!(lease::held(&path), Some(false));

    let held = lease::Lease::take(&path).expect("nobody holds it");
    assert_eq!(lease::held(&path), Some(true));

    // The kernel releases it with the last reference to the description, which is what makes a
    // holder's death indistinguishable from a holder letting go.
    drop(held);
    assert_eq!(lease::held(&path), Some(false));
}

#[test]
fn two_respawners_racing_leave_one_updater() {
    let tmp = tempfile::tempdir().unwrap();
    let path = lease_file(tmp.path());
    let winner = lease::Lease::take(&path).expect("the first takes it");
    // The loser is an updater that already exec'd; finding the lease taken is what tells it to go.
    assert!(lease::Lease::take(&path).is_err());
    drop(winner);
    assert!(lease::Lease::take(&path).is_ok());
}

#[test]
fn a_lease_nobody_can_open_answers_nothing_rather_than_released() {
    let tmp = tempfile::tempdir().unwrap();
    // A session whose lease file is missing leaves the question unasked, which a caller reads as
    // "leave it alone" rather than as a reason to start a second updater.
    assert_eq!(lease::held(&tmp.path().join("absent.lease")), None);
}

#[test]
fn the_interval_comes_from_the_header_and_stays_inside_what_the_page_survives() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = Setup::plain(tmp.path().join("libtimewarp"));
    setup.timekeep_interval = Nanos(2_000_000);
    let session = crate::session::create(&setup).unwrap();
    assert_eq!(interval_of(session.header()), Nanos(2_000_000));

    // A respawned updater reads the header rather than the environment, so a value nothing would
    // survive is held to the range instead of being taken at its word.
    let header = session.header();
    let mut copy = Setup::plain(tmp.path().join("libtimewarp"));
    copy.timekeep_interval = Nanos(0);
    let frozen = crate::session::create(&copy).unwrap();
    assert_eq!(interval_of(frozen.header()), LEAST_INTERVAL);
    assert!(interval_of(header) <= MOST_INTERVAL);
}

#[test]
fn an_updater_learns_its_session_from_the_environment_or_from_its_arguments() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(*value))
        }
    };

    // The constructor's way, for the host program the fallback execs.
    assert_eq!(
        requested(&env(&[
            (ENV_UPDATER, "1"),
            (ENV_SHARED_FILE, "/run/x.session"),
        ])),
        Some(PathBuf::from("/run/x.session")),
    );
    // A target has the session named but not the updater variable, and must not be taken for one.
    assert_eq!(
        requested(&env(&[(ENV_SHARED_FILE, "/run/x.session")])),
        None
    );
    assert_eq!(requested(&env(&[(ENV_UPDATER, "1")])), None);

    // And `main`'s way, for the library exec'd by its own path.
    let args: Vec<OsString> = ["libtimewarp.so", UPDATER_FLAG, "/run/x.session"]
        .iter()
        .map(OsString::from)
        .collect();
    assert_eq!(
        session_in_args(&args),
        Some(PathBuf::from("/run/x.session"))
    );
    assert_eq!(
        session_in_args(&args[..2]),
        None,
        "a flag with nothing after it"
    );
    assert_eq!(session_in_args(&args[..1]), None);
}

#[test]
fn a_session_with_the_page_off_keeps_no_updater() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = Setup::plain(tmp.path().join("libtimewarp"));
    setup.emulate_vdso = false;
    let session = crate::session::create(&setup).unwrap();

    // Nothing to probe and nothing to start: the lease file is not even there.
    assert!(!naming::lease_path(session.path()).exists());
    ensure(session.path(), session.header());
    assert_eq!(lease::held(&naming::lease_path(session.path())), None);
}
