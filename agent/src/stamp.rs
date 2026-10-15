//! File timestamps: the bodies behind the names every platform arm exports.
//!
//! A timestamp has to agree with the clock, so a file written just now shows a modification time
//! of about now whatever the wall clock has been set to. A read carries the genuine timestamp
//! forward from a paired sample of the two nows, `fake_now + rate * (ts - real_now)`, and a set
//! measures the instant the program named back to a genuine one, `real_now + (ts - fake_now) /
//! rate`. The rule is the Realtime family's alone: a file timestamp is a wall-clock instant, and
//! no other family has one to offer.
//!
//! Both directions are the instant shapes the hook sees: genuine to fake for a read, fake to
//! genuine for a set. The keeper answers what the hook declines, and a session on the null keeper
//! leaves every timestamp as the filesystem wrote it.
//!
//! A read converts the buffer the genuine call filled. A set converts a copy of what the program
//! asked for, never the program's own buffer, and leaves `UTIME_NOW` and `UTIME_OMIT` alone: they
//! name no instant, so there is nothing in them to convert. The genuine instant keeps every
//! nanosecond the conversion gives it, so a converted pair is set through `utimensat` or
//! `futimens` whichever call named it, and reads back as what the program set. A pair the
//! platform refuses is not converted at all: it goes to the genuine call as the program wrote it,
//! which refuses it at the point and with the `errno` it would have without us.
//!
//! `statx`, the `64` names and the `__xstat` family are Linux's alone, and the birth time inside a
//! `stat` buffer is the BSDs', so each carries a `cfg` of its own. So does `utime`: FreeBSD's libc
//! builds it out of one call to `utimes`, which we already take over, and the arm there leaves the
//! set to that call rather than converting it twice. libSystem builds it out of `utimes` too, but
//! the program's call to `utime` reaches nothing of ours through that inner call on arm64, so the
//! macOS arm catches `utime` itself. libSystem's `lutimes` sets anything but a link through
//! `utimes` as well, so on macOS a set the library makes the way libSystem would is built here
//! from the same inner calls.

#[cfg(target_os = "linux")]
use std::ffi::c_uint;
use std::ffi::{c_char, c_int};

use common::hook::{Func, Kind};
use common::platform::clock;
use common::session::{timestamp_shown, timestamp_stored};
use common::time::Nanos;

use crate::wait::unsupported;
use crate::{genuine, state};

/// The `struct stat` version `__xstat` is asked for, as this architecture's glibc headers name it.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const STAT_VER: c_int = 1;
#[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
const STAT_VER: c_int = 3;

/// What the session shows for a genuine file timestamp, or `None` when it stands as the filesystem
/// recorded it: this process runs on genuine time, or the session keeps none.
///
/// The hook goes first, in the genuine-to-fake instant shape, and the keeper answers what it
/// declines.
#[inline]
fn shown(func: Func, stamp: Nanos) -> Option<Nanos> {
    let keeper = timestamp_shown(state::session()?, stamp);
    state::hook().map_or(keeper, |hook| {
        hook.convert(
            func,
            Kind::InstantToFake,
            clock::REALTIME,
            stamp,
            keeper.unwrap_or(stamp),
        )
        .or(keeper)
    })
}

/// The genuine instant a timestamp the program asked for is stored as, or `None` when it is stored
/// as the program wrote it, under the rules [`shown`] applies in the other direction.
#[inline]
fn stored(func: Func, stamp: Nanos) -> Option<Nanos> {
    let keeper = timestamp_stored(state::session()?, stamp);
    state::hook().map_or(keeper, |hook| {
        hook.convert(
            func,
            Kind::InstantToGenuine,
            clock::REALTIME,
            stamp,
            keeper.unwrap_or(stamp),
        )
        .or(keeper)
    })
}

/// How many timestamps one `stat` buffer carries: the three POSIX names everywhere, and the
/// birth time besides on a platform whose buffer has room for it.
#[cfg(any(target_os = "freebsd", target_os = "macos"))]
const STAMPS: usize = 4;
#[cfg(not(any(target_os = "freebsd", target_os = "macos")))]
const STAMPS: usize = 3;

/// A filled `stat` buffer, whichever of the shapes a libc has: the timestamps it carries, to
/// rewrite where they lie.
trait Stamped {
    /// Every timestamp in the buffer, as the `(seconds, nanoseconds)` pair each is spelled in.
    fn stamps(&mut self) -> [(&mut libc::time_t, &mut i64); STAMPS];
}

impl Stamped for libc::stat {
    fn stamps(&mut self) -> [(&mut libc::time_t, &mut i64); STAMPS] {
        [
            (&mut self.st_atime, &mut self.st_atime_nsec),
            (&mut self.st_mtime, &mut self.st_mtime_nsec),
            (&mut self.st_ctime, &mut self.st_ctime_nsec),
            #[cfg(any(target_os = "freebsd", target_os = "macos"))]
            (&mut self.st_birthtime, &mut self.st_birthtime_nsec),
        ]
    }
}

// musl spells `stat64` as another name for `stat`, so there the impl above is already this one.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
impl Stamped for libc::stat64 {
    fn stamps(&mut self) -> [(&mut libc::time_t, &mut i64); STAMPS] {
        [
            (&mut self.st_atime, &mut self.st_atime_nsec),
            (&mut self.st_mtime, &mut self.st_mtime_nsec),
            (&mut self.st_ctime, &mut self.st_ctime_nsec),
        ]
    }
}

/// Rewrites every timestamp of a buffer the genuine call has just filled.
#[inline]
fn show_stamps<T: Stamped>(func: Func, buf: &mut T) {
    for (seconds, nanos) in buf.stamps() {
        let stamp = Nanos(seconds.saturating_mul(Nanos::SECOND).saturating_add(*nanos));
        if let Some(faked) = shown(func, stamp) {
            let split = faked.to_timespec();
            *seconds = split.tv_sec;
            *nanos = split.tv_nsec;
        }
    }
}

/// The answer of a `stat` call, with its buffer converted where the genuine call filled one.
///
/// # Safety
/// `buf` must be null or valid for writes of the buffer this call fills.
#[inline]
unsafe fn stat_result<T: Stamped>(func: Func, result: Option<c_int>, buf: *mut T) -> c_int {
    let Some(result) = result else {
        return unsupported();
    };
    if result == 0 && !buf.is_null() {
        // Safety: `buf` is non-null, the caller guarantees it, and the genuine call has filled it.
        show_stamps(func, unsafe { &mut *buf });
    }
    result
}

/// Declares the genuine side of one `stat` name: the function itself, and the `__xstat` form a
/// libc that exports no such name routes it through.
///
/// glibc exported these under their own names for the first time at 2.33, and kept the `__xstat`
/// forms for the programs built against an older one. So the pair covers both eras from one build,
/// whichever of the two names a program itself calls. FreeBSD's and macOS's libcs have only ever
/// had the plain name, so there the fallback is left out and a missing name is a missing name.
macro_rules! genuine_stat {
    ($(
        $(#[$doc:meta])*
        fn $name:ident = $plain:ident else $versioned:ident($($arg:ident: $ty:ty),* $(,)?);
    )*) => {
        $(
            $(#[$doc])*
            ///
            /// # Safety
            /// The arguments must satisfy the contract libc gives the function of this name.
            #[inline]
            unsafe fn $name($($arg: $ty),*) -> Option<c_int> {
                // Safety: the caller guarantees the arguments, which pass on unchanged.
                if let Some(result) = unsafe { genuine::$plain($($arg),*) } {
                    return Some(result);
                }
                #[cfg(target_os = "linux")]
                // Safety: as above, with the version this architecture's headers name.
                let older = unsafe { genuine::$versioned(STAT_VER, $($arg),*) };
                #[cfg(not(target_os = "linux"))]
                let older = None;
                older
            }
        )*
    };
}

genuine_stat! {
    /// The genuine `stat`.
    fn genuine_stat = stat else __xstat(path: *const c_char, buf: *mut libc::stat);

    /// The genuine `stat64`.
    #[cfg(target_os = "linux")]
    fn genuine_stat64 = stat64 else __xstat64(path: *const c_char, buf: *mut libc::stat64);

    /// The genuine `fstat`.
    fn genuine_fstat = fstat else __fxstat(fd: c_int, buf: *mut libc::stat);

    /// The genuine `fstat64`.
    #[cfg(target_os = "linux")]
    fn genuine_fstat64 = fstat64 else __fxstat64(fd: c_int, buf: *mut libc::stat64);

    /// The genuine `lstat`.
    fn genuine_lstat = lstat else __lxstat(path: *const c_char, buf: *mut libc::stat);

    /// The genuine `lstat64`.
    #[cfg(target_os = "linux")]
    fn genuine_lstat64 = lstat64 else __lxstat64(path: *const c_char, buf: *mut libc::stat64);

    /// The genuine `fstatat`.
    fn genuine_fstatat = fstatat else __fxstatat(
        dirfd: c_int,
        path: *const c_char,
        buf: *mut libc::stat,
        flags: c_int,
    );

    /// The genuine `fstatat64`.
    #[cfg(target_os = "linux")]
    fn genuine_fstatat64 = fstatat64 else __fxstatat64(
        dirfd: c_int,
        path: *const c_char,
        buf: *mut libc::stat64,
        flags: c_int,
    );
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `buf` null or valid for writes of a `stat`.
#[inline]
pub(crate) unsafe fn stat(path: *const c_char, buf: *mut libc::stat) -> c_int {
    // Safety: both arguments are the caller's, passed on unchanged.
    let result = unsafe { genuine_stat(path, buf) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Stat, result, buf) }
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `buf` null or valid for writes of a `stat64`.
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn stat64(path: *const c_char, buf: *mut libc::stat64) -> c_int {
    // Safety: both arguments are the caller's, passed on unchanged.
    let result = unsafe { genuine_stat64(path, buf) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Stat, result, buf) }
}

/// # Safety
/// `buf` must be null or valid for writes of a `stat`.
#[inline]
pub(crate) unsafe fn fstat(fd: c_int, buf: *mut libc::stat) -> c_int {
    // Safety: the buffer is the caller's, passed on unchanged.
    let result = unsafe { genuine_fstat(fd, buf) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Fstat, result, buf) }
}

/// # Safety
/// `buf` must be null or valid for writes of a `stat64`.
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn fstat64(fd: c_int, buf: *mut libc::stat64) -> c_int {
    // Safety: the buffer is the caller's, passed on unchanged.
    let result = unsafe { genuine_fstat64(fd, buf) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Fstat, result, buf) }
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `buf` null or valid for writes of a `stat`.
#[inline]
pub(crate) unsafe fn lstat(path: *const c_char, buf: *mut libc::stat) -> c_int {
    // Safety: both arguments are the caller's, passed on unchanged.
    let result = unsafe { genuine_lstat(path, buf) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Lstat, result, buf) }
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `buf` null or valid for writes of a `stat64`.
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn lstat64(path: *const c_char, buf: *mut libc::stat64) -> c_int {
    // Safety: both arguments are the caller's, passed on unchanged.
    let result = unsafe { genuine_lstat64(path, buf) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Lstat, result, buf) }
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `buf` null or valid for writes of a `stat`.
#[inline]
pub(crate) unsafe fn fstatat(
    dirfd: c_int,
    path: *const c_char,
    buf: *mut libc::stat,
    flags: c_int,
) -> c_int {
    // Safety: every argument is the caller's, passed on unchanged.
    let result = unsafe { genuine_fstatat(dirfd, path, buf, flags) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Fstatat, result, buf) }
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `buf` null or valid for writes of a `stat64`.
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn fstatat64(
    dirfd: c_int,
    path: *const c_char,
    buf: *mut libc::stat64,
    flags: c_int,
) -> c_int {
    // Safety: every argument is the caller's, passed on unchanged.
    let result = unsafe { genuine_fstatat64(dirfd, path, buf, flags) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Fstatat, result, buf) }
}

// The `__xstat` family, which glibc before 2.33 routes `stat` and its neighbours through. A
// program built against such a glibc calls these and never the plain names, so a build targeting
// one exports them too; the version the program passes is its own and travels untouched.

/// # Safety
/// `path` must be a valid NUL-terminated path, and `buf` null or valid for writes of a `stat`.
#[cfg(glibc_pre_2_33)]
#[inline]
pub(crate) unsafe fn xstat(version: c_int, path: *const c_char, buf: *mut libc::stat) -> c_int {
    // Safety: every argument is the caller's, passed on unchanged.
    let result = unsafe { genuine::__xstat(version, path, buf) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Stat, result, buf) }
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `buf` null or valid for writes of a `stat64`.
#[cfg(glibc_pre_2_33)]
#[inline]
pub(crate) unsafe fn xstat64(version: c_int, path: *const c_char, buf: *mut libc::stat64) -> c_int {
    // Safety: every argument is the caller's, passed on unchanged.
    let result = unsafe { genuine::__xstat64(version, path, buf) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Stat, result, buf) }
}

/// # Safety
/// `buf` must be null or valid for writes of a `stat`.
#[cfg(glibc_pre_2_33)]
#[inline]
pub(crate) unsafe fn fxstat(version: c_int, fd: c_int, buf: *mut libc::stat) -> c_int {
    // Safety: every argument is the caller's, passed on unchanged.
    let result = unsafe { genuine::__fxstat(version, fd, buf) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Fstat, result, buf) }
}

/// # Safety
/// `buf` must be null or valid for writes of a `stat64`.
#[cfg(glibc_pre_2_33)]
#[inline]
pub(crate) unsafe fn fxstat64(version: c_int, fd: c_int, buf: *mut libc::stat64) -> c_int {
    // Safety: every argument is the caller's, passed on unchanged.
    let result = unsafe { genuine::__fxstat64(version, fd, buf) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Fstat, result, buf) }
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `buf` null or valid for writes of a `stat`.
#[cfg(glibc_pre_2_33)]
#[inline]
pub(crate) unsafe fn lxstat(version: c_int, path: *const c_char, buf: *mut libc::stat) -> c_int {
    // Safety: every argument is the caller's, passed on unchanged.
    let result = unsafe { genuine::__lxstat(version, path, buf) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Lstat, result, buf) }
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `buf` null or valid for writes of a `stat64`.
#[cfg(glibc_pre_2_33)]
#[inline]
pub(crate) unsafe fn lxstat64(
    version: c_int,
    path: *const c_char,
    buf: *mut libc::stat64,
) -> c_int {
    // Safety: every argument is the caller's, passed on unchanged.
    let result = unsafe { genuine::__lxstat64(version, path, buf) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Lstat, result, buf) }
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `buf` null or valid for writes of a `stat`.
#[cfg(glibc_pre_2_33)]
#[inline]
pub(crate) unsafe fn fxstatat(
    version: c_int,
    dirfd: c_int,
    path: *const c_char,
    buf: *mut libc::stat,
    flags: c_int,
) -> c_int {
    // Safety: every argument is the caller's, passed on unchanged.
    let result = unsafe { genuine::__fxstatat(version, dirfd, path, buf, flags) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Fstatat, result, buf) }
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `buf` null or valid for writes of a `stat64`.
#[cfg(glibc_pre_2_33)]
#[inline]
pub(crate) unsafe fn fxstatat64(
    version: c_int,
    dirfd: c_int,
    path: *const c_char,
    buf: *mut libc::stat64,
    flags: c_int,
) -> c_int {
    // Safety: every argument is the caller's, passed on unchanged.
    let result = unsafe { genuine::__fxstatat64(version, dirfd, path, buf, flags) };
    // Safety: `buf` is the caller's, and the call above filled it.
    unsafe { stat_result(Func::Fstatat, result, buf) }
}

/// One timestamp of a `statx` buffer: whole seconds, and the nanoseconds past them.
#[cfg(target_os = "linux")]
#[derive(Debug)]
#[repr(C)]
struct StatxTimestamp {
    /// Whole seconds of the wall clock.
    tv_sec: i64,
    /// Nanoseconds past that second.
    tv_nsec: u32,
    /// Alignment only; the kernel puts nothing here.
    reserved: i32,
}

/// The buffer `statx` fills, in the kernel's own shape.
///
/// Declared here rather than taken from a libc binding, because only some of the Linux libcs we
/// build against declare `statx` at all, and the kernel's shape is the same under every one. Only
/// the mask and the four timestamps are named: everything else travels between the kernel and the
/// program untouched, so it is bytes here rather than fields.
#[cfg(target_os = "linux")]
#[derive(Debug)]
#[repr(C)]
pub(crate) struct Statx {
    /// Which fields the kernel filled in.
    mask: u32,
    /// Everything between the mask and the timestamps.
    leading: [u8; 60],
    /// Last access.
    atime: StatxTimestamp,
    /// Creation: the birth time, which is the one timestamp a `stat` buffer has no room for.
    btime: StatxTimestamp,
    /// Last status change.
    ctime: StatxTimestamp,
    /// Last modification.
    mtime: StatxTimestamp,
    /// Everything past them.
    trailing: [u8; 128],
}

/// The mask bit that says the kernel filled the access time in.
#[cfg(target_os = "linux")]
const STATX_ATIME: u32 = 0x0020;
/// The same for the modification time.
#[cfg(target_os = "linux")]
const STATX_MTIME: u32 = 0x0040;
/// The same for the status change time.
#[cfg(target_os = "linux")]
const STATX_CTIME: u32 = 0x0080;
/// The same for the birth time, the one a filesystem may not keep at all.
#[cfg(target_os = "linux")]
const STATX_BTIME: u32 = 0x0800;

#[cfg(target_os = "linux")]
const _: () = {
    assert!(size_of::<Statx>() == 256);
    assert!(align_of::<Statx>() == 8);
    assert!(std::mem::offset_of!(Statx, atime) == 64);
};

#[cfg(target_os = "linux")]
impl Statx {
    /// Every timestamp the buffer carries, each with the mask bit that says the kernel filled it.
    const fn stamps(&mut self) -> [(u32, &mut StatxTimestamp); 4] {
        let filled = self.mask;
        [
            (filled & STATX_ATIME, &mut self.atime),
            (filled & STATX_BTIME, &mut self.btime),
            (filled & STATX_CTIME, &mut self.ctime),
            (filled & STATX_MTIME, &mut self.mtime),
        ]
    }
}

/// Linux's own `stat`, which carries the birth time the older buffers have no room for and says in
/// its mask which timestamps it filled.
///
/// # Safety
/// `path` must be a valid NUL-terminated path, and `buf` null or valid for writes of a `statx`.
#[cfg(target_os = "linux")]
#[inline]
pub(crate) unsafe fn statx(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    mask: c_uint,
    buf: *mut Statx,
) -> c_int {
    // Safety: every argument is the caller's, passed on unchanged.
    let Some(result) = (unsafe { genuine::statx(dirfd, path, flags, mask, buf.cast()) }) else {
        return unsupported();
    };
    if result != 0 || buf.is_null() {
        return result;
    }
    // Safety: `buf` is non-null, the caller guarantees it, and the genuine call has filled it.
    for (filled, stamp) in unsafe { &mut *buf }.stamps() {
        if filled == 0 {
            continue;
        }
        let genuine = Nanos(
            stamp
                .tv_sec
                .saturating_mul(Nanos::SECOND)
                .saturating_add(i64::from(stamp.tv_nsec)),
        );
        if let Some(faked) = shown(Func::Statx, genuine) {
            let split = faked.to_timespec();
            stamp.tv_sec = split.tv_sec;
            stamp.tv_nsec = u32::try_from(split.tv_nsec).unwrap_or(0);
        }
    }
    result
}

/// The pair of instants a set names, as `utimensat` takes them: the access time, then the
/// modification time.
type Pair = [libc::timespec; 2];

/// The two values a set names, or `None` for the null pointer that asks for the current time.
///
/// # Safety
/// `times` must be null or valid for reads of two `T`s.
#[inline]
const unsafe fn read_two<T: Copy>(times: *const T) -> Option<[T; 2]> {
    if times.is_null() {
        return None;
    }
    // Safety: `times` is non-null, so the caller's guarantee makes this read valid.
    let first = unsafe { *times };
    // Safety: as above, for the second of the two the caller guarantees.
    let second = unsafe { times.add(1) };
    // Safety: `second` is the second of the pair, which the caller guarantees.
    let second = unsafe { *second };
    Some([first, second])
}

/// Whether the platform refuses a `timespec` a set names, which it does with `EINVAL` from the
/// call itself. Linux and FreeBSD refuse nanoseconds outside `0..1e9` other than `UTIME_NOW` and
/// `UTIME_OMIT`. libSystem hands every value to `setattrlistat`, which refuses none.
#[inline]
fn refused_timespec(ts: &libc::timespec) -> bool {
    cfg!(not(target_os = "macos"))
        && !(0..Nanos::SECOND).contains(&ts.tv_nsec)
        && ts.tv_nsec != libc::UTIME_NOW
        && ts.tv_nsec != libc::UTIME_OMIT
}

/// The same for a `timeval`: Linux and FreeBSD refuse microseconds outside `0..1e6`, and macOS's
/// `getutimes` refuses none.
#[inline]
fn refused_timeval(tv: &libc::timeval) -> bool {
    cfg!(not(target_os = "macos")) && !(0..1_000_000).contains(&tv.tv_usec)
}

/// The genuine pair a fake one becomes, each instant to the nanosecond, or `None` when nothing in
/// it is one this session converts.
///
/// `UTIME_NOW` and `UTIME_OMIT` sit in the nanoseconds field and name no instant, one asking for
/// the current time and the other for no change at all, so both pass through untouched.
#[inline]
fn stored_pair(func: Func, mut pair: Pair) -> Option<Pair> {
    let mut converted = false;
    for asked in &mut pair {
        if asked.tv_nsec == libc::UTIME_NOW || asked.tv_nsec == libc::UTIME_OMIT {
            continue;
        }
        if let Some(genuine) = stored(func, Nanos::from_timespec(asked)) {
            *asked = genuine.to_timespec();
            converted = true;
        }
    }
    converted.then_some(pair)
}

/// The genuine pair a fake `timespec` pair becomes, or `None` when the program's own pair goes to
/// the genuine call: no pair at all, one the platform refuses, which leaves the refusal and its
/// `errno` to that call, or nothing in it this session converts.
///
/// # Safety
/// `times` must be null or valid for reads of two `timespec`s.
#[inline]
unsafe fn stored_timespecs(func: Func, times: *const libc::timespec) -> Option<Pair> {
    // Safety: the caller guarantees `times`.
    let asked = unsafe { read_two(times) }?;
    if asked.iter().any(refused_timespec) {
        return None;
    }
    stored_pair(func, asked)
}

/// The same for a `timeval` pair, which has no `UTIME_NOW` to leave alone. What it becomes is a
/// `timespec` pair, which carries the nanoseconds a converted instant has.
///
/// # Safety
/// `times` must be null or valid for reads of two `timeval`s.
#[inline]
unsafe fn stored_timevals(func: Func, times: *const libc::timeval) -> Option<Pair> {
    // Safety: the caller guarantees `times`.
    let asked = unsafe { read_two(times) }?;
    if asked.iter().any(refused_timeval) {
        return None;
    }
    stored_pair(func, asked.map(|tv| Nanos::from_timeval(&tv).to_timespec()))
}

/// The same for the pair of whole seconds `utime` takes, which no platform refuses.
///
/// # Safety
/// `times` must be null or valid for reads of a `utimbuf`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[inline]
unsafe fn stored_utimbuf(times: *const libc::utimbuf) -> Option<Pair> {
    if times.is_null() {
        return None;
    }
    // Safety: `times` is non-null, so the caller's guarantee makes this read valid.
    let asked = unsafe { *times };
    let asked = [asked.actime, asked.modtime].map(|tv_sec| libc::timespec { tv_sec, tv_nsec: 0 });
    stored_pair(Func::Utime, asked)
}

/// Sets a converted pair on `path` to the nanosecond, following a link at its end unless `flags`
/// holds `AT_SYMLINK_NOFOLLOW`. This is the system call current glibc and musl build `utime`,
/// `utimes` and `lutimes` out of, so the set is theirs with the nanoseconds left in.
///
/// # Safety
/// `path` must be a valid NUL-terminated path.
#[cfg(target_os = "linux")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the other platforms set through a genuine function, which may be missing"
)]
#[inline]
unsafe fn set_path(path: *const c_char, pair: &Pair, flags: c_int) -> Option<c_int> {
    // Safety: the caller guarantees `path`.
    Some(unsafe { set_through_utimensat(libc::AT_FDCWD, path, pair, flags) })
}

/// The same on an open descriptor, through the system call glibc and musl build `futimes` out of:
/// `utimensat` with no path.
#[cfg(target_os = "linux")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the other platforms set through a genuine function, which may be missing"
)]
#[inline]
fn set_descriptor(fd: c_int, pair: &Pair) -> Option<c_int> {
    // Safety: a null path names the descriptor itself.
    Some(unsafe { set_through_utimensat(fd, std::ptr::null(), pair, 0) })
}

/// The `utimensat` system call, the one every Linux setter of the group ends in.
///
/// # Safety
/// `path` must be null or a valid NUL-terminated path.
#[cfg(target_os = "linux")]
#[inline]
unsafe fn set_through_utimensat(
    dirfd: c_int,
    path: *const c_char,
    pair: &Pair,
    flags: c_int,
) -> c_int {
    /// A pointer as a system call argument.
    fn address<T>(ptr: *const T) -> libc::c_long {
        libc::c_long::try_from(ptr.addr().cast_signed()).unwrap_or(0)
    }
    let args = [
        libc::c_long::from(dirfd),
        address(path),
        address(pair.as_ptr()),
        libc::c_long::from(flags),
        0,
        0,
    ];
    // Safety: the caller guarantees `path`, and `pair` is two live `timespec`s.
    let set = unsafe { common::platform::genuine_syscall(libc::SYS_utimensat, args) };
    c_int::try_from(set).unwrap_or(-1)
}

/// Sets a converted pair on `path` to the nanosecond through the genuine `utimensat`, following a
/// link at its end unless `flags` holds `AT_SYMLINK_NOFOLLOW`. The kernel sets it through the same
/// `setutimes` the `utimes` family ends in.
///
/// # Safety
/// `path` must be a valid NUL-terminated path.
#[cfg(target_os = "freebsd")]
#[inline]
unsafe fn set_path(path: *const c_char, pair: &Pair, flags: c_int) -> Option<c_int> {
    // Safety: the caller guarantees `path`, and `pair` is two live `timespec`s.
    unsafe { genuine::utimensat(libc::AT_FDCWD, path, pair.as_ptr(), flags) }
}

/// The same on an open descriptor, through the genuine `futimens`.
#[cfg(target_os = "freebsd")]
#[inline]
fn set_descriptor(fd: c_int, pair: &Pair) -> Option<c_int> {
    // Safety: `pair` is two live `timespec`s.
    unsafe { genuine::futimens(fd, pair.as_ptr()) }
}

/// Sets a converted pair on `path` to the nanosecond through the genuine `utimensat`, following a
/// link at its end unless `flags` holds `AT_SYMLINK_NOFOLLOW`.
///
/// libSystem's `utimensat` is `setattrlistat`, which refuses a caller that does not own the file
/// with `EACCES` where `utimes` answers `EPERM`. So when it fails, the set is made again the way
/// libSystem's `utime`, `utimes` and `lutimes` make it, on the pair rounded up to microseconds,
/// and that answer stands.
///
/// # Safety
/// `path` must be a valid NUL-terminated path.
#[cfg(target_os = "macos")]
#[inline]
unsafe fn set_path(path: *const c_char, pair: &Pair, flags: c_int) -> Option<c_int> {
    // Safety: the caller guarantees `path`, and `pair` is two live `timespec`s.
    if unsafe { genuine::utimensat(libc::AT_FDCWD, path, pair.as_ptr(), flags) }? == 0 {
        return Some(0);
    }
    let micros = in_micros(pair);
    if flags & libc::AT_SYMLINK_NOFOLLOW != 0 {
        // Safety: the caller guarantees `path`, and `micros` is two live `timeval`s.
        unsafe { set_without_following(path, micros.as_ptr()) }
    } else {
        // Safety: as above.
        unsafe { genuine::utimes(path, micros.as_ptr()) }
    }
}

/// The same on an open descriptor, through the genuine `futimens` and then the genuine `futimes`.
#[cfg(target_os = "macos")]
#[inline]
fn set_descriptor(fd: c_int, pair: &Pair) -> Option<c_int> {
    // Safety: `pair` is two live `timespec`s.
    if unsafe { genuine::futimens(fd, pair.as_ptr()) }? == 0 {
        return Some(0);
    }
    // Safety: the pair is two live `timeval`s.
    unsafe { genuine::futimes(fd, in_micros(pair).as_ptr()) }
}

/// A converted pair in the microseconds of a `timeval`, each rounded up, so it reads back no
/// earlier than the instant the program set.
#[cfg(target_os = "macos")]
#[inline]
fn in_micros(pair: &Pair) -> [libc::timeval; 2] {
    pair.map(|ts| {
        Nanos::from_timespec(&ts)
            .saturating_add(Nanos(999))
            .to_timeval()
    })
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `times` null or valid for reads of a `utimbuf`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[inline]
pub(crate) unsafe fn utime(path: *const c_char, times: *const libc::utimbuf) -> c_int {
    // Safety: `times` is the caller's, read under the contract it came with.
    let converted = unsafe { stored_utimbuf(times) };
    converted
        .map_or_else(
            // Safety: both are the caller's, passed on unchanged.
            || unsafe { set_utimbuf(path, times) },
            // Safety: `path` is the caller's.
            |pair| unsafe { set_path(path, &pair, 0) },
        )
        .unwrap_or_else(unsupported)
}

/// Sets the pair a `utimbuf` names through the genuine `utime`.
///
/// # Safety
/// `path` must be a valid NUL-terminated path, and `times` null or valid for reads of a `utimbuf`.
#[cfg(target_os = "linux")]
#[inline]
unsafe fn set_utimbuf(path: *const c_char, times: *const libc::utimbuf) -> Option<c_int> {
    // Safety: the caller guarantees both, which pass on unchanged.
    unsafe { genuine::utime(path, times) }
}

/// Sets the pair a `utimbuf` names through the genuine `utimes`, as libSystem's own `utime` does.
/// That inner call crosses into another library of the shared cache, which reaches our `utimes`
/// row on `x86_64`, so the pair is handed to the genuine `utimes` from here instead, past every
/// row.
///
/// # Safety
/// `path` must be a valid NUL-terminated path, and `times` null or valid for reads of a `utimbuf`.
#[cfg(target_os = "macos")]
#[inline]
unsafe fn set_utimbuf(path: *const c_char, times: *const libc::utimbuf) -> Option<c_int> {
    if times.is_null() {
        // Safety: the caller guarantees `path`, and a null pair asks for now.
        return unsafe { genuine::utimes(path, std::ptr::null()) };
    }
    // Safety: `times` is non-null, so the caller's guarantee makes this read valid.
    let asked = unsafe { *times };
    let pair = [asked.actime, asked.modtime].map(|seconds| libc::timeval {
        tv_sec: seconds,
        tv_usec: 0,
    });
    // Safety: the caller guarantees `path`, and `pair` is two live `timeval`s.
    unsafe { genuine::utimes(path, pair.as_ptr()) }
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `times` null or valid for reads of two
/// `timeval`s.
#[inline]
pub(crate) unsafe fn utimes(path: *const c_char, times: *const libc::timeval) -> c_int {
    // Safety: `times` is the caller's, read under the contract it came with.
    let converted = unsafe { stored_timevals(Func::Utimes, times) };
    converted
        .map_or_else(
            // Safety: both are the caller's, passed on unchanged.
            || unsafe { genuine::utimes(path, times) },
            // Safety: `path` is the caller's.
            |pair| unsafe { set_path(path, &pair, 0) },
        )
        .unwrap_or_else(unsupported)
}

/// # Safety
/// `times` must be null or valid for reads of two `timeval`s.
#[inline]
pub(crate) unsafe fn futimes(fd: c_int, times: *const libc::timeval) -> c_int {
    // Safety: `times` is the caller's, read under the contract it came with.
    let converted = unsafe { stored_timevals(Func::Futimes, times) };
    converted
        .map_or_else(
            // Safety: `times` is the caller's, passed on unchanged.
            || unsafe { genuine::futimes(fd, times) },
            |pair| set_descriptor(fd, &pair),
        )
        .unwrap_or_else(unsupported)
}

/// # Safety
/// `path` must be a valid NUL-terminated path, and `times` null or valid for reads of two
/// `timeval`s.
#[inline]
pub(crate) unsafe fn lutimes(path: *const c_char, times: *const libc::timeval) -> c_int {
    // Safety: `times` is the caller's, read under the contract it came with.
    let converted = unsafe { stored_timevals(Func::Lutimes, times) };
    converted
        .map_or_else(
            // Safety: both are the caller's, passed on unchanged.
            || unsafe { set_without_following(path, times) },
            // Safety: `path` is the caller's.
            |pair| unsafe { set_path(path, &pair, libc::AT_SYMLINK_NOFOLLOW) },
        )
        .unwrap_or_else(unsupported)
}

/// Sets a pair on `path` itself rather than on what a link there names, through the genuine
/// `lutimes`.
///
/// # Safety
/// `path` must be a valid NUL-terminated path, and `times` null or valid for reads of two
/// `timeval`s.
#[cfg(not(target_os = "macos"))]
#[inline]
unsafe fn set_without_following(path: *const c_char, times: *const libc::timeval) -> Option<c_int> {
    // Safety: the caller guarantees both, which pass on unchanged.
    unsafe { genuine::lutimes(path, times) }
}

/// Sets a pair on `path` itself the way libSystem's own `lutimes` does: through the genuine
/// `utimes` for anything but a link, and through `setattrlist` on a link, with the genuine now
/// where the program named no pair. libSystem's call to `utimes` crosses into another library of
/// the shared cache, which reaches our `utimes` row on `x86_64`, so the pair is set from here,
/// already converted once.
///
/// # Safety
/// `path` must be a valid NUL-terminated path, and `times` null or valid for reads of two
/// `timeval`s.
#[cfg(target_os = "macos")]
#[inline]
unsafe fn set_without_following(path: *const c_char, times: *const libc::timeval) -> Option<c_int> {
    // Safety: a `stat` is integers and `timespec`s, for which all zeroes is a value.
    let mut found: libc::stat = unsafe { std::mem::zeroed() };
    // Safety: the caller guarantees `path`, and `found` is a live buffer of the shape this fills.
    let looked = unsafe { genuine::lstat(path, &raw mut found) }?;
    if looked != 0 {
        return Some(looked);
    }
    if found.st_mode & libc::S_IFMT != libc::S_IFLNK {
        // Safety: the caller guarantees both, which pass on unchanged.
        return unsafe { genuine::utimes(path, times) };
    }
    let [access, modified] = if times.is_null() {
        let Some(now) = common::platform::genuine_nanos(clock::REALTIME) else {
            return Some(-1);
        };
        [now.to_timespec(); 2]
    } else {
        // Safety: `times` is non-null, so the caller's guarantee makes this read valid.
        let first = unsafe { *times };
        // Safety: as above, for the second of the two the caller guarantees.
        let second = unsafe { times.add(1) };
        // Safety: `second` is the second of the pair, which the caller guarantees.
        let second = unsafe { *second };
        [first, second].map(|tv| libc::timespec {
            tv_sec: tv.tv_sec,
            tv_nsec: i64::from(tv.tv_usec).saturating_mul(1_000),
        })
    };
    let mut asked = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_MODTIME | libc::ATTR_CMN_ACCTIME,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    // The values follow the order of their bits in the list, which puts the modification time
    // first.
    let mut values = [modified, access];
    // Safety: the caller guarantees `path`, and the list and the values are live and as long as
    // the call is told.
    Some(unsafe {
        libc::setattrlist(
            path,
            (&raw mut asked).cast(),
            values.as_mut_ptr().cast(),
            size_of_val(&values),
            libc::FSOPT_NOFOLLOW,
        )
    })
}

/// # Safety
/// `path` must be null or a valid NUL-terminated path, and `times` null or valid for reads of two
/// `timespec`s.
#[inline]
pub(crate) unsafe fn utimensat(
    dirfd: c_int,
    path: *const c_char,
    times: *const libc::timespec,
    flags: c_int,
) -> c_int {
    // Safety: `times` is the caller's, read under the contract it came with.
    let converted = unsafe { stored_timespecs(Func::Utimensat, times) };
    let times = converted.as_ref().map_or(times, |pair| pair.as_ptr());
    // Safety: `path` is the caller's, and `times` is either theirs or the copy just built.
    unsafe { genuine::utimensat(dirfd, path, times, flags) }.unwrap_or_else(unsupported)
}

/// # Safety
/// `times` must be null or valid for reads of two `timespec`s.
#[inline]
pub(crate) unsafe fn futimens(fd: c_int, times: *const libc::timespec) -> c_int {
    // Safety: `times` is the caller's, read under the contract it came with.
    let converted = unsafe { stored_timespecs(Func::Futimens, times) };
    let times = converted.as_ref().map_or(times, |pair| pair.as_ptr());
    // Safety: `times` is either the caller's or the copy just built.
    unsafe { genuine::futimens(fd, times) }.unwrap_or_else(unsupported)
}
