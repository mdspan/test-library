//! The table dyld reads, checked back out of the library that was built.
//!
//! A replacement with no row in `__DATA,__interpose` is never called, so a library that compiles
//! proves nothing: the row could have been left out, or dead-stripped by a linker that was told to
//! strip what nothing keeps. These cases parse the section out of the built file and check that
//! every replacement is in it, and that what the library exports is private names alone, since
//! exporting a libc name would make the pair self-referential.
#![cfg(target_os = "macos")]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "test support: a failed assumption should fail the test loudly, and not every helper \
              is used by every case"
)]

mod harness;

use std::collections::BTreeMap;

/// Every replacement the library carries, by the private name it exports and the libc name that
/// name stands in for. Every group, the Mach clocks included.
///
/// The libc name is the one the library must never export, so a spelling that varies by
/// architecture is written here as the plain one: we export neither.
const REPLACEMENTS: &[(&str, &str)] = &[
    ("libtimewarp_adjtime", "adjtime"),
    ("libtimewarp_aio_suspend", "aio_suspend"),
    ("libtimewarp_alarm", "alarm"),
    ("libtimewarp_clock", "clock"),
    ("libtimewarp_clock_get_time", "clock_get_time"),
    ("libtimewarp_clock_gettime", "clock_gettime"),
    ("libtimewarp_clock_gettime_nsec_np", "clock_gettime_nsec_np"),
    ("libtimewarp_clock_settime", "clock_settime"),
    ("libtimewarp_fstat", "fstat"),
    ("libtimewarp_fstatat", "fstatat"),
    ("libtimewarp_ftime", "ftime"),
    ("libtimewarp_futimens", "futimens"),
    ("libtimewarp_futimes", "futimes"),
    ("libtimewarp_getitimer", "getitimer"),
    ("libtimewarp_getrusage", "getrusage"),
    ("libtimewarp_gettimeofday", "gettimeofday"),
    (
        "libtimewarp_host_get_clock_service",
        "host_get_clock_service",
    ),
    ("libtimewarp_kevent", "kevent"),
    ("libtimewarp_kevent64", "kevent64"),
    ("libtimewarp_lstat", "lstat"),
    ("libtimewarp_lutimes", "lutimes"),
    ("libtimewarp_mach_absolute_time", "mach_absolute_time"),
    ("libtimewarp_mach_wait_until", "mach_wait_until"),
    ("libtimewarp_nanosleep", "nanosleep"),
    ("libtimewarp_poll", "poll"),
    ("libtimewarp_pselect", "pselect"),
    (
        "libtimewarp_pthread_cond_timedwait",
        "pthread_cond_timedwait",
    ),
    ("libtimewarp_select", "select"),
    ("libtimewarp_setitimer", "setitimer"),
    ("libtimewarp_settimeofday", "settimeofday"),
    ("libtimewarp_sleep", "sleep"),
    ("libtimewarp_stat", "stat"),
    ("libtimewarp_time", "time"),
    ("libtimewarp_times", "times"),
    ("libtimewarp_timespec_get", "timespec_get"),
    ("libtimewarp_usleep", "usleep"),
    ("libtimewarp_utime", "utime"),
    ("libtimewarp_utimensat", "utimensat"),
    ("libtimewarp_utimes", "utimes"),
];

/// The rows a build has only where libSystem has the name behind them: the suffixed spellings of
/// `select` and `pselect` that `x86_64` keeps, the private form of `kevent`, and the three Mach
/// counters that arrived after the one every release has had. `build.rs` asks the linker for each,
/// and sets the `cfg` this reads.
const VARIANTS: &[(&str, &str)] = &[
    #[cfg(darwin_select_1050)]
    ("libtimewarp_select_1050", "select$1050"),
    #[cfg(darwin_select_extsn)]
    ("libtimewarp_select_extsn", "select$DARWIN_EXTSN"),
    #[cfg(darwin_pselect_extsn)]
    ("libtimewarp_pselect_extsn", "pselect$DARWIN_EXTSN"),
    #[cfg(darwin_kevent_qos)]
    ("libtimewarp_kevent_qos", "kevent_qos"),
    #[cfg(darwin_mach_approximate_time)]
    ("libtimewarp_mach_approximate_time", "mach_approximate_time"),
    #[cfg(darwin_mach_continuous_time)]
    ("libtimewarp_mach_continuous_time", "mach_continuous_time"),
    #[cfg(darwin_mach_continuous_approximate_time)]
    (
        "libtimewarp_mach_continuous_approximate_time",
        "mach_continuous_approximate_time",
    ),
];

/// Every replacement of this build: the ones every macOS has, and the ones this one found.
fn replacements() -> Vec<(&'static str, &'static str)> {
    let mut every = REPLACEMENTS.to_vec();
    every.extend_from_slice(VARIANTS);
    every
}

/// The one exported name that replaces nothing: what a hook or a controller loaded into a faked
/// process calls to reach the genuine clock.
const GENUINE_CLOCK: &str = "libtimewarp_genuine_clock_gettime";

/// What a 64-bit little-endian Mach-O image opens with.
const MH_MAGIC_64: u32 = 0xfeed_facf;

/// The load command carrying a 64-bit segment and the sections inside it.
const LC_SEGMENT_64: u32 = 0x19;

/// The load command carrying the symbol table.
const LC_SYMTAB: u32 = 0x2;

/// How wide one row of the table is: the replacement and the replacee, one pointer each.
const ROW: usize = 16;

/// Which bits of a chained-fixup rebase carry the address it was written for. A linker writes the
/// section either as plain addresses or as a chain of these, so a row is read as both and matches
/// under whichever it was written as. The address is an offset from the image's base, which for a
/// library the loader may place anywhere is what the symbol table carries too.
const CHAINED_TARGET: u64 = (1 << 36) - 1;

fn word32(image: &[u8], at: usize) -> u32 {
    let bytes = image.get(at..at + 4).expect("inside the image");
    u32::from_le_bytes(bytes.try_into().unwrap())
}

fn word64(image: &[u8], at: usize) -> u64 {
    let bytes = image.get(at..at + 8).expect("inside the image");
    u64::from_le_bytes(bytes.try_into().unwrap())
}

/// A fixed sixteen-byte name, as a segment and a section carry theirs.
fn name16(image: &[u8], at: usize) -> String {
    let bytes = image.get(at..at + 16).expect("inside the image");
    let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(16);
    String::from_utf8_lossy(bytes.get(..end).unwrap()).into_owned()
}

/// A NUL-terminated name out of the string table.
fn c_string(image: &[u8], at: usize) -> String {
    let rest = image.get(at..).expect("inside the image");
    let end = rest
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(rest.len());
    String::from_utf8_lossy(rest.get(..end).unwrap()).into_owned()
}

/// Every load command of the image, as a kind and where it starts.
fn load_commands(image: &[u8]) -> Vec<(u32, usize)> {
    assert_eq!(
        word32(image, 0),
        MH_MAGIC_64,
        "the library is not a 64-bit Mach-O image"
    );
    let count = word32(image, 16);
    let mut at = 32;
    let mut found = Vec::new();
    for _ in 0..count {
        found.push((word32(image, at), at));
        at += usize::try_from(word32(image, at + 4)).unwrap();
    }
    found
}

/// Where a section's bytes are in the file, and how many there are.
fn section(image: &[u8], segment: &str, wanted: &str) -> Option<(usize, usize)> {
    for (kind, at) in load_commands(image) {
        if kind != LC_SEGMENT_64 {
            continue;
        }
        let sections = usize::try_from(word32(image, at + 64)).unwrap();
        for index in 0..sections {
            // The segment command, then one eighty-byte section entry per section.
            let entry = at + 72 + index * 80;
            if name16(image, entry) == wanted && name16(image, entry + 16) == segment {
                let size = usize::try_from(word64(image, entry + 40)).unwrap();
                let offset = usize::try_from(word32(image, entry + 48)).unwrap();
                return Some((offset, size));
            }
        }
    }
    None
}

/// Every symbol the image defines, by the name the linker spells it with.
fn symbols(image: &[u8]) -> BTreeMap<String, u64> {
    let mut found = BTreeMap::new();
    for (kind, at) in load_commands(image) {
        if kind != LC_SYMTAB {
            continue;
        }
        let table = usize::try_from(word32(image, at + 8)).unwrap();
        let count = usize::try_from(word32(image, at + 12)).unwrap();
        let strings = usize::try_from(word32(image, at + 16)).unwrap();
        for index in 0..count {
            // One sixteen-byte entry per symbol: the name's offset into the string table, three
            // fields naming where it lives, and the address itself.
            let entry = table + index * 16;
            let name = c_string(
                image,
                strings + usize::try_from(word32(image, entry)).unwrap(),
            );
            found.insert(name, word64(image, entry + 8));
        }
    }
    found
}

#[test]
fn every_replacement_has_a_row_in_the_interpose_table() {
    let image = std::fs::read(harness::library()).unwrap();
    let (at, size) = section(&image, "__DATA", "__interpose")
        .expect("the library carries a __DATA,__interpose section");
    assert_eq!(
        size % ROW,
        0,
        "the table is {size} bytes, which is no whole number of rows"
    );
    let wanted = replacements();
    assert_eq!(
        size / ROW,
        wanted.len(),
        "the table has {} rows for {} replacements",
        size / ROW,
        wanted.len()
    );

    let defined = symbols(&image);
    let rows: Vec<u64> = (0..size / ROW)
        .map(|row| word64(&image, at + row * ROW))
        .collect();
    for (name, _) in &wanted {
        let address = *defined
            .get(&format!("_{name}"))
            .unwrap_or_else(|| panic!("{name} is not in the library's symbol table"));
        assert!(
            rows.iter()
                .any(|row| *row == address || row & CHAINED_TARGET == address),
            "{name} is at {address:#x} and no row of the table names it: {rows:#x?}"
        );
    }
}

#[test]
fn the_library_exports_private_names_and_never_a_libc_one() {
    // Defined and external, which is what dyld hands another image.
    let output = std::process::Command::new("nm")
        .args(["-g", "-U"])
        .arg(harness::library())
        .output()
        .expect("nm");
    assert!(output.status.success());

    let mut exported: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(2))
        // Every C symbol carries a leading underscore here.
        .map(|name| name.strip_prefix('_').unwrap_or(name).to_owned())
        .collect();
    exported.sort();

    let every = replacements();
    let mut wanted: Vec<String> = every
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .chain([GENUINE_CLOCK.to_owned()])
        .collect();
    wanted.sort();
    assert_eq!(exported, wanted);

    // And no libc name at all. Exporting one would make its row self-referential: the replacee
    // would resolve to us, and the replacement would call itself.
    for (_, libc_name) in &every {
        assert!(
            !exported.iter().any(|name| name == libc_name),
            "{libc_name} is exported, which would make its own row point back at us"
        );
    }
}
