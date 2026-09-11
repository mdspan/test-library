//! Building the synthetic vDSO: one page carrying an ELF object whose clock entries are ours.
//!
//! The object is written so that the load bias every reader computes for it is zero: the load
//! segment claims the address the page landed at, so a symbol's value is the address of the
//! function itself. That is what lets the entries live in this library, where the read path is,
//! rather than in the page.
//!
//! Three clock reads are exported, under the names this architecture's vDSO gives them, pointing
//! at [`crate::vdso::entries`]. `clock_getres` and `getcpu` point straight at the genuine vDSO's
//! own code wherever the genuine one has them: the cheapest forwarding there is, and it leaves them
//! answering exactly what they answered before.

use common::session::layout::round_up;

use super::elf::{
    self, CLASS_64, DATA_LSB, DT_HASH, DT_NULL, DT_SONAME, DT_STRSZ, DT_STRTAB, DT_SYMENT,
    DT_SYMTAB, DT_VERDEF, DT_VERDEFNUM, DT_VERSYM, Dyn, Ehdr, GLOBAL_FUNC, Genuine, MACHINE, MAGIC,
    PF_R, PT_DYNAMIC, PT_LOAD, Phdr, SHN_DEFINED, Sym, TYPE_DYN, VER_FLG_BASE, VERSION, Verdaux,
    Verdef,
};
use super::entries;

/// The name the object gives itself, which is the name the kernel's vDSO gives itself too.
const SONAME: &[u8] = b"linux-vdso.so.1";

// The version the entries are defined under, and the names they are defined by: each architecture
// spells them the way the kernel's own vDSO on it does, which is what a target looks for.

/// What `x86_64` calls them.
#[cfg(target_arch = "x86_64")]
mod names {
    /// The version a reader asks for the entries under.
    pub(super) const VERSION: &[u8] = b"LINUX_2.6";
    /// The wall-clock and monotonic read.
    pub(super) const CLOCK_GETTIME: &[u8] = b"__vdso_clock_gettime";
    /// The wall-clock read in a `timeval`.
    pub(super) const GETTIMEOFDAY: &[u8] = b"__vdso_gettimeofday";
    /// The wall clock in whole seconds.
    pub(super) const TIME: &[u8] = b"__vdso_time";
    /// The clock resolutions, which stay the genuine vDSO's.
    pub(super) const CLOCK_GETRES: &[u8] = b"__vdso_clock_getres";
    /// Which processor the caller is on, which stays the genuine vDSO's.
    pub(super) const GETCPU: &[u8] = b"__vdso_getcpu";
}

/// What `aarch64` calls them, where the whole group carries the `__kernel_` prefix under a version
/// of its own.
#[cfg(not(target_arch = "x86_64"))]
mod names {
    /// The version a reader asks for the entries under.
    pub(super) const VERSION: &[u8] = b"LINUX_2.6.39";
    /// The wall-clock and monotonic read.
    pub(super) const CLOCK_GETTIME: &[u8] = b"__kernel_clock_gettime";
    /// The wall-clock read in a `timeval`.
    pub(super) const GETTIMEOFDAY: &[u8] = b"__kernel_gettimeofday";
    /// The wall clock in whole seconds.
    pub(super) const TIME: &[u8] = b"__kernel_time";
    /// The clock resolutions, which stay the genuine vDSO's.
    pub(super) const CLOCK_GETRES: &[u8] = b"__kernel_clock_getres";
    /// Which processor the caller is on. The kernel's vDSO has none here, so neither has ours.
    pub(super) const GETCPU: &[u8] = b"__kernel_getcpu";
}

/// How many dynamic entries the object carries, the terminator included.
const DYNAMIC_ENTRIES: usize = 10;

/// How many version definitions the object carries: its own name, and the version the entries are
/// defined under.
const VERSION_DEFINITIONS: usize = 2;

/// The index a symbol's version word carries for [`names::VERSION`]. One is the object's own name,
/// so the version the entries are defined under is the next.
const VERSION_INDEX: u16 = 2;

/// How far apart two version definitions sit: the definition and the one name it carries.
const VERDEF_STRIDE: usize = size_of::<Verdef>() + size_of::<Verdaux>();

/// One symbol the object exports.
#[derive(Debug)]
struct Export {
    /// What a reader looks it up by.
    name: &'static [u8],
    /// What a reader that finds it calls.
    target: usize,
}

/// Builds the object and answers where it was mapped, or `None` when it could not be built.
///
/// `genuine` is where the kernel mapped its own vDSO, which is both what the forwarded entries
/// point into and what says which of them this architecture has.
pub(super) fn build(genuine: usize) -> Option<usize> {
    let exports = exports(genuine);
    let len = page_size();
    let base = map(len)?;
    if write(base, len, &exports).is_none() || protect(base, len).is_none() {
        unmap(base, len);
        return None;
    }
    Some(base)
}

/// What the object exports: our three clock entries, and whatever the genuine vDSO has of the two
/// that pass through to it.
#[expect(
    clippy::fn_to_numeric_cast_any,
    reason = "the object carries each entry as the address a reader of it calls, which is what \
              this turns a function into"
)]
fn exports(genuine: usize) -> Vec<Export> {
    let clock_gettime: unsafe extern "C" fn(libc::clockid_t, *mut libc::timespec) -> libc::c_long =
        entries::clock_gettime;
    let gettimeofday: unsafe extern "C" fn(*mut libc::timeval, *mut libc::c_void) -> libc::c_long =
        entries::gettimeofday;
    let time: unsafe extern "C" fn(*mut libc::time_t) -> libc::time_t = entries::time;

    let mut exports = vec![
        Export {
            name: names::CLOCK_GETTIME,
            target: (clock_gettime as *const ()).expose_provenance(),
        },
        Export {
            name: names::GETTIMEOFDAY,
            target: (gettimeofday as *const ()).expose_provenance(),
        },
        Export {
            name: names::TIME,
            target: (time as *const ()).expose_provenance(),
        },
    ];

    // Safety: the address is the one the auxiliary vector carries for the genuine vDSO.
    let Some(kernel) = (unsafe { Genuine::parse(genuine) }) else {
        return exports;
    };
    for name in [names::CLOCK_GETRES, names::GETCPU] {
        if let Some(target) = kernel.address_of(name) {
            exports.push(Export { name, target });
        }
    }
    exports
}

/// Where each table of the object sits in the page it was reserved in.
#[derive(Debug)]
struct Tables {
    /// The ELF header.
    ehdr: usize,
    /// The two program headers.
    phdr: usize,
    /// The dynamic array.
    dynamic: usize,
    /// The hash table a name is looked up through.
    hash: usize,
    /// The symbol table.
    symtab: usize,
    /// A version index per symbol.
    versym: usize,
    /// The version definitions.
    verdef: usize,
    /// The strings every name above points into.
    strtab: usize,
}

/// Lays the whole object out in the page at `base` and fills it in.
///
/// Every piece is reserved before any is written, because the fixed-size tables carry each other's
/// addresses and the strings they name are written last.
fn write(base: usize, len: usize, exports: &[Export]) -> Option<()> {
    let symbols = exports.len().checked_add(1)?;
    let buckets = symbols;
    let mut page = Page::new(base, len);

    let tables = Tables {
        ehdr: page.reserve::<Ehdr>(1)?,
        phdr: page.reserve::<Phdr>(2)?,
        dynamic: page.reserve::<Dyn>(DYNAMIC_ENTRIES)?,
        hash: page.reserve::<u32>(2 + buckets + symbols)?,
        symtab: page.reserve::<Sym>(symbols)?,
        versym: page.reserve::<u16>(symbols)?,
        verdef: page.reserve::<u32>(VERSION_DEFINITIONS * VERDEF_STRIDE / size_of::<u32>())?,
        strtab: page.reserve::<u8>(strings_length(exports)?)?,
    };

    // The strings first, so every table below has a name to point at.
    let strings = write_strings(&page, tables.strtab, exports)?;

    write_header(&page, &tables, base, len)?;
    write_dynamic(&page, &tables, base, &strings)?;
    write_symbols(&page, tables.symtab, exports, &strings.named)?;
    write_hash(&page, tables.hash, buckets, exports)?;

    // The first symbol carries no name and no version; every other one carries the version the
    // entries are defined under.
    page.put(tables.versym, 0u16)?;
    for index in 1..symbols {
        page.put(
            tables
                .versym
                .checked_add(index.checked_mul(size_of::<u16>())?)?,
            VERSION_INDEX,
        )?;
    }
    write_versions(&page, tables.verdef, strings.soname, strings.version)
}

/// The ELF header and the two program headers: what the object is, and where a reader finds the
/// dynamic array.
fn write_header(page: &Page, tables: &Tables, base: usize, len: usize) -> Option<()> {
    page.put(
        tables.ehdr,
        Ehdr {
            ident: identification(),
            kind: TYPE_DYN,
            machine: MACHINE,
            version: u32::from(VERSION),
            entry: 0,
            phoff: as_u64(tables.phdr)?,
            shoff: 0,
            flags: 0,
            ehsize: as_u16(size_of::<Ehdr>())?,
            phentsize: as_u16(size_of::<Phdr>())?,
            phnum: 2,
            shentsize: as_u16(size_of::<Phdr>())?,
            shnum: 0,
            shstrndx: 0,
        },
    )?;

    // The load segment claims the address the page landed at, so the bias every reader works out
    // for this object is zero and a symbol's value is the address of the function itself.
    page.put(
        tables.phdr,
        Phdr {
            kind: PT_LOAD,
            flags: PF_R,
            offset: 0,
            vaddr: as_u64(base)?,
            paddr: as_u64(base)?,
            filesz: as_u64(len)?,
            memsz: as_u64(len)?,
            align: as_u64(len)?,
        },
    )?;
    page.put(
        tables.phdr.checked_add(size_of::<Phdr>())?,
        Phdr {
            kind: PT_DYNAMIC,
            flags: PF_R,
            offset: as_u64(tables.dynamic)?,
            vaddr: as_u64(base.checked_add(tables.dynamic)?)?,
            paddr: as_u64(base.checked_add(tables.dynamic)?)?,
            filesz: as_u64(DYNAMIC_ENTRIES * size_of::<Dyn>())?,
            memsz: as_u64(DYNAMIC_ENTRIES * size_of::<Dyn>())?,
            align: as_u64(align_of::<Dyn>())?,
        },
    )
}

/// The dynamic array: where every table above sits, as the addresses a reader takes them for.
fn write_dynamic(page: &Page, tables: &Tables, base: usize, strings: &Written) -> Option<()> {
    // Addresses, because the bias is zero: what a reader adds to each of these is nothing.
    let at = |offset: usize| as_u64(base.wrapping_add(offset));
    let entries = [
        (DT_HASH, at(tables.hash)?),
        (DT_STRTAB, at(tables.strtab)?),
        (DT_SYMTAB, at(tables.symtab)?),
        (DT_VERSYM, at(tables.versym)?),
        (DT_VERDEF, at(tables.verdef)?),
        (DT_VERDEFNUM, as_u64(VERSION_DEFINITIONS)?),
        (DT_SYMENT, as_u64(size_of::<Sym>())?),
        (DT_STRSZ, as_u64(strings.len)?),
        (DT_SONAME, as_u64(strings.soname)?),
        (DT_NULL, 0),
    ];
    for (index, &(tag, value)) in entries.iter().enumerate() {
        page.put(
            tables
                .dynamic
                .checked_add(index.checked_mul(size_of::<Dyn>())?)?,
            Dyn { tag, value },
        )?;
    }
    Some(())
}

/// How long the string table comes out: the empty name at its head, the object's own name, the
/// version the entries are defined under, and one name per export, each with its terminator.
fn strings_length(exports: &[Export]) -> Option<usize> {
    let mut len: usize = 1;
    for name in [SONAME, names::VERSION]
        .into_iter()
        .chain(exports.iter().map(|export| export.name))
    {
        len = len.checked_add(name.len())?.checked_add(1)?;
    }
    Some(len)
}

/// Where each name landed in the string table, and how long the table came out.
#[derive(Debug)]
struct Written {
    /// Where the object's own name sits.
    soname: usize,
    /// Where the version the entries are defined under sits.
    version: usize,
    /// Where each export's name sits, in the order the exports are in.
    named: Vec<usize>,
    /// How long the table is.
    len: usize,
}

/// Writes every name the object needs. A string table starts with the empty name, which is what a
/// symbol with no name of its own carries.
fn write_strings(page: &Page, strtab: usize, exports: &[Export]) -> Option<Written> {
    page.put(strtab, 0u8)?;
    let mut strings = Strings {
        page,
        start: strtab,
        len: 1,
    };
    let soname = strings.add(SONAME)?;
    let version = strings.add(names::VERSION)?;
    let mut named = Vec::with_capacity(exports.len());
    for export in exports {
        named.push(strings.add(export.name)?);
    }
    Some(Written {
        soname,
        version,
        named,
        len: strings.len,
    })
}

/// Writes the symbol table: the empty symbol every table starts with, then one function symbol per
/// export.
fn write_symbols(page: &Page, symtab: usize, exports: &[Export], named: &[usize]) -> Option<()> {
    page.put(
        symtab,
        Sym {
            name: 0,
            info: 0,
            other: 0,
            shndx: 0,
            value: 0,
            size: 0,
        },
    )?;
    for (index, (export, &name)) in exports.iter().zip(named).enumerate() {
        // A value is what a reader adds the bias to, and the bias is zero, so it is the address of
        // the function as it stands: ours here in this library, or the genuine vDSO's own.
        page.put(
            symtab.checked_add(index.checked_add(1)?.checked_mul(size_of::<Sym>())?)?,
            Sym {
                name: u32::try_from(name).ok()?,
                info: GLOBAL_FUNC,
                other: 0,
                shndx: SHN_DEFINED,
                value: as_u64(export.target)?,
                size: 0,
            },
        )?;
    }
    Some(())
}

/// Writes the hash table a reader looks a name up through: the two counts, then a bucket per hash
/// and a chain linking the symbols that share one.
fn write_hash(page: &Page, hash: usize, buckets: usize, exports: &[Export]) -> Option<()> {
    let symbols = exports.len().checked_add(1)?;
    let bucket_at = |index: usize| hash.checked_add((2 + index).checked_mul(size_of::<u32>())?);
    let chain_at =
        |index: usize| hash.checked_add((2 + buckets + index).checked_mul(size_of::<u32>())?);

    let width = u32::try_from(buckets).ok()?;
    page.put(hash, width)?;
    page.put(
        hash.checked_add(size_of::<u32>())?,
        u32::try_from(symbols).ok()?,
    )?;
    for index in 0..buckets {
        page.put(bucket_at(index)?, 0u32)?;
    }
    for index in 0..symbols {
        page.put(chain_at(index)?, 0u32)?;
    }

    // Each symbol goes at the head of its bucket's chain, which is the order a reader walks.
    for (index, export) in exports.iter().enumerate() {
        let index = index.checked_add(1)?;
        let bucket = usize::try_from(elf::hash(export.name) % width).ok()?;
        let head: u32 = page.get(bucket_at(bucket)?)?;
        page.put(chain_at(index)?, head)?;
        page.put(bucket_at(bucket)?, u32::try_from(index).ok()?)?;
    }
    Some(())
}

/// Writes the version definitions: the object's own name, then the version the entries carry.
fn write_versions(page: &Page, verdef: usize, soname: usize, version: usize) -> Option<()> {
    let aux = u32::try_from(size_of::<Verdef>()).ok()?;
    let second = verdef.checked_add(VERDEF_STRIDE)?;

    page.put(
        verdef,
        Verdef {
            version: 1,
            flags: VER_FLG_BASE,
            ndx: 1,
            cnt: 1,
            hash: elf::hash(SONAME),
            aux,
            next: u32::try_from(VERDEF_STRIDE).ok()?,
        },
    )?;
    page.put(
        verdef.checked_add(size_of::<Verdef>())?,
        Verdaux {
            name: u32::try_from(soname).ok()?,
            next: 0,
        },
    )?;
    page.put(
        second,
        Verdef {
            version: 1,
            flags: 0,
            ndx: VERSION_INDEX,
            cnt: 1,
            hash: elf::hash(names::VERSION),
            aux,
            next: 0,
        },
    )?;
    page.put(
        second.checked_add(size_of::<Verdef>())?,
        Verdaux {
            name: u32::try_from(version).ok()?,
            next: 0,
        },
    )
}

/// A length or an offset as the ELF field that carries it, or `None` when it does not fit.
fn as_u64(value: usize) -> Option<u64> {
    u64::try_from(value).ok()
}

/// The same, for the header's own narrower counts.
fn as_u16(value: usize) -> Option<u16> {
    u16::try_from(value).ok()
}

/// The sixteen bytes every ELF object starts with: the magic, the width, the byte order, and the
/// version, then padding.
fn identification() -> [u8; 16] {
    let mut ident = [0u8; 16];
    if let Some(head) = ident.get_mut(..MAGIC.len()) {
        head.copy_from_slice(&MAGIC);
    }
    if let Some(class) = ident.get_mut(4) {
        *class = CLASS_64;
    }
    if let Some(data) = ident.get_mut(5) {
        *data = DATA_LSB;
    }
    if let Some(version) = ident.get_mut(6) {
        *version = VERSION;
    }
    ident
}

/// The string table being filled: where it starts, and how much of it is written.
struct Strings<'page> {
    /// The page it is written in.
    page: &'page Page,
    /// Where the table starts in that page.
    start: usize,
    /// How long it is so far, the empty name at its head included.
    len: usize,
}

impl Strings<'_> {
    /// Adds a name, answering where in the table it starts.
    fn add(&mut self, name: &[u8]) -> Option<usize> {
        let at = self.len;
        self.page.put_bytes(self.start.checked_add(at)?, name)?;
        self.page
            .put(self.start.checked_add(at)?.checked_add(name.len())?, 0u8)?;
        self.len = at.checked_add(name.len())?.checked_add(1)?;
        Some(at)
    }
}

/// The page the object is built in: a bump cursor over memory nobody else holds.
#[derive(Debug)]
struct Page {
    /// Where the page starts.
    base: usize,
    /// How long it is.
    len: usize,
    /// The first byte not yet spoken for.
    at: usize,
}

impl Page {
    /// A page with nothing in it yet.
    const fn new(base: usize, len: usize) -> Self {
        Self { base, len, at: 0 }
    }

    /// Reserves room for `count` values of `T`, aligned the way `T` wants, and answers where they
    /// start. `None` when the page has no room left, which is what keeps every write below inside
    /// it.
    fn reserve<T>(&mut self, count: usize) -> Option<usize> {
        let start = round_up(self.at, align_of::<T>());
        let end = start.checked_add(count.checked_mul(size_of::<T>())?)?;
        if end > self.len {
            return None;
        }
        self.at = end;
        Some(start)
    }

    /// Writes `value` at `offset`, which a [`Page::reserve`] handed out.
    fn put<T>(&self, offset: usize, value: T) -> Option<()> {
        let at = self.address::<T>(offset, 1)?;
        // Safety: the address is inside the mapping, which this process made and nobody else
        // holds, and it has room for a `T`.
        unsafe { std::ptr::with_exposed_provenance_mut::<T>(at).write_unaligned(value) };
        Some(())
    }

    /// Reads back a value this page was given, for a table that links its own entries.
    fn get<T>(&self, offset: usize) -> Option<T> {
        let at = self.address::<T>(offset, 1)?;
        // Safety: as [`Page::put`], and the value was written by one.
        Some(unsafe { std::ptr::with_exposed_provenance::<T>(at).read_unaligned() })
    }

    /// Writes `bytes` at `offset`.
    fn put_bytes(&self, offset: usize, bytes: &[u8]) -> Option<()> {
        let at = self.address::<u8>(offset, bytes.len())?;
        // Safety: the address is inside the mapping and has room for this many bytes, which come
        // from a slice that cannot overlap it.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                std::ptr::with_exposed_provenance_mut::<u8>(at),
                bytes.len(),
            );
        }
        Some(())
    }

    /// Where `count` values of `T` sit, or `None` when they would not fit in the page.
    fn address<T>(&self, offset: usize, count: usize) -> Option<usize> {
        let end = offset.checked_add(count.checked_mul(size_of::<T>())?)?;
        (end <= self.len).then(|| self.base.wrapping_add(offset))
    }
}

/// How long this system's pages are.
fn page_size() -> usize {
    // Safety: `sysconf` takes a name and answers a number, touching nothing of ours.
    let answer = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(answer).unwrap_or(4096).max(4096)
}

/// Maps `len` bytes for the object to be built in.
fn map(len: usize) -> Option<usize> {
    // Safety: a fresh anonymous mapping asks the kernel for memory and touches nothing of ours.
    let mapped = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if std::ptr::eq(mapped, libc::MAP_FAILED) {
        return None;
    }
    Some(mapped.expose_provenance())
}

/// Takes the write permission away, so the object is what every other mapped object is: read-only
/// to the program it was built for.
fn protect(base: usize, len: usize) -> Option<()> {
    // Safety: the mapping is this process's own, made just above.
    let changed = unsafe {
        libc::mprotect(
            std::ptr::with_exposed_provenance_mut(base),
            len,
            libc::PROT_READ,
        )
    };
    (changed == 0).then_some(())
}

/// Gives the page back, for a build that could not be finished.
fn unmap(base: usize, len: usize) {
    // Safety: the mapping is this process's own and nothing points at it yet.
    unsafe { libc::munmap(std::ptr::with_exposed_provenance_mut(base), len) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The image this process would hand a target, read back the way a target reads one.
    fn built() -> (usize, Genuine) {
        let genuine = super::super::auxv::genuine().expect("a process the kernel started");
        let base = build(genuine).expect("the image is built");
        // Safety: the address is the image just built, which is an ELF object like any other.
        let parsed = unsafe { Genuine::parse(base) }.expect("the image parses as a vDSO");
        (base, parsed)
    }

    #[test]
    fn the_image_exports_the_clock_entries_under_this_architecture_names() {
        let (_, image) = built();
        for name in [names::CLOCK_GETTIME, names::GETTIMEOFDAY, names::TIME] {
            assert!(
                image.address_of(name).is_some(),
                "{} is missing",
                String::from_utf8_lossy(name)
            );
        }
        assert_eq!(image.address_of(b"__vdso_no_such_entry"), None);
    }

    #[test]
    #[expect(
        clippy::fn_to_numeric_cast_any,
        reason = "the address in the object is what this compares the function against"
    )]
    fn the_clock_entries_are_this_library_own_functions() {
        let (_, image) = built();
        let clock_gettime: unsafe extern "C" fn(
            libc::clockid_t,
            *mut libc::timespec,
        ) -> libc::c_long = entries::clock_gettime;
        assert_eq!(
            image.address_of(names::CLOCK_GETTIME),
            Some((clock_gettime as *const ()).expose_provenance())
        );
    }

    #[test]
    fn everything_else_the_kernel_vdso_has_points_back_into_it() {
        let genuine = super::super::auxv::genuine().expect("a process the kernel started");
        // Safety: the address is the one the auxiliary vector carries.
        let kernel = unsafe { Genuine::parse(genuine) }.expect("the kernel's own vDSO parses");
        let (_, image) = built();
        for name in [names::CLOCK_GETRES, names::GETCPU] {
            assert_eq!(
                image.address_of(name),
                kernel.address_of(name),
                "{} does not forward",
                String::from_utf8_lossy(name)
            );
        }
    }

    #[test]
    fn the_image_is_a_page_the_program_cannot_write() {
        let (base, _) = built();
        assert_eq!(base % page_size(), 0);
        // Safety: the page is mapped, and the first byte of it is the ELF magic.
        let magic = unsafe { std::ptr::with_exposed_provenance::<[u8; 4]>(base).read() };
        assert_eq!(magic, MAGIC);
    }
}
