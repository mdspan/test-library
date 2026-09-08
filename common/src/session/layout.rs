//! The bytes of the session file, one layout on every platform.
//!
//! The header is one *block*, not literally one page: the 4096-byte hook path the layout table
//! mandates pushes it past a 4 KiB page, so the block is the header rounded up to a page multiple
//! and the timekeep page follows it. Every offset below is derived, never assumed.

use std::sync::atomic::{AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};

use crate::time::{Line, Nanos, Rate};

/// The first eight bytes of every session file.
pub const MAGIC: [u8; 8] = *b"TIMEWARP";

/// Bumped on any layout change.
pub const ABI_VERSION: u32 = 1;

/// Long enough for `PATH_MAX`, so an attacher with a stripped environment can still load the hook.
pub const HOOK_PATH_MAX: usize = 4096;

/// Apple silicon pairs 128-byte lines, and x86 prefetches in pairs of 64, so 128 is the width that
/// keeps the seqlock words and each copy of the parameters off each other's line everywhere.
pub const CACHE_LINE: usize = 128;

/// The header spells a clock id as a signed 32-bit word, but `clockid_t` is signed on Linux and
/// FreeBSD and unsigned on macOS, so both directions of the conversion go through `TryFrom`. Every
/// id stored comes from the platform's own table, which makes the fallback unreachable.
fn clock_id<U, T: TryFrom<U> + Default>(value: U) -> T {
    T::try_from(value).unwrap_or_default()
}

/// Which timekeeper computes the families' values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Keeper {
    /// The affine keeper: a line per family over a genuine timebase.
    Affine = 1,
    /// The null keeper: every family reads genuine, which is what a fault leaves behind.
    Null = 2,
}

impl Keeper {
    /// The keeper a header's tag names, or `None` when we do not know it.
    #[must_use]
    pub const fn from_tag(tag: u32) -> Option<Self> {
        match tag {
            1 => Some(Self::Affine),
            2 => Some(Self::Null),
            _ => None,
        }
    }
}

/// One family's parameters: the line, and the genuine clock it tracks passage with.
#[derive(Debug)]
#[repr(C)]
pub struct FamilyLine {
    /// What the family shows at `timebase_anchor`.
    pub fake_anchor: AtomicI64,
    /// The genuine reading the anchor was taken against.
    pub timebase_anchor: AtomicI64,
    /// Fake nanoseconds per nanosecond of the timebase, as 32.32 fixed point.
    pub rate: AtomicU64,
    /// The genuine clock id passage is measured against.
    pub timebase: AtomicI32,
    /// Alignment only; the layout puts nothing here.
    pub padding: u32,
}

impl FamilyLine {
    /// The line, read word by word. Ordering comes from the seqlock around it, not from here.
    #[must_use]
    pub fn load(&self) -> Line {
        Line {
            fake_anchor: Nanos(self.fake_anchor.load(Ordering::Relaxed)),
            timebase_anchor: Nanos(self.timebase_anchor.load(Ordering::Relaxed)),
            rate: Rate(self.rate.load(Ordering::Relaxed)),
        }
    }

    /// The genuine clock this family measures passage against.
    #[must_use]
    pub fn timebase(&self) -> libc::clockid_t {
        clock_id(self.timebase.load(Ordering::Relaxed))
    }

    /// Writes the line and its timebase.
    pub fn store(&self, line: Line, timebase: libc::clockid_t) {
        self.fake_anchor
            .store(line.fake_anchor.0, Ordering::Relaxed);
        self.timebase_anchor
            .store(line.timebase_anchor.0, Ordering::Relaxed);
        self.rate.store(line.rate.0, Ordering::Relaxed);
        self.timebase.store(clock_id(timebase), Ordering::Relaxed);
    }
}

/// One whole copy of the parameters, live or shadow.
#[derive(Debug)]
#[repr(C, align(128))]
pub struct Params {
    /// The Realtime family, the one a wall clock reads.
    pub realtime: FamilyLine,
    /// The Monotonic family.
    pub monotonic: FamilyLine,
    /// The Boottime family, which counts suspended time too.
    pub boottime: FamilyLine,
    /// How fast the process clocks advance, as 32.32 fixed point.
    pub cpu_rate: AtomicU64,
    /// Whole seconds by which genuine `CLOCK_TAI` leads genuine `CLOCK_REALTIME`, fixed at creation.
    pub tai_offset: AtomicI64,
    /// Bumped by `set_time`, left alone by a rate change; read by the FreeBSD republisher.
    pub epoch: AtomicU64,
    /// Alignment only; the layout puts nothing here.
    pub padding: u64,
}

/// The seqlock's two words, together on a line of their own.
#[derive(Debug)]
#[repr(C, align(128))]
pub struct SeqBlock {
    /// Odd while a write is in flight, even between writes.
    pub sequence: AtomicU64,
    /// The writing process's pid packed with the shadow-complete flag.
    pub writer_state: AtomicU64,
}

/// Everything at the head of a session file, in the order the layout table gives.
#[derive(Debug)]
#[repr(C)]
pub struct Header {
    /// [`MAGIC`], so a file that is not ours is refused before anything else is believed.
    pub magic: [u8; 8],
    /// [`ABI_VERSION`] of the library that created the file.
    pub abi_version: u32,
    /// Which OS's clock ids the parameters speak, and which architecture wrote them.
    pub platform: u32,
    /// Set last by the creator, checked first by attachers.
    pub ready: AtomicU32,
    /// The [`Keeper`] that computes the families' values.
    pub keeper: u32,
    /// Whether a target that reads the clock through the vDSO is caught.
    pub emulate_vdso: u32,
    /// Whether a target whose setup failed refuses to run.
    pub fail_closed: u32,
    /// The whole file's length, so a truncated mapping is caught.
    pub file_size: u32,
    /// The page size the offsets below were derived with.
    pub page_size: u32,
    /// Where the arena starts, or zero when the session has none.
    pub arena_offset: u64,
    /// How long the arena is, or zero when the session has none.
    pub arena_size: u64,
    /// The pid that created the session.
    pub creator_pid: u32,
    /// Alignment only; the layout puts nothing here.
    pub padding0: u32,
    /// Genuine `CLOCK_REALTIME` when the session was created.
    pub creation_time: i64,
    /// Nanoseconds between republishes of the FreeBSD timekeep page.
    pub timekeep_interval: u64,
    /// The hook library an attacher loads, NUL-terminated. See [`Header::hook_path`].
    pub hook_path: [u8; HOOK_PATH_MAX],
    /// Why the session stopped faking, or zero while it still is.
    pub fault: AtomicU64,
    /// The seqlock guarding [`Header::live`] and [`Header::shadow`].
    pub seqlock: SeqBlock,
    /// The parameters readers project against.
    pub live: Params,
    /// Where a writer builds the next set before publishing it.
    pub shadow: Params,
}

const _: () = {
    assert!(size_of::<FamilyLine>() == 32);
    assert!(size_of::<Params>() == 128);
    assert!(align_of::<Params>() == CACHE_LINE);
    assert!(align_of::<SeqBlock>() == CACHE_LINE);
    assert!(size_of::<Header>() == 4608);
};

/// `value` raised to the next whole `multiple`.
#[must_use]
pub const fn round_up(value: usize, multiple: usize) -> usize {
    value.div_ceil(multiple) * multiple
}

/// The header block: the header rounded up to a whole number of pages.
#[must_use]
pub const fn header_block_size(page_size: usize) -> usize {
    round_up(size_of::<Header>(), page_size)
}

/// Where the timekeep page sits. Zero and unread on Linux and macOS.
#[must_use]
pub const fn timekeep_offset(page_size: usize) -> usize {
    header_block_size(page_size)
}

/// Where the arena would sit. Only meaningful when the session has a hook.
#[must_use]
pub const fn arena_offset(page_size: usize) -> usize {
    timekeep_offset(page_size) + page_size
}

/// How long a session file with this page size and arena is.
#[must_use]
pub const fn file_size(page_size: usize, arena_size: usize) -> usize {
    arena_offset(page_size) + round_up(arena_size, page_size)
}

/// Why a file cannot be a session of ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadHeader {
    /// The first eight bytes are not [`MAGIC`].
    Magic,
    /// Another [`ABI_VERSION`], so the layout below the header is not the one we know.
    AbiVersion,
    /// Another platform's clock ids, or another architecture's words.
    Platform,
    /// The creator never set the ready flag.
    NotReady,
    /// A [`Keeper`] tag we do not know.
    UnknownKeeper,
    /// The file is shorter than the header says it is.
    Truncated,
}

impl std::fmt::Display for BadHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Magic => "not a session file",
            Self::AbiVersion => "session file from another ABI version",
            Self::Platform => "session file from another platform",
            Self::NotReady => "session file was never finished",
            Self::UnknownKeeper => "session file names a timekeeper we do not know",
            Self::Truncated => "session file is shorter than its own header",
        };
        f.write_str(text)
    }
}

impl Header {
    /// Checks everything that says the file is a session of ours, ready flag first.
    ///
    /// # Errors
    /// The first [`BadHeader`] that applies.
    pub fn validate(&self, mapped_len: usize, platform: u32) -> Result<(), BadHeader> {
        if mapped_len < size_of::<Self>() {
            return Err(BadHeader::Truncated);
        }
        if self.magic != MAGIC {
            return Err(BadHeader::Magic);
        }
        if self.ready.load(Ordering::Acquire) != 1 {
            return Err(BadHeader::NotReady);
        }
        if self.abi_version != ABI_VERSION {
            return Err(BadHeader::AbiVersion);
        }
        if self.platform != platform {
            return Err(BadHeader::Platform);
        }
        if Keeper::from_tag(self.keeper).is_none() {
            return Err(BadHeader::UnknownKeeper);
        }
        if self.file_size as usize > mapped_len {
            return Err(BadHeader::Truncated);
        }
        Ok(())
    }

    /// The hook path up to its NUL, or the whole field when it has none.
    #[must_use]
    pub fn hook_path(&self) -> &[u8] {
        let end = self
            .hook_path
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(HOOK_PATH_MAX);
        self.hook_path.get(..end).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parameters_are_the_documented_shape() {
        assert_eq!(size_of::<Params>(), 128);
        assert_eq!(size_of::<FamilyLine>(), 32);
        assert_eq!(std::mem::offset_of!(Params, cpu_rate), 96);
        assert_eq!(std::mem::offset_of!(Params, tai_offset), 104);
        assert_eq!(std::mem::offset_of!(Params, epoch), 112);
    }

    #[test]
    fn the_header_carries_every_documented_field_in_order() {
        use std::mem::offset_of;
        assert_eq!(offset_of!(Header, magic), 0);
        assert_eq!(offset_of!(Header, abi_version), 8);
        assert_eq!(offset_of!(Header, platform), 12);
        assert_eq!(offset_of!(Header, ready), 16);
        assert_eq!(offset_of!(Header, keeper), 20);
        assert_eq!(offset_of!(Header, emulate_vdso), 24);
        assert_eq!(offset_of!(Header, fail_closed), 28);
        assert_eq!(offset_of!(Header, file_size), 32);
        assert_eq!(offset_of!(Header, page_size), 36);
        assert_eq!(offset_of!(Header, arena_offset), 40);
        assert_eq!(offset_of!(Header, arena_size), 48);
        assert_eq!(offset_of!(Header, creator_pid), 56);
        assert_eq!(offset_of!(Header, creation_time), 64);
        assert_eq!(offset_of!(Header, timekeep_interval), 72);
        assert_eq!(offset_of!(Header, hook_path), 80);
        assert_eq!(offset_of!(Header, fault), 80 + HOOK_PATH_MAX);
    }

    #[test]
    fn the_seqlock_and_each_copy_of_the_parameters_own_their_cache_line() {
        use std::mem::offset_of;
        for offset in [
            offset_of!(Header, seqlock),
            offset_of!(Header, live),
            offset_of!(Header, shadow),
        ] {
            assert_eq!(offset % CACHE_LINE, 0);
        }
        assert!(offset_of!(Header, live) - offset_of!(Header, seqlock) >= CACHE_LINE);
        assert!(offset_of!(Header, shadow) - offset_of!(Header, live) >= CACHE_LINE);
    }

    #[test]
    fn the_pieces_are_page_aligned_and_do_not_overlap() {
        for page in [4096usize, 16384] {
            assert_eq!(header_block_size(page) % page, 0);
            assert!(header_block_size(page) >= size_of::<Header>());
            assert_eq!(timekeep_offset(page), header_block_size(page));
            assert_eq!(arena_offset(page), timekeep_offset(page) + page);
            assert_eq!(file_size(page, 0), arena_offset(page));
            assert_eq!(file_size(page, 1), arena_offset(page) + page);
        }
    }
}
