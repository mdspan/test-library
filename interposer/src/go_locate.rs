//! Recognising the Go runtime's two clock fallback routines in a target by their own bytes, and
//! rewriting each entry to jump into the library.
//!
//! `runtime.fallback_nanotime` and `runtime.fallback_walltime` are hand-written, relocation-free
//! assembly, the same source from Go 1.21 through 1.27 but not the same bytes: the assembler of
//! one release encodes an instruction of it differently from another's. So each routine is matched,
//! whole body, against every encoding a release in that span gives it, never by a symbol a stripped
//! binary drops nor a struct offset a release moves. A whole body, anchored on the `clock_gettime`
//! number and the clock id, does not collide elsewhere in a text section, and a match fixes the
//! frame and the result slots the shims in [`crate::go_shim`] are written for. Exactly one hit
//! across every encoding is demanded for each routine; zero or more than one is treated as not
//! found, and the target reads genuine time.
//!
//! The rewrite overwrites the entry with a far absolute jump to the matching shim: on amd64 a
//! 14-byte `jmp [rip+0]` and the address, clobbering nothing; on arm64 a 16-byte `ldr x16`/`br x16`
//! veneer, clobbering only the scratch register a call entry leaves dead. The text is made
//! writable, written, and set back to execute without ever being both at once, so a kernel refusing
//! simultaneous write and execute is handled; the original bytes are saved first so a later
//! failure restores them.
//!
//! A program whose routines are not recognised may still be Go's, built by a release that encodes
//! them some other way. The build information the Go linker writes into every program says so, and
//! names the release.

/// One architecture's encodings of the two routines, each the whole body through its `RET`.
#[derive(Debug)]
struct Routines {
    /// How far a scan steps between candidate offsets, so a match lands on an instruction
    /// boundary.
    step: usize,
    /// Every encoding of `runtime.fallback_nanotime`.
    nanotime: &'static [&'static [u8]],
    /// Every encoding of `runtime.fallback_walltime`.
    walltime: &'static [&'static [u8]],
}

/// The two routines on amd64. Go 1.21 to 1.26 assemble the move of the clock id into `DI` as the
/// seven-byte `MOVQ`, and Go 1.27 as the five-byte `MOVL`.
#[cfg(any(test, target_arch = "x86_64"))]
const AMD64: Routines = Routines {
    step: 1,
    nanotime: &[
        &[
            0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x20, 0xb8, 0xe8, 0x00, 0x00, 0x00, 0x48,
            0xc7, 0xc7, 0x04, 0x00, 0x00, 0x00, 0x48, 0x8d, 0x74, 0x24, 0x08, 0x0f, 0x05, 0x48,
            0x8b, 0x44, 0x24, 0x08, 0x48, 0x8b, 0x54, 0x24, 0x10, 0x48, 0x69, 0xc0, 0x00, 0xca,
            0x9a, 0x3b, 0x48, 0x01, 0xd0, 0x48, 0x89, 0x44, 0x24, 0x30, 0x48, 0x83, 0xc4, 0x20,
            0x5d, 0xc3,
        ],
        &[
            0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x20, 0xb8, 0xe8, 0x00, 0x00, 0x00, 0xbf,
            0x04, 0x00, 0x00, 0x00, 0x48, 0x8d, 0x74, 0x24, 0x08, 0x0f, 0x05, 0x48, 0x8b, 0x44,
            0x24, 0x08, 0x48, 0x8b, 0x54, 0x24, 0x10, 0x48, 0x69, 0xc0, 0x00, 0xca, 0x9a, 0x3b,
            0x48, 0x01, 0xd0, 0x48, 0x89, 0x44, 0x24, 0x30, 0x48, 0x83, 0xc4, 0x20, 0x5d, 0xc3,
        ],
    ],
    walltime: &[
        &[
            0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x20, 0xb8, 0xe8, 0x00, 0x00, 0x00, 0x48,
            0xc7, 0xc7, 0x00, 0x00, 0x00, 0x00, 0x48, 0x8d, 0x74, 0x24, 0x08, 0x0f, 0x05, 0x48,
            0x8b, 0x44, 0x24, 0x08, 0x48, 0x8b, 0x54, 0x24, 0x10, 0x48, 0x89, 0x44, 0x24, 0x30,
            0x89, 0x54, 0x24, 0x38, 0x48, 0x83, 0xc4, 0x20, 0x5d, 0xc3,
        ],
        &[
            0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x20, 0xb8, 0xe8, 0x00, 0x00, 0x00, 0xbf,
            0x00, 0x00, 0x00, 0x00, 0x48, 0x8d, 0x74, 0x24, 0x08, 0x0f, 0x05, 0x48, 0x8b, 0x44,
            0x24, 0x08, 0x48, 0x8b, 0x54, 0x24, 0x10, 0x48, 0x89, 0x44, 0x24, 0x30, 0x89, 0x54,
            0x24, 0x38, 0x48, 0x83, 0xc4, 0x20, 0x5d, 0xc3,
        ],
    ],
};

/// The bytes a run of arm64 instructions occupies in memory, each word little-endian, written as
/// the words a disassembler prints.
#[cfg(any(test, target_arch = "aarch64"))]
macro_rules! words {
    ($($word:literal),+ $(,)?) => {
        &[$(le_byte($word, 0), le_byte($word, 1), le_byte($word, 2), le_byte($word, 3)),+]
    };
}

/// Byte `index` of `word` as little-endian memory holds it.
#[cfg(any(test, target_arch = "aarch64"))]
const fn le_byte(word: u32, index: usize) -> u8 {
    let [first, second, third, fourth] = word.to_le_bytes();
    match index {
        0 => first,
        1 => second,
        2 => third,
        _ => fourth,
    }
}

/// The two routines on arm64. Go 1.21.0 to 1.21.6 restore the frame pointer after popping the
/// frame, and Go 1.21.7 to 1.27 before.
#[cfg(any(test, target_arch = "aarch64"))]
const ARM64: Routines = Routines {
    step: 4,
    nanotime: &[
        words![
            0xf81d_0ffe,
            0xf81f_83fd,
            0xd100_23fd,
            0xb27e_03e0,
            0x9100_23e1,
            0xd280_1d08,
            0xd400_0001,
            0xf940_07e0,
            0xb980_13e2,
            0xd299_4003,
            0xf2a7_7343,
            0x9b03_7c00,
            0x8b02_0000,
            0xf900_1fe0,
            0x9100_c3ff,
            0xd100_23fd,
            0xd65f_03c0,
        ],
        words![
            0xf81d_0ffe,
            0xf81f_83fd,
            0xd100_23fd,
            0xb27e_03e0,
            0x9100_23e1,
            0xd280_1d08,
            0xd400_0001,
            0xf940_07e0,
            0xb980_13e2,
            0xd299_4003,
            0xf2a7_7343,
            0x9b03_7c00,
            0x8b02_0000,
            0xf900_1fe0,
            0x9100_a3fd,
            0x9100_c3ff,
            0xd65f_03c0,
        ],
    ],
    walltime: &[
        words![
            0xf81d_0ffe,
            0xf81f_83fd,
            0xd100_23fd,
            0x2a1f_03e0,
            0x9100_23e1,
            0xd280_1d08,
            0xd400_0001,
            0xf940_07e0,
            0xb980_13e1,
            0xf900_1fe0,
            0xb900_43e1,
            0x9100_c3ff,
            0xd100_23fd,
            0xd65f_03c0,
        ],
        words![
            0xf81d_0ffe,
            0xf81f_83fd,
            0xd100_23fd,
            0x2a1f_03e0,
            0x9100_23e1,
            0xd280_1d08,
            0xd400_0001,
            0xf940_07e0,
            0xb980_13e1,
            0xf900_1fe0,
            0xb900_43e1,
            0x9100_a3fd,
            0x9100_c3ff,
            0xd65f_03c0,
        ],
    ],
};

/// How long the entry veneer is: a `jmp [rip+0]` and an absolute address on amd64, an
/// `ldr x16`/`br x16` pair and an absolute address on arm64.
#[cfg(target_arch = "x86_64")]
const VENEER_LEN: usize = 14;
#[cfg(target_arch = "aarch64")]
const VENEER_LEN: usize = 16;

/// How often a routine turned up, and where when it turned up once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hits {
    /// Not at all.
    Absent,
    /// Exactly once, at this offset or address.
    Once(usize),
    /// More than once, which leaves no one entry to rewrite.
    Ambiguous,
}

impl Hits {
    /// What this and `other`, found in code elsewhere, come to together.
    const fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Absent, hits) | (hits, Self::Absent) => hits,
            _ => Self::Ambiguous,
        }
    }

    /// The same hit counted from `base`, or `None` when that runs past the address space.
    fn plus(self, base: usize) -> Option<Self> {
        match self {
            Self::Once(at) => base.checked_add(at).map(Self::Once),
            other => Some(other),
        }
    }
}

/// What a scan found of each routine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Found {
    /// `runtime.fallback_nanotime`.
    nanotime: Hits,
    /// `runtime.fallback_walltime`.
    walltime: Hits,
}

impl Found {
    /// Neither routine.
    const NOTHING: Self = Self {
        nanotime: Hits::Absent,
        walltime: Hits::Absent,
    };

    /// What this and `other`, found in code elsewhere, come to together.
    const fn and(self, other: Self) -> Self {
        Self {
            nanotime: self.nanotime.and(other.nanotime),
            walltime: self.walltime.and(other.walltime),
        }
    }

    /// The same hits counted from `base`, or `None` when that runs past the address space.
    fn plus(self, base: usize) -> Option<Self> {
        Some(Self {
            nanotime: self.nanotime.plus(base)?,
            walltime: self.walltime.plus(base)?,
        })
    }
}

impl Routines {
    /// Scans `code` once for every encoding of both routines, counting offsets from its start.
    fn scan(&self, code: &[u8]) -> Found {
        let mut found = Found::NOTHING;
        let mut at = 0;
        while let Some(rest) = code.get(at..) {
            found = found.and(Found {
                nanotime: hit(rest, self.nanotime, at),
                walltime: hit(rest, self.walltime, at),
            });
            at += self.step;
        }
        found
    }
}

/// [`Hits::Once`] at `at` when `code` starts with one of `bodies`, and [`Hits::Absent`] otherwise.
fn hit(code: &[u8], bodies: &[&[u8]], at: usize) -> Hits {
    // The first byte alone rules out nearly every offset before a whole body is compared.
    let opens = |body: &&[u8]| body.first() == code.first() && code.starts_with(body);
    if bodies.iter().any(opens) {
        Hits::Once(at)
    } else {
        Hits::Absent
    }
}

/// The bytes to write over a routine's entry so a call to it reaches `handler`.
#[cfg(target_arch = "x86_64")]
fn build_veneer(handler: usize) -> [u8; VENEER_LEN] {
    // ff 25 00 00 00 00   jmp qword ptr [rip + 0]
    // <abs64>             the handler
    let mut veneer = [0u8; VENEER_LEN];
    let address = u64::try_from(handler).unwrap_or(0).to_le_bytes();
    if let Some(head) = veneer.get_mut(..2) {
        head.copy_from_slice(&[0xff, 0x25]);
    }
    if let Some(tail) = veneer.get_mut(6..) {
        tail.copy_from_slice(&address);
    }
    veneer
}

/// The bytes to write over a routine's entry so a call to it reaches `handler`.
#[cfg(target_arch = "aarch64")]
fn build_veneer(handler: usize) -> [u8; VENEER_LEN] {
    // 58000050   ldr x16, #8
    // d61f0200   br  x16
    // <abs64>    the handler
    let mut veneer = [0u8; VENEER_LEN];
    let address = u64::try_from(handler).unwrap_or(0).to_le_bytes();
    if let Some(load) = veneer.get_mut(..4) {
        load.copy_from_slice(&0x5800_0050u32.to_le_bytes());
    }
    if let Some(branch) = veneer.get_mut(4..8) {
        branch.copy_from_slice(&0xd61f_0200u32.to_le_bytes());
    }
    if let Some(tail) = veneer.get_mut(8..) {
        tail.copy_from_slice(&address);
    }
    veneer
}

/// What the Go linker starts a program's build information with.
const BUILD_INFO_MAGIC: &[u8] = b"\xff Go buildinf:";

/// The boundary the build information starts on.
const BUILD_INFO_ALIGN: usize = 16;

/// Where the build information's strings start: past the magic, the pointer size, the flags, and
/// two words Go 1.18 and later leave empty.
const BUILD_INFO_STRINGS: usize = 32;

/// The flag saying the strings follow inline, each after its length as a varint, which Go 1.18
/// and later set.
const BUILD_INFO_INLINE: u8 = 0x2;

/// The Go release that built a program, as the program's build information names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GoRelease<'a> {
    /// The release's name, `go1.27.1` and the like.
    Named(&'a str),
    /// A release the build information does not name in a form this reads: one before Go 1.18,
    /// which keeps the name behind a pointer.
    Unnamed,
}

impl std::fmt::Display for GoRelease<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Named(name) => f.write_str(name),
            Self::Unnamed => f.write_str("an unnamed Go release"),
        }
    }
}

/// The Go release that built the program `data` is initialised data of, or `None` when `data`
/// holds no Go build information. `data` starts on a [`BUILD_INFO_ALIGN`] boundary of the program's
/// address space, as the build information does.
fn release_in(data: &[u8]) -> Option<GoRelease<'_>> {
    let info = (0..data.len())
        .step_by(BUILD_INFO_ALIGN)
        .filter_map(|at| data.get(at..))
        .find(|info| {
            info.first() == BUILD_INFO_MAGIC.first() && info.starts_with(BUILD_INFO_MAGIC)
        })?;
    Some(inline_name(info).map_or(GoRelease::Unnamed, GoRelease::Named))
}

/// The release the build information at the start of `info` names inline, when it does so in a
/// form fit to print.
fn inline_name(info: &[u8]) -> Option<&str> {
    let flags = *info.get(BUILD_INFO_MAGIC.len() + 1)?;
    if flags & BUILD_INFO_INLINE == 0 {
        return None;
    }
    // The length is a varint; a name too long for one byte of it is not reported.
    let (&len, rest) = info.get(BUILD_INFO_STRINGS..)?.split_first()?;
    if len & 0x80 != 0 {
        return None;
    }
    let name = std::str::from_utf8(rest.get(..usize::from(len))?).ok()?;
    let printable = name
        .bytes()
        .all(|byte| byte == b' ' || byte.is_ascii_graphic());
    (printable && !name.is_empty()).then_some(name)
}

#[cfg(all(target_os = "freebsd", feature = "vdso"))]
pub(crate) mod freebsd {
    use common::elf::MainImage;
    use common::session::layout::round_up;

    #[cfg(target_arch = "x86_64")]
    use super::AMD64 as HOST;
    #[cfg(target_arch = "aarch64")]
    use super::ARM64 as HOST;
    use super::{BUILD_INFO_ALIGN, Found, GoRelease, Hits, VENEER_LEN, build_veneer, release_in};

    /// The bytes a routine's entry held before a veneer overwrote them, kept so a later failure can
    /// put them back.
    #[derive(Clone, Copy)]
    struct Saved {
        /// The entry the bytes came from.
        at: usize,
        /// The bytes themselves.
        original: [u8; VENEER_LEN],
    }

    /// Why [`interpose`] left the routines as it found them.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Refusal {
        /// The two routines did not each turn up exactly once in an encoding this module matches.
        Unrecognised,
        /// The kernel refused a step of the rewrite.
        Rewrite,
    }

    /// Recognises both Go fallback routines in `image` and rewrites each entry to jump to the
    /// matching handler, remapping the text under W^X.
    ///
    /// All-or-nothing: on any failure every veneer already written is restored and the reason is
    /// returned, so the caller leaves the timekeep entry alone and the target reads genuine time.
    ///
    /// # Safety
    /// `image`'s executable ranges must be this process's own mapped code, and `nanotime` and
    /// `walltime` the shim entry addresses the routines are to jump to.
    pub(crate) unsafe fn interpose(
        image: &MainImage,
        nanotime: usize,
        walltime: usize,
    ) -> Result<(), Refusal> {
        // Safety: the caller guarantees the ranges are this process's code.
        let (nano_at, wall_at) = unsafe { locate(image) }.ok_or(Refusal::Unrecognised)?;
        let patches = [
            (nano_at, build_veneer(nanotime)),
            (wall_at, build_veneer(walltime)),
        ];
        // Safety: the addresses are entries the scan found in this process's own code.
        unsafe { apply(&patches) }.ok_or(Refusal::Rewrite)
    }

    /// The Go release that built this program, read from its build information, or `None` when Go
    /// did not build it.
    ///
    /// # Safety
    /// `image`'s writable ranges must be this process's own mapped data, which nothing writes while
    /// this reads it.
    pub(crate) unsafe fn go_release(image: &MainImage) -> Option<GoRelease<'static>> {
        image.writable_ranges().find_map(|range| {
            let start = range.start.checked_next_multiple_of(BUILD_INFO_ALIGN)?;
            let len = range.end.checked_sub(start)?;
            // Safety: the caller guarantees the range, which stays mapped as long as the process
            // runs.
            let data = unsafe {
                std::slice::from_raw_parts(std::ptr::with_exposed_provenance::<u8>(start), len)
            };
            release_in(data)
        })
    }

    /// Where each routine sits across `image`'s executable ranges, as `(nanotime, walltime)`, or
    /// `None` unless each turned up exactly once.
    ///
    /// # Safety
    /// The ranges must be this process's mapped executable memory.
    unsafe fn locate(image: &MainImage) -> Option<(usize, usize)> {
        let mut found = Found::NOTHING;
        for range in image.executable_ranges() {
            let len = range.end.checked_sub(range.start)?;
            // Safety: the range is mapped executable memory of this process.
            let code = unsafe {
                std::slice::from_raw_parts(
                    std::ptr::with_exposed_provenance::<u8>(range.start),
                    len,
                )
            };
            found = found.and(HOST.scan(code).plus(range.start)?);
        }
        match found {
            Found {
                nanotime: Hits::Once(nanotime),
                walltime: Hits::Once(walltime),
            } => Some((nanotime, walltime)),
            _ => None,
        }
    }
    /// Saves each entry's original bytes, writes the veneers, and verifies the readback. Restores
    /// everything and answers `None` on any failure.
    ///
    /// # Safety
    /// Every address in `patches` must be a routine entry in this process's own text.
    unsafe fn apply(patches: &[(usize, [u8; VENEER_LEN])]) -> Option<()> {
        let mut saved = [Saved {
            at: 0,
            original: [0; VENEER_LEN],
        }; 2];
        for (slot, &(at, _)) in patches.iter().enumerate() {
            // Safety: `at` is an entry in this process's code, `VENEER_LEN` bytes readable.
            let original = unsafe { read_bytes(at) };
            if let Some(entry) = saved.get_mut(slot) {
                *entry = Saved { at, original };
            }
        }
        let saved = saved.get(..patches.len())?;
        // Safety: the addresses are this process's text.
        if unsafe { write_all(patches) }.is_none() {
            // Safety: restore whatever was written from the saved originals.
            unsafe { restore(saved) };
            return None;
        }
        for &(at, veneer) in patches {
            // Safety: `at` is in this process's text, now holding the veneer.
            if unsafe { read_bytes(at) } != veneer {
                // Safety: as above.
                unsafe { restore(saved) };
                return None;
            }
        }
        Some(())
    }

    /// Makes the pages spanning the veneers writable, writes them, and sets the pages back to
    /// execute, never writable and executable at once.
    ///
    /// The in-place path first; on the refusal a kernel with simultaneous write-execute forbidden
    /// gives, a fresh anonymous page holding the same bytes replaces each page instead.
    ///
    /// # Safety
    /// Every address in `patches` must be in this process's own text.
    unsafe fn write_all(patches: &[(usize, [u8; VENEER_LEN])]) -> Option<()> {
        let page = page_size();
        let (base, len) = span_pages(patches, page)?;
        // Safety: the span is this process's mapped text.
        if unsafe { mprotect(base, len, libc::PROT_READ | libc::PROT_WRITE) } {
            // Safety: the span is writable and every address is inside it.
            unsafe { write_patches(patches) };
            // Safety: the rewritten range needs the instruction cache made coherent.
            unsafe { flush_icache(patches) };
            // Safety: the span is this process's own mapping; execute is restored without write.
            return unsafe { mprotect(base, len, libc::PROT_READ | libc::PROT_EXEC) }.then_some(());
        }
        if last_errno() != libc::EACCES {
            return None;
        }
        // Safety: the span is this process's mapped text.
        unsafe { anon_replace(base, len, page, patches) }
    }

    /// Replaces every page of the span with a fresh anonymous page holding the same bytes, applies
    /// the veneers, and flips the span to execute.
    ///
    /// # Safety
    /// The span must be pages of this process's own text.
    unsafe fn anon_replace(
        base: usize,
        len: usize,
        page: usize,
        patches: &[(usize, [u8; VENEER_LEN])],
    ) -> Option<()> {
        for offset in (0..len).step_by(page) {
            let at = base.checked_add(offset)?;
            let mut copy = vec![0u8; page];
            // Safety: the page is mapped readable text; `copy` is a live buffer that long.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    std::ptr::with_exposed_provenance::<u8>(at),
                    copy.as_mut_ptr(),
                    page,
                );
            }
            // Safety: a fixed anonymous mapping replaces this process's own text page.
            let mapped = unsafe {
                libc::mmap(
                    std::ptr::with_exposed_provenance_mut(at),
                    page,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_FIXED | libc::MAP_PRIVATE | libc::MAP_ANON,
                    -1,
                    0,
                )
            };
            if std::ptr::eq(mapped, libc::MAP_FAILED) {
                return None;
            }
            // Safety: the new mapping is writable and `page` bytes long.
            unsafe { std::ptr::copy_nonoverlapping(copy.as_ptr(), mapped.cast::<u8>(), page) };
        }
        // Safety: the span is writable anonymous memory now.
        unsafe { write_patches(patches) };
        // Safety: the rewritten range needs the instruction cache made coherent.
        unsafe { flush_icache(patches) };
        // Safety: the span is this process's own mapping; execute without write.
        unsafe { mprotect(base, len, libc::PROT_READ | libc::PROT_EXEC) }.then_some(())
    }

    /// Writes every veneer over its entry.
    ///
    /// # Safety
    /// Every page holding an address in `patches` must be writable, with `VENEER_LEN` bytes from
    /// each address inside it.
    unsafe fn write_patches(patches: &[(usize, [u8; VENEER_LEN])]) {
        for &(at, veneer) in patches {
            // Safety: the page holding `at` is writable and has `VENEER_LEN` bytes from it.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    veneer.as_ptr(),
                    std::ptr::with_exposed_provenance_mut::<u8>(at),
                    VENEER_LEN,
                );
            }
        }
    }

    /// Writes the saved original bytes back over each entry, best effort.
    ///
    /// # Safety
    /// Every address in `saved` must be an entry in this process's own text.
    unsafe fn restore(saved: &[Saved]) {
        let mut patches = [(0usize, [0u8; VENEER_LEN]); 2];
        let mut count = 0;
        for &entry in saved {
            if let Some(slot) = patches.get_mut(count) {
                *slot = (entry.at, entry.original);
                count += 1;
            }
        }
        if let Some(patches) = patches.get(..count) {
            // Safety: the addresses are this process's text.
            let _restored = unsafe { write_all(patches) };
        }
    }

    /// The `VENEER_LEN` bytes at `at`.
    ///
    /// # Safety
    /// `VENEER_LEN` bytes from `at` must be mapped readable memory of this process.
    const unsafe fn read_bytes(at: usize) -> [u8; VENEER_LEN] {
        let mut buffer = [0u8; VENEER_LEN];
        // Safety: the caller guarantees the bytes are readable.
        unsafe {
            std::ptr::copy_nonoverlapping(
                std::ptr::with_exposed_provenance::<u8>(at),
                buffer.as_mut_ptr(),
                VENEER_LEN,
            );
        }
        buffer
    }

    /// The page-aligned span covering every veneer, as `(base, len)`.
    fn span_pages(patches: &[(usize, [u8; VENEER_LEN])], page: usize) -> Option<(usize, usize)> {
        let mut low: Option<usize> = None;
        let mut high: Option<usize> = None;
        for &(at, _) in patches {
            let start = at & !(page - 1);
            let end = round_up(at.checked_add(VENEER_LEN)?, page);
            low = Some(low.map_or(start, |value| value.min(start)));
            high = Some(high.map_or(end, |value| value.max(end)));
        }
        let (low, high) = (low?, high?);
        Some((low, high.checked_sub(low)?))
    }

    /// Changes the protection of a span, answering whether the kernel allowed it.
    ///
    /// # Safety
    /// The span must be this process's own mapping.
    unsafe fn mprotect(at: usize, len: usize, prot: libc::c_int) -> bool {
        // Safety: the caller guarantees the span.
        unsafe { libc::mprotect(std::ptr::with_exposed_provenance_mut(at), len, prot) == 0 }
    }

    /// Makes the rewritten range coherent with the instruction stream. A no-op on amd64, whose
    /// instruction cache is coherent with stores.
    #[cfg(not(target_arch = "aarch64"))]
    const unsafe fn flush_icache(_patches: &[(usize, [u8; VENEER_LEN])]) {}

    /// Cleans each rewritten cache line to the point of unification and invalidates it in the
    /// instruction cache, then orders the invalidation before execution.
    ///
    /// # Safety
    /// Every address in `patches` must be in the rewritten range.
    #[cfg(target_arch = "aarch64")]
    unsafe fn flush_icache(patches: &[(usize, [u8; VENEER_LEN])]) {
        /// The smallest cache line either cache is built from on any arm64 this runs on.
        const LINE: usize = 64;
        for &(at, _) in patches {
            let end = at.saturating_add(VENEER_LEN);
            let mut line = at & !(LINE - 1);
            while line < end {
                // Safety: the address is in the rewritten range of this process.
                unsafe { core::arch::asm!("dc cvau, {0}", in(reg) line) };
                line += LINE;
            }
            // Safety: ordering the data-cache cleans before the instruction-cache invalidations.
            unsafe { core::arch::asm!("dsb ish") };
            let mut line = at & !(LINE - 1);
            while line < end {
                // Safety: as above.
                unsafe { core::arch::asm!("ic ivau, {0}", in(reg) line) };
                line += LINE;
            }
            // Safety: ordering the invalidations before any fetch of the rewritten bytes.
            unsafe { core::arch::asm!("dsb ish", "isb") };
        }
    }

    /// The last error the kernel reported, for telling a hardened kernel's refusal from any other.
    fn last_errno() -> libc::c_int {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    /// How long this system's pages are.
    fn page_size() -> usize {
        // Safety: `sysconf` takes a name and answers a number, touching nothing of ours.
        let answer = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        usize::try_from(answer).unwrap_or(4096).max(4096)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both architectures' tables, named the way Go names the architectures.
    const TABLES: [(&str, &Routines); 2] = [("amd64", &AMD64), ("arm64", &ARM64)];

    /// `len` zero bytes, which no encoding matches, with `pieces` written in at their offsets.
    fn code_with(len: usize, pieces: &[(usize, &[u8])]) -> Vec<u8> {
        let mut code = vec![0u8; len];
        for &(offset, piece) in pieces {
            code[offset..offset + piece.len()].copy_from_slice(piece);
        }
        code
    }

    #[test]
    fn a_scan_finds_each_encoding_where_it_sits() {
        for (arch, routines) in TABLES {
            for &body in routines.nanotime {
                let found = routines.scan(&code_with(4096, &[(128, body)]));
                let wanted = Found {
                    nanotime: Hits::Once(128),
                    walltime: Hits::Absent,
                };
                assert_eq!(found, wanted, "{arch}");
            }
            for &body in routines.walltime {
                let found = routines.scan(&code_with(4096, &[(256, body)]));
                let wanted = Found {
                    nanotime: Hits::Absent,
                    walltime: Hits::Once(256),
                };
                assert_eq!(found, wanted, "{arch}");
            }
        }
    }

    #[test]
    fn a_routine_absent_doubled_or_in_two_encodings_is_not_found() {
        for (arch, routines) in TABLES {
            assert_eq!(routines.scan(&[0u8; 4096]), Found::NOTHING, "{arch}");
            for &body in routines.walltime {
                let doubled = routines.scan(&code_with(4096, &[(64, body), (1024, body)]));
                assert_eq!(doubled.walltime, Hits::Ambiguous, "{arch}");
            }
            for pair in routines.nanotime.windows(2) {
                let both = routines.scan(&code_with(4096, &[(64, pair[0]), (1024, pair[1])]));
                assert_eq!(both.nanotime, Hits::Ambiguous, "{arch}");
            }
        }
    }

    #[test]
    fn hits_in_separate_ranges_add_up_to_one_entry_or_none() {
        let here = Found {
            nanotime: Hits::Once(8),
            walltime: Hits::Absent,
        };
        let there = Found {
            nanotime: Hits::Absent,
            walltime: Hits::Once(16),
        };
        let total = here.plus(0x1000).unwrap().and(there.plus(0x8000).unwrap());
        let wanted = Found {
            nanotime: Hits::Once(0x1008),
            walltime: Hits::Once(0x8010),
        };
        assert_eq!(total, wanted);

        // A routine doubled in one range is not rescued by a single hit in another, and a hit in
        // each of two ranges is two hits.
        let doubled = Found {
            nanotime: Hits::Ambiguous,
            walltime: Hits::Absent,
        };
        assert_eq!(doubled.and(here).nanotime, Hits::Ambiguous);
        assert_eq!(here.and(doubled).nanotime, Hits::Ambiguous);
        assert_eq!(here.and(here).nanotime, Hits::Ambiguous);
        assert_eq!(here.plus(usize::MAX), None);
    }

    /// Every encoding is a `clock_gettime` body as memory holds it: the frame set up, system call
    /// 232 made, and a return at the end. On arm64 each word reads little-endian, the order the
    /// instructions are fetched in.
    #[test]
    fn every_encoding_is_a_clock_gettime_body_in_memory_order() {
        for &body in AMD64.nanotime.iter().chain(AMD64.walltime) {
            assert!(
                body.starts_with(&[0x55, 0x48, 0x89, 0xe5]),
                "push rbp; mov rbp, rsp"
            );
            assert!(
                body.windows(5).any(|w| w == [0xb8, 0xe8, 0, 0, 0]),
                "mov eax, 232"
            );
            assert!(body.windows(2).any(|w| w == [0x0f, 0x05]), "syscall");
            assert_eq!(body.last(), Some(&0xc3), "ret");
        }
        for &body in ARM64.nanotime.iter().chain(ARM64.walltime) {
            assert_eq!(body.len() % 4, 0, "whole instructions");
            let words: Vec<u32> = body
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect();
            assert_eq!(words.first(), Some(&0xf81d_0ffe), "str x30, [sp, #-48]!");
            assert!(words.contains(&0xd280_1d08), "mov x8, #232");
            assert!(words.contains(&0xd400_0001), "svc #0");
            assert_eq!(words.last(), Some(&0xd65f_03c0), "ret");
        }
    }

    /// On amd64 the veneer is `jmp [rip+0]` and the handler address, little-endian.
    #[test]
    #[cfg(target_arch = "x86_64")]
    fn the_veneer_is_a_rip_relative_absolute_jump() {
        let handler: usize = 0x0123_4567_89ab_cdef;
        let veneer = build_veneer(handler);
        assert_eq!(veneer.get(..6), Some(&[0xff, 0x25, 0, 0, 0, 0][..]));
        assert_eq!(veneer.get(6..), Some(&handler.to_le_bytes()[..]));
    }

    /// On arm64 the veneer is `ldr x16, #8`, `br x16`, and the handler address, each little-endian.
    #[test]
    #[cfg(target_arch = "aarch64")]
    fn the_veneer_is_a_literal_load_and_a_register_branch() {
        let handler: usize = 0x0123_4567_89ab_cdef;
        let veneer = build_veneer(handler);
        assert_eq!(veneer.get(..4), Some(&[0x50, 0x00, 0x00, 0x58][..]));
        assert_eq!(veneer.get(4..8), Some(&[0x00, 0x02, 0x1f, 0xd6][..]));
        assert_eq!(veneer.get(8..), Some(&handler.to_le_bytes()[..]));
    }

    /// Build information laid out as the Go linker of 1.18 and later writes it, naming `name`.
    fn build_info(name: &[u8]) -> Vec<u8> {
        let mut info = BUILD_INFO_MAGIC.to_vec();
        info.extend_from_slice(&[8, BUILD_INFO_INLINE]);
        info.resize(BUILD_INFO_STRINGS, 0);
        info.push(u8::try_from(name.len()).unwrap());
        info.extend_from_slice(name);
        // The module information that follows, empty.
        info.push(0);
        info
    }

    #[test]
    fn the_build_information_names_the_release() {
        let data = code_with(4096, &[(1024, build_info(b"go1.27.1").as_slice())]);
        assert_eq!(release_in(&data), Some(GoRelease::Named("go1.27.1")));

        let devel = "devel go1.28-0123456789 Tue Jan 1 00:00:00 2027 +0000";
        let data = code_with(4096, &[(1024, build_info(devel.as_bytes()).as_slice())]);
        assert_eq!(release_in(&data), Some(GoRelease::Named(devel)));
    }

    #[test]
    fn build_information_without_a_name_fit_to_print_is_go_all_the_same() {
        // Before Go 1.18 the strings sit behind pointers rather than inline.
        let mut pointers = build_info(b"go1.17");
        pointers[BUILD_INFO_MAGIC.len() + 1] = 0;
        let data = code_with(4096, &[(64, pointers.as_slice())]);
        assert_eq!(release_in(&data), Some(GoRelease::Unnamed));

        let garbled = code_with(4096, &[(64, build_info(b"go1.27\x01").as_slice())]);
        assert_eq!(release_in(&garbled), Some(GoRelease::Unnamed));
    }

    #[test]
    fn data_without_build_information_on_its_boundary_is_not_go() {
        assert_eq!(release_in(&[0u8; 4096]), None);
        let off_boundary = code_with(4096, &[(1032, build_info(b"go1.27.1").as_slice())]);
        assert_eq!(release_in(&off_boundary), None);
    }

    /// Each Go release `TIMEWARP_TEST_GO_TOOLCHAINS` names, as `GOTOOLCHAIN` values such as
    /// `go1.21.13`, or else the Go in the path, builds a program for both FreeBSD architectures in
    /// which each routine turns up exactly once in an encoding of the table, and whose build
    /// information names that release.
    ///
    /// The routines carry no relocations, so a program cross-compiled here holds the bytes one
    /// built on FreeBSD holds. A machine with no Go checks nothing, unless `GO` or the list names
    /// one.
    #[test]
    #[cfg(target_os = "linux")]
    fn every_go_release_assembles_each_routine_to_one_encoding_of_the_table() {
        let named = std::env::var_os("GO");
        let listed = std::env::var("TIMEWARP_TEST_GO_TOOLCHAINS").ok();
        let required = named.is_some() || listed.is_some();
        let go = named.unwrap_or_else(|| "go".into());
        let toolchains: Vec<&str> = listed
            .as_deref()
            .map_or_else(|| vec!["local"], |list| list.split_whitespace().collect());
        let source =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/progs/go_clocks.go");
        let dir = tempfile::tempdir().unwrap();
        let mut seen = std::collections::BTreeSet::new();

        for toolchain in toolchains {
            for (arch, routines) in TABLES {
                let program = dir.path().join(format!("{toolchain}-{arch}"));
                let built = std::process::Command::new(&go)
                    .args(["build", "-o"])
                    .arg(&program)
                    .arg(&source)
                    .env("GOOS", "freebsd")
                    .env("GOARCH", arch)
                    .env("CGO_ENABLED", "0")
                    .env("GOTOOLCHAIN", toolchain)
                    .env_remove("GOFLAGS")
                    .status();
                if !required && !built.as_ref().is_ok_and(std::process::ExitStatus::success) {
                    eprintln!("no working go on this machine, so the Go encodings went unchecked");
                    return;
                }
                assert!(
                    built.unwrap().success(),
                    "{toolchain} did not build for freebsd/{arch}"
                );

                let bytes = std::fs::read(&program).unwrap();
                let found = routines.scan(&bytes);
                assert!(
                    matches!(found.nanotime, Hits::Once(_)),
                    "{toolchain} freebsd/{arch}: fallback_nanotime is {:?}",
                    found.nanotime
                );
                assert!(
                    matches!(found.walltime, Hits::Once(_)),
                    "{toolchain} freebsd/{arch}: fallback_walltime is {:?}",
                    found.walltime
                );
                match release_in(&bytes) {
                    Some(GoRelease::Named(name)) if toolchain == "local" || name == toolchain => {}
                    other => {
                        panic!("{toolchain} freebsd/{arch}: the build information gave {other:?}")
                    }
                }
                let bodies = routines.nanotime.iter().chain(routines.walltime);
                seen.extend(
                    bodies
                        .enumerate()
                        .filter(|&(_, &body)| occurrences(&bytes, body, routines.step) == 1)
                        .map(|(index, _)| (arch, index)),
                );
            }
        }

        // A list of releases backs every encoding in the table, so none sits there unchecked.
        if listed.is_some() {
            let unbacked: Vec<_> = TABLES
                .iter()
                .flat_map(|&(arch, routines)| {
                    let count = routines.nanotime.len() + routines.walltime.len();
                    (0..count).map(move |index| (arch, index))
                })
                .filter(|encoding| !seen.contains(encoding))
                .collect();
            assert!(
                unbacked.is_empty(),
                "no listed release assembles these encodings of the table: {unbacked:?}"
            );
        }
    }

    /// How many times `body` sits in `code` at an offset `step` divides.
    #[cfg(target_os = "linux")]
    fn occurrences(code: &[u8], body: &[u8], step: usize) -> usize {
        (0..code.len())
            .step_by(step)
            .filter(|&at| hit(&code[at..], &[body], at) != Hits::Absent)
            .count()
    }
}
