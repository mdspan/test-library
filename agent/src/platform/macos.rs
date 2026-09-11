//! macOS: dyld binds each call to the library that exports the name, two-level, so a second
//! definition of a libc name here would never be reached. What puts us in front of the program is
//! the `__DATA,__interpose` section instead: a table of pairs, replacement and replacee, that dyld
//! applies to every image in the process but the one the table came from.
//!
//! Two traps come with the table, and the shape below answers both. A replacement with no row is
//! never called, so [`replacements!`] emits the function and its row from one list and neither can
//! be written without the other. And exporting the libc name would make the pair self-referential,
//! since the replacee would then resolve to us, so every replacement carries a private name,
//! `libtimewarp_clock_gettime` and the rest, and the libc name is exported nowhere.
//!
//! Only the clock group is here. The Mach clocks, the sleeps, the timeouts, the timers, the file
//! timestamps and the setters land later, each entry a row of its own.
//!
//! What each row catches, which is a question of its own here because dyld rewrites bindings
//! rather than code, and the shared cache was bound to itself when it was built:
//!
//! | Row | Catches | Misses |
//! | --- | --- | --- |
//! | `clock_gettime` | The program's own call, and any library outside the shared cache. | Reads inside the cache, which reach the commpage without binding to a name. |
//! | `gettimeofday` | The same. | The cache's own use of it. |
//! | `time` | The same. | Nothing beyond that. |
//! | `ftime` | The same, for a program old enough to call it. | Nothing beyond that. |
//! | `timespec_get` | The same. | Nothing beyond that. |
//! | `clock` | The same. | Nothing beyond that: the answer is computed here rather than passed on. |
//! | `times` | The same. | The `getrusage` and `gettimeofday` libSystem builds it from, which is why the answer is scaled once, here. |
//! | `getrusage` | The same. | The cache's own calls, `times` among them. |
//!
//! None of those misses costs the group anything. Every row above answers from the session rather
//! than by calling another row, and a genuine reading of ours reaches libSystem by address, which
//! no table redirects, so nothing here is converted twice or converted back.

use std::ffi::c_void;

use crate::interpose;

/// One row of the table dyld reads at load: the replacement, and the function it stands in for.
#[repr(C)]
struct Row {
    /// Ours, which every call to `replacee` outside this library reaches instead.
    replacement: *const c_void,
    /// libSystem's, bound by the loader like any other reference to a name it exports.
    replacee: *const c_void,
}

// Safety: a row is two addresses of functions that live as long as the process, written once by
// the loader before any thread of the program exists and only read afterwards.
unsafe impl Sync for Row {}

// The three names of the group libc's own bindings do not spell for this platform. `clock` and
// `ftime` have been in libSystem for as long as macOS has existed, the second of them obsolete for
// nearly as long, and `timespec_get` arrived at 10.15.
unsafe extern "C" {
    /// The CPU time this process has consumed, in `CLOCKS_PER_SEC` units.
    fn clock() -> libc::clock_t;
    /// C11's own wall-clock read.
    fn timespec_get(ts: *mut libc::timespec, base: libc::c_int) -> libc::c_int;
    /// The wall clock in whole seconds and milliseconds, which POSIX withdrew and macOS kept.
    fn ftime(tp: *mut interpose::Timeb) -> libc::c_int;
}

/// Declares a replacement: the function the program reaches, and the row that puts it there.
///
/// One list, because a replacement with no row in the table is simply never called, and a
/// compiler that is happy proves nothing about that. The row is what the build step checks back
/// out of the built library.
macro_rules! replacements {
    ($(
        $(#[doc = $doc:expr])*
        replaces $genuine:path;
        fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty $body:block
    )*) => {
        $(
            $(#[doc = $doc])*
            ///
            /// # Safety
            /// Called by the program with libc's contract for this function.
            #[unsafe(no_mangle)]
            pub(super) unsafe extern "C" fn $name($($arg: $ty),*) -> $ret $body
        )*

        /// How many rows the table has, which is one per replacement above.
        const ROWS: usize = [$(stringify!($name)),*].len();

        /// The table itself. dyld reads this section at load and applies every row in it to every
        /// image in the process but ours.
        ///
        /// `#[used]` because nothing in this library ever reads it: the linker strips what nothing
        /// keeps, and a stripped table is a library that changes nothing at all.
        #[used]
        #[unsafe(link_section = "__DATA,__interpose")]
        #[expect(
            clippy::fn_to_numeric_cast_any,
            reason = "a row is two addresses, which is how dyld reads the table, and taking a \
                      function's address as data is the cast"
        )]
        static TABLE: [Row; ROWS] = [$(
            Row {
                replacement: $name as *const c_void,
                replacee: $genuine as *const c_void,
            }
        ),*];
    };
}

replacements! {
    /// A clock read in a `timespec`, under the id the program named.
    replaces libc::clock_gettime;
    fn libtimewarp_clock_gettime(clock: libc::clockid_t, ts: *mut libc::timespec) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { interpose::clock_gettime(clock, ts) }
    }

    /// The wall clock in a `timeval`.
    replaces libc::gettimeofday;
    fn libtimewarp_gettimeofday(tv: *mut libc::timeval, tz: *mut c_void) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { interpose::gettimeofday(tv, tz) }
    }

    /// The wall clock in whole seconds.
    replaces libc::time;
    fn libtimewarp_time(t: *mut libc::time_t) -> libc::time_t {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { interpose::time(t) }
    }

    /// C11's own wall-clock read, which answers the base it was given.
    replaces timespec_get;
    fn libtimewarp_timespec_get(ts: *mut libc::timespec, base: libc::c_int) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { interpose::timespec_get(ts, base) }
    }

    /// The wall clock in whole seconds and milliseconds, for a program old enough to ask this way.
    replaces ftime;
    fn libtimewarp_ftime(tp: *mut interpose::Timeb) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { interpose::ftime(tp) }
    }

    /// The CPU time this process has consumed, which takes no argument and so needs no contract.
    replaces clock;
    fn libtimewarp_clock() -> libc::clock_t {
        interpose::clock()
    }

    /// The process times: consumption in the buffer, elapsed time as the answer.
    replaces libc::times;
    fn libtimewarp_times(buf: *mut libc::tms) -> libc::clock_t {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { interpose::times(buf) }
    }

    /// The resource usage of a target, whose two times follow the Cpu family.
    replaces libc::getrusage;
    fn libtimewarp_getrusage(who: libc::c_int, usage: *mut libc::rusage) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { interpose::getrusage(who, usage) }
    }
}
