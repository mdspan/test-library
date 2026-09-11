//! Creating a session file, attaching to one, and the checks that keep both honest.

pub mod layout;
pub mod naming;
pub mod timekeep;

use std::ffi::{CString, OsStr, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::Ordering;

use crate::config::{
    self, BadValue, ENV_CPU_RATE, ENV_EMULATE_VDSO, ENV_FAIL_CLOSED, ENV_HOOK, ENV_KEEPER,
    ENV_RATE, ENV_REALTIME_SHIFT, ENV_SHIFT, ENV_TIME, ENV_TIMEKEEP_INTERVAL, EnvLookup, Reason,
    duration_var, flag_var, rate_var, timestamp_var,
};
use crate::hook::{self, Hook, Init, Spec};
use crate::platform::{self, clock};
use crate::sync;
use crate::time::{Family, Line, Nanos, Rate};

use layout::{BadHeader, Fault, Header, Keeper};
use timekeep::Page;

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

/// The hook the environment names, or `None` when it names none. An empty value means none.
fn hook_from_env(env: &impl EnvLookup) -> Option<Spec> {
    Spec::parse(&env.nonempty(ENV_HOOK)?)
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
    /// The hook in front of the keeper, or `None` when the session has none. Independent of the
    /// keeper: a hook over the null keeper answers what it wants and leaves the rest genuine.
    pub hook: Option<Spec>,
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
    pub fn plain(dir: PathBuf) -> Self {
        Self {
            dir,
            keeper: Keeper::Affine,
            hook: None,
            realtime_at: None,
            realtime: FamilySetup::tracking(line_timebase(Family::Realtime)),
            monotonic: FamilySetup::tracking(line_timebase(Family::Monotonic)),
            boottime: FamilySetup::tracking(line_timebase(Family::Boottime)),
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
            // Read whichever keeper the session runs: the two are independent.
            hook: hook_from_env(env),
            emulate_vdso: flag_var(env, ENV_EMULATE_VDSO, true),
            fail_closed: flag_var(env, ENV_FAIL_CLOSED, false),
            timekeep_interval: duration_var(env, ENV_TIMEKEEP_INTERVAL)?
                .unwrap_or(DEFAULT_TIMEKEEP_INTERVAL),
            ..Self::plain(dir)
        };
        // A hook answers with a function, and the timekeep page carries numbers, so a session with
        // both would claim to fake a reader of the page and hand it time the hook never saw. On
        // either keeper, which is why this comes before the null keeper leaves.
        if hook_needs_page_off(
            platform::HAS_TIMEKEEP_PAGE && cfg!(feature = "vdso"),
            session.emulate_vdso,
            session.hook.is_some(),
        ) {
            return Err(BadValue::new(ENV_HOOK, Reason::NeedsPageOff));
        }

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

        let session = Self {
            realtime: FamilySetup::from_env(env, Family::Realtime, bare_shift, bare_rate)?,
            monotonic: FamilySetup::from_env(env, Family::Monotonic, None, bare_rate)?,
            boottime: FamilySetup::from_env(env, Family::Boottime, None, bare_rate)?,
            cpu_rate: rate_var(env, ENV_CPU_RATE)?.unwrap_or(Rate::UNITY),
            realtime_at,
            ..session
        };

        // The page carries one scale for the wall clock and the elapsed clock it serves, so the
        // two families cannot be given rates of their own while it is live.
        if let Some(family) = page_elapsed_family()
            && let Some(elapsed) = session.family(family)
            && page_rates_differ(
                platform::HAS_TIMEKEEP_PAGE && cfg!(feature = "vdso"),
                session.emulate_vdso,
                session.realtime.rate,
                elapsed.rate,
            )
        {
            return Err(BadValue::new(
                config::family_vars(family).rate,
                Reason::PageSharesARate,
            ));
        }
        Ok(session)
    }

    /// One family's settings, or `None` for Cpu, which carries no line and keeps its one setting
    /// in [`Setup::cpu_rate`].
    #[must_use]
    pub const fn family(&self, family: Family) -> Option<&FamilySetup> {
        match family {
            Family::Realtime => Some(&self.realtime),
            Family::Monotonic => Some(&self.monotonic),
            Family::Boottime => Some(&self.boottime),
            Family::Cpu => None,
        }
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

/// The family the timekeep page's elapsed clock belongs to: the one `CLOCK_MONOTONIC` names,
/// which is Monotonic through FreeBSD 14.x and Boottime from 15.0.
///
/// `None` where that clock belongs to no family, which is a platform that fakes it some other way.
#[must_use]
#[cfg_attr(
    not(target_os = "freebsd"),
    expect(
        clippy::missing_const_for_fn,
        reason = "the FreeBSD arm of `family_of` asks the running kernel which column of the \
                  family table applies, so this is a call there and not a constant"
    )
)]
pub fn page_elapsed_family() -> Option<Family> {
    clock::family_of(clock::MONOTONIC)
}

/// Whether a session would leave the two clocks the timekeep page serves running at different
/// speeds.
///
/// The page carries one scale, so the wall clock and the elapsed clock it computes from that scale
/// advance together whatever the session says. Three things have to be true for that to bind: the
/// platform reads the clock from such a page, this build can write one, and the session asked for
/// it.
#[must_use]
const fn page_rates_differ(
    page_in_this_build: bool,
    emulate_vdso: bool,
    realtime: Rate,
    elapsed: Rate,
) -> bool {
    page_in_this_build && emulate_vdso && realtime.0 != elapsed.0
}

/// Whether a session asks for a hook on a platform whose timekeep page it also leaves on.
///
/// Three things have to be true: the platform reads the clock from such a page, this build can
/// write one, and the session asked for it. A build without the page, or a session that turned it
/// off, runs the hook through interposition and reaches every program that calls libc.
#[must_use]
const fn hook_needs_page_off(page_in_this_build: bool, emulate_vdso: bool, hook: bool) -> bool {
    page_in_this_build && emulate_vdso && hook
}

/// The clock a family's line starts on: its default timebase, and the wall clock for the one
/// family that has none, which is Cpu and which carries no line.
fn line_timebase(family: Family) -> libc::clockid_t {
    platform::default_timebase(family).unwrap_or(clock::REALTIME)
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
    let realtime = identity(line_timebase(Family::Realtime));
    Plan {
        realtime,
        monotonic: identity(line_timebase(Family::Monotonic)),
        boottime: identity(line_timebase(Family::Boottime)),
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
    hook: Option<Hook>,
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

    /// The hook in front of the keeper, or `None` when the session has none.
    #[must_use]
    pub const fn hook(&self) -> Option<&Hook> {
        self.hook.as_ref()
    }

    /// Loads the hook this session names, if it names one, without running its `init`.
    ///
    /// An attacher joins a session whose arena `init` already filled, so it loads the library and
    /// nothing more. A creator has its hook loaded already, and calling this again would load the
    /// library a second time.
    ///
    /// # Errors
    /// The [`BadValue`] naming what the hook library could not do. The session stays mapped, so a
    /// caller can record the fault in it before it runs on genuine time.
    pub fn load_hook(&mut self) -> Result<(), BadValue> {
        let recorded = self.header().hook_path();
        if recorded.is_empty() {
            return Ok(());
        }
        let path = Path::new(OsStr::from_bytes(recorded));
        let (arena, size) = arena_in(self.base, self.len, self.header());
        self.hook = Some(
            hook::load(path)
                .and_then(|hook| hook.open_arena(arena, size, Init::Skip))
                .map_err(|why| BadValue::new(ENV_HOOK, Reason::Hook(why)))?,
        );
        Ok(())
    }

    /// The timekeep page inside the mapping, or `None` when this session keeps none live.
    ///
    /// Live means all three: the platform reads the clock from such a page, this build can write
    /// one, and the session asked for it. Everywhere else the page is present in the file, zero,
    /// and unread.
    #[must_use]
    pub fn timekeep(&self) -> Option<&Page> {
        let page = timekeep_in(self.base, self.len, self.header())?;
        // Safety: the page lies inside a mapping this `Session` keeps alive, and every word of it
        // is an atomic, so a shared reference to it is sound however many processes hold one.
        Some(unsafe { page.as_ref() })
    }

    /// Hands the mapping, the timekeep page, and the hook to the process for good.
    #[must_use]
    pub fn leak(
        mut self,
    ) -> (
        &'static Header,
        Option<&'static Page>,
        Option<&'static Hook>,
    ) {
        let hook = self.hook.take().map(|hook| &*Box::leak(Box::new(hook)));
        let session = std::mem::ManuallyDrop::new(self);
        // Safety: the mapping outlives the process, because `ManuallyDrop` keeps `Drop` from
        // unmapping it, and both `attach` and `create` validated it is at least a header long.
        let header: &'static Header = unsafe { &*header_ptr(session.base) };
        let page = timekeep_in(session.base, session.len, header).map(|page| {
            // Safety: the page lies inside the same mapping, and every word of it is an atomic.
            unsafe { page.as_ref() }
        });
        (header, page, hook)
    }
}

/// Makes sure a session with the page on has an updater keeping it fresh.
#[cfg(feature = "vdso")]
fn ensure_updater(path: &Path, header: &Header) {
    crate::updater::ensure(path, header);
}

/// A build without the feature keeps no updater, so there is nothing to make sure of.
#[cfg(not(feature = "vdso"))]
const fn ensure_updater(_path: &Path, _header: &Header) {}

/// Whether `header` names a session that keeps its timekeep page live.
#[must_use]
pub const fn page_is_live(header: &Header) -> bool {
    platform::HAS_TIMEKEEP_PAGE && cfg!(feature = "vdso") && header.emulate_vdso == 1
}

/// Where the timekeep page sits inside a mapping, or `None` when this session keeps none live or
/// the mapping is too short to hold one.
fn timekeep_in(base: NonNull<u8>, len: usize, header: &Header) -> Option<NonNull<Page>> {
    if !page_is_live(header) {
        return None;
    }
    let offset = layout::timekeep_offset(usize::try_from(header.page_size).ok()?);
    if offset.checked_add(size_of::<Page>())? > len {
        return None;
    }
    // Safety: the page lies inside a mapping of `len` bytes, which the check above holds to.
    Some(unsafe { base.add(offset) }.cast::<Page>())
}

/// The arena inside a mapping, and how long it is: the pair a hook is handed.
///
/// A session with no arena, and one whose mapping is shorter than the arena its header names, both
/// answer null and zero, which the ABI defines as no arena at all. Never a pointer without its
/// length, and never a length without its pointer.
fn arena_in(base: NonNull<u8>, len: usize, header: &Header) -> (Option<NonNull<c_void>>, u64) {
    let whole = usize::try_from(header.arena_offset)
        .ok()
        .zip(usize::try_from(header.arena_size).ok())
        .filter(|&(_, size)| size > 0)
        .filter(|&(offset, size)| offset.checked_add(size).is_some_and(|end| end <= len));
    let Some((offset, _)) = whole else {
        return (None, 0);
    };
    // Safety: the arena lies inside a mapping of `len` bytes, which the filters above checked.
    let arena = unsafe { base.add(offset) };
    (Some(arena.cast()), header.arena_size)
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
    let mut build = Build::new(setup, fault)?;

    let (path, tmp) = naming::session_paths(&setup.dir, name);
    let len = build.len;

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
    unsafe { initialize(header_ptr(base), setup, &plan, &build) };

    // Safety: the mapping is a header long and holds the header just written.
    let header = unsafe { &*header_ptr(base) };
    // The hook's `init` fills the arena here, while the file is still under its private name, so it
    // has run before any other process can see the arena.
    let (arena, arena_size) = arena_in(base, len, header);
    let opened = build
        .hook
        .take()
        .map(|hook| hook.open_arena(arena, arena_size, Init::Run))
        .transpose();
    let hook = match opened {
        Ok(hook) => hook,
        Err(why) => {
            // Safety: the mapping is ours and unaliased.
            unsafe { libc::munmap(base.as_ptr().cast(), len) };
            // Safety: `c_tmp` names the temporary file we created.
            unsafe { libc::unlink(c_tmp.as_ptr()) };
            return Err(BadValue::new(ENV_HOOK, Reason::Hook(why)).into());
        }
    };

    // The timekeep page, while the file is still under its private name, so the first process to
    // find it finds a page a reader can already follow.
    if let Some(page) = timekeep_in(base, len, header) {
        // Safety: the page lies inside the mapping, which nothing else can reach yet.
        let page = unsafe { page.as_ref() };
        page.initialize();
        if let Some(basis) = timekeep::counter::basis() {
            timekeep::republish(header, page, basis);
        }
    }

    // The lock file carries no data, so it is made here and never touched again: writers take the
    // lock on it and readers probe it, whichever generation of the state the session file holds. It
    // comes after the session file, so a creator that gives up leaves neither behind.
    sync::install(&naming::lock_path(&path))?;
    // The lease belongs to the updater that keeps the timekeep page fresh, so a session with the
    // page off has no lease to keep and gets the write lock's file alone.
    if platform::HAS_TIMEKEEP_PAGE && cfg!(feature = "vdso") && setup.emulate_vdso {
        sync::create(&naming::lease_path(&path))?;
    }

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

    // The updater keeps the page fresh for as long as anyone is attached. Spawned once the session
    // is in place, so the process it execs finds a session to attach to.
    ensure_updater(&path, header);

    Ok(Session {
        base,
        len,
        path,
        hook,
    })
}

/// What a creator settles before it touches the disk: the file's shape, the hook path to record,
/// and who to blame for a fault.
///
/// Everything here can fail, and all of it fails before the first byte is written.
#[derive(Debug)]
struct Build {
    page: usize,
    len: usize,
    arena: usize,
    hook_path: PathBuf,
    hook: Option<Hook>,
    pid: u32,
    fault: Option<Fault>,
}

impl Build {
    /// Settles the shape and loads the hook library, before the first byte is written, so a hook the
    /// loader refuses leaves the session directory as clean as any other bad value does.
    ///
    /// # Errors
    /// The [`BadValue`] naming what the hook could not do: a library that will not load, a path too
    /// long to record, or an arena larger than a session file holds.
    fn new(setup: &Setup, fault: Option<Fault>) -> Result<Self, BadValue> {
        let page = page_size();
        let bad = |why| BadValue::new(ENV_HOOK, Reason::Hook(why));
        let hook_path = setup
            .hook
            .as_ref()
            .map(Spec::recorded_path)
            .unwrap_or_default();
        if hook_path.as_os_str().len() >= layout::HOOK_PATH_MAX {
            return Err(bad(hook::HookError::PathTooLong));
        }
        let (arena, hook) = match &setup.hook {
            Some(spec) => (
                spec.arena_bytes(page).map_err(bad)?,
                Some(hook::load(&hook_path).map_err(bad)?),
            ),
            None => (0, None),
        };
        Ok(Self {
            page,
            len: layout::file_size(page, arena),
            arena,
            hook_path,
            hook,
            pid: std::process::id(),
            fault,
        })
    }
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
unsafe fn initialize(header: *mut Header, setup: &Setup, plan: &Plan, build: &Build) {
    // Safety: the caller guarantees a zeroed, header-sized mapping nobody else can reach.
    let header = unsafe { &mut *header };
    header.magic = layout::MAGIC;
    header.abi_version = layout::ABI_VERSION;
    header.platform = platform::PLATFORM_TAG;
    header.keeper = setup.keeper as u32;
    header.emulate_vdso = u32::from(setup.emulate_vdso);
    header.fail_closed = u32::from(setup.fail_closed);
    header.file_size = u32::try_from(build.len).unwrap_or(u32::MAX);
    header.page_size = u32::try_from(build.page).unwrap_or(u32::MAX);
    // The arena sits after the library's own structures, page-aligned, and is absent when the hook
    // asked for none: offset and size are zero together.
    header.arena_offset = if build.arena == 0 {
        0
    } else {
        layout::arena_offset(build.page) as u64
    };
    header.arena_size = build.arena as u64;
    write_hook_path(header, &build.hook_path);
    header.creator_pid = build.pid;
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

    if let Some(fault) = build.fault {
        header.record_fault(fault);
    }

    header.ready.store(1, Ordering::Release);
}

/// Records the hook library every process of the session loads, NUL-terminated, or leaves the field
/// empty when the session has no hook. `Build` has already refused a path too long to fit.
fn write_hook_path(header: &mut Header, path: &Path) {
    let bytes = path.as_os_str().as_bytes();
    if let Some(field) = header.hook_path.get_mut(..bytes.len()) {
        field.copy_from_slice(bytes);
    }
}

fn copy_params(from: &layout::Params, to: &layout::Params) {
    to.store(&from.load());
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
        hook: None,
    };
    if let Err(why) = session.header().validate(len, platform::PLATFORM_TAG) {
        return Err(SessionError::BadSessionFile(why));
    }
    // So this process's readers can probe the write lock. A session made by an older process
    // always has the file; one that somehow does not gets it here.
    sync::install(&naming::lock_path(path))?;
    // Every attach to a session with the page on probes the lease, because an updater that died
    // leaves the page to go stale and nothing else is watching for it.
    ensure_updater(path, session.header());
    Ok(session)
}

/// What the session shows for `clock`, or `None` when the call belongs to the genuine clock.
///
/// The read path: the seqlock's read flow around the parameter words and one genuine clock sample.
/// No lock, no allocation, nothing written to shared memory.
///
/// The family is settled before the flow is entered, so a clock we leave alone costs one lookup
/// rather than a wait on a writer whose parameters it was never going to read. Dispatch is static:
/// the keeper is an enum, so choosing costs one predictable branch.
#[inline]
pub fn clock_now(header: &Header, clock: libc::clockid_t) -> Option<Nanos> {
    match Keeper::from_tag(header.keeper)? {
        Keeper::Affine => {
            let family = clock::family_of(clock)?;
            sync::read(header, |params| affine_now(params, family, clock))
        }
        // The genuine reading, which the caller reaches by letting the call through untouched.
        Keeper::Null => None,
    }
}

/// What the session shows for an amount of CPU time already consumed on `clock`, `value` genuine
/// nanoseconds of it, or `None` when the amount belongs to the genuine clock.
///
/// This is what `times` and `getrusage` need: the consumption is in the call's own fields rather
/// than in a clock the keeper could read for itself. The Cpu family scales each reading on its own,
/// so a quantity of it converts with nothing but the family's rate, and a clock of any other family
/// has no such quantity to convert.
///
/// The same read path as [`clock_now`]: no lock, no allocation, nothing written to shared memory.
#[inline]
pub fn cpu_scaled(header: &Header, clock: libc::clockid_t, value: Nanos) -> Option<Nanos> {
    match Keeper::from_tag(header.keeper)? {
        Keeper::Affine => {
            if clock::family_of(clock) != Some(Family::Cpu) {
                return None;
            }
            sync::read(header, |params| {
                Some(Nanos(
                    Rate(params.cpu_rate.load(Ordering::Relaxed)).scale(value.0),
                ))
            })
        }
        Keeper::Null => None,
    }
}

/// The genuine duration a wait of `fake` nanoseconds on `clock` blocks for, or `None` when the
/// wait belongs to the genuine clock.
///
/// A wait is a promise about faketime: wake when the clock has advanced this far. So the duration
/// is divided by the rate of the family the wait follows, and a frozen family owes it the longest
/// duration there is. An absolute deadline reaches here as the fake duration still remaining.
///
/// The same read path as [`clock_now`]: no lock, no allocation, nothing written to shared memory.
#[inline]
pub fn wait_duration(header: &Header, clock: libc::clockid_t, fake: Nanos) -> Option<Nanos> {
    match Keeper::from_tag(header.keeper)? {
        Keeper::Affine => {
            let family = clock::family_of(clock)?;
            sync::read(header, |params| {
                Some(Nanos(family_rate(params, family).unscale(fake.0)))
            })
        }
        // Genuine time underneath, so a wait blocks for exactly as long as it asked.
        Keeper::Null => None,
    }
}

/// The fake duration a timer's genuine remaining stands for, or `None` when the timer belongs to
/// the genuine clock.
///
/// The other direction from [`wait_duration`], and the one a timer query needs: the kernel counts
/// down in genuine time, and the program is owed the count in its own. So the remaining is
/// multiplied by the rate of the family the timer follows.
///
/// The same read path as [`clock_now`]: no lock, no allocation, nothing written to shared memory.
#[inline]
pub fn timer_remaining(header: &Header, clock: libc::clockid_t, genuine: Nanos) -> Option<Nanos> {
    match Keeper::from_tag(header.keeper)? {
        Keeper::Affine => {
            let family = clock::family_of(clock)?;
            sync::read(header, |params| {
                Some(remaining_of(family_rate(params, family), genuine))
            })
        }
        // Genuine time underneath, so a timer counts down in exactly the units it was armed in.
        Keeper::Null => None,
    }
}

/// What the session shows for a genuine file timestamp, or `None` when the timestamp stands as the
/// filesystem recorded it.
///
/// A timestamp must agree with the clock: a file written just now shows a modification time of
/// about now. So the genuine instant is placed against a paired sample of the two nows and carried
/// forward at the family's rate, `fake_now + rate * (ts - real_now)`. A result below zero clamps
/// there: that case is a clock set far into the past with an old file, and no timestamp reads
/// before the epoch because of us.
///
/// The rule is the Realtime family's alone, because a file timestamp is a wall-clock instant.
///
/// The same read path as [`clock_now`]: no lock, no allocation, nothing written to shared memory.
#[inline]
pub fn timestamp_shown(header: &Header, stamp: Nanos) -> Option<Nanos> {
    match Keeper::from_tag(header.keeper)? {
        Keeper::Affine => sync::read(header, |params| {
            let (fake_now, real_now) = realtime_pair(params)?;
            let rate = family_rate(params, Family::Realtime);
            let carried = Nanos(rate.scale(stamp.saturating_sub(real_now).0));
            Some(Nanos(fake_now.saturating_add(carried).0.max(0)))
        }),
        // Genuine time underneath, so a timestamp reads as the filesystem wrote it.
        Keeper::Null => None,
    }
}

/// The genuine instant a file timestamp the program asked for is stored as, or `None` when it is
/// stored as the program wrote it.
///
/// The other direction from [`timestamp_shown`]: the program names an instant in the faked wall
/// clock and the filesystem keeps genuine ones, so the instant is measured back from the pair,
/// `real_now + (ts - fake_now) / rate`. A frozen family stores the genuine now, because a clock
/// that never advances puts every instant it could name infinitely far away.
///
/// The same read path as [`clock_now`]: no lock, no allocation, nothing written to shared memory.
#[inline]
pub fn timestamp_stored(header: &Header, stamp: Nanos) -> Option<Nanos> {
    match Keeper::from_tag(header.keeper)? {
        Keeper::Affine => sync::read(header, |params| {
            let (fake_now, real_now) = realtime_pair(params)?;
            let rate = family_rate(params, Family::Realtime);
            if rate == Rate::FROZEN {
                return Some(real_now);
            }
            let measured = Nanos(rate.unscale(stamp.saturating_sub(fake_now).0));
            Some(real_now.saturating_add(measured))
        }),
        Keeper::Null => None,
    }
}

/// One paired sample of faked and genuine `CLOCK_REALTIME`, which is what a file timestamp is
/// converted against.
///
/// With no timebase override the two come from a single reading, so the pair is exact. An override
/// makes the family track another clock, which has a reading of its own to take.
#[inline]
fn realtime_pair(params: &layout::Params) -> Option<(Nanos, Nanos)> {
    let real_now = platform::genuine_nanos(clock::REALTIME)?;
    let timebase = params.realtime.timebase();
    let passage = if timebase == clock::REALTIME {
        real_now
    } else {
        platform::genuine_nanos(timebase)?
    };
    Some((params.realtime.load().project(passage), real_now))
}

/// One timer query's arithmetic.
///
/// Nothing left stays nothing left, because a timer with no remaining is a disarmed one rather
/// than one about to fire. Something left never becomes nothing: a frozen family owes the timer
/// the longest duration there is, the way it owes a wait one, and a family too slow for the
/// remaining to round to a nanosecond still owes it the nanosecond that says it is armed.
#[inline]
fn remaining_of(rate: Rate, genuine: Nanos) -> Nanos {
    if genuine.0 <= 0 {
        return Nanos::ZERO;
    }
    if rate == Rate::FROZEN {
        return Nanos(i64::MAX);
    }
    Nanos(rate.scale(genuine.0).max(1))
}

/// How fast one family runs, off shared memory a writer may be changing. Every bit pattern is a
/// rate, so a torn read is discarded by the seqlock around this rather than caught inside it.
#[inline]
fn family_rate(params: &layout::Params, family: Family) -> Rate {
    Rate(
        match family {
            Family::Realtime => &params.realtime.rate,
            Family::Monotonic => &params.monotonic.rate,
            Family::Boottime => &params.boottime.rate,
            // Cpu has no line, only a rate, which is the one thing a wait on it needs.
            Family::Cpu => &params.cpu_rate,
        }
        .load(Ordering::Relaxed),
    )
}

/// The affine keeper: the family's line projected over a genuine reading of its timebase.
///
/// Every value here comes out of shared memory a writer may be changing, so every step saturates
/// and the arithmetic holds for any bit pattern. A torn read is discarded by the seqlock around
/// this, not prevented inside it.
#[inline]
fn affine_now(params: &layout::Params, family: Family, clock: libc::clockid_t) -> Option<Nanos> {
    match family {
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

#[cfg(test)]
mod tests;
