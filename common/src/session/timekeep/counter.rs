//! The hardware counter the timekeep page is written against.
//!
//! A reader of the page does the arithmetic itself: it takes the counter, subtracts the snapshot
//! the page carries, masks the difference, and multiplies by the page's scale. So a page of ours is
//! only right if we can take the very same counter the reader will take, and know how fast it runs.
//!
//! Both facts come from the kernel's own page wherever we can read its counter: the page says which
//! counter it is, how it is read, how wide it is, and — in its scale — how many genuine seconds a
//! tick is worth. Copying them makes our page exact by construction. Where the kernel picked a
//! counter we cannot take ourselves, we fall back to the one this architecture always has and ask
//! the system how fast it runs.

use std::sync::OnceLock;

use super::{ALGO_NATIVE, Timekeep};

/// What a reader of the page computes from: which counter, how to read it, how wide it is, and how
/// fast it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Basis {
    /// The algorithm word the page carries, which is how a reader knows which counter to take.
    pub algo: u32,
    /// The machine-dependent word beside it: how far right the counter is shifted on `x86_64`, and
    /// whether the physical rather than the virtual counter is meant on aarch64.
    pub flavour: u32,
    /// Which bits of a difference of two readings count.
    pub mask: u32,
    /// Genuine seconds per tick as a 64-bit binary fraction, which is 2^64 over the counter's
    /// frequency. The page's own scale is this times the session's rate.
    pub scale: u64,
}

impl Basis {
    /// The counter as it stands, taken the way a reader of the page takes it.
    #[must_use]
    pub fn read(self) -> u32 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a reader of the page keeps the low 32 bits of the counter and so must we"
        )]
        let counted = raw(self.flavour) as u32;
        counted & self.mask
    }

    /// The basis the kernel's own page at `genuine` is written against, or `None` when there is no
    /// page there, it is not one we understand, or it names a counter we cannot take ourselves.
    ///
    /// # Safety
    /// `genuine` must be the address the auxiliary vector carries for the kernel's timekeep page,
    /// or zero.
    #[must_use]
    unsafe fn of_kernel(genuine: usize) -> Option<Self> {
        if genuine == 0 || !CAN_READ_COUNTER {
            return None;
        }
        // Safety: the caller guarantees the address, where the kernel maps a page of this shape for
        // the life of the process.
        let page = unsafe { &*std::ptr::with_exposed_provenance::<Timekeep>(genuine) };
        let hands = page.load()?;
        (hands.algo == ALGO_NATIVE && hands.scale != 0 && hands.counter_mask != 0).then_some(Self {
            algo: hands.algo,
            flavour: hands.flavour,
            mask: hands.counter_mask,
            scale: hands.scale,
        })
    }

    /// The counter this architecture always has, with the frequency the system reports for it, or
    /// `None` where neither is available.
    #[must_use]
    fn native() -> Option<Self> {
        let frequency = frequency().filter(|&hertz| hertz >= LEAST_FREQUENCY)?;
        Some(Self {
            algo: ALGO_NATIVE,
            flavour: 0,
            mask: u32::MAX,
            // 2^64 over the frequency, which is the unit the page's scale is written in. A
            // frequency of one would be the only divisor this could not hold, and the guard below
            // it rules that out along with every other implausible answer.
            scale: u64::try_from((1u128 << u64::BITS) / u128::from(frequency)).ok()?,
        })
    }
}

/// The lowest counter frequency worth believing: a hertz below this leaves a tick too coarse to
/// carry a clock, and a one-hertz counter would not even fit the scale.
const LEAST_FREQUENCY: u64 = 1_000_000;

/// What this process publishes against, settled once.
static BASIS: OnceLock<Option<Basis>> = OnceLock::new();

/// Remembers the kernel's own page, before anything of ours can move the entry that names it.
///
/// A target rewrites its own `AT_TIMEKEEP`, and libc answers that entry from a copy it takes the
/// first time anybody asks, so the genuine address has to be taken and kept before the rewrite. The
/// agent's constructor does that and passes it here.
pub fn prime(genuine: usize) {
    // Safety: the caller took this address out of the auxiliary vector, which is what names the
    // kernel's own page.
    // A second call finds the answer already there, which is what a fork after the first leaves.
    let _settled = BASIS.set(unsafe { settle(genuine) });
}

/// The basis this process publishes against, or `None` when this process cannot write a page a
/// reader could use.
///
/// A process that never primed one — a controller outside a faked tree — still has an untouched
/// entry of its own, so the answer is found here instead.
#[must_use]
pub fn basis() -> Option<Basis> {
    *BASIS.get_or_init(|| {
        // Safety: the entry is this process's own and nothing of ours has rewritten it, since a
        // process that rewrites one primes this first.
        unsafe { settle(genuine_page()) }
    })
}

/// The kernel's page where we can use it, and the architecture's own counter where we cannot.
///
/// # Safety
/// `genuine` must be the address the auxiliary vector carries for the kernel's timekeep page, or
/// zero.
unsafe fn settle(genuine: usize) -> Option<Basis> {
    // Safety: the caller guarantees the address.
    unsafe { Basis::of_kernel(genuine) }.or_else(Basis::native)
}

/// Where the kernel mapped its own timekeep page, through the reader this platform's libc gives.
#[cfg(target_os = "freebsd")]
#[must_use]
fn genuine_page() -> usize {
    let mut at: *mut libc::c_void = std::ptr::null_mut();
    let len = libc::c_int::try_from(size_of::<*mut libc::c_void>()).unwrap_or(0);
    // Safety: the buffer is a live pointer-sized word, which is what this entry answers in.
    let asked =
        unsafe { libc::elf_aux_info(crate::platform::AT_TIMEKEEP, (&raw mut at).cast(), len) };
    if asked == 0 { at.addr() } else { 0 }
}

/// No other platform publishes such a page, so there is none to find.
#[cfg(not(target_os = "freebsd"))]
#[must_use]
const fn genuine_page() -> usize {
    0
}

/// Whether this build can take the counter a kernel page would name at all.
const CAN_READ_COUNTER: bool = cfg!(any(target_arch = "x86_64", target_arch = "aarch64"));

/// The counter, unmasked, as the flavour word says to take it.
#[cfg(target_arch = "x86_64")]
#[inline]
fn raw(flavour: u32) -> u64 {
    let (low, high): (u32, u32);
    // Safety: both are plain instructions every x86_64 has, taking nothing and touching no memory.
    // The fence is the one a reader of the page executes before its own `rdtsc`, so the two samples
    // are taken the same way. `rdtsc` answers in `eax` and `edx` and clobbers neither flags nor
    // memory.
    unsafe {
        std::arch::asm!(
            "lfence",
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    let counted = (u64::from(high) << u32::BITS) | u64::from(low);
    // The shift is what makes a fast counter fit the 32 bits the page carries, and a reader applies
    // it before truncating. Masked to the register's width, because a wider one shifts nothing out.
    counted >> (flavour & (u64::BITS - 1))
}

/// The same, for the generic timer both of whose counters read in one instruction.
#[cfg(target_arch = "aarch64")]
#[inline]
fn raw(flavour: u32) -> u64 {
    let counted: u64;
    if flavour == 0 {
        // Safety: `cntvct_el0` is readable from user mode and the barrier before it is what orders
        // the read, which is exactly the pair a reader of the page executes.
        unsafe {
            std::arch::asm!(
                "isb",
                "mrs {counted}, cntvct_el0",
                counted = out(reg) counted,
                options(nomem, nostack),
            );
        }
    } else {
        // Safety: as above, for the physical counter the page's flavour word may name instead.
        unsafe {
            std::arch::asm!(
                "isb",
                "mrs {counted}, cntpct_el0",
                counted = out(reg) counted,
                options(nomem, nostack),
            );
        }
    }
    counted
}

/// Nothing to take on an architecture we have no counter for, which is also an architecture
/// [`Basis::native`] answers `None` for, so this is never reached.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline]
const fn raw(_flavour: u32) -> u64 {
    0
}

/// How many times a second the counter [`raw`] takes advances, or `None` when the system will not
/// say.
#[cfg(all(target_arch = "x86_64", target_os = "freebsd"))]
fn frequency() -> Option<u64> {
    let mut hertz: u64 = 0;
    let mut len = size_of::<u64>();
    // Safety: the name is NUL-terminated and `hertz` with `len` is a live word of the type this
    // sysctl answers in.
    let asked = unsafe {
        libc::sysctlbyname(
            c"machdep.tsc_freq".as_ptr(),
            (&raw mut hertz).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (asked == 0).then_some(hertz)
}

/// The page is FreeBSD's mechanism, so no other system is asked how fast its counter runs.
#[cfg(all(target_arch = "x86_64", not(target_os = "freebsd")))]
const fn frequency() -> Option<u64> {
    None
}

/// The same, from the register the architecture publishes it in, which every aarch64 has.
#[cfg(target_arch = "aarch64")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "one shape for every architecture, and the others have nowhere to read a frequency \
              from"
)]
fn frequency() -> Option<u64> {
    let hertz: u64;
    // Safety: `cntfrq_el0` is readable from user mode and holds a constant of the running system.
    unsafe {
        std::arch::asm!(
            "mrs {hertz}, cntfrq_el0",
            hertz = out(reg) hertz,
            options(nomem, nostack, preserves_flags),
        );
    }
    Some(hertz)
}

/// No counter, so no frequency for one.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const fn frequency() -> Option<u64> {
    None
}

#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
mod tests {
    use super::*;

    /// A basis over the counter this machine has, at a made-up frequency, so the arithmetic above
    /// can be exercised wherever the suite runs rather than on FreeBSD alone.
    const fn made_up() -> Basis {
        Basis {
            algo: ALGO_NATIVE,
            flavour: 0,
            mask: u32::MAX,
            scale: (1u64 << 40),
        }
    }

    #[test]
    fn the_counter_advances_and_stays_inside_its_mask() {
        let basis = made_up();
        let first = basis.read();
        let mut moved = false;
        for _ in 0..1_000_000 {
            if basis.read() != first {
                moved = true;
                break;
            }
        }
        assert!(moved, "the counter never moved");

        // A narrower mask is the one a page may carry, and a reading must fit it.
        let narrow = Basis {
            mask: 0xffff,
            ..basis
        };
        assert!(narrow.read() <= 0xffff);
    }

    /// The shift a page carries is applied before the reading is truncated, so a shifted counter
    /// runs slower than an unshifted one over the same span.
    #[test]
    fn the_flavour_word_shifts_the_counter_on_x86() {
        if !cfg!(target_arch = "x86_64") {
            return;
        }
        let plain = Basis {
            flavour: 0,
            ..made_up()
        };
        let shifted = Basis {
            flavour: 8,
            ..made_up()
        };
        let (before_plain, before_shifted) = (plain.read(), shifted.read());
        for _ in 0..100_000 {
            std::hint::spin_loop();
        }
        let moved_plain = plain.read().wrapping_sub(before_plain);
        let moved_shifted = shifted.read().wrapping_sub(before_shifted);
        assert!(
            moved_plain >= moved_shifted,
            "the shifted counter moved further: {moved_plain} against {moved_shifted}",
        );
    }
}
