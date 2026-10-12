//! The tool's stdout: `key=value` lines, one fact each.

use std::fmt::Display;
use std::io::Write;

use crate::failure::Failure;

/// Where the facts of a run are written.
#[derive(Debug)]
pub(crate) struct Output<W: Write> {
    out: W,
}

impl<W: Write> Output<W> {
    pub(crate) const fn new(out: W) -> Self {
        Self { out }
    }

    /// One fact, on its own line.
    pub(crate) fn say(&mut self, key: &str, value: impl Display) -> Result<(), Failure> {
        writeln!(self.out, "{key}={value}").map_err(Failure::Output)
    }

    /// One fact whose value is written byte for byte, such as a path.
    pub(crate) fn say_bytes(&mut self, key: &str, value: &[u8]) -> Result<(), Failure> {
        let line = [key.as_bytes(), b"=", value, b"\n"].concat();
        self.out.write_all(&line).map_err(Failure::Output)
    }

    /// Text as it is.
    pub(crate) fn text(&mut self, text: &str) -> Result<(), Failure> {
        self.out.write_all(text.as_bytes()).map_err(Failure::Output)
    }

    /// Flushes what is still buffered.
    pub(crate) fn finish(mut self) -> Result<(), Failure> {
        self.out.flush().map_err(Failure::Output)
    }
}
