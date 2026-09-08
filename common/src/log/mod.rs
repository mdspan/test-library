//! The log: one line per event, written from anywhere a clock read can happen.
//!
//! We live inside somebody else's process, so nothing is written until a user names a destination.
//! With neither [`ENV_LOG`] nor [`ENV_LOG_FD`] set, no descriptor is opened and no logger is
//! installed, and a [`log!`](crate::log!) call costs one atomic load and a branch.
//!
//! A line is assembled on the stack, capped at [`line::CAPACITY`], and leaves in one `write(2)`:
//! no allocation, no locks, nothing that could recurse into a symbol we interpose. That is what
//! lets the read path and a signal handler write one.

pub mod line;

#[cfg(test)]
mod tests;

use std::ffi::{CString, OsStr};
use std::fmt::{self, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::sync::atomic::{AtomicI32, AtomicU8, AtomicU64, Ordering};

use libc::c_int;

use crate::config::{ENV_LOG, ENV_LOG_FD, ENV_LOG_LEVEL, EnvLookup};
use line::Line;

/// How much of the log a destination keeps: the named level and everything above it.
///
/// The more severe the level, the smaller its representation, so a line is kept when its own
/// representation is at or below the configured one. Zero is not a level: it is the word
/// [`installed`] reads when no logger was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Level {
    /// The library could not set up what it was asked for, and the process runs on genuine time.
    Error = 1,
    /// Something the library repaired or worked around, such as a suspected writer death.
    Warn = 2,
    /// The default.
    Info = 3,
    /// Detail for somebody debugging the library.
    Debug = 4,
    /// Every step of it.
    Trace = 5,
}

impl Level {
    /// What `LIBTIMEWARP_LOG_LEVEL` means when it is unset, or when its value is not a level.
    pub const DEFAULT: Self = Self::Info;

    /// Every level, most severe first.
    pub const ALL: [Self; 5] = [
        Self::Error,
        Self::Warn,
        Self::Info,
        Self::Debug,
        Self::Trace,
    ];

    /// The level `text` names, or `None` when it names none.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        Self::ALL
            .into_iter()
            .find(|level| level.as_str().eq_ignore_ascii_case(text))
    }

    /// The name a user writes for this level, and the name a line carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    const fn from_repr(repr: u8) -> Option<Self> {
        match repr {
            1 => Some(Self::Error),
            2 => Some(Self::Warn),
            3 => Some(Self::Info),
            4 => Some(Self::Debug),
            5 => Some(Self::Trace),
            _ => None,
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where lines go, and how much of the log lands there.
///
/// The descriptor is private to the library and is never closed: it must outlive everything in the
/// process that could still write a line, and `O_CLOEXEC` retires it at the next exec.
#[derive(Debug, Clone, Copy)]
pub struct Sink {
    descriptor: c_int,
    level: Level,
}

impl Sink {
    /// The destination the environment names, or `None` when it names none.
    ///
    /// [`ENV_LOG_FD`] wins over [`ENV_LOG`], and a descriptor named there is duplicated to a
    /// private number: a program that closes the number it passed us, and reuses it for a data
    /// file, never finds our line inside its data.
    #[must_use]
    pub fn from_env(env: &impl EnvLookup) -> Option<Self> {
        let level = env
            .nonempty(ENV_LOG_LEVEL)
            .and_then(|value| Level::parse(&value.to_string_lossy()))
            .unwrap_or(Level::DEFAULT);

        let named = env.nonempty(ENV_LOG_FD);
        if let Some(descriptor) = named.as_deref().and_then(private_copy_of) {
            return Some(Self { descriptor, level });
        }

        let sink = Self {
            descriptor: open_appending(&env.nonempty(ENV_LOG)?)?,
            level,
        };
        if named.is_some() {
            sink.write(
                Level::Error,
                format_args!("{ENV_LOG_FD}: not an open descriptor"),
            );
        }
        Some(sink)
    }

    /// The descriptor lines go to.
    #[must_use]
    pub const fn descriptor(self) -> c_int {
        self.descriptor
    }

    /// Whether this destination keeps lines at `level`.
    #[must_use]
    pub const fn enabled(self, level: Level) -> bool {
        level as u8 <= self.level as u8
    }

    /// Writes one line, if this destination keeps that level.
    pub fn write(self, level: Level, args: fmt::Arguments<'_>) {
        if !self.enabled(level) {
            return;
        }
        let mut line = Line::new();
        // Safety: `getpid` reads the caller's own process, takes no pointer, and cannot fail.
        let pid = unsafe { libc::getpid() };
        let _ = write!(line, "libtimewarp[{pid}] {level}: ");
        let _ = line.write_fmt(args);
        emit(self.descriptor, line.finish());
    }
}

/// No logger installed: what the level word holds until [`install`] runs, and what it keeps for a
/// process that named no destination.
const OFF: u8 = 0;

static LEVEL: AtomicU8 = AtomicU8::new(OFF);
static DESCRIPTOR: AtomicI32 = AtomicI32::new(-1);

/// Reads the three logging variables and installs the destination they name, if any.
///
/// Called once per process, at attach. The three are not setup variables: a process joining
/// somebody else's session reads them just the same, and a process that execs reads them again.
pub fn init(env: &impl EnvLookup) {
    if let Some(sink) = Sink::from_env(env) {
        install(sink);
    }
}

/// Publishes `sink` to [`log!`](crate::log!).
pub fn install(sink: Sink) {
    DESCRIPTOR.store(sink.descriptor, Ordering::Relaxed);
    // Released last, so a thread that sees a level also sees the descriptor it belongs to.
    LEVEL.store(sink.level as u8, Ordering::Release);
}

/// The installed destination, or `None` when this process logs nothing.
#[must_use]
pub fn installed() -> Option<Sink> {
    Some(Sink {
        level: Level::from_repr(LEVEL.load(Ordering::Acquire))?,
        descriptor: DESCRIPTOR.load(Ordering::Relaxed),
    })
}

/// Whether a line at `level` would be written. One atomic load and a branch.
#[inline]
#[must_use]
pub fn enabled(level: Level) -> bool {
    level as u8 <= LEVEL.load(Ordering::Acquire)
}

/// Writes one line to the installed destination. Prefer [`log!`](crate::log!), which skips
/// assembling the arguments when nothing would keep them.
pub fn write_line(level: Level, args: fmt::Arguments<'_>) {
    if let Some(sink) = installed() {
        sink.write(level, args);
    }
}

/// Writes one line at `level`, `format!`-style, to whatever destination this process installed.
///
/// ```
/// # use common::{log, log::Level};
/// log!(Level::Error, "{}: not a valid value", "LIBTIMEWARP_RATE");
/// ```
#[macro_export]
macro_rules! log {
    ($level:expr, $($arg:tt)*) => {{
        let level = $level;
        if $crate::log::enabled(level) {
            $crate::log::write_line(level, ::core::format_args!($($arg)*));
        }
    }};
}

/// Picks the one caller that reports an incident, so a condition every clock read runs into writes
/// one line rather than flooding the log.
///
/// The record is per process, because a reader leaves shared memory untouched: a session with many
/// processes logs one line each. An incident is named by a number that changes when the incident
/// does, such as the seqlock counter a reader found stuck. Zero is not a name: it is the word this
/// starts at, and so reads as already reported.
#[derive(Debug)]
pub struct OncePerIncident(AtomicU64);

impl OncePerIncident {
    /// Nothing reported yet.
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Whether this caller is the one that writes the line for `incident`.
    ///
    /// True once per incident per process. A different incident claims the slot from the one
    /// before it, so a condition that comes back after being repaired is reported again.
    pub fn claim(&self, incident: u64) -> bool {
        let mut reported = self.0.load(Ordering::Relaxed);
        loop {
            if reported == incident {
                return false;
            }
            match self.0.compare_exchange_weak(
                reported,
                incident,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(found) => reported = found,
            }
        }
    }
}

impl Default for OncePerIncident {
    fn default() -> Self {
        Self::new()
    }
}

/// The lowest number a private copy may take: out of the way of the descriptors a program hands
/// out, and inside the process's own limit. Zero when the limit leaves no room up there, which
/// gives the copy the lowest free number instead of no copy at all.
fn private_fd_floor() -> c_int {
    const WANTED: c_int = 1023;

    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // Safety: `limit` is a live, correctly typed buffer.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return 0;
    }
    c_int::try_from(limit.rlim_cur.saturating_sub(1)).map_or(WANTED, |room| room.min(WANTED))
}

/// A private `O_CLOEXEC` copy of the descriptor `value` names, or `None` when it names no open one.
fn private_copy_of(value: &OsStr) -> Option<c_int> {
    let named: c_int = value.to_string_lossy().trim().parse().ok()?;
    (named >= 0).then(|| dup_high(named)).flatten()
}

/// Duplicates `named` to a private number at or above [`private_fd_floor`], falling back to the
/// lowest free one for a process whose table is already crowded.
fn dup_high(named: c_int) -> Option<c_int> {
    [private_fd_floor(), 0].into_iter().find_map(|floor| {
        // Safety: `F_DUPFD_CLOEXEC` takes the floor as an `int`, and a descriptor that is not open
        // is refused rather than read.
        let copy = unsafe { libc::fcntl(named, libc::F_DUPFD_CLOEXEC, floor) };
        (copy >= 0).then_some(copy)
    })
}

/// Opens `path` for appending, creating it `0600`, or `None` when the system refuses.
///
/// `O_NOFOLLOW` refuses a symlink planted where the log should be, and `O_APPEND` keeps concurrent
/// lines whole across a session, since every line is one `write`.
fn open_appending(path: &OsStr) -> Option<c_int> {
    /// A log is for whoever set the variable, and no one else.
    const MODE: c_int = 0o600;

    let path = CString::new(path.as_bytes()).ok()?;
    // Safety: `path` is a live NUL-terminated string, and `open` takes the mode as an `int`.
    let opened = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            MODE,
        )
    };
    if opened < 0 {
        return None;
    }
    // `open` hands out a low number, which a program may later `dup2` a pipe of its own onto. Ours
    // moves out of the way for the same reason an inherited descriptor does.
    let Some(copy) = dup_high(opened) else {
        return Some(opened);
    };
    // Safety: `opened` is ours alone, and `copy` has taken its place.
    unsafe { libc::close(opened) };
    Some(copy)
}

/// One `write(2)`, retried only when a signal cut it off before anything left.
///
/// A short write loses the tail rather than sending it in a second call. The line's wholeness
/// comes from being one write: a destination shared by a whole session interleaves at every call
/// boundary, so a completed line here would be a torn line there. A destination that takes fewer
/// bytes than it was given is out of room, and a truncated line is what a full disk looks like.
fn emit(descriptor: c_int, bytes: &[u8]) {
    loop {
        // Safety: the pointer and length name a live buffer, which `write` only reads.
        let written = unsafe {
            libc::write(
                descriptor,
                bytes.as_ptr().cast::<libc::c_void>(),
                bytes.len(),
            )
        };
        if written >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return;
        }
    }
}
