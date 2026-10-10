//! The C surface over the [`controller`] crate, declared by
//! `include/libtimewarp_controller.h`.
//!
//! Every entry point answers a [`Code`] and leaves the message beside it in a slot this thread
//! owns, which `libtimewarp_last_error` hands back. Nothing here panics: a null pointer, a family
//! number we do not know, and a handle that is not one of ours are all refusals rather than
//! crashes, because the caller is somebody else's program.
//!
//! Two handles travel: a session, which is an `Arc` behind a raw pointer, and a transaction, which
//! keeps a clone of that `Arc`. So C may close the session while a transaction is still open, and
//! the mapping the transaction borrows from lives until both are gone.
//!
//! This is a `cdylib` of its own rather than a second crate type on `controller`, so that the
//! names below are exported by `libtimewarp_controller.so` alone and not by the preloaded
//! `libtimewarp.so`, whose exports are the interposed functions and nothing else.

use std::cell::RefCell;
use std::ffi::{CStr, CString, OsStr, c_char, c_double, c_int, c_uint};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

use common::session::layout::{HOOK_PATH_MAX, Keeper};
use common::time::{Family, Nanos};

use controller::{Code, Controller, Error, Transaction};

/// What a call handed a null pointer where it needed one is refused with.
const NULL_ARGUMENT: &str = "a required pointer argument was null";

/// What a call naming a family number we do not know is refused with.
const UNKNOWN_FAMILY: &str = "no family carries that number";

/// The four families, by the number the C enumeration spells each of them with.
const FAMILIES: [Family; 4] = Family::ALL;

/// The C shape of [`Info`](controller::Info), filled in by [`libtimewarp_info`].
///
/// The fields keep the header's own widths, so the struct has no padding a C compiler could lay
/// out differently. `hook_path` is bytes here and `char` there, which is the same thing on every
/// platform we support, and avoids the question of whether `c_char` is signed.
#[repr(C)]
#[derive(Debug)]
pub struct CInfo {
    /// The timekeeper, from the C `libtimewarp_keeper` enumeration.
    pub keeper: c_uint,
    /// 1 when the session poses as the vDSO on Linux, or interposes the Go runtime's clock
    /// fallback routines on FreeBSD.
    pub emulate_vdso: c_uint,
    /// 1 when a process that cannot set up what it was asked for exits rather than running genuine.
    pub fail_closed: c_uint,
    /// The process that created the session.
    pub creator_pid: c_uint,
    /// Genuine `CLOCK_REALTIME` when the session was created, in nanoseconds.
    pub creation_time: i64,
    /// Why the session stopped faking, or 0 while it still is.
    pub fault_code: c_uint,
    /// The process that hit the fault, meaningful only beside a nonzero code.
    pub fault_pid: c_uint,
    /// Each family's rate, indexed by the C `libtimewarp_family` enumeration.
    pub rate: [c_double; FAMILIES.len()],
    /// Each family's timebase, indexed the same way. Cpu tracks none, and its slot is 0.
    pub timebase: [i32; FAMILIES.len()],
    /// The hook library every process of the session loads, NUL-terminated, empty when it has none.
    pub hook_path: [u8; HOOK_PATH_MAX],
}

/// A transaction as C holds it: the transaction itself, and the session it borrows from.
///
/// `transaction` is declared first, so it is dropped first, which is what keeps its borrow of the
/// mapping inside the life of the `Arc` below it.
#[derive(Debug)]
pub struct Open {
    /// The change in flight, borrowing the header out of the mapping below.
    transaction: Transaction<'static>,
    /// What keeps that mapping alive, whether or not C still holds a session handle.
    #[expect(
        dead_code,
        reason = "held for its lifetime: dropping it unmaps what `transaction` borrows"
    )]
    session: Arc<Controller>,
}

impl Open {
    /// **W1** over a session C holds a handle to.
    fn begin(session: Arc<Controller>) -> Result<Self, Error> {
        // Safety: the `Controller` sits in an allocation the `Arc` owns and never moves out of, so
        // the borrow is good for as long as any clone of the `Arc` lives. This struct keeps one,
        // and drops the transaction before it.
        let owner: &'static Controller = unsafe { &*Arc::as_ptr(&session) };
        Ok(Self {
            transaction: owner.begin()?,
            session,
        })
    }
}

thread_local! {
    /// The message beside the code the last call on this thread answered with.
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

/// Leaves `text` in this thread's message slot.
fn set_message(text: &str) {
    // A message of ours carries no NUL; one that somehow did would be truncated by `CString`, so
    // the bytes are replaced rather than refused.
    let message = CString::new(text.replace('\0', " ")).unwrap_or_default();
    LAST_ERROR.with_borrow_mut(|slot| *slot = message);
}

/// What every entry point returns: the code, with the message left where the caller can read it.
fn answer(result: Result<(), Error>) -> c_int {
    match result {
        Ok(()) => {
            set_message("");
            Code::Ok as c_int
        }
        Err(err) => {
            set_message(&err.to_string());
            err.code() as c_int
        }
    }
}

/// The refusal a bad argument at this edge answers with.
fn refuse(what: &'static str) -> c_int {
    answer(Err(Error::BadArgument(what)))
}

/// The family a number names, or `None` when it names none.
fn family_of(tag: c_uint) -> Option<Family> {
    FAMILIES.get(usize::try_from(tag).ok()?).copied()
}

/// The session a handle names, or `None` when the handle is null.
///
/// # Safety
/// A non-null `handle` must be one [`libtimewarp_attach`] gave out and has not been closed.
const unsafe fn session_of<'a>(handle: *const Controller) -> Option<&'a Controller> {
    // Safety: the caller guarantees a live handle.
    unsafe { handle.as_ref() }
}

/// The message beside the code the last call on this thread answered with, NUL-terminated and
/// never null. Empty after a call that succeeded.
///
/// The string belongs to the library and stays valid until the next call on this thread.
#[unsafe(no_mangle)]
pub extern "C" fn libtimewarp_last_error() -> *const c_char {
    LAST_ERROR.with_borrow(|slot| slot.as_ptr())
}

/// Attaches by the path of the session file, applying the same security checks a target applies.
///
/// On success `*out` holds a session handle, which [`libtimewarp_close`] releases.
///
/// # Safety
/// `path` must be a NUL-terminated string and `out` must point at a writable handle slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libtimewarp_attach(
    path: *const c_char,
    out: *mut *const Controller,
) -> c_int {
    if path.is_null() || out.is_null() {
        return refuse(NULL_ARGUMENT);
    }
    // Safety: the caller guarantees a NUL-terminated string.
    let bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
    match Controller::attach(Path::new(OsStr::from_bytes(bytes))) {
        Ok(controller) => {
            // Safety: the caller guarantees a writable slot.
            unsafe { out.write(Arc::into_raw(Arc::new(controller))) };
            answer(Ok(()))
        }
        Err(err) => answer(Err(err)),
    }
}

/// Releases a session handle. A null handle is nothing to release.
///
/// A transaction open on the session keeps the mapping alive on its own, so closing here first is
/// allowed; the transaction still has to be committed or aborted.
///
/// # Safety
/// A non-null `handle` must be one [`libtimewarp_attach`] gave out, and must not be used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libtimewarp_close(handle: *const Controller) {
    if handle.is_null() {
        return;
    }
    // Safety: the caller guarantees a handle `Arc::into_raw` produced, handed back once.
    drop(unsafe { Arc::from_raw(handle) });
}

/// What the family shows now, in nanoseconds, written to `*out`.
///
/// # Safety
/// `handle` must be a live session handle and `out` must point at a writable `int64_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libtimewarp_get(
    handle: *const Controller,
    family: c_uint,
    out: *mut i64,
) -> c_int {
    // Safety: the caller guarantees a live handle.
    let Some(session) = (unsafe { session_of(handle) }) else {
        return refuse(NULL_ARGUMENT);
    };
    if out.is_null() {
        return refuse(NULL_ARGUMENT);
    }
    let Some(family) = family_of(family) else {
        return refuse(UNKNOWN_FAMILY);
    };
    match session.get(family) {
        Ok(showing) => {
            // Safety: the caller guarantees a writable slot.
            unsafe { out.write(showing.0) };
            answer(Ok(()))
        }
        Err(err) => answer(Err(err)),
    }
}

/// How the session was set up, what each family is doing, and what went wrong with it.
///
/// # Safety
/// `handle` must be a live session handle and `out` must point at a writable `libtimewarp_info`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libtimewarp_info(handle: *const Controller, out: *mut CInfo) -> c_int {
    // Safety: the caller guarantees a live handle.
    let Some(session) = (unsafe { session_of(handle) }) else {
        return refuse(NULL_ARGUMENT);
    };
    if out.is_null() {
        return refuse(NULL_ARGUMENT);
    }
    let info = session.info();
    let mut filled = CInfo {
        keeper: info.keeper as c_uint,
        emulate_vdso: c_uint::from(info.emulate_vdso),
        fail_closed: c_uint::from(info.fail_closed),
        creator_pid: info.creator_pid,
        creation_time: info.creation_time.0,
        fault_code: info.fault.map_or(0, |fault| fault.code as c_uint),
        fault_pid: info.fault.map_or(0, |fault| fault.pid),
        rate: [0.0; FAMILIES.len()],
        timebase: [0; FAMILIES.len()],
        hook_path: [0; HOOK_PATH_MAX],
    };
    for (index, family) in FAMILIES.into_iter().enumerate() {
        let line = info.family(family);
        if let Some(slot) = filled.rate.get_mut(index) {
            *slot = line.rate.as_f64();
        }
        if let Some(slot) = filled.timebase.get_mut(index) {
            *slot = line.timebase.map_or(0, common::platform::clock_id);
        }
    }
    if let Some(hook) = &info.hook {
        let bytes = hook.as_os_str().as_bytes();
        // One byte short of the field, so the copy always leaves room for the NUL after it.
        let room = bytes.len().min(HOOK_PATH_MAX - 1);
        if let (Some(field), Some(source)) = (filled.hook_path.get_mut(..room), bytes.get(..room)) {
            field.copy_from_slice(source);
        }
    }
    // Safety: the caller guarantees a writable struct.
    unsafe { out.write(filled) };
    answer(Ok(()))
}

/// **W1**: takes the write lock, and nothing else. On success `*out` holds a transaction handle,
/// which [`libtimewarp_commit`] or [`libtimewarp_abort`] spends.
///
/// # Safety
/// `handle` must be a live session handle and `out` must point at a writable handle slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libtimewarp_begin(
    handle: *const Controller,
    out: *mut *mut Open,
) -> c_int {
    if handle.is_null() || out.is_null() {
        return refuse(NULL_ARGUMENT);
    }
    // Safety: the caller guarantees a handle `Arc::into_raw` produced and still live, so the count
    // this raises is one the clone below accounts for.
    unsafe { Arc::increment_strong_count(handle) };
    // Safety: the count above is this reference.
    let session = unsafe { Arc::from_raw(handle) };
    match Open::begin(session) {
        Ok(open) => {
            // Safety: the caller guarantees a writable slot.
            unsafe { out.write(Box::into_raw(Box::new(open))) };
            answer(Ok(()))
        }
        Err(err) => answer(Err(err)),
    }
}

/// Stages a move of the family by a duration in nanoseconds. Nothing is written to the session.
///
/// # Safety
/// `handle` must be a transaction handle [`libtimewarp_begin`] gave out and has not spent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libtimewarp_shift(handle: *mut Open, family: c_uint, nanos: i64) -> c_int {
    // Safety: the caller guarantees a live handle.
    let Some(open) = (unsafe { handle.as_mut() }) else {
        return refuse(NULL_ARGUMENT);
    };
    let Some(family) = family_of(family) else {
        return refuse(UNKNOWN_FAMILY);
    };
    answer(open.transaction.shift(family, Nanos(nanos)))
}

/// Stages a value in nanoseconds for the family to show now. Nothing is written to the session.
///
/// # Safety
/// `handle` must be a transaction handle [`libtimewarp_begin`] gave out and has not spent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libtimewarp_set_time(
    handle: *mut Open,
    family: c_uint,
    nanos: i64,
) -> c_int {
    // Safety: the caller guarantees a live handle.
    let Some(open) = (unsafe { handle.as_mut() }) else {
        return refuse(NULL_ARGUMENT);
    };
    let Some(family) = family_of(family) else {
        return refuse(UNKNOWN_FAMILY);
    };
    answer(open.transaction.set_time(family, Nanos(nanos)))
}

/// Stages how fast the family runs: fake nanoseconds per nanosecond of its timebase, with 0
/// freezing it. Nothing is written to the session.
///
/// # Safety
/// `handle` must be a transaction handle [`libtimewarp_begin`] gave out and has not spent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libtimewarp_set_rate(
    handle: *mut Open,
    family: c_uint,
    rate: c_double,
) -> c_int {
    // Safety: the caller guarantees a live handle.
    let Some(open) = (unsafe { handle.as_mut() }) else {
        return refuse(NULL_ARGUMENT);
    };
    let Some(family) = family_of(family) else {
        return refuse(UNKNOWN_FAMILY);
    };
    answer(open.transaction.set_rate(family, rate))
}

/// **W2** through **W12**: the staged operations applied in order against one anchor, and published
/// once.
///
/// The handle is spent either way: a refused operation aborts the whole transaction with nothing
/// written, and releases the lock just the same.
///
/// # Safety
/// `handle` must be a transaction handle [`libtimewarp_begin`] gave out, and must not be used
/// again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libtimewarp_commit(handle: *mut Open) -> c_int {
    if handle.is_null() {
        return refuse(NULL_ARGUMENT);
    }
    // Safety: the caller guarantees a handle `Box::into_raw` produced, handed back once.
    let open = unsafe { Box::from_raw(handle) };
    answer(open.transaction.commit())
}

/// Discards the staged operations and releases the write lock, having written nothing. A null
/// handle is nothing to discard.
///
/// # Safety
/// A non-null `handle` must be a transaction handle [`libtimewarp_begin`] gave out, and must not be
/// used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libtimewarp_abort(handle: *mut Open) {
    if handle.is_null() {
        return;
    }
    // Safety: the caller guarantees a handle `Box::into_raw` produced, handed back once.
    drop(unsafe { Box::from_raw(handle) });
}

/// The keeper numbers the C enumeration spells, which are the header's own tags.
const _: () = {
    assert!(Keeper::Affine as u32 == 1);
    assert!(Keeper::Null as u32 == 2);
};

#[cfg(test)]
mod tests {
    use std::mem::offset_of;

    use super::*;

    #[test]
    fn the_family_numbers_are_the_family_tables_order() {
        assert_eq!(family_of(0), Some(Family::Realtime));
        assert_eq!(family_of(1), Some(Family::Monotonic));
        assert_eq!(family_of(2), Some(Family::Boottime));
        assert_eq!(family_of(3), Some(Family::Cpu));
        assert_eq!(family_of(4), None);
        assert_eq!(family_of(c_uint::MAX), None);
    }

    #[test]
    fn the_reported_struct_is_the_shape_the_header_declares() {
        assert_eq!(offset_of!(CInfo, keeper), 0);
        assert_eq!(offset_of!(CInfo, emulate_vdso), 4);
        assert_eq!(offset_of!(CInfo, fail_closed), 8);
        assert_eq!(offset_of!(CInfo, creator_pid), 12);
        assert_eq!(offset_of!(CInfo, creation_time), 16);
        assert_eq!(offset_of!(CInfo, fault_code), 24);
        assert_eq!(offset_of!(CInfo, fault_pid), 28);
        assert_eq!(offset_of!(CInfo, rate), 32);
        assert_eq!(offset_of!(CInfo, timebase), 64);
        assert_eq!(offset_of!(CInfo, hook_path), 80);
        assert_eq!(size_of::<CInfo>(), 80 + HOOK_PATH_MAX);
    }

    #[test]
    fn a_message_is_left_where_the_caller_can_read_it() {
        assert_eq!(answer(Ok(())), Code::Ok as c_int);
        // Safety: the pointer is this thread's own slot, NUL-terminated and never null.
        let empty = unsafe { CStr::from_ptr(libtimewarp_last_error()) };
        assert_eq!(empty.to_bytes(), b"");

        assert_eq!(refuse(NULL_ARGUMENT), Code::BadArgument as c_int);
        // Safety: as above.
        let message = unsafe { CStr::from_ptr(libtimewarp_last_error()) };
        assert_eq!(message.to_bytes(), NULL_ARGUMENT.as_bytes());
    }
}
