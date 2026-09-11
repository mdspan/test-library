//! The hook: a user's own function in front of the timekeeper.
//!
//! `LIBTIMEWARP_HOOK=/path/to/lib.so@4096` names a shared library and the size of the
//! [arena](Hook::arena), the block of shared state every process of the session sees. The library
//! is loaded into every process of the session, and every interposed call is channelled to one
//! fixed symbol in it, [`ANSWER_SYMBOL`]. The hook answers the call or declines it; a declined
//! call gets the keeper's answer, and so does every call when there is no hook.
//!
//! Adding an interposed function to the list is two lines here: a [`Func`] value, and a call to
//! [`Hook::read_clock`] or [`Hook::convert`] with the [`Kind`] that function's conversion is. The
//! gate, the genuine reading, the keeper's answer, and the status all come from there, so a new
//! function cannot forget one of them.

mod abi;
mod gate;

pub use abi::{ANSWER_SYMBOL, Call, Func, INIT_SYMBOL, Kind, Status};

use std::ffi::{CString, OsStr, c_char, c_void};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use crate::platform;
use crate::session::layout::{HOOK_PATH_MAX, arena_offset};
use crate::time::Nanos;

/// What `LIBTIMEWARP_HOOK` names: the library to load, and how much arena it asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// The library, as the user wrote it.
    pub path: PathBuf,
    /// The arena's size in bytes before rounding, or zero for no arena at all.
    pub arena_size: u64,
}

impl Spec {
    /// The hook `value` names, or `None` when it names none.
    ///
    /// An empty value reads as unset, like every other variable. The number after the last `@` is
    /// the arena size; a size that is not a plain count of bytes leaves the whole value as the path,
    /// so a directory with an `@` in its name is a path and not a parse error.
    #[must_use]
    pub fn parse(value: &OsStr) -> Option<Self> {
        let bytes = value.as_bytes();
        if bytes.is_empty() {
            return None;
        }
        let split = bytes
            .iter()
            .rposition(|&byte| byte == b'@')
            .and_then(|at| {
                let (path, size) = bytes.split_at(at);
                let digits = std::str::from_utf8(size.get(1..)?).ok()?;
                let size: u64 = digits.parse().ok()?;
                Some((path, size))
            })
            .filter(|&(path, _)| !path.is_empty());
        let (path, arena_size) = split.unwrap_or((bytes, 0));
        Some(Self {
            path: PathBuf::from(OsStr::from_bytes(path)),
            arena_size,
        })
    }

    /// The path the session records, so an attacher with a stripped environment still loads it.
    ///
    /// Absolute where the file can be found now, because a child that changed directory would never
    /// find a relative one; as the user wrote it otherwise, because a bare library name is for the
    /// loader's own search path to resolve.
    #[must_use]
    pub fn recorded_path(&self) -> PathBuf {
        std::fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone())
    }

    /// How many bytes of arena the session file carries: the size asked for, raised to a whole
    /// number of pages, and zero when none was asked for.
    ///
    /// # Errors
    /// [`HookError::Oversized`] when a session file that size could not be built.
    pub fn arena_bytes(&self, page: usize) -> Result<usize, HookError> {
        if self.arena_size == 0 {
            return Ok(0);
        }
        let rounded = usize::try_from(self.arena_size)
            .ok()
            .and_then(|size| size.checked_next_multiple_of(page))
            .ok_or(HookError::Oversized)?;
        // The header spells the whole file's length as a 32-bit word, so that is the ceiling.
        let whole = rounded
            .checked_add(arena_offset(page))
            .ok_or(HookError::Oversized)?;
        u32::try_from(whole).map_err(|_| HookError::Oversized)?;
        Ok(rounded)
    }
}

/// Why a hook could not be put in front of the keeper. Every one of these is a bad value: it is
/// logged, and by default the process runs on genuine time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookError {
    /// The path names nothing the loader will load.
    Unloadable,
    /// The library exports no `libtimewarp_hook`.
    NoSymbol,
    /// The library's own `init` refused the arena.
    Refused,
    /// The path is longer than the session file's field for it.
    PathTooLong,
    /// A larger arena than a session file can carry.
    Oversized,
}

impl std::fmt::Display for HookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unloadable => "the hook library could not be loaded",
            Self::NoSymbol => "the hook library exports no libtimewarp_hook",
            Self::Refused => "the hook library's own init refused the arena",
            Self::PathTooLong => "the hook library's path is too long to record in the session",
            Self::Oversized => "a larger arena than a session file can carry",
        })
    }
}

/// Whether opening the arena runs the hook's own `init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Init {
    /// The creator runs it, once, before any other process can see the arena.
    Run,
    /// An attacher skips it: it joins a session whose arena is already filled.
    Skip,
}

/// What every interposed call goes to.
type Answer = unsafe extern "C" fn(*const Call, *mut i64) -> i32;

/// What runs once, at session creation.
type Initialize = unsafe extern "C" fn(*mut c_void, u64) -> i32;

/// A loaded hook library: the symbol every call goes to, and the arena it shares.
///
/// Loading is a one-time cost at startup. A call is a branch on the gate, one genuine clock
/// reading, a struct built on the stack, and an indirect call: no allocation, no lock, and nothing
/// written to shared memory.
#[derive(Debug)]
pub struct Hook {
    answer: Answer,
    initialize: Option<Initialize>,
    arena: Option<NonNull<c_void>>,
    size: u64,
}

// Safety: the arena is shared memory and the symbol is process-global, so both are as valid in one
// thread as in another. Whether the hook's own state is safe to share is the hook's contract; the
// ABI says a hook that runs where a clock read runs keeps itself so.
unsafe impl Send for Hook {}
// Safety: as for `Send`.
unsafe impl Sync for Hook {}

/// Loads the hook library at `path` and finds the symbols the ABI names.
///
/// The arena comes afterwards, through [`Hook::open_arena`]: it lives in the session file, which a
/// creator has not built yet at the point it loads the hook. Loading first is what lets a library
/// that will not load leave the session directory untouched.
///
/// # Errors
/// The [`HookError`] naming what the library could not do.
pub fn load(path: &Path) -> Result<Hook, HookError> {
    if path.as_os_str().len() >= HOOK_PATH_MAX {
        return Err(HookError::PathTooLong);
    }
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| HookError::Unloadable)?;

    // `RTLD_NOW` so a library with a symbol it cannot resolve fails here rather than inside a clock
    // read, and `RTLD_LOCAL` so the hook's own symbols never answer for the program's.
    // Safety: `c_path` is a live NUL-terminated path.
    let library = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if library.is_null() {
        clear_loader_error();
        return Err(HookError::Unloadable);
    }
    // The library is never closed: it answers calls for the life of the process.

    // Safety: `library` is a live handle and the symbol name is NUL-terminated.
    let found = unsafe { symbol(library, ANSWER_SYMBOL) };
    let Some(answer) = found else {
        clear_loader_error();
        return Err(HookError::NoSymbol);
    };
    // Safety: the symbol is declared by the ABI with this signature.
    let answer: Answer = unsafe { std::mem::transmute::<*mut c_void, Answer>(answer.as_ptr()) };

    // Optional: a hook that fills nothing at creation exports none.
    // Safety: `library` is a live handle and the symbol name is NUL-terminated.
    let initialize = unsafe { symbol(library, INIT_SYMBOL) }.map(|found| {
        // Safety: the symbol is declared by the ABI with this signature.
        unsafe { std::mem::transmute::<*mut c_void, Initialize>(found.as_ptr()) }
    });
    if initialize.is_none() {
        clear_loader_error();
    }

    Ok(Hook {
        answer,
        initialize,
        arena: None,
        size: 0,
    })
}

/// A symbol of a loaded library, or `None` when it exports none by that name.
///
/// # Safety
/// `library` must be a live handle from `dlopen`, and `name` NUL-terminated.
unsafe fn symbol(library: *mut c_void, name: &[u8]) -> Option<NonNull<c_void>> {
    // Safety: the caller guarantees both arguments.
    NonNull::new(unsafe { libc::dlsym(library, name.as_ptr().cast::<c_char>()) })
}

impl Hook {
    /// The arena this process sees, or null when the session has none.
    #[must_use]
    pub fn arena(&self) -> *mut c_void {
        self.arena.map_or(std::ptr::null_mut(), NonNull::as_ptr)
    }

    /// The arena's size in bytes, zero when the session has none.
    #[must_use]
    pub const fn arena_size(&self) -> u64 {
        self.size
    }

    /// Points the hook at the arena inside the session file, and runs its `init` when asked to.
    ///
    /// A creator runs `init` here, with the session file still under its private name, so the arena
    /// is filled before any other process can see it. An attacher skips it: the arena it joins was
    /// filled once, by the creator.
    ///
    /// # Errors
    /// [`HookError::Refused`] when the library's own `init` returns anything but 0.
    pub fn open_arena(
        mut self,
        arena: Option<NonNull<c_void>>,
        size: u64,
        init: Init,
    ) -> Result<Self, HookError> {
        self.arena = arena;
        self.size = size;
        if let (Init::Run, Some(initialize)) = (init, self.initialize) {
            // Safety: the arena is ours, mapped and this long, and nothing else can reach it yet.
            if unsafe { initialize(self.arena(), size) } != 0 {
                return Err(HookError::Refused);
            }
        }
        Ok(self)
    }

    /// A clock read: the genuine reading of `clock` becomes what the program sees, so the number to
    /// convert and the genuine reading are the same one.
    ///
    /// `keeper` is what the timekeeper answers, or `None` for a clock it leaves genuine, where the
    /// genuine reading is the keeper's answer.
    ///
    /// `None` back means the keeper's answer stands: the hook declined, this thread is already
    /// inside the hook, or the kernel would not read the clock.
    #[must_use]
    #[inline]
    pub fn read_clock(
        &self,
        func: Func,
        clock: libc::clockid_t,
        keeper: Option<Nanos>,
    ) -> Option<Nanos> {
        let held = gate::enter()?;
        let real = platform::genuine_nanos(clock)?;
        let call = self.call(
            func,
            Kind::InstantToFake,
            clock,
            real,
            real,
            keeper.unwrap_or(real),
        );
        self.ask(&held, &call)
    }

    /// Every other shape: `value` is the number to convert and `keeper` what the keeper would
    /// answer for it.
    ///
    /// `None` back means the keeper's answer stands, as in [`Hook::read_clock`].
    #[must_use]
    #[inline]
    pub fn convert(
        &self,
        func: Func,
        kind: Kind,
        clock: libc::clockid_t,
        value: Nanos,
        keeper: Nanos,
    ) -> Option<Nanos> {
        let held = gate::enter()?;
        let real = platform::genuine_nanos(clock)?;
        let call = self.call(func, kind, clock, value, real, keeper);
        self.ask(&held, &call)
    }

    /// A setter: the program asked the clock to show an instant, and the hook may take that set
    /// into its own state instead of the keeper.
    ///
    /// The fake-to-genuine instant shape, with `value` the instant the program asked for. A setter
    /// is the one call with no number to decline to, so `keeper` carries the genuine instant that
    /// one lands at, which is now; [`Status::Handled`] means the program sees success and `*out` is
    /// ignored.
    ///
    /// [`Status::Declined`] means the keeper runs the set, which is also the answer when this
    /// thread is already inside the hook or the kernel will not read `clock`.
    #[must_use]
    #[inline]
    pub fn set(&self, func: Func, clock: libc::clockid_t, value: Nanos) -> Status {
        let Some(held) = gate::enter() else {
            return Status::Declined;
        };
        let Some(real) = platform::genuine_nanos(clock) else {
            return Status::Declined;
        };
        let call = self.call(func, Kind::InstantToGenuine, clock, value, real, real);
        if self.ask(&held, &call).is_some() {
            Status::Handled
        } else {
            Status::Declined
        }
    }

    /// One conversion, as the hook sees it: this hook's arena and the numbers of one call.
    #[inline]
    fn call(
        &self,
        func: Func,
        kind: Kind,
        clock: libc::clockid_t,
        value: Nanos,
        real: Nanos,
        keeper: Nanos,
    ) -> Call {
        Call::new(func, kind, clock, value.0, real.0, keeper.0).with_arena(self.arena(), self.size)
    }

    /// One call into the hook. `held` is the gate, so this cannot be reached without it.
    ///
    /// `*out` starts at the keeper's own answer, so a hook that reports handled without writing it
    /// hands the program the keeper's number rather than whatever the stack held.
    #[inline]
    fn ask(&self, _held: &gate::Held<'_>, call: &Call) -> Option<Nanos> {
        let mut out: i64 = call.keeper;
        // Safety: the call and the output live on this stack for the length of the call, which is
        // the whole of what the ABI asks of the caller.
        let status = unsafe { (self.answer)(std::ptr::from_ref(call), &raw mut out) };
        match Status::from_code(status) {
            Status::Handled => Some(Nanos(out)),
            Status::Declined => None,
        }
    }
}

#[cfg(test)]
impl Hook {
    /// A hook around `answer` in this process, for the cases that exercise the seam itself without a
    /// library on disk.
    fn stub(answer: Answer, arena: Option<NonNull<c_void>>, size: u64) -> Self {
        Self {
            answer,
            initialize: None,
            arena,
            size,
        }
    }
}

/// Drops the message the loader left in this thread.
///
/// `dlerror` reports and clears, so leaving ours set would hand the target program our message the
/// next time it asked about a failure of its own. An optional symbol a library does not export is
/// not a failure at all.
fn clear_loader_error() {
    // Safety: `dlerror` takes nothing and reports on the calling thread alone.
    let _reported = unsafe { libc::dlerror() };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(value: &str) -> Option<Spec> {
        Spec::parse(OsStr::new(value))
    }

    #[test]
    fn a_hook_value_names_a_library_and_an_arena_size() {
        assert_eq!(
            spec("/path/to/lib.so@4096"),
            Some(Spec {
                path: PathBuf::from("/path/to/lib.so"),
                arena_size: 4096,
            })
        );
    }

    #[test]
    fn the_size_defaults_to_zero_when_it_is_omitted() {
        assert_eq!(
            spec("/path/to/lib.so"),
            Some(Spec {
                path: PathBuf::from("/path/to/lib.so"),
                arena_size: 0,
            })
        );
        assert_eq!(
            spec("/path/to/lib.so@0").map(|spec| spec.arena_size),
            Some(0)
        );
    }

    #[test]
    fn an_empty_value_means_unset() {
        assert_eq!(spec(""), None);
    }

    #[test]
    fn a_size_that_is_not_a_count_of_bytes_is_part_of_the_path() {
        // A directory with an `@` in its name, a trailing `@`, and a size nothing could parse: each
        // is a path, and a path that will not load is what reports the problem.
        for value in [
            "/home/user@corp/lib.so",
            "/path/lib.so@",
            "/path/lib.so@lots",
            "/path/lib.so@-1",
            "@4096",
        ] {
            let parsed = spec(value).expect("a non-empty value names a hook");
            assert_eq!(parsed.path, PathBuf::from(value), "{value}");
            assert_eq!(parsed.arena_size, 0, "{value}");
        }
    }

    #[test]
    fn the_last_at_is_the_one_that_counts() {
        assert_eq!(
            spec("/home/user@corp/lib.so@8192"),
            Some(Spec {
                path: PathBuf::from("/home/user@corp/lib.so"),
                arena_size: 8192,
            })
        );
    }

    #[test]
    fn an_arena_is_a_whole_number_of_pages_and_nothing_when_none_was_asked_for() {
        let page = 4096;
        assert_eq!(spec("lib.so").unwrap().arena_bytes(page), Ok(0));
        assert_eq!(spec("lib.so@1").unwrap().arena_bytes(page), Ok(page));
        assert_eq!(spec("lib.so@4096").unwrap().arena_bytes(page), Ok(page));
        assert_eq!(spec("lib.so@4097").unwrap().arena_bytes(page), Ok(2 * page));
    }

    #[test]
    fn an_arena_larger_than_a_session_file_can_carry_is_refused() {
        for size in [u64::MAX, u64::from(u32::MAX), 1 << 32] {
            let spec = Spec {
                path: PathBuf::from("lib.so"),
                arena_size: size,
            };
            assert_eq!(spec.arena_bytes(4096), Err(HookError::Oversized), "{size}");
        }
    }

    #[test]
    fn a_path_that_names_nothing_is_a_bad_value() {
        let missing = Path::new("/nonexistent/libtimewarp-no-such-hook.so");
        assert_eq!(load(missing).err(), Some(HookError::Unloadable));
    }

    #[test]
    fn a_path_too_long_to_record_is_a_bad_value() {
        let long = PathBuf::from("/".repeat(HOOK_PATH_MAX));
        assert_eq!(load(&long).err(), Some(HookError::PathTooLong));
    }

    #[test]
    fn a_recorded_path_is_absolute_where_the_file_exists() {
        let spec = Spec {
            path: PathBuf::from("."),
            arena_size: 0,
        };
        assert!(spec.recorded_path().is_absolute());

        // A bare name is left alone, for the loader's own search path to resolve.
        let bare = Spec {
            path: PathBuf::from("libtimewarp-no-such-hook.so"),
            arena_size: 0,
        };
        assert_eq!(bare.recorded_path(), bare.path);
    }
}

/// The dispatch itself, against a hook in this process rather than one on disk.
#[cfg(test)]
mod answering {
    use std::cell::Cell;

    use super::*;
    use crate::platform::clock;

    /// What the last call handed the hook.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Seen {
        func: u32,
        kind: u32,
        clock: i32,
        value: i64,
        real_ns: i64,
        keeper: i64,
        arena: usize,
        size: u64,
    }

    thread_local! {
        /// The call the hook was handed, or `None` when it was never reached.
        static SEEN: Cell<Option<Seen>> = const { Cell::new(None) };
        /// What the hook answers next: a status, and the number it writes when it handles the call.
        static ANSWER: Cell<(i32, i64)> = const { Cell::new((0, 0)) };
        /// Whether the gate refused the hook a call of its own.
        static NESTED_REFUSED: Cell<Option<bool>> = const { Cell::new(None) };
    }

    /// A hook that records what it was handed and answers whatever [`ANSWER`] says.
    unsafe extern "C" fn answer(call: *const Call, out: *mut i64) -> i32 {
        // Safety: the library hands a live call and a live output word.
        let call = unsafe { &*call };
        SEEN.set(Some(Seen {
            func: call.func,
            kind: call.kind,
            clock: call.clock,
            value: call.value,
            real_ns: call.real_ns,
            keeper: call.keeper,
            arena: call.arena as usize,
            size: call.size,
        }));
        // A nested clock read from inside the hook runs the keeper alone, which is the gate refusing.
        NESTED_REFUSED.set(Some(gate::enter().is_none()));

        let (status, value) = ANSWER.get();
        if Status::from_code(status) == Status::Handled {
            // Safety: the library guarantees the output word for the length of the call.
            unsafe { *out = value };
        }
        status
    }

    fn hook() -> Hook {
        SEEN.set(None);
        NESTED_REFUSED.set(None);
        Hook::stub(answer, None, 0)
    }

    fn answers(status: Status, value: i64) {
        ANSWER.set((status as i32, value));
    }

    #[test]
    fn a_handled_clock_read_is_what_the_program_sees() {
        let hook = hook();
        answers(Status::Handled, 42);
        assert_eq!(
            hook.read_clock(Func::ClockGettime, clock::REALTIME, Some(Nanos(7))),
            Some(Nanos(42))
        );

        let seen = SEEN.get().expect("the hook was reached");
        assert_eq!(seen.func, Func::ClockGettime as u32);
        assert_eq!(seen.kind, Kind::InstantToFake as u32);
        assert_eq!(seen.clock, platform::clock_id(clock::REALTIME));
        assert_eq!(seen.keeper, 7, "the keeper's answer travels with the call");
        // A clock read converts the genuine reading, so the two are one number.
        assert_eq!(seen.value, seen.real_ns);
        assert!(seen.real_ns > 1_700_000_000 * Nanos::SECOND);
        assert_eq!((seen.arena, seen.size), (0, 0), "this session has no arena");
    }

    #[test]
    fn a_declined_call_leaves_the_keepers_answer_standing() {
        let hook = hook();
        answers(Status::Declined, 42);
        assert_eq!(
            hook.read_clock(Func::Gettimeofday, clock::REALTIME, Some(Nanos(7))),
            None
        );
        assert!(SEEN.get().is_some(), "the hook was asked all the same");
    }

    #[test]
    fn a_status_the_library_does_not_know_declines() {
        let hook = hook();
        ANSWER.set((7, 42));
        assert_eq!(hook.read_clock(Func::Time, clock::REALTIME, None), None);
    }

    #[test]
    fn a_clock_the_keeper_leaves_genuine_carries_its_genuine_reading() {
        let hook = hook();
        answers(Status::Handled, 42);
        assert_eq!(
            hook.read_clock(Func::ClockGettime, clock::MONOTONIC, None),
            Some(Nanos(42))
        );
        let seen = SEEN.get().expect("the hook was reached");
        assert_eq!(
            (seen.keeper, seen.value),
            (seen.real_ns, seen.real_ns),
            "with no keeper answer, the genuine reading is the one"
        );
    }

    #[test]
    fn the_other_three_shapes_carry_the_number_they_were_given() {
        for kind in [
            Kind::InstantToGenuine,
            Kind::DurationToGenuine,
            Kind::DurationToFake,
        ] {
            let hook = hook();
            answers(Status::Handled, 99);
            let (func, clock) = match kind {
                Kind::InstantToGenuine => (Func::Utimensat, clock::REALTIME),
                Kind::DurationToGenuine => (Func::Nanosleep, clock::MONOTONIC),
                _ => (Func::Getitimer, clock::REALTIME),
            };
            assert_eq!(
                hook.convert(func, kind, clock, Nanos(500), Nanos(250)),
                Some(Nanos(99))
            );
            let seen = SEEN.get().expect("the hook was reached");
            assert_eq!((seen.kind, seen.func), (kind as u32, func as u32));
            assert_eq!((seen.value, seen.keeper), (500, 250));
            assert_ne!(seen.real_ns, 0, "the genuine reading is there too");
        }
    }

    #[test]
    fn the_gate_refuses_the_hook_a_call_of_its_own() {
        let hook = hook();
        answers(Status::Handled, 1);
        assert_eq!(
            hook.read_clock(Func::ClockGettime, clock::REALTIME, None),
            Some(Nanos(1))
        );
        assert_eq!(
            NESTED_REFUSED.get(),
            Some(true),
            "a nested call reached the hook again"
        );
        // And the gate is free again once the call is over.
        assert_eq!(
            hook.read_clock(Func::ClockGettime, clock::REALTIME, None),
            Some(Nanos(1))
        );
    }

    #[test]
    fn a_handled_setter_is_success_and_a_declined_one_leaves_the_set_to_the_keeper() {
        let hook = hook();
        answers(Status::Handled, -42);
        assert_eq!(
            hook.set(Func::Settimeofday, clock::REALTIME, Nanos(1_234)),
            Status::Handled
        );

        let seen = SEEN.get().expect("the hook was reached");
        assert_eq!(seen.func, Func::Settimeofday as u32);
        assert_eq!(seen.kind, Kind::InstantToGenuine as u32);
        assert_eq!(seen.clock, platform::clock_id(clock::REALTIME));
        assert_eq!(seen.value, 1_234, "the instant the program asked for");
        // A setter has no number to decline to, so the field carries the genuine instant the one
        // it asked for lands at, which is now.
        assert_eq!(seen.keeper, seen.real_ns);
        assert!(seen.real_ns > 1_700_000_000 * Nanos::SECOND);

        answers(Status::Declined, -42);
        assert_eq!(
            hook.set(Func::ClockSettime, clock::REALTIME, Nanos(1_234)),
            Status::Declined
        );
    }

    #[test]
    fn a_setter_on_a_clock_the_kernel_will_not_read_declines_without_reaching_the_hook() {
        let hook = hook();
        answers(Status::Handled, 0);
        assert_eq!(
            hook.set(Func::ClockSettime, 0x7fff_ffff, Nanos(1_234)),
            Status::Declined
        );
        assert_eq!(SEEN.get(), None, "there was no genuine reading to hand it");
    }

    #[test]
    fn a_clock_the_kernel_will_not_read_never_reaches_the_hook() {
        let hook = hook();
        answers(Status::Handled, 1);
        assert_eq!(hook.read_clock(Func::ClockGettime, 0x7fff_ffff, None), None);
        assert_eq!(SEEN.get(), None, "there was no genuine reading to hand it");
    }

    #[test]
    fn the_arena_travels_with_every_call() {
        let mut word: i64 = 3;
        let arena = NonNull::new((&raw mut word).cast::<c_void>());
        let hook = Hook::stub(answer, arena, 8);
        assert_eq!(hook.arena(), arena.unwrap().as_ptr());
        assert_eq!(hook.arena_size(), 8);

        answers(Status::Handled, 1);
        assert_eq!(
            hook.read_clock(Func::ClockGettime, clock::REALTIME, None),
            Some(Nanos(1))
        );
        let seen = SEEN.get().expect("the hook was reached");
        assert_eq!(seen.arena, arena.unwrap().as_ptr() as usize);
        assert_eq!(seen.size, 8);
    }
}
