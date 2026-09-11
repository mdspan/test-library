//! The ELF the synthetic vDSO is written in, and the reader that finds a function in the genuine
//! one.
//!
//! A vDSO is a shared object the kernel maps into every process, and a target that skips libc
//! parses it by hand: the program headers for the load segment and the dynamic array, the dynamic
//! array for the symbol table, the strings, and the hash table, then the symbol by name and
//! version. Everything below is what such a parse walks, on both architectures we support, which
//! are 64-bit and little-endian alike.

/// The first four bytes of every ELF file.
pub(super) const MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

/// 64-bit objects, the only width either architecture we support has a vDSO in.
pub(super) const CLASS_64: u8 = 2;

/// Two's-complement little-endian data, which both architectures are.
pub(super) const DATA_LSB: u8 = 1;

/// The one ELF version there has ever been.
pub(super) const VERSION: u8 = 1;

/// A shared object, which is what a vDSO is.
pub(super) const TYPE_DYN: u16 = 3;

/// The machine this build runs on, as the ELF header spells it.
pub(super) const MACHINE: u16 = if cfg!(target_arch = "x86_64") {
    62
} else {
    183
};

/// A loadable segment.
pub(super) const PT_LOAD: u32 = 1;

/// The segment holding the dynamic array.
pub(super) const PT_DYNAMIC: u32 = 2;

/// A segment the program may read.
pub(super) const PF_R: u32 = 4;

/// The dynamic tags we write and read. The three above `DT_LOOS` are the versioning ones, which
/// carry the version a symbol is defined under.
pub(super) const DT_NULL: i64 = 0;
pub(super) const DT_HASH: i64 = 4;
pub(super) const DT_STRTAB: i64 = 5;
pub(super) const DT_SYMTAB: i64 = 6;
pub(super) const DT_STRSZ: i64 = 10;
pub(super) const DT_SYMENT: i64 = 11;
pub(super) const DT_SONAME: i64 = 14;
pub(super) const DT_VERSYM: i64 = 0x6fff_fff0;
pub(super) const DT_VERDEF: i64 = 0x6fff_fffc;
pub(super) const DT_VERDEFNUM: i64 = 0x6fff_fffd;

/// A global binding and a function type, which is what a reader demands of a symbol it will call.
pub(super) const GLOBAL_FUNC: u8 = 0x12;

/// The section index of a symbol that is defined here. Any index but `SHN_UNDEF` says the symbol
/// has a definition; the sections themselves are never looked at, because a vDSO carries no
/// section headers a reader could reach.
pub(super) const SHN_DEFINED: u16 = 1;

/// The flag on the version definition that names the object itself rather than one of its
/// versions.
pub(super) const VER_FLG_BASE: u16 = 1;

/// The ELF header, at the address the auxiliary vector carries.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub(super) struct Ehdr {
    /// The magic, the class, the byte order, and the version.
    pub(super) ident: [u8; 16],
    /// [`TYPE_DYN`] for a vDSO.
    pub(super) kind: u16,
    /// The architecture.
    pub(super) machine: u16,
    /// [`VERSION`], widened.
    pub(super) version: u32,
    /// Where execution would start, which a vDSO has no use for.
    pub(super) entry: u64,
    /// Where the program headers sit, counted from this header.
    pub(super) phoff: u64,
    /// Where the section headers sit. A vDSO publishes none.
    pub(super) shoff: u64,
    /// Architecture flags, none of which either of ours sets.
    pub(super) flags: u32,
    /// How long this header is.
    pub(super) ehsize: u16,
    /// How long one program header is.
    pub(super) phentsize: u16,
    /// How many program headers there are.
    pub(super) phnum: u16,
    /// How long one section header would be.
    pub(super) shentsize: u16,
    /// How many section headers there are.
    pub(super) shnum: u16,
    /// Which section holds the section names.
    pub(super) shstrndx: u16,
}

/// One program header: a piece of the object, and where it lands.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub(super) struct Phdr {
    /// [`PT_LOAD`], [`PT_DYNAMIC`], or something we walk past.
    pub(super) kind: u32,
    /// What the program may do with it.
    pub(super) flags: u32,
    /// Where the piece starts, counted from the ELF header.
    pub(super) offset: u64,
    /// The address the piece was written for.
    pub(super) vaddr: u64,
    /// The same, for a loader that cares about physical addresses. Nothing here does.
    pub(super) paddr: u64,
    /// How long the piece is in the file.
    pub(super) filesz: u64,
    /// How long it is in memory.
    pub(super) memsz: u64,
    /// What it is aligned to.
    pub(super) align: u64,
}

/// One entry of the dynamic array: what it names, and where.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub(super) struct Dyn {
    /// Which of the `DT_` tags this entry carries.
    pub(super) tag: i64,
    /// The address or the number that tag takes.
    pub(super) value: u64,
}

/// One symbol.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub(super) struct Sym {
    /// Where the symbol's name starts in the string table.
    pub(super) name: u32,
    /// The binding and the type, [`GLOBAL_FUNC`] for everything we export.
    pub(super) info: u8,
    /// The visibility, default for everything we export.
    pub(super) other: u8,
    /// The section the symbol is defined in, [`SHN_DEFINED`] for everything we export.
    pub(super) shndx: u16,
    /// The address, once the load bias is added.
    pub(super) value: u64,
    /// How long the symbol is, which no reader of a vDSO asks.
    pub(super) size: u64,
}

/// One version definition: the object's own name, or a version symbols are defined under.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub(super) struct Verdef {
    /// One, the only revision there is.
    pub(super) version: u16,
    /// [`VER_FLG_BASE`] on the definition that names the object itself.
    pub(super) flags: u16,
    /// The index a symbol's version word carries to refer to this definition.
    pub(super) ndx: u16,
    /// How many names follow, which is one for each of ours.
    pub(super) cnt: u16,
    /// [`hash`] of the name, which is what a reader matches on first.
    pub(super) hash: u32,
    /// Where the first name sits, counted from this definition.
    pub(super) aux: u32,
    /// Where the next definition sits, or zero at the end of the chain.
    pub(super) next: u32,
}

/// One name of a version definition.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub(super) struct Verdaux {
    /// Where the name starts in the string table.
    pub(super) name: u32,
    /// Where the next name sits, or zero at the end.
    pub(super) next: u32,
}

/// The hash ELF has had since the beginning, which is the one a version definition carries and the
/// one a lookup through `DT_HASH` buckets by.
pub(super) fn hash(name: &[u8]) -> u32 {
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
pub(super) struct Genuine {
    /// What a symbol's value is counted from.
    bias: usize,
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
    pub(super) unsafe fn parse(base: usize) -> Option<Self> {
        // Safety: the caller guarantees the address, and the header is the first thing at it.
        let header: Ehdr = unsafe { read(base) };
        if header.ident.get(..4)? != MAGIC || header.ident.get(4) != Some(&CLASS_64) {
            return None;
        }

        let mut bias = None;
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
                    bias = Some(
                        base.wrapping_add(offset)
                            .wrapping_sub(usize::try_from(phdr.vaddr).ok()?),
                    );
                }
                PT_DYNAMIC => dynamic = Some(base.checked_add(offset)?),
                _ => {}
            }
        }

        let (bias, dynamic) = (bias?, dynamic?);
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
            symbols: symbols?,
            strings: strings?,
            count: count?,
        })
    }

    /// The address of the function `name` names, or `None` when this vDSO has no such function.
    pub(super) fn address_of(&self, name: &[u8]) -> Option<usize> {
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
    fn the_structures_are_the_widths_the_format_fixes() {
        assert_eq!(size_of::<Ehdr>(), 64);
        assert_eq!(size_of::<Phdr>(), 56);
        assert_eq!(size_of::<Dyn>(), 16);
        assert_eq!(size_of::<Sym>(), 24);
        assert_eq!(size_of::<Verdef>(), 20);
        assert_eq!(size_of::<Verdaux>(), 8);
    }
}
