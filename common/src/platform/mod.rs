//! The platform seam.
//!
//! Everything above this module is portable. Each arm supplies the same items:
//!
//! | Item | Contract |
//! |---|---|
//! | `OS_TAG` | The value the session header's platform field carries for this OS. |
//! | `clock` | The clock ids of the families, by the names the family table gives them. |
//! | `genuine_clock_gettime` | Reads a genuine clock into the caller's `timespec`, libc's convention. Async-signal-safe, and never routed through a symbol we interpose. |
//! | `random_bytes` | Fills a buffer with randomness, for the session name. |

use crate::time::Nanos;

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        mod linux;
        pub use linux::*;
    } else if #[cfg(target_os = "freebsd")] {
        mod freebsd;
        pub use freebsd::*;
    } else if #[cfg(target_os = "macos")] {
        mod macos;
        pub use macos::*;
    } else {
        compile_error!(
            "libtimewarp supports Linux, macOS, and FreeBSD. Add an arm under common/src/platform \
             supplying the items in the table in platform/mod.rs."
        );
    }
}

/// The architecture half of the header's platform tag.
pub const ARCH_TAG: u32 = if cfg!(target_arch = "x86_64") {
    1
} else if cfg!(target_arch = "aarch64") {
    2
} else {
    0
};

/// What the header's platform field carries: which OS's clock ids the parameters speak, and which
/// architecture wrote them.
pub const PLATFORM_TAG: u32 = (OS_TAG << 16) | ARCH_TAG;

/// Reads a genuine clock. `None` only when the kernel refuses the id.
#[must_use]
pub fn genuine_nanos(clock: libc::clockid_t) -> Option<Nanos> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Safety: `ts` is a live, correctly typed buffer.
    if unsafe { genuine_clock_gettime(clock, &raw mut ts) } != 0 {
        return None;
    }
    Some(Nanos::from_timespec(&ts))
}

// Only Linux has a filled-in arm to read.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn the_genuine_clocks_read() {
        assert!(genuine_nanos(clock::REALTIME).unwrap() > Nanos(1_700_000_000 * Nanos::SECOND));
        let monotonic = genuine_nanos(clock::MONOTONIC).unwrap();
        assert!(monotonic >= Nanos::ZERO);
        assert!(genuine_nanos(clock::BOOTTIME).unwrap() >= monotonic);
    }

    #[test]
    fn an_unknown_clock_id_fails_rather_than_lying() {
        assert_eq!(genuine_nanos(0x7fff_ffff), None);
    }

    #[test]
    fn randomness_is_not_a_constant() {
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        random_bytes(&mut a).unwrap();
        random_bytes(&mut b).unwrap();
        assert_ne!(a, b);
    }
}
