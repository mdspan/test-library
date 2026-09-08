//! The platform seam: how the portable bodies in [`crate::interpose`] reach a program.
//!
//! `LD_PRELOAD` makes an ordinary export the whole mechanism; dyld needs a `__DATA,__interpose`
//! row per replacement and the libc name kept unexported. Only Linux is filled in.

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        mod linux;
    } else if #[cfg(target_os = "freebsd")] {
        mod freebsd;
    } else if #[cfg(target_os = "macos")] {
        mod macos;
    } else {
        compile_error!(
            "libtimewarp supports Linux, macOS, and FreeBSD. Add an arm under agent/src/platform \
             exporting the entry points; see the note in platform/mod.rs."
        );
    }
}
