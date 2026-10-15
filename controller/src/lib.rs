//! The controller: how a session changes after it is created.
//!
//! A change is a [transaction](Transaction). [`Controller::begin`] takes the write lock and does
//! nothing else, operations are staged on the transaction in order, and `commit` runs one write
//! section: it samples the anchor inside the section, applies the staged operations against it,
//! and publishes once. Every process in the session sees all of the change or none of it, on its
//! next read.
//!
//! Reads are the other half: [`Controller::get`] is what a family shows now and
//! [`Controller::info`] is how the session was set up, what each family is doing, and what went
//! wrong with it. Both work on a session running the null keeper, where every change is refused.
//!
//! This is the primitive layer. Anything with memory, like a freeze that remembers the rate to
//! come back to, belongs one level up, in the program that drives it. The crate is shared by the
//! interposer's setters and the `libtimewarp` tool.

mod error;
mod info;
mod transaction;

use std::io;
use std::path::Path;

use common::platform::{default_timebase, genuine_nanos};
use common::session::layout::Keeper;
use common::session::{self, Session, clock_now, naming};
use common::time::{Family, Nanos};

pub use error::{Code, Error};
pub use info::{FamilyInfo, Info};
pub use transaction::Transaction;

/// What a family whose clock the kernel will not read is refused with. Nothing but a kernel that
/// has taken the id away reaches it.
const NO_READING: &str = "the family's clock would not read";

/// A session opened for change, by the path of its session file.
///
/// Holding one keeps the session mapped; it says nothing about whether a transaction is in flight,
/// which the write lock alone decides.
#[derive(Debug)]
pub struct Controller {
    session: Session,
}

impl Controller {
    /// Attaches by the path of the session file, applying the same security checks a target
    /// applies: the directory and the file must be ours, the file a regular file, and neither a
    /// planted symlink nor a planted FIFO is followed.
    ///
    /// Names are generated, so there is no name to type: the path comes from
    /// `LIBTIMEWARP__SHARED_FILE` in the environment of any process of the tree, or from reading
    /// the session directory.
    ///
    /// # Errors
    /// [`Error::NoSession`] when the path holds nothing, [`Error::BadSessionFile`] when what it
    /// holds is not a session this library can read, [`Error::NotOurs`] when the file or its
    /// directory belongs to somebody else, and [`Error::Io`] when a step fails outright.
    pub fn attach(path: &Path) -> Result<Self, Error> {
        Ok(Self {
            session: session::attach(path)?,
        })
    }

    /// Where the session file lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.session.path()
    }

    /// What `family` shows now.
    ///
    /// The read path a target takes, so the answer is the one every process of the session is
    /// getting at this instant. A session on the null keeper answers with the genuine reading,
    /// which is what the null keeper hands its own processes.
    ///
    /// # Errors
    /// [`Error::BadArgument`] for Cpu, which has one setting and no single value it shows now,
    /// because each of its clocks scales its own consumption. [`Error::Io`] when the kernel will
    /// not read the family's clock at all.
    pub fn get(&self, family: Family) -> Result<Nanos, Error> {
        let clock =
            default_timebase(family).ok_or(Error::BadArgument(error::CPU_HAS_ONE_SETTING))?;
        let header = self.session.header();
        let showing = match self.keeper() {
            Keeper::Affine => clock_now(header, clock),
            Keeper::Null => genuine_nanos(clock),
        };
        showing.ok_or_else(|| Error::Io(io::Error::other(NO_READING)))
    }

    /// How the session was set up, what each family is doing, and what went wrong with it.
    ///
    /// This is where a [fault](common::session::layout::Fault) is read: a creator that could not
    /// set up what the environment asked for still publishes a session, on the null keeper with no
    /// hook, with the fault recorded in it.
    #[must_use]
    pub fn info(&self) -> Info {
        Info::of(self.session.header())
    }

    /// The keeper this session computes through.
    ///
    /// [`Controller::attach`] refused a tag naming a keeper we do not know, so the tag here always
    /// names one.
    fn keeper(&self) -> Keeper {
        Keeper::from_tag(self.session.header().keeper).unwrap_or(Keeper::Null)
    }

    /// **W1**: takes the write lock, and nothing else.
    ///
    /// The sequence counter stays even, so readers keep reading the old parameters for as long as
    /// the transaction is open and only other writers wait. The wait has no bound: a held lock
    /// means a live writer that will finish.
    ///
    /// # Errors
    /// [`Error::UnsupportedForKeeper`] on a null session, which has no parameters to change, and
    /// [`Error::Io`] when the lock file cannot be opened, is not ours, or cannot be locked.
    pub fn begin(&self) -> Result<Transaction<'_>, Error> {
        Transaction::begin(
            self.session.header(),
            &naming::lock_path(self.session.path()),
        )
    }
}
