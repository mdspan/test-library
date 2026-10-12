//! Why a run did not do what it was asked, and the status it exits with.

use std::io::{self, Write};
use std::process::ExitCode;

use controller::Code;

use crate::args::{SYNOPSIS, Usage};

/// Why a run stopped short.
#[derive(Debug)]
pub(crate) enum Failure {
    /// The command line did not parse.
    Usage(String),
    /// The controller refused.
    Refused(controller::Error),
    /// Anything else, with the message to print.
    Other(String),
    /// Writing the output failed.
    Output(io::Error),
}

impl From<controller::Error> for Failure {
    fn from(err: controller::Error) -> Self {
        Self::Refused(err)
    }
}

impl From<Usage> for Failure {
    fn from(Usage(message): Usage) -> Self {
        Self::Usage(message)
    }
}

impl Failure {
    /// Writes the failure to stderr and gives the status to exit with: 2 for usage, 1 otherwise. A
    /// reader that closed stdout ends the run quietly with 0.
    pub(crate) fn report(self) -> ExitCode {
        let mut stderr = io::stderr().lock();
        let (written, status) = match self {
            Self::Usage(message) => (writeln!(stderr, "libtimewarp: {message}\n{SYNOPSIS}"), 2),
            Self::Refused(err) => (
                writeln!(stderr, "libtimewarp: {}: {err}", code_name(err.code())),
                1,
            ),
            Self::Other(message) => (writeln!(stderr, "libtimewarp: {message}"), 1),
            Self::Output(err) if err.kind() == io::ErrorKind::BrokenPipe => {
                return ExitCode::SUCCESS;
            }
            Self::Output(err) => (
                writeln!(stderr, "libtimewarp: writing the output: {err}"),
                1,
            ),
        };
        // Nothing is left to tell when stderr is gone too.
        drop(written);
        ExitCode::from(status)
    }
}

/// The `snake_case` name of a refusal's code.
const fn code_name(code: Code) -> &'static str {
    match code {
        Code::Ok => "ok",
        Code::NoSession => "no_session",
        Code::BadSessionFile => "bad_session_file",
        Code::BadArgument => "bad_argument",
        Code::WouldGoBackwards => "would_go_backwards",
        Code::UnsupportedForKeeper => "unsupported_for_keeper",
        Code::NotOurs => "not_ours",
        Code::Io => "io",
    }
}
