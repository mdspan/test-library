//! The bytes of the session file, one layout on every platform.
//!
//! The header is one *block*, not literally one page: the 4096-byte hook path the layout table
//! mandates pushes it past a 4 KiB page, so the block is the header rounded up to a page multiple
//! and the arena, when the session has one, follows it. Every offset below is derived, never
//! assumed.

use std::sync::atomic::{AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};

use crate::platform::clock_id;
use crate::time::{Line, Nanos, Rate};

/// The first eight bytes of every session file.
pub const MAGIC: [u8; 8] = *b"TIMEWARP";

/// Bumped on any layout change.
pub const ABI_VERSION: u32 = 2;

/// Long enough for `PATH_MAX`, so an attacher with a stripped environment can still load the hook.
pub const HOOK_PATH_MAX: usize = 4096;

/// Apple silicon pairs 128-byte lines, and x86 prefetches in pairs of 64, so 128 is the width that
/// keeps the seqlock words and each copy of the parameters off each other's line everywhere.
pub const CACHE_LINE: usize = 128;

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

    /// The keeper `text` names, or `None` when it names none.
    #[must_use]
    pub fn from_name(text: &str) -> Option<Self> {
        let text = text.trim();
        [Self::Affine, Self::Null]
            .into_iter()
            .find(|keeper| keeper.as_str().eq_ignore_ascii_case(text))
    }

    /// The name a user writes for this keeper.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Affine => "affine",
            Self::Null => "null",
        }
    }
}

/// What a [`Fault`] says went wrong.
///
/// One code covers everything this library can fail to set up, because the outcome is the same
/// one: the null keeper with no hook. The log line beside it names the variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FaultCode {
    /// The environment asked for something the library could not set up.
    BadValue = 1,
}

impl FaultCode {
    /// The code `value` names, or `None` when it names none.
    #[must_use]
    pub const fn from_code(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::BadValue),
            _ => None,
        }
    }
}

impl std::fmt::Display for FaultCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadValue => f.write_str("a value the library could not use"),
        }
    }
}

/// Why a session stopped faking: a [`FaultCode`] and the pid that hit it.
///
/// The pair lives in one 64-bit word, the code in the low half and the pid in the high one, so it
/// is written in a single store and a crash mid-write leaves a whole value behind. Zero is not a
/// fault: it is what the slot holds while the session is still faking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    /// What went wrong.
    pub code: FaultCode,
    /// The process that hit it.
    pub pid: u32,
}

impl Fault {
    /// The fault `pid` hit.
    #[must_use]
    pub const fn new(code: FaultCode, pid: u32) -> Self {
        Self { code, pid }
    }

    /// The word the fault slot holds.
    #[must_use]
    pub const fn word(self) -> u64 {
        ((self.pid as u64) << u32::BITS) | self.code as u64
    }

    /// The fault a slot's word names, or `None` when it names none.
    #[must_use]
    pub fn from_word(word: u64) -> Option<Self> {
        Some(Self {
            code: FaultCode::from_code(u32::try_from(word & u64::from(u32::MAX)).ok()?)?,
            pid: u32::try_from(word >> u32::BITS).ok()?,
        })
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} in pid {}", self.code, self.pid)
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

    /// The line and its timebase together, read word by word like [`FamilyLine::load`].
    #[must_use]
    pub fn value(&self) -> FamilyValue {
        FamilyValue {
            line: self.load(),
            timebase: self.timebase(),
        }
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

/// One family's parameters off shared memory, whole: the line and the clock it tracks passage
/// against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilyValue {
    /// The line the family projects along.
    pub line: Line,
    /// The genuine clock passage is measured against.
    pub timebase: libc::clockid_t,
}

/// A whole copy of the parameters, off shared memory.
///
/// This is what a writer builds at W6 and what the reader's validation covers: the words are read
/// or written one at a time, and the seqlock around them is what makes the copy whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamsValue {
    /// The Realtime family, the one a wall clock reads.
    pub realtime: FamilyValue,
    /// The Monotonic family.
    pub monotonic: FamilyValue,
    /// The Boottime family, which counts suspended time too.
    pub boottime: FamilyValue,
    /// How fast the process clocks advance.
    pub cpu_rate: Rate,
    /// Whole seconds by which genuine `CLOCK_TAI` leads genuine `CLOCK_REALTIME`.
    pub tai_offset: i64,
    /// Bumped by `set_time`, left alone by a rate change.
    pub epoch: u64,
}

impl ParamsValue {
    /// The three families that carry a line, in the family table's order.
    pub const fn lines_mut(&mut self) -> [&mut FamilyValue; 3] {
        [&mut self.realtime, &mut self.monotonic, &mut self.boottime]
    }

    /// The same three, to read.
    #[must_use]
    pub const fn lines(&self) -> [&FamilyValue; 3] {
        [&self.realtime, &self.monotonic, &self.boottime]
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
    /// A discontinuity counter in the published parameters: bumped by `set_time`, left alone by a
    /// rate change, so a reader can tell a deliberate step from continuous motion.
    pub epoch: AtomicU64,
    /// Alignment only; the layout puts nothing here.
    pub padding: u64,
}

impl Params {
    /// Every word of the copy, read one at a time. Ordering comes from the seqlock around it.
    #[must_use]
    pub fn load(&self) -> ParamsValue {
        ParamsValue {
            realtime: self.realtime.value(),
            monotonic: self.monotonic.value(),
            boottime: self.boottime.value(),
            cpu_rate: Rate(self.cpu_rate.load(Ordering::Relaxed)),
            tai_offset: self.tai_offset.load(Ordering::Relaxed),
            epoch: self.epoch.load(Ordering::Relaxed),
        }
    }

    /// Writes every word of the copy, one at a time. The release that publishes them is the
    /// seqlock's, not one of these stores.
    pub fn store(&self, value: &ParamsValue) {
        for (slot, family) in [
            (&self.realtime, &value.realtime),
            (&self.monotonic, &value.monotonic),
            (&self.boottime, &value.boottime),
        ] {
            slot.store(family.line, family.timebase);
        }
        self.cpu_rate.store(value.cpu_rate.0, Ordering::Relaxed);
        self.tai_offset.store(value.tai_offset, Ordering::Relaxed);
        self.epoch.store(value.epoch, Ordering::Relaxed);
    }
}

/// Who is inside the write section, and whether the shadow holds a whole copy of what that write
/// intends to publish.
///
/// One machine word, so a reader loads the pair atomically: the pid in the high half and the flag
/// in bit 0, with the bits between them unused. Zero is no writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WriterState {
    /// The process inside the section. The lock, not this, decides liveness; the pid is for the
    /// log line and the fault record.
    pub pid: u32,
    /// Whether the shadow holds a whole copy of the value this write intends to publish.
    pub shadow_complete: bool,
}

impl WriterState {
    /// The word a section opens with: the writer's own pid, shadow-complete clear.
    #[must_use]
    pub const fn writing(pid: u32) -> Self {
        Self {
            pid,
            shadow_complete: false,
        }
    }

    /// The same writer, with the shadow now whole.
    #[must_use]
    pub const fn published(pid: u32) -> Self {
        Self {
            pid,
            shadow_complete: true,
        }
    }

    /// The word the slot holds.
    #[must_use]
    pub const fn word(self) -> u64 {
        ((self.pid as u64) << u32::BITS) | self.shadow_complete as u64
    }

    /// The state a word carries. Every bit pattern is a state, so a torn or stale word decodes
    /// rather than failing: the reader's recheck is what catches one.
    #[must_use]
    pub const fn from_word(word: u64) -> Self {
        Self {
            // The high half is exactly a pid's width, so the shift cannot lose one.
            pid: (word >> u32::BITS) as u32,
            shadow_complete: word & 1 == 1,
        }
    }
}

/// The seqlock's two words, together on a line of their own.
#[derive(Debug)]
#[repr(C, align(128))]
pub struct SeqBlock {
    /// Odd while a write is in flight, even between writes.
    pub sequence: AtomicU64,
    /// The [`WriterState`] word: who is writing, and whether the shadow is whole.
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

/// Where the arena sits: directly after the header block. Only meaningful when the session has a
/// hook.
#[must_use]
pub const fn arena_offset(page_size: usize) -> usize {
    header_block_size(page_size)
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

    /// Records `fault`, and says whether this one took the slot.
    ///
    /// First fault wins: the slot keeps it for the session's life, and later faults in the same
    /// session stay in the log. Only a process attaching or creating comes through here; a clock
    /// read never does, because the read hot path writes no shared memory at all.
    pub fn record_fault(&self, fault: Fault) -> bool {
        self.fault
            .compare_exchange(0, fault.word(), Ordering::Release, Ordering::Relaxed)
            .is_ok()
    }

    /// The fault this session carries, or `None` while it is still faking.
    #[must_use]
    pub fn fault(&self) -> Option<Fault> {
        Fault::from_word(self.fault.load(Ordering::Acquire))
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
        assert_eq!(offset_of!(Header, hook_path), 72);
        assert_eq!(offset_of!(Header, fault), 72 + HOOK_PATH_MAX);
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
    fn a_keeper_is_named_the_way_the_variable_spells_it() {
        assert_eq!(Keeper::from_name("affine"), Some(Keeper::Affine));
        assert_eq!(Keeper::from_name(" NULL "), Some(Keeper::Null));
        assert_eq!(Keeper::from_name("genuine"), None);
        assert_eq!(Keeper::from_name(""), None);
        for keeper in [Keeper::Affine, Keeper::Null] {
            assert_eq!(Keeper::from_name(keeper.as_str()), Some(keeper));
        }
    }

    #[test]
    fn a_fault_word_carries_the_code_low_and_the_pid_above() {
        let fault = Fault::new(FaultCode::BadValue, 4321);
        assert_eq!(
            fault.word() & u64::from(u32::MAX),
            FaultCode::BadValue as u64
        );
        assert_eq!(fault.word() >> u32::BITS, 4321);
        assert_eq!(Fault::from_word(fault.word()), Some(fault));
    }

    #[test]
    fn zero_is_not_a_fault_and_neither_is_a_code_we_do_not_know() {
        assert_eq!(Fault::from_word(0), None);
        assert_eq!(Fault::from_word(u64::from(u32::MAX)), None);
        // A pid of zero is still a whole fault: it is the code alone that says there is one.
        assert_eq!(
            Fault::from_word(FaultCode::BadValue as u64),
            Some(Fault::new(FaultCode::BadValue, 0))
        );
    }

    #[test]
    fn a_writer_state_word_carries_the_pid_above_and_the_flag_in_bit_zero() {
        let writing = WriterState::writing(4321);
        assert_eq!(writing.word() >> u32::BITS, 4321);
        assert_eq!(writing.word() & 1, 0);
        assert_eq!(WriterState::published(4321).word() & 1, 1);
        for state in [writing, WriterState::published(4321)] {
            assert_eq!(WriterState::from_word(state.word()), state);
        }
    }

    #[test]
    fn a_cleared_writer_state_word_is_zero_and_names_nobody() {
        assert_eq!(WriterState::default().word(), 0);
        assert_eq!(WriterState::from_word(0), WriterState::writing(0));
        // The bits between the pid and the flag are unused, so a word carrying them still decodes.
        let padded = WriterState::published(7).word() | 0x0f0;
        assert_eq!(WriterState::from_word(padded), WriterState::published(7));
    }

    #[test]
    fn the_pieces_are_page_aligned_and_do_not_overlap() {
        for page in [4096usize, 16384] {
            assert_eq!(header_block_size(page) % page, 0);
            assert!(header_block_size(page) >= size_of::<Header>());
            assert_eq!(arena_offset(page), header_block_size(page));
            assert_eq!(file_size(page, 0), arena_offset(page));
            assert_eq!(file_size(page, 1), arena_offset(page) + page);
        }
    }
}
