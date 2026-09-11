//! A session of our own and a controller attached to it, which every case in this crate's suites
//! starts from.
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test support: a failed assumption should fail the test loudly"
)]

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard, PoisonError};

use common::platform::{clock, default_timebase, genuine_nanos};
use common::session::layout::{Header, Keeper, ParamsValue};
use common::session::{self, Session, Setup, clock_now};
use common::time::{Family, Nanos};
use controller::Controller;

pub(crate) const SECOND: i64 = Nanos::SECOND;
pub(crate) const HOUR: i64 = 3_600 * SECOND;

/// The clock a family tracks by default here. FreeBSD picks between two columns of the family
/// table by the running kernel's version, so a case that names a family's clock asks for it rather
/// than spelling it.
pub(crate) fn timebase_of(family: Family) -> libc::clockid_t {
    default_timebase(family).unwrap_or(clock::REALTIME)
}

/// Every case takes this, for two reasons. A fork while another thread of this process is inside a
/// transaction would hand the child the in-process half of the write lock already held, and the
/// child would wait on it forever. And the lock file a reader probes is named once per process.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

pub(crate) fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A session of our own, and a controller attached to it by its path the way any other process
/// would attach.
pub(crate) struct Fixture {
    _dir: tempfile::TempDir,
    pub(crate) session: Session,
    pub(crate) controller: Controller,
}

pub(crate) fn fixture_on(keeper: Keeper) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let session = session::create(&Setup {
        keeper,
        ..Setup::plain(dir.path().join("libtimewarp"))
    })
    .unwrap();
    let controller = Controller::attach(session.path()).unwrap();
    Fixture {
        _dir: dir,
        session,
        controller,
    }
}

pub(crate) fn fixture() -> Fixture {
    fixture_on(Keeper::Affine)
}

impl Fixture {
    pub(crate) fn header(&self) -> &Header {
        self.session.header()
    }

    pub(crate) fn live(&self) -> ParamsValue {
        self.header().live.load()
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.session.path().to_path_buf()
    }

    /// The directory the session file sits in, which is where a case plants a file of its own.
    pub(crate) fn dir(&self) -> PathBuf {
        self.session
            .path()
            .parent()
            .expect("a session file sits in a directory")
            .to_path_buf()
    }

    /// How many times this session has been published: the sequence counter, which one write
    /// section moves by exactly two.
    pub(crate) fn counter(&self) -> u64 {
        self.header().seqlock.sequence.load(Ordering::Acquire)
    }

    pub(crate) fn showing(&self, clock: libc::clockid_t) -> Nanos {
        clock_now(self.header(), clock).unwrap()
    }
}

pub(crate) fn genuine(clock: libc::clockid_t) -> Nanos {
    genuine_nanos(clock).unwrap()
}
