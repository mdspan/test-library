//! The seven codes a controller call answers with, each raised where the chapter says it is, and
//! each carrying a message.
//!
//! Creating a session reads a genuine clock and the system's randomness, so these cases need a
//! filled-in platform arm.
#![cfg(any(target_os = "linux", target_os = "freebsd"))]
#![expect(
    dead_code,
    reason = "test support: not every helper of the shared fixture is used by every case"
)]
#![expect(
    clippy::unwrap_used,
    reason = "test support: a failed assumption should fail the test loudly"
)]
#![expect(
    clippy::significant_drop_tightening,
    reason = "a transaction is consumed by `commit` or `abort`, which is where its lock is \
              released; the lint does not see through a method taking `self` and suggests a drop \
              that would not compile"
)]

mod fixture;

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use common::session::layout::{BadHeader, Keeper};
use common::time::{Family, Nanos};
use controller::{Code, Controller, Error};

use fixture::{SECOND, fixture, fixture_on, one_at_a_time};

/// Every code, with the message beside it, so a case naming one has also shown that it is a code
/// of ours rather than a number.
#[track_caller]
fn refusal(err: &Error, code: Code) {
    assert_eq!(err.code(), code, "{err}");
    assert!(!err.to_string().is_empty(), "{code:?} carries no message");
}

fn mkfifo(path: &Path) {
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // Safety: `c_path` is a live NUL-terminated path in a directory of ours.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
}

#[test]
fn no_session_names_a_path_holding_nothing() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let missing = fixture.dir().join("pid-1-nothing.session");

    let err = Controller::attach(&missing).unwrap_err();
    refusal(&err, Code::NoSession);
    assert!(matches!(err, Error::NoSession));
}

#[test]
fn bad_session_file_names_a_file_that_fails_the_header_checks() {
    let _one = one_at_a_time();
    let fixture = fixture();

    // Long enough to map, and not a session of ours.
    let garbage = fixture.dir().join("pid-2-garbage.session");
    std::fs::write(&garbage, vec![0u8; 64 * 1024]).unwrap();
    let err = Controller::attach(&garbage).unwrap_err();
    refusal(&err, Code::BadSessionFile);
    assert!(matches!(err, Error::BadSessionFile(BadHeader::Magic)));

    // Shorter than the header it claims to carry, which is the other shape of the same refusal.
    let stub = fixture.dir().join("pid-3-stub.session");
    std::fs::write(&stub, b"TIMEWARP").unwrap();
    refusal(
        &Controller::attach(&stub).unwrap_err(),
        Code::BadSessionFile,
    );
}

#[test]
fn not_ours_names_a_file_of_the_wrong_kind() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let fifo = fixture.dir().join("pid-4-fifo.session");
    mkfifo(&fifo);

    let err = Controller::attach(&fifo).unwrap_err();
    refusal(&err, Code::NotOurs);
    assert!(matches!(err, Error::NotOurs(_)));
}

#[test]
fn io_names_a_step_the_operating_system_refused() {
    let _one = one_at_a_time();
    let fixture = fixture();

    // A planted symlink: the open carries `O_NOFOLLOW`, so the kernel refuses the path itself.
    // That says nothing about any contents, which is what leaves it here rather than under
    // `BadSessionFile`.
    let link = fixture.dir().join("pid-5-link.session");
    std::os::unix::fs::symlink(fixture.path(), &link).unwrap();

    let err = Controller::attach(&link).unwrap_err();
    refusal(&err, Code::Io);
    assert!(matches!(err, Error::Io(_)));
}

#[test]
fn unsupported_for_keeper_names_a_change_on_a_null_session() {
    let _one = one_at_a_time();
    let fixture = fixture_on(Keeper::Null);

    let err = fixture.controller.begin().unwrap_err();
    refusal(&err, Code::UnsupportedForKeeper);
    assert!(matches!(err, Error::UnsupportedForKeeper));
}

#[test]
fn bad_argument_names_a_value_the_family_will_not_take() {
    let _one = one_at_a_time();
    let fixture = fixture();

    // A read of the family that has no single value to show.
    refusal(
        &fixture.controller.get(Family::Cpu).unwrap_err(),
        Code::BadArgument,
    );

    let mut transaction = fixture.controller.begin().unwrap();
    // Cpu's rate is fixed at creation, so all three operations on it are refused.
    refusal(
        &transaction.shift(Family::Cpu, Nanos(SECOND)).unwrap_err(),
        Code::BadArgument,
    );
    refusal(
        &transaction
            .set_time(Family::Cpu, Nanos(SECOND))
            .unwrap_err(),
        Code::BadArgument,
    );
    refusal(
        &transaction.set_rate(Family::Cpu, 2.0).unwrap_err(),
        Code::BadArgument,
    );
    // A rate the fixed point cannot carry.
    for rate in [-1.0, f64::NAN, 4.3e9] {
        refusal(
            &transaction.set_rate(Family::Realtime, rate).unwrap_err(),
            Code::BadArgument,
        );
    }
    // A Realtime value outside the range an instant holds, which the commit is where it is judged.
    transaction
        .shift(Family::Realtime, Nanos(i64::MAX))
        .unwrap();
    refusal(&transaction.commit().unwrap_err(), Code::BadArgument);
}

#[test]
fn would_go_backwards_names_a_refused_move_on_a_nondecreasing_family() {
    let _one = one_at_a_time();
    let fixture = fixture();

    for family in [Family::Monotonic, Family::Boottime] {
        let mut transaction = fixture.controller.begin().unwrap();
        transaction.shift(family, Nanos(-SECOND)).unwrap();
        let err = transaction.commit().unwrap_err();
        refusal(&err, Code::WouldGoBackwards);
        assert!(matches!(err, Error::WouldGoBackwards(refused) if refused == family));
    }
    // Realtime takes a move into the past, so the refusal belongs to the other two alone.
    let mut transaction = fixture.controller.begin().unwrap();
    transaction.shift(Family::Realtime, Nanos(-SECOND)).unwrap();
    transaction.commit().unwrap();
}
