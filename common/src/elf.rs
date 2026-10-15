//! The ELF a vDSO is written in: what the interposer writes its synthetic one in, and the reader
//! that finds a function in the kernel's.
//!
//! A vDSO is a shared object the kernel maps into every process, and a target that skips libc
//! parses it by hand: the program headers for the load segment and the dynamic array, the dynamic
//! array for the symbol table, the strings, and the hash table, then the symbol by name and
//! version. What is below is what such a parse walks, on both architectures we support, which are
//! 64-bit and little-endian alike, and [`MainImage`], which finds the main program's own segments
//! through the auxiliary vector.
//!
//! On Linux the genuine clock reads through the kernel's vDSO as well, which is why this lives here
//! rather than beside the synthetic object alone.

/// The first four bytes of every ELF file.
pub const MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

/// 64-bit objects, the only width either architecture we support has a vDSO in.
pub const CLASS_64: u8 = 2;

/// Two's-complement little-endian data, which both architectures are.
pub const DATA_LSB: u8 = 1;

/// The one ELF version there has ever been.
pub const VERSION: u8 = 1;

/// A shared object, which is what a vDSO is.
pub const TYPE_DYN: u16 = 3;

/// The machine this build runs on, as the ELF header spells it.
pub const MACHINE: u16 = if cfg!(target_arch = "x86_64") {
    62
} else {
    183
};

/// A loadable segment.
pub const PT_LOAD: u32 = 1;

/// The segment holding the dynamic array.
pub const PT_DYNAMIC: u32 = 2;

/// The segment that points at the program header array itself, which carries the address the array
/// was linked for and so the load bias.
pub const PT_PHDR: u32 = 6;

/// A segment the program may read.
pub const PF_R: u32 = 4;

/// A segment the program may execute, which is where its own code sits.
pub const PF_X: u32 = 1;

/// A segment the program may write, which is where its own data sits.
pub const PF_W: u32 = 2;

// The auxiliary-vector tags that name the program's own headers: where the array is in memory, how
// long one entry is, and how many there are. A target that reads the clock without libc carries
// that code in its own executable segments, which these locate.

/// Where the program header array is in memory.
pub const AT_PHDR: libc::c_ulong = 3;
/// How long one program header is.
pub const AT_PHENT: libc::c_ulong = 4;
/// How many program headers there are.
pub const AT_PHNUM: libc::c_ulong = 5;

// The dynamic tags we write and read. The three above `DT_LOOS` are the versioning ones, which
// carry the version a symbol is defined under.

/// The entry that ends the dynamic array.
pub const DT_NULL: i64 = 0;
/// The hash table.
pub const DT_HASH: i64 = 4;
/// The string table.
pub const DT_STRTAB: i64 = 5;
/// The symbol table.
pub const DT_SYMTAB: i64 = 6;
/// How long the string table is.
pub const DT_STRSZ: i64 = 10;
/// How long one symbol is.
pub const DT_SYMENT: i64 = 11;
/// The object's own name, in the string table.
pub const DT_SONAME: i64 = 14;
/// A version index per symbol.
pub const DT_VERSYM: i64 = 0x6fff_fff0;
/// The version definitions.
pub const DT_VERDEF: i64 = 0x6fff_fffc;
/// How many version definitions there are.
pub const DT_VERDEFNUM: i64 = 0x6fff_fffd;

/// A global binding and a function type, which is what a reader demands of a symbol it will call.
pub const GLOBAL_FUNC: u8 = 0x12;

/// The section index of a symbol that is defined here.
///
/// Any index but `SHN_UNDEF` says the symbol has a definition; the sections themselves are never
/// looked at, because a vDSO carries no section headers a reader could reach.
pub const SHN_DEFINED: u16 = 1;

/// The flag on the version definition that names the object itself rather than one of its
/// versions.
pub const VER_FLG_BASE: u16 = 1;

/// The ELF header, at the address the auxiliary vector carries.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Ehdr {
    /// The magic, the class, the byte order, and the version.
    pub ident: [u8; 16],
    /// [`TYPE_DYN`] for a vDSO.
    pub kind: u16,
    /// The architecture.
    pub machine: u16,
    /// [`VERSION`], widened.
    pub version: u32,
    /// Where execution would start, which a vDSO has no use for.
    pub entry: u64,
    /// Where the program headers sit, counted from this header.
    pub phoff: u64,
    /// Where the section headers sit. A vDSO publishes none.
    pub shoff: u64,
    /// Architecture flags, none of which either of ours sets.
    pub flags: u32,
    /// How long this header is.
    pub ehsize: u16,
    /// How long one program header is.
    pub phentsize: u16,
    /// How many program headers there are.
    pub phnum: u16,
    /// How long one section header would be.
    pub shentsize: u16,
    /// How many section headers there are.
    pub shnum: u16,
    /// Which section holds the section names.
    pub shstrndx: u16,
}

/// One program header: a piece of the object, and where it lands.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Phdr {
    /// [`PT_LOAD`], [`PT_DYNAMIC`], or something we walk past.
    pub kind: u32,
    /// What the program may do with it.
    pub flags: u32,
    /// Where the piece starts, counted from the ELF header.
    pub offset: u64,
    /// The address the piece was written for.
    pub vaddr: u64,
    /// The same, for a loader that cares about physical addresses. Nothing here does.
    pub paddr: u64,
    /// How long the piece is in the file.
    pub filesz: u64,
    /// How long it is in memory.
    pub memsz: u64,
    /// What it is aligned to.
    pub align: u64,
}

/// One entry of the dynamic array: what it names, and where.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Dyn {
    /// Which of the `DT_` tags this entry carries.
    pub tag: i64,
    /// The address or the number that tag takes.
    pub value: u64,
}

/// One symbol.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Sym {
    /// Where the symbol's name starts in the string table.
    pub name: u32,
    /// The binding and the type, [`GLOBAL_FUNC`] for everything we export.
    pub info: u8,
    /// The visibility, default for everything we export.
    pub other: u8,
    /// The section the symbol is defined in, [`SHN_DEFINED`] for everything we export.
    pub shndx: u16,
    /// The address, once the load bias is added.
    pub value: u64,
    /// How long the symbol is, which no reader of a vDSO asks.
    pub size: u64,
}

/// One version definition: the object's own name, or a version symbols are defined under.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Verdef {
    /// One, the only revision there is.
    pub version: u16,
    /// [`VER_FLG_BASE`] on the definition that names the object itself.
    pub flags: u16,
    /// The index a symbol's version word carries to refer to this definition.
    pub ndx: u16,
    /// How many names follow, which is one for each of ours.
    pub cnt: u16,
    /// [`hash`] of the name, which is what a reader matches on first.
    pub hash: u32,
    /// Where the first name sits, counted from this definition.
    pub aux: u32,
    /// Where the next definition sits, or zero at the end of the chain.
    pub next: u32,
}

/// One name of a version definition.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Verdaux {
    /// Where the name starts in the string table.
    pub name: u32,
    /// Where the next name sits, or zero at the end.
    pub next: u32,
}

/// The hash ELF has had since the beginning, which is the one a version definition carries and the
/// one a lookup through `DT_HASH` buckets by.
#[must_use]
pub fn hash(name: &[u8]) -> u32 {
    let mut value: u32 = 0;
    for &byte in name {
        value = (value << 4).wrapping_add(u32::from(byte));
        let high = value & 0xf000_0000;
        if high != 0 {
            value ^= high >> 24;
        }
        value &= !high;
    }
    value
}

/// Reads a `T` out of memory the program did not write, whatever its alignment.
///
/// # Safety
/// `at` must be an address a `T` can be read from.
const unsafe fn read<T>(at: usize) -> T {
    // Safety: the caller guarantees the address, and an unaligned read asks nothing of it.
    unsafe { std::ptr::with_exposed_provenance::<T>(at).read_unaligned() }
}

/// The genuine vDSO, parsed far enough to find a function in it.
///
/// The same walk a target that skips libc makes, which is what makes it the right one: a function
/// this finds is a function such a target would have called.
#[derive(Debug)]
pub struct Genuine {
    /// What a symbol's value is counted from.
    bias: usize,
    /// Where the load segment sits in memory, which is where the object's own code is.
    load: std::ops::Range<usize>,
    /// Where the symbol table starts.
    symbols: usize,
    /// Where the string table starts.
    strings: usize,
    /// How many symbols there are, which the hash table's chain count gives.
    count: usize,
}

impl Genuine {
    /// Parses the vDSO mapped at `base`, or `None` when nothing there is one.
    ///
    /// # Safety
    /// `base` must be the address the auxiliary vector carries for the vDSO.
    #[must_use]
    pub unsafe fn parse(base: usize) -> Option<Self> {
        // Safety: the caller guarantees the address, and the header is the first thing at it.
        let header: Ehdr = unsafe { read(base) };
        if header.ident.get(..4)? != MAGIC || header.ident.get(4) != Some(&CLASS_64) {
            return None;
        }

        let mut bias = None;
        let mut load = None;
        let mut dynamic = None;
        for index in 0..usize::from(header.phnum) {
            let at = base
                .checked_add(usize::try_from(header.phoff).ok()?)?
                .checked_add(index.checked_mul(usize::from(header.phentsize))?)?;
            // Safety: the header says a program header sits here, and the vDSO is the kernel's.
            let phdr: Phdr = unsafe { read(at) };
            let offset = usize::try_from(phdr.offset).ok()?;
            match phdr.kind {
                PT_LOAD if bias.is_none() => {
                    let start = base.checked_add(offset)?;
                    bias = Some(start.wrapping_sub(usize::try_from(phdr.vaddr).ok()?));
                    load = Some(start..start.checked_add(usize::try_from(phdr.memsz).ok()?)?);
                }
                PT_DYNAMIC => dynamic = Some(base.checked_add(offset)?),
                _ => {}
            }
        }

        let (bias, load, dynamic) = (bias?, load?, dynamic?);
        let (mut symbols, mut strings, mut count) = (None, None, None);
        for index in 0..MAX_DYNAMIC {
            let at = dynamic.checked_add(index.checked_mul(size_of::<Dyn>())?)?;
            // Safety: the program header says the dynamic array sits here.
            let entry: Dyn = unsafe { read::<Dyn>(at) };
            let value = bias.wrapping_add(usize::try_from(entry.value).ok()?);
            match entry.tag {
                DT_NULL => break,
                DT_SYMTAB => symbols = Some(value),
                DT_STRTAB => strings = Some(value),
                // The hash table's second word is how many symbols the table it indexes holds,
                // which is the only count a dynamic array carries.
                DT_HASH => {
                    // Safety: the dynamic array says the hash table sits here.
                    count =
                        Some(usize::try_from(unsafe { read::<u32>(value.checked_add(4)?) }).ok()?);
                }
                _ => {}
            }
        }

        Some(Self {
            bias,
            load,
            symbols: symbols?,
            strings: strings?,
            count: count?,
        })
    }

    /// The address of the function `name` names, or `None` when this vDSO has no such function.
    #[must_use]
    pub fn address_of(&self, name: &[u8]) -> Option<usize> {
        for index in 0..self.count {
            let at = self
                .symbols
                .checked_add(index.checked_mul(size_of::<Sym>())?)?;
            // Safety: the dynamic array says the symbol table sits here and how long it is.
            let symbol: Sym = unsafe { read(at) };
            if symbol.info != GLOBAL_FUNC || symbol.value == 0 {
                continue;
            }
            let spelt = self
                .strings
                .checked_add(usize::try_from(symbol.name).ok()?)?;
            // Safety: a symbol's name is an offset into the string table the dynamic array named.
            if unsafe { named(spelt, name) } {
                return Some(self.bias.wrapping_add(usize::try_from(symbol.value).ok()?));
            }
        }
        None
    }

    /// The address of the function `name` names, when the function is this object's own code.
    ///
    /// The kernel's vDSO carries its functions inside its own load segment. A synthetic one, ours
    /// among them, points its symbols at code in a library outside it, so this refuses such an
    /// object's functions and keeps the kernel's.
    #[must_use]
    pub fn own_address_of(&self, name: &[u8]) -> Option<usize> {
        self.address_of(name)
            .filter(|address| self.load.contains(address))
    }
}

/// How far a dynamic array is walked before it is taken for something other than one. Longer than
/// any vDSO's, which runs to a couple of dozen entries.
const MAX_DYNAMIC: usize = 128;

/// Whether the NUL-terminated string at `at` is `name`.
///
/// # Safety
/// `at` must be a NUL-terminated string inside the object being read.
unsafe fn named(at: usize, name: &[u8]) -> bool {
    for (index, &wanted) in name.iter().enumerate() {
        let Some(spelt) = at.checked_add(index) else {
            return false;
        };
        // Safety: the caller guarantees the string, and the walk stops at the first byte that
        // differs, so it never reads past the terminator.
        if unsafe { read::<u8>(spelt) } != wanted {
            return false;
        }
    }
    // Safety: as above; the byte after a name that matched so far is still inside the string.
    at.checked_add(name.len())
        .is_some_and(|end| unsafe { read::<u8>(end) } == 0)
}

/// How many load segments of one kind [`MainImage`] keeps. A program links one or two of each; the
/// bound is well above any real count and is what a malformed header cannot grow the walk past.
const MAX_SEGMENTS: usize = 8;

/// Up to [`MAX_SEGMENTS`] ranges of memory, in the order the program headers list them.
#[derive(Debug, Clone, Copy)]
struct Spans {
    /// The ranges, as `(start, end)` pairs.
    spans: [(usize, usize); MAX_SEGMENTS],
    /// How many of `spans` are filled.
    count: usize,
}

impl Spans {
    /// No ranges.
    const EMPTY: Self = Self {
        spans: [(0, 0); MAX_SEGMENTS],
        count: 0,
    };

    /// Adds `start..end`, or leaves it out once [`MAX_SEGMENTS`] are kept.
    fn push(&mut self, start: usize, end: usize) {
        if let Some(slot) = self.spans.get_mut(self.count) {
            *slot = (start, end);
            self.count += 1;
        }
    }

    /// The ranges.
    fn iter(&self) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
        self.spans
            .iter()
            .take(self.count)
            .map(|&(start, end)| start..end)
    }
}

/// The main program's own image, found through the auxiliary vector rather than through a shared
/// object.
///
/// A target that reads the clock without libc, the Go runtime among them, carries that code in its
/// own executable load segments. This derives the load bias every address in the program is
/// counted from and the ranges that code sits in, so a scan can recognise a routine by its bytes,
/// and the ranges the program's initialised data sits in, where the Go linker records the release
/// that built the program.
#[derive(Debug)]
pub struct MainImage {
    /// What a linked address has added to it to reach where the segment landed.
    bias: usize,
    /// The executable load ranges.
    executable: Spans,
    /// The part of each writable, non-executable load segment that the file supplies, which leaves
    /// out the zeroed tail no initialised data sits in.
    writable: Spans,
}

impl MainImage {
    /// Locates the image from the three `AT_PHDR` auxiliary-vector values: the in-memory address
    /// of the program header array, the size of one entry, and the count.
    ///
    /// `None` when the entry size is not this build's [`Phdr`], when no load bias can be derived,
    /// or when the headers name no executable segment.
    ///
    /// # Safety
    /// `phdr` must be the address the auxiliary vector carries for this process's program headers,
    /// with `phnum` entries of `phent` bytes each.
    #[must_use]
    pub unsafe fn locate(phdr: usize, phent: usize, phnum: usize) -> Option<Self> {
        if phdr == 0 || phent != size_of::<Phdr>() {
            return None;
        }
        // Safety: the caller guarantees the array.
        let bias = unsafe { load_bias(phdr, phnum) }?;
        let mut executable = Spans::EMPTY;
        let mut writable = Spans::EMPTY;
        for index in 0..phnum {
            let at = phdr.checked_add(index.checked_mul(phent)?)?;
            // Safety: the auxiliary vector says a program header sits here.
            let header: Phdr = unsafe { read(at) };
            if header.kind != PT_LOAD {
                continue;
            }
            let start = bias.wrapping_add(usize::try_from(header.vaddr).ok()?);
            if header.flags & PF_X != 0 {
                executable.push(
                    start,
                    start.checked_add(usize::try_from(header.memsz).ok()?)?,
                );
            } else if header.flags & PF_W != 0 {
                writable.push(
                    start,
                    start.checked_add(usize::try_from(header.filesz).ok()?)?,
                );
            }
        }
        (executable.count > 0).then_some(Self {
            bias,
            executable,
            writable,
        })
    }

    /// What a linked address has added to it to reach where this image landed.
    #[must_use]
    pub const fn bias(&self) -> usize {
        self.bias
    }

    /// The executable ranges, where the program's own code sits.
    pub fn executable_ranges(&self) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
        self.executable.iter()
    }

    /// The ranges the program's initialised data sits in: the part of each writable,
    /// non-executable load segment that the file supplies.
    pub fn writable_ranges(&self) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
        self.writable.iter()
    }
}

/// The load bias of the main program: where a segment landed less where it was linked to land.
///
/// From [`PT_PHDR`] where there is one, which every position-independent executable carries.
/// Otherwise from the ELF header, which a linker places at the image base with the program header
/// array just past it, so the header sits one header-length below the array; its base is where the
/// load segment that maps file offset zero landed, and the bias follows from that segment's vaddr.
///
/// # Safety
/// As [`MainImage::locate`].
unsafe fn load_bias(phdr: usize, phnum: usize) -> Option<usize> {
    for index in 0..phnum {
        let at = phdr.checked_add(index.checked_mul(size_of::<Phdr>())?)?;
        // Safety: the caller guarantees the array.
        let header: Phdr = unsafe { read(at) };
        if header.kind == PT_PHDR {
            return Some(phdr.wrapping_sub(usize::try_from(header.vaddr).ok()?));
        }
    }

    // No `PT_PHDR`: the header is the length of itself below the array.
    let base = phdr.checked_sub(size_of::<Ehdr>())?;
    // Safety: the array is mapped and the header precedes it in the same first page.
    let header: Ehdr = unsafe { read(base) };
    if header.ident.get(..4)? != MAGIC || usize::try_from(header.phoff).ok()? != size_of::<Ehdr>() {
        return None;
    }
    // The load segment whose file offset is zero maps that offset, the header, to `base`.
    for index in 0..phnum {
        let at = phdr.checked_add(index.checked_mul(size_of::<Phdr>())?)?;
        // Safety: as above.
        let segment: Phdr = unsafe { read(at) };
        if segment.kind == PT_LOAD && segment.offset == 0 {
            return Some(base.wrapping_sub(usize::try_from(segment.vaddr).ok()?));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hashes the Go runtime carries for the versions it looks a vDSO entry up under. They are
    /// constants there, so ours has to agree with them byte for byte.
    #[test]
    fn the_version_hashes_are_the_ones_a_reader_looks_for() {
        assert_eq!(hash(b"LINUX_2.6"), 0x03ae_75f6);
        assert_eq!(hash(b"LINUX_2.6.39"), 0x075f_cb89);
        assert_eq!(hash(b"__vdso_clock_gettime"), 0x0d35_ec75);
        assert_eq!(hash(b"__vdso_gettimeofday"), 0x0315_ca59);
        assert_eq!(hash(b""), 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn the_kernel_vdso_clock_is_its_own_code() {
        // Safety: `getauxval` takes a tag and answers a number.
        let base = unsafe { libc::getauxval(crate::platform::AT_SYSINFO_EHDR) };
        let base = usize::try_from(base).unwrap();
        assert_ne!(base, 0, "the kernel mapped no vDSO into this process");
        // Safety: the address is the one the auxiliary vector carries for the vDSO.
        let kernel = unsafe { Genuine::parse(base) }.unwrap();
        let name: &[u8] = if cfg!(target_arch = "x86_64") {
            b"__vdso_clock_gettime"
        } else {
            b"__kernel_clock_gettime"
        };
        let found = kernel.own_address_of(name);
        assert!(found.is_some());
        assert_eq!(found, kernel.address_of(name));
    }

    #[test]
    fn the_structures_are_the_widths_the_format_fixes() {
        assert_eq!(size_of::<Ehdr>(), 64);
        assert_eq!(size_of::<Phdr>(), 56);
        assert_eq!(size_of::<Dyn>(), 16);
        assert_eq!(size_of::<Sym>(), 24);
        assert_eq!(size_of::<Verdef>(), 20);
        assert_eq!(size_of::<Verdaux>(), 8);
    }

    /// The bias and the ranges this process computes for itself from its own `AT_PHDR` family land
    /// this function's own address inside an executable range. A test binary is built
    /// position-independent, so it carries a `PT_PHDR` and the bias is non-trivial.
    #[test]
    #[cfg(target_os = "linux")]
    #[expect(
        clippy::fn_to_numeric_cast_any,
        reason = "the runtime address of this code is what the executable range is checked to hold"
    )]
    fn the_main_image_covers_this_programs_own_code() {
        // Safety: `getauxval` takes a tag and answers a number, touching nothing of ours.
        let value = |tag| usize::try_from(unsafe { libc::getauxval(tag) }).unwrap();
        let (phdr, phent, phnum) = (value(AT_PHDR), value(AT_PHENT), value(AT_PHNUM));
        assert_eq!(phent, size_of::<Phdr>());
        // Safety: the three values are this process's own, from the auxiliary vector.
        let image = unsafe { MainImage::locate(phdr, phent, phnum) }.expect("the image is located");

        let here = (the_main_image_covers_this_programs_own_code as *const ()).expose_provenance();
        assert!(
            image.executable_ranges().any(|range| range.contains(&here)),
            "this function sits outside every executable range the image found"
        );
        // The program header array itself is inside the image, above its bias.
        assert!(phdr > image.bias());
    }

    /// A static this program initialised to something other than zero sits in the part of a
    /// writable segment the file supplies, which is where the image says its data is.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_main_image_covers_this_programs_own_data() {
        static DATA: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0x5eed);

        // Safety: `getauxval` takes a tag and answers a number, touching nothing of ours.
        let value = |tag| usize::try_from(unsafe { libc::getauxval(tag) }).unwrap();
        let (phdr, phent, phnum) = (value(AT_PHDR), value(AT_PHENT), value(AT_PHNUM));
        // Safety: the three values are this process's own, from the auxiliary vector.
        let image = unsafe { MainImage::locate(phdr, phent, phnum) }.expect("the image is located");

        let here = std::ptr::from_ref(&DATA).addr();
        assert!(
            image.writable_ranges().any(|range| range.contains(&here)),
            "this program's data sits outside every writable range the image found"
        );
        assert!(
            !image.executable_ranges().any(|range| range.contains(&here)),
            "this program's data sits inside an executable range"
        );
    }
}
