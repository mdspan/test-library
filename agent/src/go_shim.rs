//! The entry shims a patched Go fallback clock routine lands on, and the per-thread scratch stacks
//! they run the read path from.
//!
//! A Go runtime that reads the clock without a timekeep page calls `runtime.fallback_nanotime` and
//! `runtime.fallback_walltime`. Those run on a goroutine stack with only the few words they reserve
//! for themselves, far too little for the read path. So the entry of each is rewritten to jump
//! here, into a shim written in assembly that switches to a scratch stack of ours before it calls
//! any Rust: no Rust runs on the goroutine stack, and the whole entry allocates nothing and takes
//! no lock, so it is safe wherever Go makes a clock read, a signal handler included.
//!
//! The scratch stacks are a fixed arena mapped at install. A thread is keyed by its thread pointer,
//! read in one instruction, and hashed to one slot. A per-slot word is a try-lock claimed with an
//! atomic swap: a nested read, which a `SIGPROF` sample taken inside an in-flight read is, finds
//! its slot already claimed and takes a bounded genuine `clock_gettime` in the assembly instead,
//! the one place a faked Go program here observes genuine time. The slot protocol the swap carries
//! out is modelled and checked under `loom` in the tests; the assembly mirrors that model.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use common::hook::Func;
use common::platform::clock;
use common::time::Nanos;

use crate::interpose;

/// How many scratch stacks the arena holds, which is how many threads can be inside the read path
/// at once before the rest take the genuine path. A power of two, so the thread key picks one with
/// a shift rather than a division.
const SLOTS: usize = 256;

/// How long one scratch stack is, the guard page at its low end included. Ample for the read path,
/// and a power of two so the shim reaches a stack's top with a shift.
const STACK_SIZE: usize = 64 * 1024;

/// `log2(STACK_SIZE)`: what the shim shifts a slot index by to reach that stack's top.
const STACK_SHIFT: u32 = 16;

/// `64 - log2(SLOTS)`: what the shim shifts the mixed thread key right by to land in `0..SLOTS`.
const SLOT_SHIFT: u32 = 56;

/// The multiplier the shim mixes the thread key with, Fibonacci hashing, the same constant the
/// hook gate keys its table with.
const PHI: u64 = 0x9E37_79B9_7F4A_7C15;

const _: () = assert!(1 << STACK_SHIFT == STACK_SIZE);
const _: () = assert!(1usize << (64 - SLOT_SHIFT) == SLOTS);

/// The `clock_gettime` system call number the in-assembly genuine path makes, which differs by
/// platform and, on Linux, by architecture. `CLOCK_MONOTONIC` differs by platform too.
#[cfg(target_os = "freebsd")]
const SYS_CLOCK_GETTIME: i32 = 232;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SYS_CLOCK_GETTIME: i32 = 228;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const SYS_CLOCK_GETTIME: i32 = 113;

/// The `CLOCK_MONOTONIC` id the in-assembly genuine nanotime path reads.
#[cfg(target_os = "freebsd")]
const GENUINE_MONOTONIC: i32 = 4;
#[cfg(target_os = "linux")]
const GENUINE_MONOTONIC: i32 = 1;

/// The flag naming a fresh anonymous mapping, spelled one way on Linux and another on the BSDs.
#[cfg(target_os = "linux")]
const MAP_ANON: libc::c_int = libc::MAP_ANONYMOUS;
#[cfg(target_os = "freebsd")]
const MAP_ANON: libc::c_int = libc::MAP_ANON;

/// Where the scratch-stack arena is, or zero before [`install_arena`] has mapped it. A shim that
/// reads zero here takes the genuine path.
static ARENA_BASE: AtomicUsize = AtomicUsize::new(0);

/// One try-lock word per slot. A shim swaps a one in with an atomic exchange; seeing a zero means
/// the slot was free and the read path may run, and seeing a one means it was taken and the genuine
/// path runs instead. The release stores a zero back.
static BUSY: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];

/// The faked monotonic reading written where a patched `runtime.fallback_nanotime` would have
/// written its own: the `int64` result slot the shim passes in `out`.
///
/// Always writes, so a result slot is never left as the caller found it: the read path answers a
/// faked reading where the session fakes Monotonic and a genuine one otherwise, and on the reading
/// no clock will answer, which the kernel refusing `CLOCK_MONOTONIC` would be, zero stands in.
///
/// # Safety
/// `out` must be valid for a write of an `i64`: the Go caller's nanotime result slot.
unsafe extern "C" fn tw_go_fallback_nanotime(out: *mut i64) {
    let now = interpose::reading(Func::ClockGettime, clock::MONOTONIC).unwrap_or(Nanos::ZERO);
    // Safety: the caller guarantees `out`.
    unsafe { *out = now.0 };
}

/// The faked realtime reading split into the `sec`/`nsec` slots a patched
/// `runtime.fallback_walltime` would have filled. Always writes both, for the reason
/// [`tw_go_fallback_nanotime`] always writes.
///
/// # Safety
/// `seconds` and `nanos` must be valid for writes of an `i64` and an `i32`: the Go caller's
/// walltime result slots.
unsafe extern "C" fn tw_go_fallback_walltime(seconds: *mut i64, nanos: *mut i32) {
    let ts = interpose::reading(Func::Gettimeofday, clock::REALTIME)
        .unwrap_or(Nanos::ZERO)
        .to_timespec();
    // Safety: the caller guarantees `seconds`.
    unsafe { *seconds = ts.tv_sec };
    // Safety: the caller guarantees `nanos`.
    unsafe { *nanos = i32::try_from(ts.tv_nsec).unwrap_or(0) };
}

unsafe extern "C" {
    /// The assembly shim a patched `runtime.fallback_nanotime` jumps to.
    fn tw_go_fallback_nanotime_shim();
    /// The assembly shim a patched `runtime.fallback_walltime` jumps to.
    fn tw_go_fallback_walltime_shim();
}

/// The address a patched `runtime.fallback_nanotime` is pointed at.
#[expect(
    clippy::fn_to_numeric_cast_any,
    reason = "the rewrite writes this code's address into the target's text, which is what this \
              turns the shim into"
)]
pub(crate) fn nanotime_handler() -> usize {
    (tw_go_fallback_nanotime_shim as *const ()).expose_provenance()
}

/// The address a patched `runtime.fallback_walltime` is pointed at.
#[expect(
    clippy::fn_to_numeric_cast_any,
    reason = "the rewrite writes this code's address into the target's text, which is what this \
              turns the shim into"
)]
pub(crate) fn walltime_handler() -> usize {
    (tw_go_fallback_walltime_shim as *const ()).expose_provenance()
}

/// Maps the scratch-stack arena this process's shims switch onto and publishes its base.
///
/// [`SLOTS`] stacks of [`STACK_SIZE`] each, a guard page at the low end of every one so an overflow
/// faults rather than reaching a neighbour. `false` when the kernel gave no memory, in which case
/// every shim takes the in-assembly genuine path. Idempotent and safe to race: the first mapping to
/// publish wins and any other is given back to the kernel.
pub(crate) fn install_arena() -> bool {
    if ARENA_BASE.load(Ordering::Acquire) != 0 {
        return true;
    }
    let Some(base) = map_arena() else {
        return false;
    };
    if ARENA_BASE
        .compare_exchange(0, base, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // Safety: the mapping is this process's own and no shim has been pointed at it.
        unsafe { libc::munmap(std::ptr::with_exposed_provenance_mut(base), arena_len()) };
    }
    true
}

/// How long the whole arena is.
const fn arena_len() -> usize {
    SLOTS * STACK_SIZE
}

/// Maps the arena and sets a guard page at the low end of every slot, or `None` when the kernel
/// refused the mapping or a guard.
fn map_arena() -> Option<usize> {
    let len = arena_len();
    // Safety: a fresh anonymous mapping asks the kernel for memory and touches nothing of ours.
    let mapped = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | MAP_ANON,
            -1,
            0,
        )
    };
    if std::ptr::eq(mapped, libc::MAP_FAILED) {
        return None;
    }
    let base = mapped.expose_provenance();
    let page = page_size();
    for slot in 0..SLOTS {
        let guard = base.wrapping_add(slot * STACK_SIZE);
        // Safety: the page is at the low end of a slot inside the mapping just made.
        let protected = unsafe {
            libc::mprotect(
                std::ptr::with_exposed_provenance_mut(guard),
                page,
                libc::PROT_NONE,
            )
        };
        if protected != 0 {
            // Safety: the mapping is this process's own and nothing points at it yet.
            unsafe { libc::munmap(mapped, len) };
            return None;
        }
    }
    Some(base)
}

/// How long this system's pages are.
fn page_size() -> usize {
    // Safety: `sysconf` takes a name and answers a number, touching nothing of ours.
    let answer = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(answer).unwrap_or(4096).max(4096)
}

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".globl tw_go_fallback_nanotime_shim",
    "tw_go_fallback_nanotime_shim:",
    // Thread key -> slot index in rcx.
    "mov   rax, qword ptr fs:[0]",
    "mov   rcx, rax",
    "mov   r11, {phi}",
    "imul  rcx, r11",
    "shr   rcx, {slot_shift}",
    // Try-lock the slot with an atomic exchange; a non-zero old value means it was taken.
    "lea   r10, [rip + {busy}]",
    "mov   eax, 1",
    "xchg  rax, qword ptr [r10 + rcx*8]",
    "test  rax, rax",
    "jnz   .Lnano_genuine",
    // The arena, or the genuine path when there is none.
    "mov   rsi, qword ptr [rip + {arena}]",
    "test  rsi, rsi",
    "jz    .Lnano_release",
    // Scratch top = arena + (idx+1) * STACK_SIZE, 16-aligned.
    "mov   r8, rcx",
    "inc   r8",
    "shl   r8, {stack_shift}",
    "lea   r9, [rsi + r8]",
    "and   r9, -16",
    "mov   qword ptr [r9 - 8], rsp",
    "mov   qword ptr [r9 - 16], rcx",
    "lea   rdi, [rsp + 8]",
    "lea   rsp, [r9 - 32]",
    "call  {nanotime}",
    "mov   rcx, qword ptr [rsp + 16]",
    "mov   rsp, qword ptr [rsp + 24]",
    "lea   r10, [rip + {busy}]",
    "mov   qword ptr [r10 + rcx*8], 0",
    "ret",
    // Reached holding the slot but with no arena: release, then fall through.
    ".Lnano_release:",
    "lea   r10, [rip + {busy}]",
    "mov   qword ptr [r10 + rcx*8], 0",
    // A direct genuine monotonic read. Reached by a nested read without clearing the slot it did
    // not claim, or from the release above.
    ".Lnano_genuine:",
    "sub   rsp, 16",
    "mov   eax, {sys}",
    "mov   edi, {mono}",
    "mov   rsi, rsp",
    "syscall",
    "mov   rax, qword ptr [rsp]",
    "imul  rax, rax, 1000000000",
    "add   rax, qword ptr [rsp + 8]",
    "add   rsp, 16",
    "mov   qword ptr [rsp + 8], rax",
    "ret",
    ".p2align 4",
    ".globl tw_go_fallback_walltime_shim",
    "tw_go_fallback_walltime_shim:",
    "mov   rax, qword ptr fs:[0]",
    "mov   rdx, rax",
    "mov   r11, {phi}",
    "imul  rdx, r11",
    "shr   rdx, {slot_shift}",
    "lea   r10, [rip + {busy}]",
    "mov   eax, 1",
    "xchg  rax, qword ptr [r10 + rdx*8]",
    "test  rax, rax",
    "jnz   .Lwall_genuine",
    "mov   r11, qword ptr [rip + {arena}]",
    "test  r11, r11",
    "jz    .Lwall_release",
    "mov   r8, rdx",
    "inc   r8",
    "shl   r8, {stack_shift}",
    "lea   r9, [r11 + r8]",
    "and   r9, -16",
    "mov   qword ptr [r9 - 8], rsp",
    "mov   qword ptr [r9 - 16], rdx",
    "lea   rdi, [rsp + 8]",
    "lea   rsi, [rsp + 16]",
    "lea   rsp, [r9 - 32]",
    "call  {walltime}",
    "mov   rdx, qword ptr [rsp + 16]",
    "mov   rsp, qword ptr [rsp + 24]",
    "lea   r10, [rip + {busy}]",
    "mov   qword ptr [r10 + rdx*8], 0",
    "ret",
    ".Lwall_release:",
    "lea   r10, [rip + {busy}]",
    "mov   qword ptr [r10 + rdx*8], 0",
    ".Lwall_genuine:",
    "sub   rsp, 16",
    "mov   eax, {sys}",
    "mov   edi, 0",
    "mov   rsi, rsp",
    "syscall",
    "mov   rax, qword ptr [rsp]",
    "mov   ecx, dword ptr [rsp + 8]",
    "add   rsp, 16",
    "mov   qword ptr [rsp + 8], rax",
    "mov   dword ptr [rsp + 16], ecx",
    "ret",
    nanotime = sym tw_go_fallback_nanotime,
    walltime = sym tw_go_fallback_walltime,
    busy = sym BUSY,
    arena = sym ARENA_BASE,
    phi = const PHI,
    slot_shift = const SLOT_SHIFT,
    stack_shift = const STACK_SHIFT,
    sys = const SYS_CLOCK_GETTIME,
    mono = const GENUINE_MONOTONIC,
);

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".globl tw_go_fallback_nanotime_shim",
    "tw_go_fallback_nanotime_shim:",
    "mrs   x9, TPIDR_EL0",
    "movz  x10, #{phi0}",
    "movk  x10, #{phi1}, lsl #16",
    "movk  x10, #{phi2}, lsl #32",
    "movk  x10, #{phi3}, lsl #48",
    "mul   x9, x9, x10",
    "lsr   x11, x9, #{slot_shift}",
    "adrp  x12, {busy}",
    "add   x12, x12, :lo12:{busy}",
    "add   x12, x12, x11, lsl #3",
    "mov   w13, #1",
    "1:",
    "ldaxr x14, [x12]",
    "cbnz  x14, .Lnano_genuine",
    "stlxr w15, x13, [x12]",
    "cbnz  w15, 1b",
    "adrp  x16, {arena}",
    "ldr   x16, [x16, :lo12:{arena}]",
    "cbz   x16, .Lnano_release",
    "add   x17, x11, #1",
    "lsl   x17, x17, #{stack_shift}",
    "add   x16, x16, x17",
    "and   x16, x16, #-16",
    "mov   x1, sp",
    "sub   x16, x16, #32",
    "mov   sp, x16",
    "stp   x1, x30, [sp, #16]",
    "str   x11, [sp, #8]",
    "add   x0, x1, #8",
    "bl    {nanotime}",
    "ldr   x11, [sp, #8]",
    "ldp   x1, x30, [sp, #16]",
    "mov   sp, x1",
    "adrp  x12, {busy}",
    "add   x12, x12, :lo12:{busy}",
    "add   x12, x12, x11, lsl #3",
    "stlr  xzr, [x12]",
    "ret",
    ".Lnano_release:",
    "adrp  x12, {busy}",
    "add   x12, x12, :lo12:{busy}",
    "add   x12, x12, x11, lsl #3",
    "stlr  xzr, [x12]",
    ".Lnano_genuine:",
    "sub   sp, sp, #32",
    "mov   x0, #{mono}",
    "mov   x1, sp",
    "mov   x8, #{sys}",
    "svc   #0",
    "ldr   x2, [sp]",
    "ldr   w3, [sp, #8]",
    "movz  x4, #0xca00",
    "movk  x4, #0x3b9a, lsl #16",
    "madd  x2, x2, x4, x3",
    "add   sp, sp, #32",
    "str   x2, [sp, #8]",
    "ret",
    ".p2align 4",
    ".globl tw_go_fallback_walltime_shim",
    "tw_go_fallback_walltime_shim:",
    "mrs   x9, TPIDR_EL0",
    "movz  x10, #{phi0}",
    "movk  x10, #{phi1}, lsl #16",
    "movk  x10, #{phi2}, lsl #32",
    "movk  x10, #{phi3}, lsl #48",
    "mul   x9, x9, x10",
    "lsr   x11, x9, #{slot_shift}",
    "adrp  x12, {busy}",
    "add   x12, x12, :lo12:{busy}",
    "add   x12, x12, x11, lsl #3",
    "mov   w13, #1",
    "2:",
    "ldaxr x14, [x12]",
    "cbnz  x14, .Lwall_genuine",
    "stlxr w15, x13, [x12]",
    "cbnz  w15, 2b",
    "adrp  x16, {arena}",
    "ldr   x16, [x16, :lo12:{arena}]",
    "cbz   x16, .Lwall_release",
    "add   x17, x11, #1",
    "lsl   x17, x17, #{stack_shift}",
    "add   x16, x16, x17",
    "and   x16, x16, #-16",
    "mov   x1, sp",
    "sub   x16, x16, #32",
    "mov   sp, x16",
    "stp   x1, x30, [sp, #16]",
    "str   x11, [sp, #8]",
    "add   x0, x1, #8",
    "add   x1, x1, #16",
    "bl    {walltime}",
    "ldr   x11, [sp, #8]",
    "ldp   x1, x30, [sp, #16]",
    "mov   sp, x1",
    "adrp  x12, {busy}",
    "add   x12, x12, :lo12:{busy}",
    "add   x12, x12, x11, lsl #3",
    "stlr  xzr, [x12]",
    "ret",
    ".Lwall_release:",
    "adrp  x12, {busy}",
    "add   x12, x12, :lo12:{busy}",
    "add   x12, x12, x11, lsl #3",
    "stlr  xzr, [x12]",
    ".Lwall_genuine:",
    "sub   sp, sp, #32",
    "mov   x0, #0",
    "mov   x1, sp",
    "mov   x8, #{sys}",
    "svc   #0",
    "ldr   x2, [sp]",
    "ldr   w3, [sp, #8]",
    "add   sp, sp, #32",
    "str   x2, [sp, #8]",
    "str   w3, [sp, #16]",
    "ret",
    nanotime = sym tw_go_fallback_nanotime,
    walltime = sym tw_go_fallback_walltime,
    busy = sym BUSY,
    arena = sym ARENA_BASE,
    phi0 = const PHI & 0xffff,
    phi1 = const (PHI >> 16) & 0xffff,
    phi2 = const (PHI >> 32) & 0xffff,
    phi3 = const PHI >> 48,
    slot_shift = const SLOT_SHIFT,
    stack_shift = const STACK_SHIFT,
    sys = const SYS_CLOCK_GETTIME,
    mono = const GENUINE_MONOTONIC,
);

#[cfg(test)]
mod tests {
    /// The slot protocol the shim's atomic exchange carries out, modelled over an `AtomicU64` so
    /// `loom` can drive the interleavings the exchange itself hides from it.
    ///
    /// `swap(1, AcqRel)` models the amd64 `xchg` and the arm64 `ldaxr`/`stlxr` pair: the old value
    /// says whether the slot was free, and a one left behind holds it. Release stores a zero back.
    /// The assembly mirrors this under exactly these orderings.
    mod slot {
        #[cfg(loom)]
        pub(super) use loom::sync::Arc;
        #[cfg(loom)]
        pub(super) use loom::sync::atomic::{AtomicU64, Ordering};
        #[cfg(loom)]
        pub(super) use loom::thread;

        #[cfg(not(loom))]
        pub(super) use std::sync::Arc;
        #[cfg(not(loom))]
        pub(super) use std::sync::atomic::{AtomicU64, Ordering};
        #[cfg(not(loom))]
        pub(super) use std::thread;

        /// Tries to claim the slot. `true` when it was free and is now held.
        pub(super) fn claim(word: &AtomicU64) -> bool {
            word.swap(1, Ordering::AcqRel) == 0
        }

        /// Releases a held slot.
        pub(super) fn release(word: &AtomicU64) {
            word.store(0, Ordering::Release);
        }

        /// Runs `scenario` once, or under every interleaving when `loom` drives the build.
        pub(super) fn explore(scenario: impl Fn() + Sync + Send + 'static) {
            #[cfg(loom)]
            loom::model(scenario);
            #[cfg(not(loom))]
            scenario();
        }
    }

    #[test]
    fn two_threads_on_one_slot_let_exactly_one_in() {
        slot::explore(|| {
            let word = slot::Arc::new(slot::AtomicU64::new(0));
            let other = {
                let word = word.clone();
                slot::thread::spawn(move || slot::claim(&word))
            };
            let here = slot::claim(&word);
            let there = other.join().expect("the other thread ran");
            assert!(here ^ there, "exactly one thread claims the slot");
        });
    }

    #[test]
    fn a_nested_claim_on_a_held_slot_takes_the_genuine_path_and_release_frees_it() {
        slot::explore(|| {
            let word = slot::AtomicU64::new(0);
            assert!(slot::claim(&word), "the first claim takes the slot");
            assert!(
                !slot::claim(&word),
                "a nested claim finds the slot held and takes the genuine path"
            );
            // Only the claim that took the slot releases it; the nested one leaves it alone.
            slot::release(&word);
            assert!(slot::claim(&word), "the slot is free once released");
        });
    }

    /// The crafted-frame harness: enter the real shim the way a patched Go routine does, with the
    /// result slots just above the return address, and check what comes back.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    mod frame {
        use std::sync::atomic::Ordering;

        use super::super::{
            BUSY, PHI, SLOT_SHIFT, install_arena, nanotime_handler, walltime_handler,
        };

        // Sentinels loaded into the callee-saved registers before the shim runs, read back after
        // to prove it left them alone.
        #[cfg(target_arch = "x86_64")]
        core::arch::global_asm!(
            ".text",
            ".p2align 4",
            ".globl tw_test_invoke_nanotime",
            // fn(out_value: *mut i64, out_regs_ok: *mut u8)
            "tw_test_invoke_nanotime:",
            "push  rbp",
            "push  rbx",
            "push  r12",
            "push  r13",
            "push  r14",
            "push  r15",
            "push  rdi",
            "push  rsi",
            "mov   rbx, 0x1111111111111111",
            "mov   r12, 0x2222222222222222",
            "mov   r13, 0x3333333333333333",
            "mov   r14, 0x4444444444444444",
            "mov   r15, 0x5555555555555555",
            // Reserve the result slot so that, after the call pushes the return address, it sits at
            // S0+8, where the Go caller reads the nanotime value.
            "sub   rsp, 16",
            "call  tw_go_fallback_nanotime_shim",
            "mov   r11, qword ptr [rsp]",
            "add   rsp, 16",
            "mov   eax, 1",
            "mov   edx, 0",
            "mov   rcx, 0x1111111111111111",
            "cmp   rbx, rcx",
            "cmovne eax, edx",
            "mov   rcx, 0x2222222222222222",
            "cmp   r12, rcx",
            "cmovne eax, edx",
            "mov   rcx, 0x3333333333333333",
            "cmp   r13, rcx",
            "cmovne eax, edx",
            "mov   rcx, 0x4444444444444444",
            "cmp   r14, rcx",
            "cmovne eax, edx",
            "mov   rcx, 0x5555555555555555",
            "cmp   r15, rcx",
            "cmovne eax, edx",
            "pop   rsi",
            "pop   rdi",
            "mov   qword ptr [rdi], r11",
            "mov   byte ptr [rsi], al",
            "pop   r15",
            "pop   r14",
            "pop   r13",
            "pop   r12",
            "pop   rbx",
            "pop   rbp",
            "ret",
            ".p2align 4",
            ".globl tw_test_invoke_walltime",
            // fn(out_sec: *mut i64, out_nsec: *mut i32)
            "tw_test_invoke_walltime:",
            "push  rdi",
            "push  rsi",
            "sub   rsp, 16",
            "call  tw_go_fallback_walltime_shim",
            "mov   rax, qword ptr [rsp]",
            "mov   ecx, dword ptr [rsp + 8]",
            "add   rsp, 16",
            "pop   rsi",
            "pop   rdi",
            "mov   qword ptr [rdi], rax",
            "mov   dword ptr [rsi], ecx",
            "ret",
        );

        // The same on arm64, where the return address travels in the link register, so the result
        // slots start eight bytes above the stack pointer. Each sentinel repeats a pattern `mov`
        // takes as a bitmask immediate.
        #[cfg(target_arch = "aarch64")]
        core::arch::global_asm!(
            ".text",
            ".p2align 4",
            ".globl tw_test_invoke_nanotime",
            // fn(out_value: *mut i64, out_regs_ok: *mut u8)
            "tw_test_invoke_nanotime:",
            "stp   x29, x30, [sp, #-16]!",
            "stp   x27, x28, [sp, #-16]!",
            "stp   x25, x26, [sp, #-16]!",
            "stp   x23, x24, [sp, #-16]!",
            "stp   x21, x22, [sp, #-16]!",
            "stp   x19, x20, [sp, #-16]!",
            "stp   x0, x1, [sp, #-16]!",
            "mov   x19, #0x1111111111111111",
            "mov   x20, #0x2222222222222222",
            "mov   x21, #0x3333333333333333",
            "mov   x22, #0x4444444444444444",
            "mov   x23, #0x5555555555555555",
            "mov   x24, #0x6666666666666666",
            "mov   x25, #0x7777777777777777",
            "mov   x26, #0x8888888888888888",
            "mov   x27, #0x9999999999999999",
            "mov   x28, #0xaaaaaaaaaaaaaaaa",
            "mov   x29, #0xcccccccccccccccc",
            // The result slot at SP+8, where the Go caller reads the nanotime value.
            "sub   sp, sp, #16",
            "bl    tw_go_fallback_nanotime_shim",
            "ldr   x9, [sp, #8]",
            "add   sp, sp, #16",
            "mov   w10, #1",
            "mov   x11, #0x1111111111111111",
            "cmp   x19, x11",
            "csel  w10, w10, wzr, eq",
            "mov   x11, #0x2222222222222222",
            "cmp   x20, x11",
            "csel  w10, w10, wzr, eq",
            "mov   x11, #0x3333333333333333",
            "cmp   x21, x11",
            "csel  w10, w10, wzr, eq",
            "mov   x11, #0x4444444444444444",
            "cmp   x22, x11",
            "csel  w10, w10, wzr, eq",
            "mov   x11, #0x5555555555555555",
            "cmp   x23, x11",
            "csel  w10, w10, wzr, eq",
            "mov   x11, #0x6666666666666666",
            "cmp   x24, x11",
            "csel  w10, w10, wzr, eq",
            "mov   x11, #0x7777777777777777",
            "cmp   x25, x11",
            "csel  w10, w10, wzr, eq",
            "mov   x11, #0x8888888888888888",
            "cmp   x26, x11",
            "csel  w10, w10, wzr, eq",
            "mov   x11, #0x9999999999999999",
            "cmp   x27, x11",
            "csel  w10, w10, wzr, eq",
            "mov   x11, #0xaaaaaaaaaaaaaaaa",
            "cmp   x28, x11",
            "csel  w10, w10, wzr, eq",
            "mov   x11, #0xcccccccccccccccc",
            "cmp   x29, x11",
            "csel  w10, w10, wzr, eq",
            "ldp   x0, x1, [sp], #16",
            "str   x9, [x0]",
            "strb  w10, [x1]",
            "ldp   x19, x20, [sp], #16",
            "ldp   x21, x22, [sp], #16",
            "ldp   x23, x24, [sp], #16",
            "ldp   x25, x26, [sp], #16",
            "ldp   x27, x28, [sp], #16",
            "ldp   x29, x30, [sp], #16",
            "ret",
            ".p2align 4",
            ".globl tw_test_invoke_walltime",
            // fn(out_sec: *mut i64, out_nsec: *mut i32)
            "tw_test_invoke_walltime:",
            "stp   x29, x30, [sp, #-16]!",
            "stp   x0, x1, [sp, #-16]!",
            // The two result slots at SP+8 and SP+16, the stack pointer kept 16-aligned.
            "sub   sp, sp, #32",
            "bl    tw_go_fallback_walltime_shim",
            "ldr   x9, [sp, #8]",
            "ldr   w10, [sp, #16]",
            "add   sp, sp, #32",
            "ldp   x0, x1, [sp], #16",
            "str   x9, [x0]",
            "str   w10, [x1]",
            "ldp   x29, x30, [sp], #16",
            "ret",
        );

        unsafe extern "C" {
            fn tw_test_invoke_nanotime(out_value: *mut i64, out_regs_ok: *mut u8);
            fn tw_test_invoke_walltime(out_sec: *mut i64, out_nsec: *mut i32);
        }

        /// A direct reading of `clock`, to bracket what the shim writes.
        fn now(clock: libc::clockid_t) -> i64 {
            let mut ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // Safety: `ts` is a live, correctly typed buffer.
            let read = unsafe { libc::clock_gettime(clock, &raw mut ts) };
            assert_eq!(read, 0);
            ts.tv_sec * 1_000_000_000 + ts.tv_nsec
        }

        /// The slot the shim keys this thread to.
        fn own_slot() -> usize {
            let pointer: u64;
            #[cfg(target_arch = "x86_64")]
            // Safety: reads this thread's own pointer, the word the shim keys a slot by.
            unsafe {
                core::arch::asm!(
                    "mov {}, qword ptr fs:[0]",
                    out(reg) pointer,
                    options(nostack, readonly, preserves_flags)
                );
            }
            #[cfg(target_arch = "aarch64")]
            // Safety: as above.
            unsafe {
                core::arch::asm!(
                    "mrs {}, TPIDR_EL0",
                    out(reg) pointer,
                    options(nomem, nostack, preserves_flags)
                );
            }
            usize::try_from(pointer.wrapping_mul(PHI) >> SLOT_SHIFT).unwrap()
        }

        /// Runs `read` with this thread's slot held, the way a read already in flight on the
        /// thread holds it, and says whether the slot was still held when `read` returned.
        fn holding_own_slot(read: impl FnOnce()) -> bool {
            let slot = &BUSY[own_slot()];
            while slot.swap(1, Ordering::AcqRel) != 0 {
                std::hint::spin_loop();
            }
            read();
            slot.swap(0, Ordering::AcqRel) == 1
        }

        #[test]
        fn the_nanotime_shim_writes_the_slot_and_keeps_the_callee_saved_registers() {
            assert!(install_arena(), "the arena maps");
            assert_ne!(nanotime_handler(), 0);

            let before = now(libc::CLOCK_MONOTONIC);
            let mut value: i64 = -1;
            let mut regs_ok: u8 = 0;
            // Safety: both pointers are live and correctly typed for the harness to fill.
            unsafe { tw_test_invoke_nanotime(&raw mut value, &raw mut regs_ok) };
            let after = now(libc::CLOCK_MONOTONIC);

            assert_eq!(regs_ok, 1, "the shim trod on a callee-saved register");
            assert!(
                before <= value && value <= after,
                "the shim wrote {value}, outside the reads around it at {before} and {after}"
            );
        }

        #[test]
        fn the_walltime_shim_writes_both_result_slots() {
            assert!(install_arena(), "the arena maps");
            assert_ne!(walltime_handler(), 0);

            let mut sec: i64 = -1;
            let mut nsec: i32 = -1;
            // Safety: both pointers are live and correctly typed for the harness to fill.
            unsafe { tw_test_invoke_walltime(&raw mut sec, &raw mut nsec) };

            assert!(
                sec > 1_700_000_000,
                "the seconds slot reads a recent wall clock"
            );
            assert!(
                (0..1_000_000_000).contains(&nsec),
                "the nanoseconds slot is in range"
            );
        }

        #[test]
        fn a_held_slot_sends_the_nanotime_shim_down_its_genuine_path() {
            assert!(install_arena(), "the arena maps");

            let mut value: i64 = -1;
            let mut regs_ok: u8 = 0;
            let mut around = (0, 0);
            let held = holding_own_slot(|| {
                around.0 = now(libc::CLOCK_MONOTONIC);
                // Safety: both pointers are live and correctly typed for the harness to fill.
                unsafe { tw_test_invoke_nanotime(&raw mut value, &raw mut regs_ok) };
                around.1 = now(libc::CLOCK_MONOTONIC);
            });

            let (before, after) = around;
            assert!(held, "the genuine path let go of a slot it never claimed");
            assert_eq!(regs_ok, 1, "the shim trod on a callee-saved register");
            assert!(
                before <= value && value <= after,
                "the shim wrote {value}, outside the reads around it at {before} and {after}"
            );
        }

        #[test]
        fn a_held_slot_sends_the_walltime_shim_down_its_genuine_path() {
            assert!(install_arena(), "the arena maps");

            let mut sec: i64 = -1;
            let mut nsec: i32 = -1;
            let mut around = (0, 0);
            let held = holding_own_slot(|| {
                around.0 = now(libc::CLOCK_REALTIME);
                // Safety: both pointers are live and correctly typed for the harness to fill.
                unsafe { tw_test_invoke_walltime(&raw mut sec, &raw mut nsec) };
                around.1 = now(libc::CLOCK_REALTIME);
            });

            let (before, after) = around;
            let read = sec * 1_000_000_000 + i64::from(nsec);
            assert!(held, "the genuine path let go of a slot it never claimed");
            assert!(
                (0..1_000_000_000).contains(&nsec),
                "the nanoseconds slot is in range"
            );
            assert!(
                before <= read && read <= after,
                "the shim wrote {read}, outside the reads around it at {before} and {after}"
            );
        }
    }
}
