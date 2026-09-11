//! Posing as the vDSO, so a target that never calls libc still reads our clock.
//!
//! The kernel maps a small shared object into every process and names it in the auxiliary vector,
//! and a runtime that would rather not pay for a system call finds the clock there instead of in
//! libc. The Go runtime is the one this library exists for. Interposing a libc symbol never sees
//! such a read, so we build an object of our own whose clock entries are ours, and point the
//! target's own `AT_SYSINFO_EHDR` at it.
//!
//! The session decides whether this happens: `LIBTIMEWARP_EMULATE_VDSO` is read when the session is
//! created and recorded in its header, and every process that attaches follows what it says. The
//! `vdso` build feature decides whether the machinery is here at all; a build without it says so
//! and leaves every other way of faking the clock alone.

#[cfg(not(feature = "vdso"))]
use common::config::ENV_EMULATE_VDSO;
use common::log::Level;
use common::session::layout::Header;

#[cfg(feature = "vdso")]
mod auxv;
#[cfg(feature = "vdso")]
mod elf;
#[cfg(feature = "vdso")]
mod entries;
#[cfg(feature = "vdso")]
mod image;

/// Finds what the object will be built from, before the program can move it.
///
/// Called from the constructor, ahead of everything that touches the environment: the auxiliary
/// vector is reached through `environ`, and a process that has replaced that pointer has taken the
/// only way to it away.
#[cfg(feature = "vdso")]
pub(crate) fn prime() {
    auxv::prime();
}

/// Nothing to find in a build that cannot pose as anything.
#[cfg(not(feature = "vdso"))]
pub(crate) const fn prime() {}

/// Points this process's auxiliary vector at an object of ours, when the session says to.
///
/// Called once the session is installed, so a target that finds the entry and calls straight into
/// it reads the session rather than genuine time.
#[cfg(feature = "vdso")]
pub(crate) fn install(header: &Header) {
    if header.emulate_vdso != 1 {
        return;
    }
    if pose().is_none() {
        common::log!(
            Level::Info,
            "no vDSO to pose as in this process, so a target that reads the clock without libc \
             reads genuine time"
        );
    }
}

/// Builds the object and points the vector at it. `None` when this process has no vDSO to pose as,
/// or no page to build one in.
#[cfg(feature = "vdso")]
fn pose() -> Option<()> {
    let base = image::build(auxv::genuine_vdso()?)?;
    auxv::point_at(base).then_some(())
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
