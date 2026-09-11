//! The stable code every controller call answers with, and the message beside it.

use std::io;

use common::session::SessionError;
use common::session::layout::BadHeader;
use common::time::Family;

/// Why a controller call did not do what it was asked.
///
/// The variant is the stable part: the message beside it names the particular value or file and
/// may be reworded, the code may not.
#[derive(Debug)]
pub enum Error {
    /// No session file at that path.
    NoSession,
    /// The file exists and fails the checks a session file must pass.
    BadSessionFile(BadHeader),
    /// A value out of range: a negative rate, a rate above the ceiling, a value outside the range
    /// an instant can hold, or an operation the family has no meaning for.
    BadArgument(&'static str),
    /// A refused backwards move on Monotonic or Boottime.
    WouldGoBackwards(Family),
    /// A change on a null session. Every change goes through a transaction, and the null keeper
    /// has no parameters to change.
    UnsupportedForKeeper,
    /// The file or its directory is owned by another uid, or is the wrong kind of file.
    NotOurs(&'static str),
    /// Anything else the operating system refused.
    Io(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSession => f.write_str("no session file at that path"),
            Self::BadSessionFile(why) => write!(f, "{why}"),
            Self::BadArgument(what) | Self::NotOurs(what) => f.write_str(what),
            Self::WouldGoBackwards(family) => {
                write!(f, "{family} refuses a move to a value it has already shown")
            }
            Self::UnsupportedForKeeper => {
                f.write_str("a session on the null keeper has no parameters to change")
            }
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<SessionError> for Error {
    /// The attach failures a target meets, under the names a controller reports them by.
    ///
    /// A collision and a bad value belong to creating a session, which a controller never does, so
    /// both land as [`Error::Io`] rather than as a code of their own.
    fn from(err: SessionError) -> Self {
        match err {
            SessionError::NoSession => Self::NoSession,
            SessionError::BadSessionFile(why) => Self::BadSessionFile(why),
            SessionError::NotOurs(what) => Self::NotOurs(what),
            SessionError::Io(err) => Self::Io(err),
            other => Self::Io(io::Error::other(other.to_string())),
        }
    }
}
