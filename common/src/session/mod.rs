//! Creating a session file, attaching to one, and the checks that keep both honest.

pub mod layout;
pub mod naming;

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::Ordering;

use crate::config::{
    ENV_EMULATE_VDSO, ENV_FAIL_CLOSED, ENV_SHIFT, ENV_TIMEKEEP_INTERVAL, EnvLookup, ParseError,
    parse_duration, parse_flag,
};
use crate::platform::{self, clock};
use crate::time::{Line, Nanos, Rate};

use layout::{BadHeader, Header, Keeper};

/// What the environment asked for when no session is inherited.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The directory the session file is created in.
    pub dir: PathBuf,
    /// How far the Realtime family is moved from the genuine clock.
    pub realtime_shift: Nanos,
    /// Whether a target that reads the clock through the vDSO is caught.
    pub emulate_vdso: bool,
    /// Whether a target whose setup failed refuses to run.
    pub fail_closed: bool,
    /// How often the FreeBSD timekeep page is republished.
    pub timekeep_interval: Nanos,
}

/// A variable whose value the library could not use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadValue {
    /// The variable's name.
    pub variable: &'static str,
    /// What its parser said.
    pub error: ParseError,
}

impl std::fmt::Display for BadValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.variable, self.error)
    }
}

const DEFAULT_TIMEKEEP_INTERVAL: Nanos = Nanos(1_000_000);

impl Setup {
    /// Reads every variable that describes a new session.
    ///
    /// # Errors
    /// The first [`BadValue`] found, so the caller can name the variable the user got wrong.
    pub fn from_env(env: &impl EnvLookup, dir: PathBuf) -> Result<Self, BadValue> {
        let duration = |name: &'static str, default: Nanos| -> Result<Nanos, BadValue> {
            env.nonempty(name).map_or(Ok(default), |value| {
                parse_duration(&value.to_string_lossy())
                    .map(Nanos)
                    .map_err(|error| BadValue {
                        variable: name,
                        error,
                    })
            })
        };
        let flag = |name: &str, default: bool| {
            env.nonempty(name)
                .map_or(default, |v| parse_flag(&v.to_string_lossy()))
        };

        Ok(Self {
            dir,
            realtime_shift: duration(ENV_SHIFT, Nanos::ZERO)?,
            emulate_vdso: flag(ENV_EMULATE_VDSO, true),
            fail_closed: flag(ENV_FAIL_CLOSED, false),
            timekeep_interval: duration(ENV_TIMEKEEP_INTERVAL, DEFAULT_TIMEKEEP_INTERVAL)?,
        })
    }
}

/// Why a session could not be created or attached to.
#[derive(Debug)]
pub enum SessionError {
    /// No session file at that path.
    NoSession,
    /// The file exists and fails the header checks. The fix is deleting it; we leave it in place,
    /// because a newer layout may belong to a newer library that is running on it right now.
    BadSessionFile(BadHeader),
    /// The file or its directory is owned by another uid, or is the wrong kind of file.
    NotOurs(&'static str),
    /// A generated name landed on an existing file. Nothing is written; we never take over a
    /// session that may be live.
    Collision(PathBuf),
    /// The operating system refused a step of the work.
    Io(io::Error),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSession => f.write_str("no session file at that path"),
            Self::BadSessionFile(why) => write!(f, "{why}"),
            Self::NotOurs(what) => write!(f, "{what}"),
            Self::Collision(path) => {
                write!(f, "a session file already exists at {}", path.display())
            }
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<io::Error> for SessionError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// A mapped session file. Dropping it unmaps; [`Session::leak`] is how a target keeps it for the
/// life of the process.
#[derive(Debug)]
pub struct Session {
    base: NonNull<u8>,
    len: usize,
    path: PathBuf,
}

// Safety: the mapping is shared memory, and every field the library touches through it is an
// atomic, so a reference to it can cross threads.
unsafe impl Send for Session {}
// Safety: as for `Send`.
unsafe impl Sync for Session {}

/// The header at the base of a mapping.
#[expect(
    clippy::cast_ptr_alignment,
    reason = "`mmap` returns page-aligned memory, which is stricter than the header needs"
)]
const fn header_ptr(base: NonNull<u8>) -> *mut Header {
    base.as_ptr().cast::<Header>()
}

impl Session {
    /// The mapped header.
    #[must_use]
    pub fn header(&self) -> &Header {
        // Safety: `attach` and `create` both validate the mapping is at least a header long.
        unsafe { &*header_ptr(self.base) }
    }

    /// Where the session file lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Hands the mapping to the process for good.
    #[must_use]
    pub fn leak(self) -> &'static Header {
        let session = std::mem::ManuallyDrop::new(self);
        // Safety: the mapping outlives the process, because `ManuallyDrop` keeps `Drop` from
        // unmapping it.
        unsafe { &*header_ptr(session.base) }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Safety: the mapping came from `mmap` with this length and is not aliased.
        unsafe { libc::munmap(self.base.as_ptr().cast(), self.len) };
    }
}

fn cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL"))
}

fn page_size() -> usize {
    // Safety: `sysconf` reads a constant of the running system.
    let value = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(value)
        .ok()
        .filter(|&v| v > 0)
        .unwrap_or(4096)
}

fn euid() -> u32 {
    // Safety: `geteuid` reads process state and cannot fail.
    unsafe { libc::geteuid() }
}

/// Refuses a session directory that is not ours: an adversary owning it could unlink our files
/// from under a running tree, or plant files of their own inside it.
///
/// Creation and attach both run this, so neither can be the weaker door.
///
/// # Errors
/// [`SessionError::NotOurs`] when the directory fails a check, or [`SessionError::Io`] when it
/// cannot be stat'd at all.
pub fn check_dir(dir: &Path) -> Result<(), SessionError> {
    let c_dir = cstring(dir)?;
    // Safety: `stat` is a plain-old-data struct, so an all-zero value is a valid one.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // Safety: `c_dir` is a live path and `st` a live `stat`.
    if unsafe { libc::lstat(c_dir.as_ptr(), &raw mut st) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(SessionError::NotOurs(
            "the session directory is not a directory",
        ));
    }
    if st.st_uid != euid() {
        return Err(SessionError::NotOurs(
            "the session directory belongs to another user",
        ));
    }
    if st.st_mode & 0o777 != 0o700 {
        return Err(SessionError::NotOurs(
            "the session directory is not private to us",
        ));
    }
    Ok(())
}

/// Creates the session directory if it is missing, then applies [`check_dir`].
///
/// # Errors
/// Whatever [`check_dir`] rejects, or [`SessionError::Io`] when the directory cannot be made.
pub fn ensure_dir(dir: &Path) -> Result<(), SessionError> {
    let c_dir = cstring(dir)?;
    // Safety: `c_dir` is a live NUL-terminated path.
    if unsafe { libc::mkdir(c_dir.as_ptr(), 0o700) } != 0 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::AlreadyExists {
            return Err(err.into());
        }
    } else {
        // mkdir's mode passes through the umask, so say it again now that the directory is ours.
        // Safety: `c_dir` is a live NUL-terminated path.
        if unsafe { libc::chmod(c_dir.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    check_dir(dir)
}

fn map(fd: libc::c_int, len: usize) -> Result<NonNull<u8>, SessionError> {
    // Safety: `fd` is open and at least `len` bytes long.
    let base = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if base == libc::MAP_FAILED {
        return Err(io::Error::last_os_error().into());
    }
    NonNull::new(base.cast::<u8>())
        .ok_or_else(|| SessionError::Io(io::Error::other("mmap returned a null mapping")))
}

/// Creates a session under a generated name.
///
/// The file is built under a private temporary name and linked into place, so the directory holds
/// whole session files only and a creator that dies mid-build leaves a `.tmp` behind instead of a
/// half-written session.
///
/// # Errors
/// Whatever [`create_named`] rejects, or [`SessionError::Io`] when the name's randomness cannot be
/// read.
pub fn create(setup: &Setup) -> Result<Session, SessionError> {
    let mut random = [0u8; 16];
    platform::random_bytes(&mut random)?;
    create_named(setup, &naming::session_name(std::process::id(), random))
}

/// [`create`] with the name supplied. The name is generated in every path a target takes; this is
/// the seam the tests use to force a collision.
///
/// # Errors
/// [`SessionError::Collision`] when the name is taken, or [`SessionError::Io`] when a step of the
/// build fails.
pub fn create_named(setup: &Setup, name: &str) -> Result<Session, SessionError> {
    ensure_dir(&setup.dir)?;

    let pid = std::process::id();
    let (path, tmp) = naming::session_paths(&setup.dir, name);

    let page = page_size();
    let len = layout::file_size(page, 0);

    let c_tmp = cstring(&tmp)?;
    // Safety: `c_tmp` is a live NUL-terminated path.
    let fd = unsafe {
        libc::open(
            c_tmp.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let guard = FdGuard(fd);

    // `len` is a handful of pages, so the conversion cannot fail.
    let file_len = libc::off_t::try_from(len).unwrap_or(libc::off_t::MAX);
    // Safety: `fd` is open for writing.
    if unsafe { libc::ftruncate(fd, file_len) } != 0 {
        let err = io::Error::last_os_error();
        // Safety: `c_tmp` is a live path we just created.
        unsafe { libc::unlink(c_tmp.as_ptr()) };
        return Err(err.into());
    }

    let base = match map(fd, len) {
        Ok(base) => base,
        Err(err) => {
            // Safety: `c_tmp` is a live path we just created.
            unsafe { libc::unlink(c_tmp.as_ptr()) };
            return Err(err);
        }
    };
    drop(guard);

    // Safety: nothing else can reach this file: it is under a private name and not yet linked.
    unsafe { initialize(header_ptr(base), setup, pid, page, len) };

    let c_path = cstring(&path)?;
    // Safety: `c_tmp` and `c_path` are live NUL-terminated paths.
    if unsafe { libc::link(c_tmp.as_ptr(), c_path.as_ptr()) } != 0 {
        let err = io::Error::last_os_error();
        // Safety: the mapping is ours and unaliased.
        unsafe { libc::munmap(base.as_ptr().cast(), len) };
        // Safety: `c_tmp` names the temporary file we created.
        unsafe { libc::unlink(c_tmp.as_ptr()) };
        return Err(match err.kind() {
            io::ErrorKind::AlreadyExists => SessionError::Collision(path),
            _ => SessionError::Io(err),
        });
    }
    // Safety: `c_tmp` names the link we just duplicated.
    unsafe { libc::unlink(c_tmp.as_ptr()) };

    Ok(Session { base, len, path })
}

struct FdGuard(libc::c_int);

impl Drop for FdGuard {
    fn drop(&mut self) {
        // Safety: we own this descriptor. The mapping outlives it.
        unsafe { libc::close(self.0) };
    }
}

/// Fills a fresh, zeroed mapping and sets `ready` last. Every field the layout names is written,
/// including the ones no reader looks at yet, so a later release need not bump the ABI version.
///
/// # Safety
/// `header` must point at a zeroed mapping of at least `size_of::<Header>()` bytes that no other
/// process can reach.
unsafe fn initialize(header: *mut Header, setup: &Setup, pid: u32, page: usize, len: usize) {
    {
        let realtime = platform::genuine_nanos(clock::REALTIME).unwrap_or(Nanos::ZERO);
        let monotonic = platform::genuine_nanos(clock::MONOTONIC).unwrap_or(Nanos::ZERO);
        let boottime = platform::genuine_nanos(clock::BOOTTIME).unwrap_or(monotonic);
        let tai = platform::genuine_nanos(clock::TAI).unwrap_or(realtime);

        // Safety: the caller guarantees a zeroed, header-sized mapping nobody else can reach.
        let header = unsafe { &mut *header };
        header.magic = layout::MAGIC;
        header.abi_version = layout::ABI_VERSION;
        header.platform = platform::PLATFORM_TAG;
        header.keeper = Keeper::Affine as u32;
        header.emulate_vdso = u32::from(setup.emulate_vdso);
        header.fail_closed = u32::from(setup.fail_closed);
        header.file_size = u32::try_from(len).unwrap_or(u32::MAX);
        header.page_size = u32::try_from(page).unwrap_or(u32::MAX);
        header.arena_offset = 0;
        header.arena_size = 0;
        header.creator_pid = pid;
        header.creation_time = realtime.0;
        // A negative interval is not a thing; it lands as zero rather than as an enormous one.
        header.timekeep_interval = u64::try_from(setup.timekeep_interval.0).unwrap_or(0);

        let params = &header.live;
        params.realtime.store(
            Line::anchored(realtime, setup.realtime_shift),
            clock::REALTIME,
        );
        params
            .monotonic
            .store(Line::anchored(monotonic, Nanos::ZERO), clock::MONOTONIC);
        params
            .boottime
            .store(Line::anchored(boottime, Nanos::ZERO), clock::BOOTTIME);
        params.cpu_rate.store(Rate::UNITY.0, Ordering::Relaxed);
        // Today's offset whatever era Realtime is set to, and exact because it is whole seconds.
        params.tai_offset.store(
            (tai.0 - realtime.0 + Nanos::SECOND / 2).div_euclid(Nanos::SECOND),
            Ordering::Relaxed,
        );
        params.epoch.store(0, Ordering::Relaxed);
        copy_params(&header.live, &header.shadow);

        header.ready.store(1, Ordering::Release);
    }
}

fn copy_params(from: &layout::Params, to: &layout::Params) {
    for (src, dst) in [
        (&from.realtime, &to.realtime),
        (&from.monotonic, &to.monotonic),
        (&from.boottime, &to.boottime),
    ] {
        dst.store(src.load(), src.timebase());
    }
    to.cpu_rate
        .store(from.cpu_rate.load(Ordering::Relaxed), Ordering::Relaxed);
    to.tai_offset
        .store(from.tai_offset.load(Ordering::Relaxed), Ordering::Relaxed);
    to.epoch
        .store(from.epoch.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// Maps an existing session file, applying the security checks.
///
/// # Errors
/// [`SessionError::NoSession`] when the path holds nothing, [`SessionError::NotOurs`] or
/// [`SessionError::BadSessionFile`] when what it holds is not a session of ours, and
/// [`SessionError::Io`] when a step fails outright.
pub fn attach(path: &Path) -> Result<Session, SessionError> {
    if let Some(dir) = path.parent() {
        check_dir(dir)?;
    }

    let c_path = cstring(path)?;
    // A planted symlink is refused, and a planted FIFO returns instead of blocking us inside
    // somebody's constructor.
    // Safety: `c_path` is a live NUL-terminated path.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let err = io::Error::last_os_error();
        return Err(match err.kind() {
            io::ErrorKind::NotFound => SessionError::NoSession,
            _ => SessionError::Io(err),
        });
    }
    let guard = FdGuard(fd);

    // Safety: `stat` is a plain-old-data struct, so an all-zero value is a valid one.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // Safety: `fd` is open and `st` is a live `stat`.
    if unsafe { libc::fstat(fd, &raw mut st) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(SessionError::NotOurs(
            "the session file is not a regular file",
        ));
    }
    if st.st_uid != euid() {
        return Err(SessionError::NotOurs(
            "the session file belongs to another user",
        ));
    }

    // A size that does not fit a `usize` cannot be a mapping either; it fails the length check.
    let len = usize::try_from(st.st_size).unwrap_or(0);
    if len < size_of::<Header>() {
        return Err(SessionError::BadSessionFile(BadHeader::Truncated));
    }

    let base = map(fd, len)?;
    drop(guard);

    let session = Session {
        base,
        len,
        path: path.to_path_buf(),
    };
    match session.header().validate(len, platform::PLATFORM_TAG) {
        Ok(()) => Ok(session),
        Err(why) => Err(SessionError::BadSessionFile(why)),
    }
}

/// What the Realtime family shows now.
///
/// The read path: the parameter words and one genuine clock sample. No lock, no allocation,
/// nothing written to shared memory. The seqlock read flow goes around this later; today the
/// parameters are written once, before the file is linked into place, and never change, and
/// attach's acquire on `ready` is what orders them ahead of every read.
#[inline]
pub fn realtime_now(header: &Header) -> Option<Nanos> {
    let line = header.live.realtime.load();
    let timebase = header.live.realtime.timebase();
    Some(line.project(platform::genuine_nanos(timebase)?))
}

// Creating a session reads a genuine clock and the system's randomness, so these cases need a
// filled-in platform arm.
#[cfg(all(test, target_os = "linux"))]
mod tests;
