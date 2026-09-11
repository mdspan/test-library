//! Posing as the timekeep page, so a target that never calls libc still reads our clock.
//!
//! The kernel publishes a page of linear parameters and names it in the auxiliary vector, and a
//! target that skips libc extrapolates its clock from it. Unlike the vDSO the same target finds on
//! Linux, there is no function of ours to substitute: the page holds numbers and the target does
//! the arithmetic. So the session carries a page of its own, written from its own parameters, and
//! this points `AT_TIMEKEEP` at it.
//!
//! The page the session carries is [`common::session::timekeep`]; what lives here is the two things
//! that are this process's: remembering the kernel's own page before the entry naming it is
//! rewritten, and the rewrite itself.

use common::log::Level;
use common::session::layout::Header;
use common::session::timekeep;
use common::updater;

use crate::state;

use super::auxv;

/// Remembers where the kernel's own page is, and where this library lives.
///
/// Both have to be taken before anything else runs. The entry is rewritten below, and libc answers
/// it from a copy it takes the first time anybody asks, so the genuine address is only reliably
/// there beforehand. The library's path is the loader's to say, and this is the one place in the
/// process that can ask it.
pub(super) fn prime() {
    timekeep::counter::prime(auxv::genuine().unwrap_or(0));
    if let Some(path) = own_library() {
        updater::install_library(path);
    }
}

/// Points this process's auxiliary vector at the session's page, having made the page fresh first.
///
/// `None` when the session keeps no page, or when this process has no counter it could write one
/// against, or when there is no entry to point.
pub(super) fn install(header: &Header) -> Option<()> {
    let page = state::timekeep()?;
    let basis = timekeep::counter::basis()?;
    // Fresh before anybody follows the entry to it. An attacher may be the first process in a while
    // to touch a session whose updater died, and a page left stale would answer with the instant it
    // went stale at. A republish somebody else is making at this very instant is as good as ours,
    // so the entry is pointed at the page either way.
    timekeep::republish(header, page, basis);
    // The structure a reader follows sits at the front of the page, so the entry names its
    // first byte and what the republishers keep behind it stays out of sight.
    let at = std::ptr::from_ref(&page.timekeep).expose_provenance();
    auxv::point_at(at).then_some(())
}

/// Where this library's own file is, as the loader has it.
///
/// The updater is this library, exec'd by its own path or preloaded into a host program, so a
/// process that cannot answer this cannot start one. A target always can: it has us loaded.
fn own_library() -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;

    // Safety: `Dl_info` is plain data, so an all-zero value is a valid one.
    let mut found: libc::Dl_info = unsafe { std::mem::zeroed() };
    let here = std::ptr::from_ref(&AN_ADDRESS_IN_THIS_LIBRARY).cast::<libc::c_void>();
    // Safety: the address is a static of this library's own, and `found` is a live `Dl_info`.
    if unsafe { libc::dladdr(here, &raw mut found) } == 0 || found.dli_fname.is_null() {
        common::log!(
            Level::Info,
            "the loader will not say where this library is, so a session here keeps no updater"
        );
        return None;
    }
    // Safety: the loader answers with a NUL-terminated path that lives as long as the object does.
    let name = unsafe { std::ffi::CStr::from_ptr(found.dli_fname) };
    Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
        name.to_bytes(),
    )))
}

/// A byte of this library, for the loader to name the file it came out of.
///
/// A static rather than a function, because a function would have to be cast to a pointer and the
/// loader is being asked about the object rather than about the symbol.
static AN_ADDRESS_IN_THIS_LIBRARY: u8 = 0;
