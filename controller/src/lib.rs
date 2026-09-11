//! The controller: how a session changes after it is created.
//!
//! A change is a [transaction](Transaction). [`Controller::begin`] takes the write lock and does
//! nothing else, operations are staged on the transaction in order, and `commit` runs one write
//! section: it samples the anchor inside the section, applies the staged operations against it,
//! and publishes once. Every process in the session sees all of the change or none of it, on its
//! next read.
//!
//! This is the primitive layer. Anything with memory, like a freeze that remembers the rate to
//! come back to, belongs one level up, in the program that drives it.

mod error;
mod transaction;

use std::path::Path;

use common::session::{self, Session, naming};

pub use error::Error;
pub use transaction::Transaction;

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
