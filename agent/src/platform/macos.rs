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
//! Every group is here, the Mach clocks included: those read a hardware counter rather than a
//! clock id, so a row of one converts through `mach_timebase_info` on the way out and back, and so
//! does `mach_wait_until`, the one sleep measured in their ticks. `crate::mach` has the rules.
//!
//! Some names have more than one spelling, and which the program calls is the ABI it was compiled
//! against: on `x86_64` `select` has three, the plain one of 10.4, the `$1050` of the conforming
//! semantics and the `$DARWIN_EXTSN` of the unlimited descriptor sets, `pselect` has the first and
//! the last of those, and the four `stat` names carry the `$INODE64` of the wider buffer. On
//! aarch64 there is one spelling of each. A
//! spelling is a row of its own, because dyld matches a row by address and two spellings are two
//! addresses. Whether the image has one at all is asked of the linker in `build.rs` rather than
//! assumed, so a name that is not there costs a row rather than the build.
//!
//! What each row catches. Every row catches the program's own call and any library outside the
//! shared cache. dyld rewrites pointers rather than code, so a call one library of the cache makes
//! into another reaches a row when it goes through a pointer. On `x86_64` every such call does. On
//! arm64 the cache builder turned them into direct branches, except for the names it keeps
//! interposable, which among ours are `time`, `ftime`, `times`, `poll`, `pthread_cond_timedwait`,
//! `setitimer`, `getitimer`, `stat`, `fstat` and `lstat`. A call inside one library is a direct
//! branch on both. "The cache's own use" below is the calls that reach no row by that rule.
//!
//! | Row | Also catches | Misses |
//! | --- | --- | --- |
//! | `clock_gettime` | Nothing. | The cache's own use of it, and reads inside the cache that reach the commpage without calling it. |
//! | the four `mach_*_time` names | Nothing. | The cache's own use of them, and a program that reads the counter register or the commpage itself rather than calling any of them. |
//! | `clock_gettime_nsec_np` | Nothing. | The cache's own use of it. |
//! | `host_get_clock_service` | Nothing, and it converts nothing either: the port it hands back is a name, and the read on that name is the row below. | The cache's own calls, whose ports this never sees, so a read on one of those passes through. |
//! | `clock_get_time` | Nothing. | A port no row above handed out, and the `mach_msg` a caller could send the port itself. |
//! | `gettimeofday`, `time`, `timespec_get`, `ftime` | Nothing. | The cache's own use of them. |
//! | `clock` | Nothing: the answer is computed here rather than passed on. | Nothing beyond that. |
//! | `times` | Nothing. | The cache's own use of it. The genuine answer is built from `getrusage` and `gettimeofday` called from this library, so it is scaled once, here. |
//! | `getrusage` | Nothing. | The cache's own calls. |
//! | `nanosleep` | Nothing. | The `sleep` and `usleep` libSystem builds out of it, which is why each of those is a row too. |
//! | `sleep`, `usleep` | Nothing. | Nothing beyond that: each converts and then blocks in the genuine `nanosleep`. |
//! | `mach_wait_until` | Nothing. | The cache's own use of it. |
//! | `poll` | Nothing. | The cache's own use of it. |
//! | the three `select` names, the two `pselect` names | Nothing. | The cache's own use of them. Each answers through the widest spelling the image has, so nothing a caller could name is refused here that would have been allowed without us. |
//! | `kevent`, `kevent64` | Nothing. | The cache's own use of them. |
//! | `kevent_qos` | Nothing, and it converts nothing either: the call names no deadline. | The timers a caller arms through it as events, libdispatch's among them, which stay genuine. |
//! | `pthread_cond_timedwait` | Nothing. | The relative form beside it, which belongs to no group. |
//! | `aio_suspend` | Nothing. | Nothing beyond that. |
//! | `setitimer`, `getitimer` | Nothing. | The cache's own use of them. |
//! | `alarm` | Nothing. | Nothing beyond that: it converts and arms through the genuine `setitimer`. |
//! | the four `stat` names | Nothing. | The cache's own use of them, and the unsuffixed `x86_64` names, which fill a buffer of another shape and so have no row. |
//! | `utime` | Nothing. | Nothing beyond that: the pair converts here and is set through the genuine `utimes`, the call libSystem builds `utime` out of. |
//! | `utimes`, `futimes`, `lutimes`, `utimensat`, `futimens` | Nothing. | The cache's own use of them. |
//! | `settimeofday`, `clock_settime`, `adjtime` | Nothing. | The cache's own use of them. |
//!
//! None of those misses costs a group anything. Every row answers from the session rather than by
//! calling another row, and a genuine call of ours is a call through this library's own import of
//! the name, which dyld binds past the table. Where libSystem builds a genuine function out of
//! another name the table carries, `utime` out of `utimes` and the counter clocks out of the
//! `mach_*_time` names among them, the genuine side is built here from the inner calls instead,
//! since on `x86_64` libSystem's own inner call would come back through a row. So nothing here is
//! converted twice or converted back.

use std::ffi::{c_char, c_void};

use crate::{interpose, mach, setter, stamp, timer, wait};

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

// The names libc's own bindings do not spell for this platform, spell for one architecture of it,
// or spell only behind a deprecation. `clock` and `ftime` have been in libSystem for as long as
// macOS has existed, the second of them obsolete for nearly as long, and `timespec_get` arrived at
// 10.15. The Mach names are there but deprecated in favour of a crate this workspace does not
// depend on, and the Mach routines proper are not there at all. The rest are the
// spellings: the binding for `select` is the suffixed one on `x86_64` and the plain one on
// aarch64, and the binding for the `stat` names is the plain one on both, which on `x86_64` is the
// narrower
// buffer rather than the one this libc declares. A row has to name the address it means, so each
// is written out here.
unsafe extern "C" {
    /// The CPU time this process has consumed, in `CLOCKS_PER_SEC` units.
    fn clock() -> libc::clock_t;
    /// C11's own wall-clock read.
    fn timespec_get(ts: *mut libc::timespec, base: libc::c_int) -> libc::c_int;
    /// The wall clock in whole seconds and milliseconds, which POSIX withdrew and macOS kept.
    fn ftime(tp: *mut interpose::Timeb) -> libc::c_int;

    /// The unsuffixed `select`, which on `x86_64` is the wait as 10.4 defined it.
    #[link_name = "select"]
    fn plain_select(
        nfds: libc::c_int,
        readfds: *mut libc::fd_set,
        writefds: *mut libc::fd_set,
        exceptfds: *mut libc::fd_set,
        timeout: *mut libc::timeval,
    ) -> libc::c_int;

    /// The conforming `select` of 10.5, which `x86_64` keeps under a suffix and aarch64 does not
    /// have at all: there the one spelling above is already this one.
    #[cfg(darwin_select_1050)]
    #[link_name = "select$1050"]
    fn select_1050(
        nfds: libc::c_int,
        readfds: *mut libc::fd_set,
        writefds: *mut libc::fd_set,
        exceptfds: *mut libc::fd_set,
        timeout: *mut libc::timeval,
    ) -> libc::c_int;

    /// The `select` a program compiled against the Darwin extensions calls, which takes descriptor
    /// sets of any width.
    #[cfg(darwin_select_extsn)]
    #[link_name = "select$DARWIN_EXTSN"]
    fn select_extsn(
        nfds: libc::c_int,
        readfds: *mut libc::fd_set,
        writefds: *mut libc::fd_set,
        exceptfds: *mut libc::fd_set,
        timeout: *mut libc::timeval,
    ) -> libc::c_int;

    /// The same for `pselect`.
    #[cfg(darwin_pselect_extsn)]
    #[link_name = "pselect$DARWIN_EXTSN"]
    fn pselect_extsn(
        nfds: libc::c_int,
        readfds: *mut libc::fd_set,
        writefds: *mut libc::fd_set,
        exceptfds: *mut libc::fd_set,
        timeout: *const libc::timespec,
        sigmask: *const libc::sigset_t,
    ) -> libc::c_int;

    /// The `stat` that fills the buffer this libc declares.
    #[cfg_attr(darwin_inode64, link_name = "stat$INODE64")]
    #[cfg_attr(not(darwin_inode64), link_name = "stat")]
    fn wide_stat(path: *const c_char, buf: *mut libc::stat) -> libc::c_int;

    /// The `fstat` of that set.
    #[cfg_attr(darwin_inode64, link_name = "fstat$INODE64")]
    #[cfg_attr(not(darwin_inode64), link_name = "fstat")]
    fn wide_fstat(fd: libc::c_int, buf: *mut libc::stat) -> libc::c_int;

    /// The `lstat` of that set.
    #[cfg_attr(darwin_inode64, link_name = "lstat$INODE64")]
    #[cfg_attr(not(darwin_inode64), link_name = "lstat")]
    fn wide_lstat(path: *const c_char, buf: *mut libc::stat) -> libc::c_int;

    /// The `fstatat` of that set.
    #[cfg_attr(darwin_inode64, link_name = "fstatat$INODE64")]
    #[cfg_attr(not(darwin_inode64), link_name = "fstatat")]
    fn wide_fstatat(
        dirfd: libc::c_int,
        path: *const c_char,
        buf: *mut libc::stat,
        flags: libc::c_int,
    ) -> libc::c_int;

    /// Ticks of the counter that stops while the machine sleeps.
    fn mach_absolute_time() -> u64;

    /// The same counter read from the last tick rather than from the hardware.
    #[cfg(darwin_mach_approximate_time)]
    fn mach_approximate_time() -> u64;

    /// Ticks of the counter that keeps counting while the machine sleeps.
    #[cfg(darwin_mach_continuous_time)]
    fn mach_continuous_time() -> u64;

    /// The same read from the last tick.
    #[cfg(darwin_mach_continuous_approximate_time)]
    fn mach_continuous_approximate_time() -> u64;

    /// A clock read in nanoseconds, under the ids `clock_gettime` takes.
    fn clock_gettime_nsec_np(clock: libc::clockid_t) -> u64;

    /// A sleep until the counter reads a given tick.
    fn mach_wait_until(deadline: u64) -> libc::kern_return_t;

    /// A port naming one of the Mach clocks.
    fn host_get_clock_service(
        host: libc::host_t,
        id: libc::c_int,
        port: *mut libc::mach_port_t,
    ) -> libc::kern_return_t;

    /// A read on such a port.
    fn clock_get_time(port: libc::mach_port_t, at: *mut mach::MachTimespec) -> libc::kern_return_t;

    /// The `kevent_qos` of the same family, a name Apple keeps private and the SDK may not let
    /// us reach at all.
    #[cfg(darwin_kevent_qos)]
    fn kevent_qos(
        kq: libc::c_int,
        changelist: *const c_void,
        nchanges: libc::c_int,
        eventlist: *mut c_void,
        nevents: libc::c_int,
        data_out: *mut c_void,
        data_available: *mut libc::size_t,
        flags: libc::c_uint,
    ) -> libc::c_int;
}

/// Declares a replacement: the function the program reaches, and the row that puts it there.
///
/// One list, because a replacement with no row in the table is simply never called, and a
/// compiler that is happy proves nothing about that. The row is what the build step checks back
/// out of the built library.
macro_rules! replacements {
    ($(
        $(#[doc = $doc:expr])*
        $(#[cfg($on:meta)])?
        replaces $genuine:path;
        fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty $body:block
    )*) => {
        $(
            $(#[doc = $doc])*
            ///
            /// # Safety
            /// Called by the program with libc's contract for this function.
            $(#[cfg($on)])?
            #[unsafe(no_mangle)]
            pub(super) unsafe extern "C" fn $name($($arg: $ty),*) -> $ret $body
        )*

        /// How many rows the table has, which is one per replacement above that this build has a
        /// name for.
        const ROWS: usize = [$( $(#[cfg($on)])? stringify!($name) ),*].len();

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
            $(#[cfg($on)])?
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

    // The Mach clocks. Each reads a counter rather than a clock id, so the family's nanoseconds
    // convert back into ticks on the way out; `mach` has the rules. Which family a counter belongs
    // to is whether it stops while the machine sleeps.

    /// Ticks of the counter that stops while the machine sleeps, which is the Monotonic family's.
    replaces mach_absolute_time;
    fn libtimewarp_mach_absolute_time() -> u64 {
        mach::mach_absolute_time()
    }

    /// The same counter read from the last tick, which is the same family.
    #[cfg(darwin_mach_approximate_time)]
    replaces mach_approximate_time;
    fn libtimewarp_mach_approximate_time() -> u64 {
        mach::mach_approximate_time()
    }

    /// Ticks of the counter that counts while the machine sleeps, which is Boottime's.
    #[cfg(darwin_mach_continuous_time)]
    replaces mach_continuous_time;
    fn libtimewarp_mach_continuous_time() -> u64 {
        mach::mach_continuous_time()
    }

    /// The same counter read from the last tick.
    #[cfg(darwin_mach_continuous_approximate_time)]
    replaces mach_continuous_approximate_time;
    fn libtimewarp_mach_continuous_approximate_time() -> u64 {
        mach::mach_continuous_approximate_time()
    }

    /// A clock read in nanoseconds, which follows the family of the id it was handed.
    replaces clock_gettime_nsec_np;
    fn libtimewarp_clock_gettime_nsec_np(clock: libc::clockid_t) -> u64 {
        mach::clock_gettime_nsec_np(clock)
    }

    /// A port naming one of the Mach clocks, remembered so that a read on it knows which.
    replaces host_get_clock_service;
    fn libtimewarp_host_get_clock_service(
        host: libc::host_t,
        id: libc::c_int,
        port: *mut libc::mach_port_t,
    ) -> libc::kern_return_t {
        // Safety: the caller is the program, calling under Mach's contract.
        unsafe { mach::host_get_clock_service(host, id, port) }
    }

    /// A read on such a port, which answers whichever clock the port names.
    replaces clock_get_time;
    fn libtimewarp_clock_get_time(
        port: libc::mach_port_t,
        at: *mut mach::MachTimespec,
    ) -> libc::kern_return_t {
        // Safety: the caller is the program, calling under Mach's contract.
        unsafe { mach::clock_get_time(port, at) }
    }

    // The sleeps and the timeouts. Each converts the duration or the deadline the program asked
    // for and blocks in the genuine function; `wait` has the rules.

    /// A sleep of a relative duration, which every other sleep here converts through.
    replaces libc::nanosleep;
    fn libtimewarp_nanosleep(
        req: *const libc::timespec,
        rem: *mut libc::timespec,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { wait::nanosleep(req, rem) }
    }

    /// Whole seconds of sleep, which takes a count and so needs no contract.
    replaces libc::sleep;
    fn libtimewarp_sleep(seconds: libc::c_uint) -> libc::c_uint {
        wait::sleep(seconds)
    }

    /// Microseconds of sleep, which takes a count and so needs no contract.
    replaces libc::usleep;
    fn libtimewarp_usleep(micros: libc::useconds_t) -> libc::c_int {
        wait::usleep(micros)
    }

    /// A sleep to an absolute deadline in ticks of the counter above, which converts through the
    /// timebase on both ends and is otherwise the sleep every other one here is.
    replaces mach_wait_until;
    fn libtimewarp_mach_wait_until(deadline: u64) -> libc::kern_return_t {
        mach::mach_wait_until(deadline)
    }

    /// A wait on a set of descriptors with a timeout in milliseconds.
    replaces libc::poll;
    fn libtimewarp_poll(
        fds: *mut libc::pollfd,
        nfds: libc::nfds_t,
        timeout: libc::c_int,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { wait::poll(fds, nfds, timeout) }
    }

    /// A wait on three sets of descriptors, in the spelling of 10.4.
    replaces plain_select;
    fn libtimewarp_select(
        nfds: libc::c_int,
        readfds: *mut libc::fd_set,
        writefds: *mut libc::fd_set,
        exceptfds: *mut libc::fd_set,
        timeout: *mut libc::timeval,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { wait::select(nfds, readfds, writefds, exceptfds, timeout) }
    }

    /// The same wait in the conforming spelling of 10.5, which is a second address and so a second
    /// row. Only `x86_64` has one; aarch64 keeps the single name above.
    #[cfg(darwin_select_1050)]
    replaces select_1050;
    fn libtimewarp_select_1050(
        nfds: libc::c_int,
        readfds: *mut libc::fd_set,
        writefds: *mut libc::fd_set,
        exceptfds: *mut libc::fd_set,
        timeout: *mut libc::timeval,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { wait::select(nfds, readfds, writefds, exceptfds, timeout) }
    }

    /// The same wait in the spelling a program compiled against the Darwin extensions calls.
    #[cfg(darwin_select_extsn)]
    replaces select_extsn;
    fn libtimewarp_select_extsn(
        nfds: libc::c_int,
        readfds: *mut libc::fd_set,
        writefds: *mut libc::fd_set,
        exceptfds: *mut libc::fd_set,
        timeout: *mut libc::timeval,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { wait::select(nfds, readfds, writefds, exceptfds, timeout) }
    }

    /// The same again with a `timespec` timeout and a signal mask.
    replaces libc::pselect;
    fn libtimewarp_pselect(
        nfds: libc::c_int,
        readfds: *mut libc::fd_set,
        writefds: *mut libc::fd_set,
        exceptfds: *mut libc::fd_set,
        timeout: *const libc::timespec,
        sigmask: *const libc::sigset_t,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { wait::pselect(nfds, readfds, writefds, exceptfds, timeout, sigmask) }
    }

    /// The same in the Darwin-extensions spelling.
    #[cfg(darwin_pselect_extsn)]
    replaces pselect_extsn;
    fn libtimewarp_pselect_extsn(
        nfds: libc::c_int,
        readfds: *mut libc::fd_set,
        writefds: *mut libc::fd_set,
        exceptfds: *mut libc::fd_set,
        timeout: *const libc::timespec,
        sigmask: *const libc::sigset_t,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { wait::pselect(nfds, readfds, writefds, exceptfds, timeout, sigmask) }
    }

    /// The platform's own wait on a set of events.
    replaces libc::kevent;
    fn libtimewarp_kevent(
        kq: libc::c_int,
        changelist: *const libc::kevent,
        nchanges: libc::c_int,
        eventlist: *mut libc::kevent,
        nevents: libc::c_int,
        timeout: *const libc::timespec,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { wait::kevent(kq, changelist, nchanges, eventlist, nevents, timeout) }
    }

    /// The same over the wider event, which is a function of its own rather than a spelling.
    replaces libc::kevent64;
    fn libtimewarp_kevent64(
        kq: libc::c_int,
        changelist: *const libc::kevent64_s,
        nchanges: libc::c_int,
        eventlist: *mut libc::kevent64_s,
        nevents: libc::c_int,
        flags: libc::c_uint,
        timeout: *const libc::timespec,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { wait::kevent64(kq, changelist, nchanges, eventlist, nevents, flags, timeout) }
    }

    /// The form that carries a quality-of-service class, which converts nothing because it names
    /// no deadline. The row is here so the call is caught rather than missed by omission.
    #[cfg(darwin_kevent_qos)]
    replaces kevent_qos;
    fn libtimewarp_kevent_qos(
        kq: libc::c_int,
        changelist: *const c_void,
        nchanges: libc::c_int,
        eventlist: *mut c_void,
        nevents: libc::c_int,
        data_out: *mut c_void,
        data_available: *mut libc::size_t,
        flags: libc::c_uint,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe {
            wait::kevent_qos(
                kq,
                changelist,
                nchanges,
                eventlist,
                nevents,
                data_out,
                data_available,
                flags,
            )
        }
    }

    /// A condition variable with a deadline, which this platform always reads on the wall clock.
    replaces libc::pthread_cond_timedwait;
    fn libtimewarp_pthread_cond_timedwait(
        cond: *mut libc::pthread_cond_t,
        mutex: *mut libc::pthread_mutex_t,
        abstime: *const libc::timespec,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { wait::pthread_cond_timedwait(cond, mutex, abstime) }
    }

    /// A wait for an asynchronous read, with a relative timeout.
    replaces libc::aio_suspend;
    fn libtimewarp_aio_suspend(
        list: *const *const libc::aiocb,
        nent: libc::c_int,
        timeout: *const libc::timespec,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { wait::aio_suspend(list, nent, timeout) }
    }

    // The timers. The arm converts the duration the program asked for, and the query converts the
    // genuine remaining back into the program's own units; `timer` has the rules. These three are
    // the whole group here: this platform has no POSIX timers and no timerfd.

    /// An interval timer armed, which follows Realtime or Cpu by the timer it names.
    replaces libc::setitimer;
    fn libtimewarp_setitimer(
        which: libc::c_int,
        new_value: *const libc::itimerval,
        old_value: *mut libc::itimerval,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { timer::setitimer(which, new_value, old_value) }
    }

    /// The same timer queried.
    replaces libc::getitimer;
    fn libtimewarp_getitimer(
        which: libc::c_int,
        curr_value: *mut libc::itimerval,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { timer::getitimer(which, curr_value) }
    }

    /// Whole seconds of wall-clock alarm, which takes a count and so needs no contract.
    replaces libc::alarm;
    fn libtimewarp_alarm(seconds: libc::c_uint) -> libc::c_uint {
        timer::alarm(seconds)
    }

    // The file timestamps. A read converts the buffer the genuine call filled, and a set converts
    // a copy of what the program asked for; `stamp` has the rules. The buffer here carries a birth
    // time besides the three POSIX names, which is faked with the rest.

    /// A file's timestamps by path.
    replaces wide_stat;
    fn libtimewarp_stat(path: *const c_char, buf: *mut libc::stat) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { stamp::stat(path, buf) }
    }

    /// The same by descriptor.
    replaces wide_fstat;
    fn libtimewarp_fstat(fd: libc::c_int, buf: *mut libc::stat) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { stamp::fstat(fd, buf) }
    }

    /// The same without following a link.
    replaces wide_lstat;
    fn libtimewarp_lstat(path: *const c_char, buf: *mut libc::stat) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { stamp::lstat(path, buf) }
    }

    /// The same relative to a directory.
    replaces wide_fstatat;
    fn libtimewarp_fstatat(
        dirfd: libc::c_int,
        path: *const c_char,
        buf: *mut libc::stat,
        flags: libc::c_int,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { stamp::fstatat(dirfd, path, buf, flags) }
    }

    /// A set in whole seconds, which this libc keeps as a call of its own.
    replaces libc::utime;
    fn libtimewarp_utime(path: *const c_char, times: *const libc::utimbuf) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { stamp::utime(path, times) }
    }

    /// A set in microseconds, by path.
    replaces libc::utimes;
    fn libtimewarp_utimes(
        path: *const c_char,
        times: *const libc::timeval,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { stamp::utimes(path, times) }
    }

    /// The same by descriptor.
    replaces libc::futimes;
    fn libtimewarp_futimes(
        fd: libc::c_int,
        times: *const libc::timeval,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { stamp::futimes(fd, times) }
    }

    /// The same without following a link.
    replaces libc::lutimes;
    fn libtimewarp_lutimes(
        path: *const c_char,
        times: *const libc::timeval,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { stamp::lutimes(path, times) }
    }

    /// A set in nanoseconds, relative to a directory, which is the one that carries `UTIME_NOW`
    /// and `UTIME_OMIT`.
    replaces libc::utimensat;
    fn libtimewarp_utimensat(
        dirfd: libc::c_int,
        path: *const c_char,
        times: *const libc::timespec,
        flags: libc::c_int,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { stamp::utimensat(dirfd, path, times, flags) }
    }

    /// The same by descriptor.
    replaces libc::futimens;
    fn libtimewarp_futimens(
        fd: libc::c_int,
        times: *const libc::timespec,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { stamp::futimens(fd, times) }
    }

    // The setters. Each runs a one-operation transaction on the Realtime family where this process
    // may set the clock, and refuses the way the kernel would otherwise; `setter` has the rules.

    /// The wall clock set from a `timeval`.
    replaces libc::settimeofday;
    fn libtimewarp_settimeofday(
        tv: *const libc::timeval,
        tz: *const c_void,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { setter::settimeofday(tv, tz) }
    }

    /// A clock set from a `timespec`, which succeeds for the wall clock alone.
    replaces libc::clock_settime;
    fn libtimewarp_clock_settime(
        clock: libc::clockid_t,
        tp: *const libc::timespec,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { setter::clock_settime(clock, tp) }
    }

    /// The wall clock moved by a delta, applied at once rather than slewed.
    replaces libc::adjtime;
    fn libtimewarp_adjtime(
        delta: *const libc::timeval,
        olddelta: *mut libc::timeval,
    ) -> libc::c_int {
        // Safety: the caller is the program, calling under libc's contract.
        unsafe { setter::adjtime(delta, olddelta) }
    }
}
