//! The line buffer: a fixed slab of the caller's stack, filled with `write!` and drained by one
//! `write(2)`.

use std::fmt;

/// How long a line may get, newline included. A longer message is cut at a character boundary.
pub const CAPACITY: usize = 512;

/// A line under assembly. Nothing here allocates, locks, or fails: a message too long for the slab
/// is truncated, because losing the tail of a diagnostic beats refusing to report it.
pub struct Line {
    bytes: [u8; CAPACITY],
    len: usize,
}

impl Line {
    /// An empty line.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: [0; CAPACITY],
            len: 0,
        }
    }

    /// The assembled bytes, newline included. The last byte is always the newline: the buffer
    /// keeps room for it whatever the message does.
    pub fn finish(&mut self) -> &[u8] {
        if let Some(slot) = self.bytes.get_mut(self.len) {
            *slot = b'\n';
            self.len += 1;
        }
        self.bytes.get(..self.len).unwrap_or_default()
    }

    /// What the line says so far, without its newline.
    #[must_use]
    pub fn text(&self) -> &str {
        // The buffer is filled from `&str` at character boundaries alone, so its prefix is valid.
        self.bytes
            .get(..self.len)
            .and_then(|filled| std::str::from_utf8(filled).ok())
            .unwrap_or_default()
    }
}

impl Default for Line {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Line").field(&self.text()).finish()
    }
}

impl fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // One byte is held back for the newline `finish` appends, so it can never be crowded out.
        let mut take = CAPACITY
            .saturating_sub(1)
            .saturating_sub(self.len)
            .min(s.len());
        while take > 0 && !s.is_char_boundary(take) {
            take -= 1;
        }
        if let (Some(slot), Some(head)) = (
            self.bytes.get_mut(self.len..self.len.saturating_add(take)),
            s.get(..take),
        ) {
            slot.copy_from_slice(head.as_bytes());
            self.len += take;
        }
        Ok(())
    }
}
