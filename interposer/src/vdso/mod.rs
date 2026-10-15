//! Posing as whatever a target reads the clock from when it will not call libc.
//!
//! Two platforms hand such a target the clock without libc, each its own way. Linux maps a small
//! shared object, the vDSO, and names it in the auxiliary vector; a runtime that skips the system
//! call reads the clock from there, so we build a vDSO of our own and point the target's entry at
//! it. FreeBSD's Go runtime reads a page of kernel parameters the auxiliary vector names, and when
//! that page is absent falls back to two leaf routines in its own text that make the system call
//! directly; so we withhold the page entry and interpose those routines, which steers the runtime
//! through the library's own read path. The Go runtime is the one this library exists for, and
//! interposing a libc symbol never sees either read.
//!
//! The session decides whether this happens: `LIBTIMEWARP_EMULATE_VDSO` is read when the session is
//! created and recorded in its header, and every process that attaches follows what it says. The
//! `vdso` build feature decides whether the machinery is here at all; a build without it says so
//! and leaves every other way of faking the clock alone.

#[cfg(not(feature = "vdso"))]
use common::config::ENV_EMULATE_VDSO;
use common::log::Level;
use common::session::layout::Header;

// The auxiliary vector is the one piece both mechanisms share: the entry each platform names the
// clock's source in, which the constructor found before anything could move the environment the
// vector sits behind.
#[cfg(feature = "vdso")]
use crate::auxv;

#[cfg(all(feature = "vdso", target_os = "linux"))]
mod entries;
#[cfg(all(feature = "vdso", target_os = "linux"))]
mod image;

/// Puts something of ours where a target that skips libc reads the clock, when the session says
/// to.
///
/// Called once the session is installed, so a target that reads the clock without libc reads the
/// session rather than genuine time.
#[cfg(feature = "vdso")]
pub(crate) fn install(header: &Header) {
    if header.emulate_vdso != 1 {
        return;
    }
    match pose(header) {
        Ok(()) => {}
        Err(Miss::Nothing) => common::log!(
            Level::Info,
            "nothing to pose as in this process, so a target that reads the clock without libc \
             reads genuine time"
        ),
        #[cfg(target_os = "freebsd")]
        Err(Miss::NotGo) => common::log!(
            Level::Debug,
            "Go did not build this program, so it has no clock fallback routines to interpose"
        ),
        #[cfg(target_os = "freebsd")]
        Err(Miss::UnrecognisedGo(release)) => common::log!(
            Level::Warn,
            "this program's Go runtime, built by {release}, has clock fallback routines in no \
             encoding this library matches, so it reads genuine time"
        ),
    }
}

/// Why nothing of ours stands where a target that skips libc reads the clock.
#[cfg(feature = "vdso")]
enum Miss {
    /// Nothing in this process to pose as, or no means to: no vDSO on Linux, and on FreeBSD no
    /// memory or permission for the rewrite.
    Nothing,
    /// A program Go did not build, which has no clock fallback routines.
    #[cfg(target_os = "freebsd")]
    NotGo,
    /// A Go program whose clock fallback routines are in no encoding the library matches, and the
    /// release that built it.
    #[cfg(target_os = "freebsd")]
    UnrecognisedGo(crate::go_locate::GoRelease<'static>),
}

/// Builds the object and points the vector at it. An error when this process has no vDSO to pose
/// as, or no page to build one in.
#[cfg(all(feature = "vdso", target_os = "linux"))]
fn pose(_header: &Header) -> Result<(), Miss> {
    let base = auxv::genuine()
        .and_then(image::build)
        .ok_or(Miss::Nothing)?;
    if auxv::point_at(base) {
        Ok(())
    } else {
        Err(Miss::Nothing)
    }
}

/// Interposes the Go runtime's clock fallback routines and withholds the timekeep entry, so a Go
/// target reaches the session's read path through the routines rather than the kernel's page.
///
/// All-or-nothing: the scratch-stack arena is mapped, both routines are recognised by their bytes
/// and their entries remapped to our shims, and only on that full success is `AT_TIMEKEEP` zeroed.
/// An error when any step does not hold, in which case
/// [`crate::go_locate::freebsd::interpose`] has already put back whatever bytes it wrote and the
/// target reads genuine time.
#[cfg(all(feature = "vdso", target_os = "freebsd"))]
fn pose(_header: &Header) -> Result<(), Miss> {
    use common::elf::MainImage;

    use crate::go_locate::freebsd::{self, Refusal};

    if !crate::go_shim::install_arena() {
        return Err(Miss::Nothing);
    }
    let (phdr, phent, phnum) = auxv::program_headers().ok_or(Miss::Nothing)?;
    // Safety: the three values are this process's own, from the auxiliary vector.
    let image = unsafe { MainImage::locate(phdr, phent, phnum) }.ok_or(Miss::Nothing)?;
    // Safety: the ranges are this process's own mapped code, and the handlers are this library's
    // shims, which is exactly what `interpose` is to point the routines at.
    let interposed = unsafe {
        freebsd::interpose(
            &image,
            crate::go_shim::nanotime_handler(),
            crate::go_shim::walltime_handler(),
        )
    };
    match interposed {
        Ok(()) => {}
        // Routines in no encoding the library matches may still be Go's, built by a release that
        // encodes them some other way, which the build information tells apart.
        Err(Refusal::Unrecognised) => {
            // Safety: the writable ranges are this process's own data, read from the constructor
            // before the program has run a line that could write them.
            let release = unsafe { freebsd::go_release(&image) };
            return Err(release.map_or(Miss::NotGo, Miss::UnrecognisedGo));
        }
        Err(Refusal::Rewrite) => return Err(Miss::Nothing),
    }
    // Last, and only now the routines jump to our shims: a Go runtime that finds no page falls back
    // to them, so withholding the entry is what sends it there.
    if auxv::withhold_timekeep() {
        Ok(())
    } else {
        Err(Miss::Nothing)
    }
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
