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
    self, BadValue, ENV_CPU_RATE, ENV_EMULATE_VDSO, ENV_FAIL_CLOSED, ENV_KEEPER, ENV_RATE,
    ENV_REALTIME_SHIFT, ENV_SHIFT, ENV_TIME, ENV_TIMEKEEP_INTERVAL, EnvLookup, Reason,
    duration_var, flag_var, rate_var, timestamp_var,
};
use crate::platform::{self, clock};
use crate::time::{Family, Line, Nanos, Rate};

use layout::{BadHeader, Fault, Header, Keeper};

/// What the environment asked one family to do. Cpu has no line, so it has none of this: its rate
/// is [`Setup::cpu_rate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilySetup {
    /// How far the family is moved from its default timebase's genuine reading.
    pub shift: Nanos,
    /// How fast the family runs: fake nanoseconds per nanosecond of the timebase.
    pub rate: Rate,
    /// The genuine clock the family tracks passage against.
    pub timebase: libc::clockid_t,
}

impl FamilySetup {
    /// The family left where its timebase is, running at real speed.
    #[must_use]
    pub const fn tracking(timebase: libc::clockid_t) -> Self {
        Self {
            shift: Nanos::ZERO,
            rate: Rate::UNITY,
            timebase,
        }
    }

    /// Reads one family's variables. `bare_shift` and `bare_rate` are the settings that name no
    /// family; the family's own variable wins over either.
    ///
    /// # Errors
    /// The [`BadValue`] naming the variable that could not be used.
    fn from_env(
        env: &impl EnvLookup,
        family: Family,
        bare_shift: Option<Nanos>,
        bare_rate: Option<Rate>,
    ) -> Result<Self, BadValue> {
        let vars = config::family_vars(family);
        let own_shift = match vars.shift {
            Some(name) => duration_var(env, name)?,
            None => None,
        };
        Ok(Self {
            shift: own_shift.or(bare_shift).unwrap_or(Nanos::ZERO),
            rate: rate_var(env, vars.rate)?
                .or(bare_rate)
                .unwrap_or(Rate::UNITY),
            timebase: timebase_from_env(env, family)?,
        })
    }
}

/// The timekeeper the environment names, or the affine one it defaults to.
fn keeper_from_env(env: &impl EnvLookup) -> Result<Keeper, BadValue> {
    env.nonempty(ENV_KEEPER)
        .map_or(Ok(Keeper::Affine), |value| {
            Keeper::from_name(&value.to_string_lossy())
                .ok_or_else(|| BadValue::new(ENV_KEEPER, Reason::UnknownKeeper))
        })
}

/// Whether a process that cannot set up what it was asked for exits rather than running genuine.
///
/// The choice belongs to the session: it is read at creation, recorded in the header, and followed
/// by every process that attaches, since an attacher reads none of the setup variables itself. A
/// failure landing before any session exists has no header to follow, so there the variable in the
/// environment decides on its own.
#[must_use]
pub fn fails_closed(session: Option<&Header>, env: &impl EnvLookup) -> bool {
    session.map_or_else(
        || flag_var(env, ENV_FAIL_CLOSED, false),
        |header| header.fail_closed == 1,
    )
}

/// The clock a family tracks passage against: the name the user gave, checked against what the
/// family may track, or the family's default timebase.
fn timebase_from_env(env: &impl EnvLookup, family: Family) -> Result<libc::clockid_t, BadValue> {
    let vars = config::family_vars(family);
    let named = vars
        .timebase
        .and_then(|name| env.nonempty(name).map(|value| (name, value)));
    let Some((name, value)) = named else {
        // Cpu has no timebase, so it is the one family with no default to fall back on.
        return platform::default_timebase(family)
            .ok_or_else(|| BadValue::new(vars.rate, Reason::ForbiddenTimebase));
    };

    let timebase = platform::clock_named(&value.to_string_lossy())
        .ok_or_else(|| BadValue::new(name, Reason::UnknownClock))?;
    if !platform::timebase_allowed(family, timebase) {
        return Err(BadValue::new(name, Reason::ForbiddenTimebase));
    }
    Ok(timebase)
}

/// What the environment asked for when no session is inherited.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The directory the session file is created in.
    pub dir: PathBuf,
    /// Which timekeeper computes what the families show.
    pub keeper: Keeper,
    /// An absolute value for the Realtime family, from `LIBTIMEWARP_TIME`, in place of a shift.
    pub realtime_at: Option<Nanos>,
    /// The wall clock.
    pub realtime: FamilySetup,
    /// The clocks that stop across suspend.
    pub monotonic: FamilySetup,
    /// The clocks that keep counting across suspend.
    pub boottime: FamilySetup,
    /// How fast process and thread CPU time runs. The Cpu family's one setting, fixed for the life
    /// of the session.
    pub cpu_rate: Rate,
    /// Whether a target that reads the clock through the vDSO is caught.
    pub emulate_vdso: bool,
    /// Whether a target whose setup failed refuses to run.
    pub fail_closed: bool,
    /// How often the FreeBSD timekeep page is republished.
    pub timekeep_interval: Nanos,
}

const DEFAULT_TIMEKEEP_INTERVAL: Nanos = Nanos(1_000_000);

impl Setup {
    /// A session that fakes nothing: every family where its genuine clock is, at real speed.
    #[must_use]
    pub const fn plain(dir: PathBuf) -> Self {
        Self {
            dir,
            keeper: Keeper::Affine,
            realtime_at: None,
            realtime: FamilySetup::tracking(clock::REALTIME),
            monotonic: FamilySetup::tracking(clock::MONOTONIC),
            boottime: FamilySetup::tracking(clock::BOOTTIME),
            cpu_rate: Rate::UNITY,
            emulate_vdso: true,
            fail_closed: false,
            timekeep_interval: DEFAULT_TIMEKEEP_INTERVAL,
        }
    }

    /// What a creator publishes when it cannot set up what the environment asked for: the null
    /// keeper with no hook, so the program runs genuine rather than half-faked.
    ///
    /// The two flags the session owns are still the environment's to say, and neither can be a bad
    /// value of its own. None of the clock settings are read, because the null keeper reads none.
    #[must_use]
    pub fn fallback(env: &impl EnvLookup, dir: PathBuf) -> Self {
        Self {
            keeper: Keeper::Null,
            emulate_vdso: flag_var(env, ENV_EMULATE_VDSO, true),
            fail_closed: flag_var(env, ENV_FAIL_CLOSED, false),
            ..Self::plain(dir)
        }
    }

    /// Reads every variable that describes a new session.
    ///
    /// A bare `LIBTIMEWARP_SHIFT` moves Realtime alone, because "two hours ago" leaves the uptime
    /// counter where it is. A bare `LIBTIMEWARP_RATE` covers the three families that measure
    /// elapsed time and leaves Cpu at unity, because that family measures consumption instead.
    ///
    /// # Errors
    /// The first [`BadValue`] found, so the caller can name the variable the user got wrong.
    pub fn from_env(env: &impl EnvLookup, dir: PathBuf) -> Result<Self, BadValue> {
        let session = Self {
            keeper: keeper_from_env(env)?,
            emulate_vdso: flag_var(env, ENV_EMULATE_VDSO, true),
            fail_closed: flag_var(env, ENV_FAIL_CLOSED, false),
            timekeep_interval: duration_var(env, ENV_TIMEKEEP_INTERVAL)?
                .unwrap_or(DEFAULT_TIMEKEEP_INTERVAL),
            ..Self::plain(dir)
        };
        // The clock settings belong to the affine keeper alone, so a null session reads none of
        // them, and a value none of them will be asked for cannot be a bad one.
        if session.keeper == Keeper::Null {
            return Ok(session);
        }

        let bare_shift = duration_var(env, ENV_SHIFT)?;
        let bare_rate = rate_var(env, ENV_RATE)?;
        let realtime_at = timestamp_var(env, ENV_TIME)?;

        let said_twice = [ENV_REALTIME_SHIFT, ENV_SHIFT]
            .into_iter()
            .find(|name| env.nonempty(name).is_some());
        if let (Some(_), Some(shift)) = (realtime_at, said_twice) {
            return Err(BadValue::new(ENV_TIME, Reason::SaidTwice(shift)));
        }

        Ok(Self {
            realtime: FamilySetup::from_env(env, Family::Realtime, bare_shift, bare_rate)?,
            monotonic: FamilySetup::from_env(env, Family::Monotonic, None, bare_rate)?,
            boottime: FamilySetup::from_env(env, Family::Boottime, None, bare_rate)?,
            cpu_rate: rate_var(env, ENV_CPU_RATE)?.unwrap_or(Rate::UNITY),
            realtime_at,
            ..session
        })
    }

    /// Turns what the environment asked for into the parameters the session starts with.
    ///
    /// `read` supplies the genuine readings, so the creator's own clocks and a test's chosen
    /// values go through the same arithmetic. A family's zero comes from its default timebase and
    /// its passage from its timebase; with no override the two are one clock, read once.
    ///
    /// # Errors
    /// A [`BadValue`] for a clock the kernel refuses, a Realtime value outside the range an
    /// instant can hold, or a shift that would put Monotonic or Boottime below zero.
    pub fn plan(&self, read: &dyn Fn(libc::clockid_t) -> Option<Nanos>) -> Result<Plan, BadValue> {
        let realtime_zero = sample(read, Family::Realtime, clock::REALTIME)?;
        let fake_anchor = match self.realtime_at {
            Some(at) => at,
            None => realtime_zero
                .0
                .checked_add(self.realtime.shift.0)
                .map(Nanos)
                .ok_or_else(|| BadValue::new(ENV_REALTIME_SHIFT, Reason::OutOfRange))?,
        };

        Ok(Plan {
            realtime: Line {
                fake_anchor,
                timebase_anchor: anchor(read, Family::Realtime, realtime_zero, &self.realtime)?,
                rate: self.realtime.rate,
            },
            monotonic: nondecreasing_line(read, Family::Monotonic, &self.monotonic)?,
            boottime: nondecreasing_line(read, Family::Boottime, &self.boottime)?,
            cpu_rate: self.cpu_rate,
            tai_offset: tai_offset(read, realtime_zero),
            creation_time: realtime_zero,
        })
    }
}

/// Whole seconds by which genuine `CLOCK_TAI` leads genuine `CLOCK_REALTIME`: today's offset
/// whatever era Realtime is set to, and exact because it is whole seconds. A kernel that never had
/// the offset set reports the wall clock, so zero.
fn tai_offset(read: &dyn Fn(libc::clockid_t) -> Option<Nanos>, realtime_zero: Nanos) -> i64 {
    read(clock::TAI).map_or(0, |tai| {
        tai.0
            .saturating_sub(realtime_zero.0)
            .saturating_add(Nanos::SECOND / 2)
            .div_euclid(Nanos::SECOND)
    })
}

/// The genuine reading of a family's clock, or the bad value naming the variable that chose it.
fn sample(
    read: &dyn Fn(libc::clockid_t) -> Option<Nanos>,
    family: Family,
    timebase: libc::clockid_t,
) -> Result<Nanos, BadValue> {
    let vars = config::family_vars(family);
    read(timebase)
        .ok_or_else(|| BadValue::new(vars.timebase.unwrap_or(vars.rate), Reason::UnreadableClock))
}

/// The timebase reading the family's zero is paired with. An override changes where passage comes
/// from and leaves the zero alone, so only an overridden family is read a second time.
fn anchor(
    read: &dyn Fn(libc::clockid_t) -> Option<Nanos>,
    family: Family,
    zero: Nanos,
    setup: &FamilySetup,
) -> Result<Nanos, BadValue> {
    match platform::default_timebase(family) {
        Some(default) if default == setup.timebase => Ok(zero),
        _ => sample(read, family, setup.timebase),
    }
}

/// A Monotonic or Boottime line. A creation shift that would put the family below zero is refused,
/// so a faked monotonic clock never reads negative.
fn nondecreasing_line(
    read: &dyn Fn(libc::clockid_t) -> Option<Nanos>,
    family: Family,
    setup: &FamilySetup,
) -> Result<Line, BadValue> {
    let default = platform::default_timebase(family).ok_or_else(|| {
        BadValue::new(config::family_vars(family).rate, Reason::ForbiddenTimebase)
    })?;
    let zero = sample(read, family, default)?;
    let fake_anchor = zero.saturating_add(setup.shift);
    if fake_anchor < Nanos::ZERO {
        let vars = config::family_vars(family);
        return Err(BadValue::new(
            vars.shift.unwrap_or(vars.rate),
            Reason::BelowZero,
        ));
    }
    Ok(Line {
        fake_anchor,
        timebase_anchor: anchor(read, family, zero, setup)?,
        rate: setup.rate,
    })
}

/// The parameters a session that fakes nothing publishes: every family showing its own genuine
/// clock at real speed.
///
/// The null keeper reads none of them, so nothing here can refuse a reading the way a plan does: a
/// clock the kernel will not give up lands as zero rather than as a bad value, and a creator
/// recording a fault still gets its session on disk.
fn genuine_plan(read: &dyn Fn(libc::clockid_t) -> Option<Nanos>) -> Plan {
    let identity = |clock| {
        let now = read(clock).unwrap_or(Nanos::ZERO);
        Line {
            fake_anchor: now,
            timebase_anchor: now,
            rate: Rate::UNITY,
        }
    };
    let realtime = identity(clock::REALTIME);
    Plan {
        realtime,
        monotonic: identity(clock::MONOTONIC),
        boottime: identity(clock::BOOTTIME),
        cpu_rate: Rate::UNITY,
        // What the kernel says, like everything else here: a null session's TAI leads its wall
        // clock by exactly what the genuine pair does.
        tai_offset: tai_offset(read, realtime.fake_anchor),
        creation_time: realtime.fake_anchor,
    }
}

/// The parameters a fresh session starts with, from one round of genuine readings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// The wall clock's line.
    pub realtime: Line,
    /// The Monotonic family's line.
    pub monotonic: Line,
    /// The Boottime family's line.
    pub boottime: Line,
    /// How fast process and thread CPU time runs.
    pub cpu_rate: Rate,
    /// Whole seconds by which genuine `CLOCK_TAI` leads genuine `CLOCK_REALTIME`.
    pub tai_offset: i64,
    /// Genuine `CLOCK_REALTIME` when the readings were taken.
    pub creation_time: Nanos,
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
    /// The environment asked for something the library could not set up. Nothing is written, so
    /// the directory is left clean for whatever the creator publishes in its place.
    BadValue(BadValue),
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
            Self::BadValue(bad) => write!(f, "{bad}"),
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

impl From<BadValue> for SessionError {
    fn from(bad: BadValue) -> Self {
        Self::BadValue(bad)
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
    create_generated(setup, None)
}

/// [`create`], with `fault` recorded in the session before it is published, so children attach to
/// a session that already says what went wrong and a controller can read it later.
///
/// # Errors
/// Whatever [`create`] rejects.
pub fn create_faulted(setup: &Setup, fault: Fault) -> Result<Session, SessionError> {
    create_generated(setup, Some(fault))
}

fn create_generated(setup: &Setup, fault: Option<Fault>) -> Result<Session, SessionError> {
    let mut random = [0u8; 16];
    platform::random_bytes(&mut random)?;
    create_named(
        setup,
        &naming::session_name(std::process::id(), random),
        fault,
    )
}

/// [`create`] with the name supplied. The name is generated in every path a target takes; this is
/// the seam the tests use to force a collision.
///
/// # Errors
/// [`SessionError::BadValue`] when the setup cannot be turned into parameters,
/// [`SessionError::Collision`] when the name is taken, or [`SessionError::Io`] when a step of the
/// build fails.
pub fn create_named(
    setup: &Setup,
    name: &str,
    fault: Option<Fault>,
) -> Result<Session, SessionError> {
    ensure_dir(&setup.dir)?;
    // Before anything is on disk, so a creator that cannot set up what it was asked for leaves the
    // directory clean.
    let plan = match setup.keeper {
        Keeper::Affine => setup.plan(&platform::genuine_nanos)?,
        Keeper::Null => genuine_plan(&platform::genuine_nanos),
    };

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
    unsafe { initialize(header_ptr(base), setup, &plan, fault, pid, page, len) };

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
unsafe fn initialize(
    header: *mut Header,
    setup: &Setup,
    plan: &Plan,
    fault: Option<Fault>,
    pid: u32,
    page: usize,
    len: usize,
) {
    // Safety: the caller guarantees a zeroed, header-sized mapping nobody else can reach.
    let header = unsafe { &mut *header };
    header.magic = layout::MAGIC;
    header.abi_version = layout::ABI_VERSION;
    header.platform = platform::PLATFORM_TAG;
    header.keeper = setup.keeper as u32;
    header.emulate_vdso = u32::from(setup.emulate_vdso);
    header.fail_closed = u32::from(setup.fail_closed);
    header.file_size = u32::try_from(len).unwrap_or(u32::MAX);
    header.page_size = u32::try_from(page).unwrap_or(u32::MAX);
    header.arena_offset = 0;
    header.arena_size = 0;
    header.creator_pid = pid;
    header.creation_time = plan.creation_time.0;
    // A negative interval is not a thing; it lands as zero rather than as an enormous one.
    header.timekeep_interval = u64::try_from(setup.timekeep_interval.0).unwrap_or(0);

    let params = &header.live;
    params
        .realtime
        .store(plan.realtime, setup.realtime.timebase);
    params
        .monotonic
        .store(plan.monotonic, setup.monotonic.timebase);
    params
        .boottime
        .store(plan.boottime, setup.boottime.timebase);
    params.cpu_rate.store(plan.cpu_rate.0, Ordering::Relaxed);
    params.tai_offset.store(plan.tai_offset, Ordering::Relaxed);
    params.epoch.store(0, Ordering::Relaxed);
    copy_params(&header.live, &header.shadow);

    if let Some(fault) = fault {
        header.record_fault(fault);
    }

    header.ready.store(1, Ordering::Release);
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

/// What the session shows for `clock`, or `None` when the call belongs to the genuine clock.
///
/// The read path: the parameter words and one genuine clock sample. No lock, no allocation,
/// nothing written to shared memory. The seqlock read flow goes around this later; today the
/// parameters are written once, before the file is linked into place, and never change, and
/// attach's acquire on `ready` is what orders them ahead of every read.
///
/// Dispatch is static: the keeper is an enum, so choosing costs one predictable branch.
#[inline]
pub fn clock_now(header: &Header, clock: libc::clockid_t) -> Option<Nanos> {
    match Keeper::from_tag(header.keeper)? {
        Keeper::Affine => affine_now(&header.live, clock),
        // The genuine reading, which the caller reaches by letting the call through untouched.
        Keeper::Null => None,
    }
}

/// The affine keeper: the family's line projected over a genuine reading of its timebase.
///
/// Every value here comes out of shared memory a writer may be changing, so every step saturates
/// and the arithmetic holds for any bit pattern. A torn read is discarded by the seqlock around
/// this, not prevented inside it.
#[inline]
fn affine_now(params: &layout::Params, clock: libc::clockid_t) -> Option<Nanos> {
    match clock::family_of(clock)? {
        Family::Realtime => {
            let now = project(&params.realtime)?;
            if clock::is_tai(clock) {
                // The whole-second offset genuine `CLOCK_TAI` leads genuine `CLOCK_REALTIME` by,
                // fixed at creation, so a Realtime set to 1990 shows TAI at 1990 plus today's.
                let offset = params.tai_offset.load(Ordering::Relaxed);
                return Some(now.saturating_add(Nanos(offset.saturating_mul(Nanos::SECOND))));
            }
            Some(now)
        }
        Family::Monotonic => project(&params.monotonic),
        Family::Boottime => project(&params.boottime),
        // Cpu has no timebase and no anchor pair: each clock scales its own consumption, which
        // only grows, so the line is continuous by construction and the read path keeps no state.
        Family::Cpu => {
            let rate = Rate(params.cpu_rate.load(Ordering::Relaxed));
            Some(Nanos(rate.scale(platform::genuine_nanos(clock)?.0)))
        }
    }
}

#[inline]
fn project(line: &layout::FamilyLine) -> Option<Nanos> {
    Some(
        line.load()
            .project(platform::genuine_nanos(line.timebase())?),
    )
}

// Creating a session reads a genuine clock and the system's randomness, so these cases need a
// filled-in platform arm.
#[cfg(all(test, target_os = "linux"))]
mod tests;
