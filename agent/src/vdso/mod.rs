//! Posing as whatever a target reads the clock from when it will not call libc.
//!
//! Two platforms hand such a target something the kernel published and named in the auxiliary
//! vector: Linux a small shared object, the vDSO, and FreeBSD a page of linear parameters, the
//! timekeep page. A runtime that would rather not pay for a system call finds the clock there
//! instead of in libc, and the Go runtime is the one this library exists for. Interposing a libc
//! symbol never sees such a read, so we build one of our own and point the target's own entry at
//! it: an object whose clock entries are ours on Linux, and a page written from the session's
//! parameters on FreeBSD.
//!
//! The session decides whether this happens: `LIBTIMEWARP_EMULATE_VDSO` is read when the session is
//! created and recorded in its header, and every process that attaches follows what it says. The
//! `vdso` build feature decides whether the machinery is here at all; a build without it says so
//! and leaves every other way of faking the clock alone.

#[cfg(not(feature = "vdso"))]
use common::config::ENV_EMULATE_VDSO;
use common::log::Level;
use common::session::layout::Header;

// The auxiliary vector is the one piece both mechanisms share: the same walk, past the same
// environment terminator, for a tag each platform spells differently.
#[cfg(feature = "vdso")]
mod auxv;

#[cfg(all(feature = "vdso", target_os = "linux"))]
mod elf;
#[cfg(all(feature = "vdso", target_os = "linux"))]
mod entries;
#[cfg(all(feature = "vdso", target_os = "linux"))]
mod image;

#[cfg(all(feature = "vdso", target_os = "freebsd"))]
mod page;

/// Finds what the object will be built from, before the program can move it.
///
/// Called from the constructor, ahead of everything that touches the environment: the auxiliary
/// vector is reached through `environ`, and a process that has replaced that pointer has taken the
/// only way to it away. On FreeBSD it also settles which hardware counter this process publishes
/// against, which the kernel's own page is the exact answer for and which the rewrite below would
/// otherwise put out of reach.
#[cfg(feature = "vdso")]
pub(crate) fn prime() {
    auxv::prime();
    #[cfg(target_os = "freebsd")]
    page::prime();
}

/// Nothing to find in a build that cannot pose as anything.
#[cfg(not(feature = "vdso"))]
pub(crate) const fn prime() {}

/// Points this process's auxiliary vector at something of ours, when the session says to.
///
/// Called once the session is installed, so a target that finds the entry and reads straight from
/// it reads the session rather than genuine time.
#[cfg(feature = "vdso")]
pub(crate) fn install(header: &Header) {
    if header.emulate_vdso != 1 {
        return;
    }
    if pose(header).is_none() {
        common::log!(
            Level::Info,
            "nothing to pose as in this process, so a target that reads the clock without libc \
             reads genuine time"
        );
    }
}

/// Builds the object and points the vector at it. `None` when this process has no vDSO to pose as,
/// or no page to build one in.
#[cfg(all(feature = "vdso", target_os = "linux"))]
fn pose(_header: &Header) -> Option<()> {
    let base = image::build(auxv::genuine()?)?;
    auxv::point_at(base).then_some(())
}

/// Republishes the session's timekeep page and points the vector at it. `None` when the session
/// keeps no page, or when this process cannot write one a reader would follow.
#[cfg(all(feature = "vdso", target_os = "freebsd"))]
fn pose(header: &Header) -> Option<()> {
    page::install(header)
}

/// Says that the variable the session recorded asks for something this build cannot do.
///
/// One line, and only where the session asked: everything else about the session works as it
/// always does, and a target that reads the clock through libc is faked as usual.
#[cfg(not(feature = "vdso"))]
pub(crate) fn install(header: &Header) {
    if header.emulate_vdso == 1 {
        common::log!(
            Level::Info,
            "{ENV_EMULATE_VDSO} is ignored: this build was made without the vdso feature, so a \
             target that reads the clock without libc reads genuine time"
        );
    }
}
