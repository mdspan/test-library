//! The entry shims a patched Go fallback clock routine lands on, and the scratch stacks they run
//! the read path from.
//!
//! A Go runtime that reads the clock without a timekeep page calls `runtime.fallback_nanotime` and
//! `runtime.fallback_walltime`. Those run on a goroutine stack with only the few words they reserve
//! for themselves, far too little for the read path. So the entry of each is rewritten to jump
//! here, into a shim written in assembly that switches to a scratch stack of ours before it calls
//! any Rust: no Rust runs on the goroutine stack, and the whole entry takes no lock and calls no
//! library function, so it is safe wherever Go makes a clock read, a signal handler included.
//!
//! A read holds a scratch stack from its entry to its return. The stacks are a fixed arena mapped
//! at install, each with a try-lock word claimed by an atomic exchange. A read walks the arena from
//! the slot its thread pointer hashes to and runs on the first stack it claims. With every slot
//! held it claims a spare stack instead, and maps a new spare when no spare is free, so a read
//! never goes without a stack because other threads are reading. A thread-local word is set while
//! the thread has a read in flight: a nested read, which a `SIGPROF` sample taken inside an
//! in-flight read is, finds it set and takes a bounded genuine `clock_gettime` in the assembly
//! instead, the one place a faked Go program here observes genuine time. A kernel that refuses a
//! spare's memory sends that read there as well.
//!
//! The `interleavings` tests model the protocol and check it under `loom`; the assembly mirrors
//! that model.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use common::hook::Func;
use common::platform::clock;
use common::time::Nanos;

use crate::interpose;

/// How many scratch stacks the arena holds, which is how many reads can be in flight at once
/// before one runs on a spare. A power of two, so the thread key picks a slot with a shift and the
/// walk wraps with a mask.
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

/// Where a spare stack's header starts, from the base of its mapping: the stack's try-lock word,
/// then the header of the spare pushed before it, or zero. The stack runs down from there.
const SPARE_HEADER: usize = STACK_SIZE - 16;

/// The low bits a page-aligned address has clear, at the smallest page of any system here. A raw
/// `mmap` that fails answers an error number, negated on Linux, which has one of them set.
const PAGE_LOW: u64 = 4095;

const _: () = assert!(1 << STACK_SHIFT == STACK_SIZE);
const _: () = assert!(1usize << (64 - SLOT_SHIFT) == SLOTS);

/// The system calls the assembly makes itself, numbered by platform and, on Linux, by
/// architecture: `clock_gettime` on the genuine path, and `mmap`, `mprotect` and `munmap` to map a
/// spare stack.
#[cfg(target_os = "freebsd")]
mod sys {
    pub(super) const CLOCK_GETTIME: i32 = 232;
    pub(super) const MMAP: i32 = 477;
    pub(super) const MPROTECT: i32 = 74;
    pub(super) const MUNMAP: i32 = 73;
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod sys {
    pub(super) const CLOCK_GETTIME: i32 = 228;
    pub(super) const MMAP: i32 = 9;
    pub(super) const MPROTECT: i32 = 10;
    pub(super) const MUNMAP: i32 = 11;
}
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
mod sys {
    pub(super) const CLOCK_GETTIME: i32 = 113;
    pub(super) const MMAP: i32 = 222;
    pub(super) const MPROTECT: i32 = 226;
    pub(super) const MUNMAP: i32 = 215;
}

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

/// The flags of every scratch-stack mapping: private and anonymous.
const ANON_PRIVATE: libc::c_int = libc::MAP_PRIVATE | MAP_ANON;

/// The protection a scratch stack is mapped with.
const READ_WRITE: libc::c_int = libc::PROT_READ | libc::PROT_WRITE;

/// Where the scratch-stack arena is, or zero before [`install_arena`] has mapped it. A shim that
/// reads zero here goes straight to the spares.
static ARENA_BASE: AtomicUsize = AtomicUsize::new(0);

/// One try-lock word per arena slot. A shim swaps a one in with an atomic exchange; seeing a zero
/// means the stack was free and is now the read's, and seeing a one sends the walk on to the next
/// slot. The read stores a zero back on its way out.
static BUSY: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];

/// The header of the spare stack mapped last, or zero before any read has found the arena full.
/// Spares are pushed here and never taken off: one is mapped only when every stack already on the
/// list was held as the walk passed it.
static SPARES: AtomicUsize = AtomicUsize::new(0);

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
/// faults rather than reaching a neighbour. `false` when the kernel gave no memory. Idempotent and
/// safe to race: the first mapping to publish wins and any other is given back to the kernel.
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
    let mapped = unsafe { libc::mmap(std::ptr::null_mut(), len, READ_WRITE, ANON_PRIVATE, -1, 0) };
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

// Both shims load the Rust entry and their genuine path into two scratch registers and go on to
// one shared entry, `tw_go_fallback_enter`, which claims a stack, runs the entry on it with the
// result slots as arguments, and gives the stack back.
//
// `tw_go_in_flight` is the thread-local word, in the static block libthr gives every thread it
// starts. Reached in initial-exec form: one load of its offset and one access off the thread
// pointer, with no call. A FreeBSD Go program the library loads into is linked with cgo, so every
// thread of its runtime is libthr's and the thread pointer is the one libthr set.
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".pushsection .tbss,\"awT\",@nobits",
    ".p2align 3",
    ".globl tw_go_in_flight",
    ".type tw_go_in_flight, @tls_object",
    ".size tw_go_in_flight, 8",
    "tw_go_in_flight:",
    ".zero 8",
    ".popsection",
    ".text",
    ".p2align 4",
    ".globl tw_go_fallback_nanotime_shim",
    "tw_go_fallback_nanotime_shim:",
    "lea   r11, [rip + {nanotime}]",
    "lea   r10, [rip + .Lnano_genuine]",
    "jmp   .Lenter",
    ".p2align 4",
    ".globl tw_go_fallback_walltime_shim",
    "tw_go_fallback_walltime_shim:",
    "lea   r11, [rip + {walltime}]",
    "lea   r10, [rip + .Lwall_genuine]",
    "jmp   .Lenter",
    // Entered on the Go caller's stack, the Rust entry in r11 and the genuine path in r10. A read
    // already in flight on this thread takes the genuine path.
    ".p2align 4",
    ".globl tw_go_fallback_enter",
    "tw_go_fallback_enter:",
    ".Lenter:",
    "mov   rax, qword ptr [rip + tw_go_in_flight@GOTTPOFF]",
    "cmp   qword ptr fs:[rax], 0",
    "jne   .Lgenuine",
    "mov   qword ptr fs:[rax], 1",
    // Every arena slot from the one the thread pointer hashes to, until one is claimed.
    "mov   rsi, qword ptr [rip + {arena}]",
    "test  rsi, rsi",
    "jz    .Lspares",
    "mov   rcx, qword ptr fs:[0]",
    "mov   rdx, {phi}",
    "imul  rcx, rdx",
    "shr   rcx, {slot_shift}",
    "lea   r8, [rip + {busy}]",
    "mov   r9d, {slots}",
    ".Lprobe:",
    "lea   rdi, [r8 + rcx*8]",
    "cmp   qword ptr [rdi], 0",
    "jne   .Lprobe_next",
    "mov   eax, 1",
    "xchg  rax, qword ptr [rdi]",
    "test  rax, rax",
    "jz    .Lon_slot",
    ".Lprobe_next:",
    "inc   rcx",
    "and   rcx, {last_slot}",
    "dec   r9d",
    "jnz   .Lprobe",
    // Every slot held: the first free spare, or a new one.
    ".Lspares:",
    "mov   rdi, qword ptr [rip + {spares}]",
    ".Lwalk:",
    "test  rdi, rdi",
    "jz    .Lgrow",
    "cmp   qword ptr [rdi], 0",
    "jne   .Lwalk_next",
    "mov   eax, 1",
    "xchg  rax, qword ptr [rdi]",
    "test  rax, rax",
    "jz    .Lon_spare",
    ".Lwalk_next:",
    "mov   rdi, qword ptr [rdi + 8]",
    "jmp   .Lwalk",
    ".Lon_slot:",
    "lea   rdx, [rcx + 1]",
    "shl   rdx, {stack_shift}",
    "add   rdx, rsi",
    "jmp   .Lrun",
    ".Lon_spare:",
    "mov   rdx, rdi",
    // The claimed stack's top is in rdx and its try-lock word in rdi; both go back off the top of
    // the stack, with the caller's stack pointer, once the entry returns.
    ".Lrun:",
    "mov   qword ptr [rdx - 8], rsp",
    "mov   qword ptr [rdx - 16], rdi",
    "lea   rdi, [rsp + 8]",
    "lea   rsi, [rsp + 16]",
    "lea   rsp, [rdx - 32]",
    "call  r11",
    "mov   rdi, qword ptr [rsp + 16]",
    "mov   rsp, qword ptr [rsp + 24]",
    "mov   qword ptr [rdi], 0",
    "mov   rax, qword ptr [rip + tw_go_in_flight@GOTTPOFF]",
    "mov   qword ptr fs:[rax], 0",
    "ret",
    // A new spare: mapped, its low page made the guard, and pushed already claimed. The system
    // calls take r10 and r11, so those wait on the caller's stack, inside the frame the routine
    // reserved for itself.
    ".Lgrow:",
    "push  r10",
    "push  r11",
    "mov   eax, {sys_mmap}",
    "xor   edi, edi",
    "mov   esi, {stack_size}",
    "mov   edx, {read_write}",
    "mov   r10d, {anon_private}",
    "mov   r8, -1",
    "xor   r9d, r9d",
    "syscall",
    "test  eax, {page_low}",
    "jnz   .Lgrow_failed",
    "mov   rdi, rax",
    "mov   eax, {sys_mprotect}",
    "mov   esi, 1",
    "xor   edx, edx",
    "syscall",
    "test  rax, rax",
    "jnz   .Lgrow_unmap",
    "add   rdi, {spare_header}",
    "mov   qword ptr [rdi], 1",
    "mov   rax, qword ptr [rip + {spares}]",
    ".Lpush:",
    "mov   qword ptr [rdi + 8], rax",
    "lock  cmpxchg qword ptr [rip + {spares}], rdi",
    "jne   .Lpush",
    "pop   r11",
    "pop   r10",
    "jmp   .Lon_spare",
    ".Lgrow_unmap:",
    "mov   eax, {sys_munmap}",
    "mov   esi, {stack_size}",
    "syscall",
    ".Lgrow_failed:",
    "pop   r11",
    "pop   r10",
    "mov   rax, qword ptr [rip + tw_go_in_flight@GOTTPOFF]",
    "mov   qword ptr fs:[rax], 0",
    ".Lgenuine:",
    "jmp   r10",
    // Direct genuine reads on the caller's stack, inside the frame the routine reserved.
    ".Lnano_genuine:",
    "sub   rsp, 16",
    "mov   eax, {sys_clock_gettime}",
    "mov   edi, {mono}",
    "mov   rsi, rsp",
    "syscall",
    "mov   rax, qword ptr [rsp]",
    "imul  rax, rax, 1000000000",
    "add   rax, qword ptr [rsp + 8]",
    "add   rsp, 16",
    "mov   qword ptr [rsp + 8], rax",
    "ret",
    ".Lwall_genuine:",
    "sub   rsp, 16",
    "mov   eax, {sys_clock_gettime}",
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
    spares = sym SPARES,
    phi = const PHI,
    slot_shift = const SLOT_SHIFT,
    slots = const SLOTS,
    last_slot = const SLOTS - 1,
    stack_shift = const STACK_SHIFT,
    stack_size = const STACK_SIZE,
    spare_header = const SPARE_HEADER,
    page_low = const PAGE_LOW,
    read_write = const READ_WRITE,
    anon_private = const ANON_PRIVATE,
    sys_mmap = const sys::MMAP,
    sys_mprotect = const sys::MPROTECT,
    sys_munmap = const sys::MUNMAP,
    sys_clock_gettime = const sys::CLOCK_GETTIME,
    mono = const GENUINE_MONOTONIC,
);

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".pushsection .tbss,\"awT\",%nobits",
    ".p2align 3",
    ".globl tw_go_in_flight",
    ".type tw_go_in_flight, %tls_object",
    ".size tw_go_in_flight, 8",
    "tw_go_in_flight:",
    ".zero 8",
    ".popsection",
    ".text",
    ".p2align 4",
    ".globl tw_go_fallback_nanotime_shim",
    "tw_go_fallback_nanotime_shim:",
    "adrp  x14, {nanotime}",
    "add   x14, x14, :lo12:{nanotime}",
    "adr   x15, .Lnano_genuine",
    "b     .Lenter",
    ".p2align 4",
    ".globl tw_go_fallback_walltime_shim",
    "tw_go_fallback_walltime_shim:",
    "adrp  x14, {walltime}",
    "add   x14, x14, :lo12:{walltime}",
    "adr   x15, .Lwall_genuine",
    "b     .Lenter",
    // Entered on the Go caller's stack with its return address in x30, the Rust entry in x14 and
    // the genuine path in x15. Nothing here touches x18 or Go's g in x28.
    ".p2align 4",
    ".globl tw_go_fallback_enter",
    "tw_go_fallback_enter:",
    ".Lenter:",
    "adrp  x9, :gottprel:tw_go_in_flight",
    "ldr   x9, [x9, :gottprel_lo12:tw_go_in_flight]",
    "mrs   x10, TPIDR_EL0",
    "add   x9, x9, x10",
    "ldr   x11, [x9]",
    "cbnz  x11, .Lgenuine",
    "mov   x11, #1",
    "str   x11, [x9]",
    "mov   x1, #1",
    "adrp  x12, {arena}",
    "ldr   x12, [x12, :lo12:{arena}]",
    "cbz   x12, .Lspares",
    "movz  x11, #{phi0}",
    "movk  x11, #{phi1}, lsl #16",
    "movk  x11, #{phi2}, lsl #32",
    "movk  x11, #{phi3}, lsl #48",
    "mul   x11, x10, x11",
    "lsr   x11, x11, #{slot_shift}",
    "adrp  x13, {busy}",
    "add   x13, x13, :lo12:{busy}",
    "mov   x17, #{slots}",
    ".Lprobe:",
    "add   x16, x13, x11, lsl #3",
    "ldr   x10, [x16]",
    "cbnz  x10, .Lprobe_next",
    "1:",
    "ldaxr x10, [x16]",
    "cbnz  x10, .Lprobe_next",
    "stlxr w2, x1, [x16]",
    "cbnz  w2, 1b",
    "add   x13, x11, #1",
    "lsl   x13, x13, #{stack_shift}",
    "add   x13, x12, x13",
    "b     .Lrun",
    ".Lprobe_next:",
    "add   x11, x11, #1",
    "and   x11, x11, #{last_slot}",
    "subs  x17, x17, #1",
    "b.ne  .Lprobe",
    ".Lspares:",
    "adrp  x16, {spares}",
    "add   x16, x16, :lo12:{spares}",
    "ldar  x16, [x16]",
    ".Lwalk:",
    "cbz   x16, .Lgrow",
    "ldr   x10, [x16]",
    "cbnz  x10, .Lwalk_next",
    "2:",
    "ldaxr x10, [x16]",
    "cbnz  x10, .Lwalk_next",
    "stlxr w2, x1, [x16]",
    "cbnz  w2, 2b",
    "mov   x13, x16",
    "b     .Lrun",
    ".Lwalk_next:",
    "add   x16, x16, #8",
    "ldar  x16, [x16]",
    "b     .Lwalk",
    // The claimed stack's top is in x13, its try-lock word in x16 and the in-flight word's address
    // in x9; all three go back off the top of the stack, with the caller's stack pointer and
    // return address, once the entry returns.
    ".Lrun:",
    "mov   x10, sp",
    "sub   sp, x13, #32",
    "stp   x10, x30, [sp, #16]",
    "stp   x16, x9, [sp]",
    "add   x0, x10, #8",
    "add   x1, x10, #16",
    "blr   x14",
    "ldp   x16, x9, [sp]",
    "ldp   x10, x30, [sp, #16]",
    "mov   sp, x10",
    "stlr  xzr, [x16]",
    "str   xzr, [x9]",
    "ret",
    // A new spare: mapped, its low page made the guard, and pushed already claimed.
    ".Lgrow:",
    "mov   x0, #0",
    "mov   x1, #{stack_size}",
    "mov   x2, #{read_write}",
    "mov   x3, #{anon_private}",
    "mov   x4, #-1",
    "mov   x5, #0",
    "mov   x8, #{sys_mmap}",
    "svc   #0",
    "tst   x0, #{page_low}",
    "b.ne  .Lgrow_failed",
    "mov   x12, x0",
    "mov   x1, #1",
    "mov   x2, #0",
    "mov   x8, #{sys_mprotect}",
    "svc   #0",
    "cbnz  x0, .Lgrow_unmap",
    "mov   x13, #{spare_header}",
    "add   x13, x12, x13",
    "mov   x10, #1",
    "str   x10, [x13]",
    "adrp  x16, {spares}",
    "add   x16, x16, :lo12:{spares}",
    ".Lpush:",
    "ldar  x10, [x16]",
    "str   x10, [x13, #8]",
    "3:",
    "ldaxr x11, [x16]",
    "cmp   x11, x10",
    "b.ne  .Lpush",
    "stlxr w2, x13, [x16]",
    "cbnz  w2, 3b",
    "mov   x16, x13",
    "b     .Lrun",
    ".Lgrow_unmap:",
    "mov   x0, x12",
    "mov   x1, #{stack_size}",
    "mov   x8, #{sys_munmap}",
    "svc   #0",
    ".Lgrow_failed:",
    "str   xzr, [x9]",
    ".Lgenuine:",
    "br    x15",
    // Direct genuine reads on the caller's stack, inside the frame the routine reserved.
    ".Lnano_genuine:",
    "sub   sp, sp, #32",
    "mov   x0, #{mono}",
    "mov   x1, sp",
    "mov   x8, #{sys_clock_gettime}",
    "svc   #0",
    "ldr   x2, [sp]",
    "ldr   w3, [sp, #8]",
    "movz  x4, #0xca00",
    "movk  x4, #0x3b9a, lsl #16",
    "madd  x2, x2, x4, x3",
    "add   sp, sp, #32",
    "str   x2, [sp, #8]",
    "ret",
    ".Lwall_genuine:",
    "sub   sp, sp, #32",
    "mov   x0, #0",
    "mov   x1, sp",
    "mov   x8, #{sys_clock_gettime}",
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
    spares = sym SPARES,
    phi0 = const PHI & 0xffff,
    phi1 = const (PHI >> 16) & 0xffff,
    phi2 = const (PHI >> 32) & 0xffff,
    phi3 = const PHI >> 48,
    slot_shift = const SLOT_SHIFT,
    slots = const SLOTS,
    last_slot = const SLOTS - 1,
    stack_shift = const STACK_SHIFT,
    stack_size = const STACK_SIZE,
    spare_header = const SPARE_HEADER,
    page_low = const PAGE_LOW,
    read_write = const READ_WRITE,
    anon_private = const ANON_PRIVATE,
    sys_mmap = const sys::MMAP,
    sys_mprotect = const sys::MPROTECT,
    sys_munmap = const sys::MUNMAP,
    sys_clock_gettime = const sys::CLOCK_GETTIME,
    mono = const GENUINE_MONOTONIC,
);

#[cfg(test)]
mod interleavings;

/// The crafted-frame harness: enter the real shims the way a patched Go routine does, with the
/// result slots just above the return address, and check what comes back.
#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use super::{
        ARENA_BASE, BUSY, PHI, SLOT_SHIFT, SLOTS, SPARE_HEADER, SPARES, STACK_SIZE, install_arena,
        nanotime_handler, walltime_handler,
    };

    // Sentinels loaded into the callee-saved registers before the shim runs, read back after to
    // prove it left them alone.
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
        ".p2align 4",
        ".globl tw_test_enter",
        // fn(out: *mut usize, entry: Entry, genuine: unsafe extern "C" fn()): the shared entry,
        // entered the way a shim enters it.
        "tw_test_enter:",
        "push  rdi",
        "mov   r11, rsi",
        "mov   r10, rdx",
        "sub   rsp, 16",
        "call  tw_go_fallback_enter",
        "mov   rax, qword ptr [rsp]",
        "add   rsp, 16",
        "pop   rdi",
        "mov   qword ptr [rdi], rax",
        "ret",
        ".p2align 4",
        ".globl tw_test_genuine",
        // A genuine path that writes the mark rather than reading a clock.
        "tw_test_genuine:",
        "mov   qword ptr [rsp + 8], {mark}",
        "ret",
        ".p2align 4",
        ".globl tw_test_in_flight",
        // fn() -> *mut u64: this thread's in-flight word.
        "tw_test_in_flight:",
        "mov   rax, qword ptr [rip + tw_go_in_flight@GOTTPOFF]",
        "add   rax, qword ptr fs:[0]",
        "ret",
        mark = const GENUINE_MARK,
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
        ".p2align 4",
        ".globl tw_test_enter",
        // fn(out: *mut usize, entry: Entry, genuine: unsafe extern "C" fn()): the shared entry,
        // entered the way a shim enters it.
        "tw_test_enter:",
        "stp   x29, x30, [sp, #-16]!",
        "stp   x0, x1, [sp, #-16]!",
        "mov   x14, x1",
        "mov   x15, x2",
        "sub   sp, sp, #32",
        "bl    tw_go_fallback_enter",
        "ldr   x9, [sp, #8]",
        "add   sp, sp, #32",
        "ldp   x0, x1, [sp], #16",
        "str   x9, [x0]",
        "ldp   x29, x30, [sp], #16",
        "ret",
        ".p2align 4",
        ".globl tw_test_genuine",
        // A genuine path that writes the mark rather than reading a clock.
        "tw_test_genuine:",
        "mov   x9, #{mark}",
        "str   x9, [sp, #8]",
        "ret",
        ".p2align 4",
        ".globl tw_test_in_flight",
        // fn() -> *mut u64: this thread's in-flight word.
        "tw_test_in_flight:",
        "adrp  x0, :gottprel:tw_go_in_flight",
        "ldr   x0, [x0, :gottprel_lo12:tw_go_in_flight]",
        "mrs   x1, TPIDR_EL0",
        "add   x0, x0, x1",
        "ret",
        mark = const GENUINE_MARK,
    );

    /// A Rust entry as the shared entry calls one.
    type Entry = unsafe extern "C" fn(*mut usize, *mut i32);

    unsafe extern "C" {
        fn tw_test_invoke_nanotime(out_value: *mut i64, out_regs_ok: *mut u8);
        fn tw_test_invoke_walltime(out_sec: *mut i64, out_nsec: *mut i32);
        fn tw_test_enter(out: *mut usize, entry: Entry, genuine: unsafe extern "C" fn());
        fn tw_test_genuine();
        fn tw_test_in_flight() -> *mut u64;
    }

    /// What [`tw_test_genuine`] writes into the result slot: `-2`, which no frame's address is.
    const GENUINE_MARK: usize = usize::MAX - 1;

    /// Taken by every case here. They hold arena slots and read the spares, which are the
    /// process's, so one case's reads would land where another's expect a stack to be free.
    static ALONE: Mutex<()> = Mutex::new(());

    fn alone() -> MutexGuard<'static, ()> {
        ALONE.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The entry [`read_through_the_entry`] runs in place of the session's read path: it writes
    /// where its own frame is, which says which stack the read ran on.
    unsafe extern "C" fn frame_of_the_read(out: *mut usize, _nanos: *mut i32) {
        let marker = 0_u8;
        let at = std::hint::black_box(&raw const marker).addr();
        // Safety: the harness passes its own result slot.
        unsafe { out.write(at) };
    }

    /// Enters the shared entry the way a shim does, with [`frame_of_the_read`] as the read path and
    /// [`tw_test_genuine`] as the genuine one, and answers what the read wrote.
    fn read_through_the_entry() -> usize {
        let mut out = 0;
        // Safety: `out` is live, and both routines are the harness's own.
        unsafe { tw_test_enter(&raw mut out, frame_of_the_read, tw_test_genuine) };
        out
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

    /// The slot the shim's walk starts from on this thread.
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

    /// The arena slot whose stack `at` lies on, or `None` when it lies on none of them.
    fn arena_slot_of(at: usize) -> Option<usize> {
        let base = ARENA_BASE.load(Ordering::Acquire);
        at.checked_sub(base)
            .map(|offset| offset / STACK_SIZE)
            .filter(|&slot| slot < SLOTS)
    }

    /// A word of a spare's header.
    const fn header_word(at: usize) -> &'static AtomicU64 {
        // Safety: a spare is never unmapped, and its header words are aligned and only ever
        // accessed atomically.
        unsafe { &*std::ptr::with_exposed_provenance::<AtomicU64>(at) }
    }

    /// The header of every spare, newest first.
    fn spares() -> Vec<usize> {
        let mut found = Vec::new();
        let mut header = SPARES.load(Ordering::Acquire);
        while header != 0 {
            found.push(header);
            header = usize::try_from(header_word(header + 8).load(Ordering::Acquire)).unwrap();
        }
        found
    }

    /// The header of the spare whose stack `at` lies on.
    fn spare_of(at: usize) -> Option<usize> {
        spares()
            .into_iter()
            .find(|&header| (header - SPARE_HEADER..header).contains(&at))
    }

    /// Arena slots held the way reads in flight on other threads hold them, until this drops.
    struct Held(Vec<usize>);

    fn hold(slots: impl IntoIterator<Item = usize>) -> Held {
        let slots: Vec<usize> = slots.into_iter().collect();
        for &slot in &slots {
            while BUSY[slot].swap(1, Ordering::AcqRel) != 0 {
                std::hint::spin_loop();
            }
        }
        Held(slots)
    }

    impl Drop for Held {
        fn drop(&mut self) {
            for &slot in &self.0 {
                BUSY[slot].store(0, Ordering::Release);
            }
        }
    }

    /// Whether this thread's in-flight word is set.
    fn in_flight() -> bool {
        // Safety: the word is this thread's own.
        let word = unsafe { tw_test_in_flight() };
        // Safety: as above, and only this thread writes it.
        unsafe { word.read() != 0 }
    }

    /// Runs `read` with this thread's in-flight word set, the way a read already in flight on the
    /// thread leaves it, and says whether the word was still set when `read` returned.
    fn inside_a_read(read: impl FnOnce()) -> bool {
        // Safety: the word is this thread's own.
        let word = unsafe { tw_test_in_flight() };
        // Safety: as above, and only this thread writes it.
        unsafe { word.write(1) };
        read();
        let still = in_flight();
        // Safety: as above.
        unsafe { word.write(0) };
        still
    }

    /// Runs the nanotime shim and checks its value against reads taken on either side of it and
    /// its callee-saved registers.
    fn nanotime_through_the_shim() {
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

    /// Runs the walltime shim and checks both its slots against reads taken on either side of it.
    fn walltime_through_the_shim() {
        let before = now(libc::CLOCK_REALTIME);
        let mut sec: i64 = -1;
        let mut nsec: i32 = -1;
        // Safety: both pointers are live and correctly typed for the harness to fill.
        unsafe { tw_test_invoke_walltime(&raw mut sec, &raw mut nsec) };
        let after = now(libc::CLOCK_REALTIME);

        assert!(
            (0..1_000_000_000).contains(&nsec),
            "the nanoseconds slot holds {nsec}"
        );
        let read = sec * 1_000_000_000 + i64::from(nsec);
        assert!(
            before <= read && read <= after,
            "the shim wrote {read}, outside the reads around it at {before} and {after}"
        );
    }

    #[test]
    fn the_nanotime_shim_writes_the_slot_and_keeps_the_callee_saved_registers() {
        let _alone = alone();
        assert!(install_arena(), "the arena maps");
        assert_ne!(nanotime_handler(), 0);

        nanotime_through_the_shim();
        assert!(!in_flight(), "the read left its thread marked as reading");
    }

    #[test]
    fn the_walltime_shim_writes_both_result_slots() {
        let _alone = alone();
        assert!(install_arena(), "the arena maps");
        assert_ne!(walltime_handler(), 0);

        walltime_through_the_shim();
        assert!(!in_flight(), "the read left its thread marked as reading");
    }

    #[test]
    fn a_read_runs_on_the_slot_its_thread_hashes_to_and_gives_it_back() {
        let _alone = alone();
        assert!(install_arena(), "the arena maps");

        let at = read_through_the_entry();
        assert_eq!(
            arena_slot_of(at),
            Some(own_slot()),
            "the read ran at {at:#x}"
        );
        assert_eq!(BUSY[own_slot()].load(Ordering::Acquire), 0);
        assert!(!in_flight(), "the read left its thread marked as reading");
    }

    #[test]
    fn a_read_whose_slot_another_thread_holds_runs_on_a_stack_of_its_own() {
        let _alone = alone();
        assert!(install_arena(), "the arena maps");
        let home = own_slot();
        let next = (home + 1) % SLOTS;

        let held = hold([home]);
        let at = read_through_the_entry();
        assert_ne!(
            at, GENUINE_MARK,
            "a read with no read of its own in flight took the genuine path"
        );
        assert_eq!(
            arena_slot_of(at),
            Some(next),
            "the read ran at {at:#x}, not on the next slot's stack"
        );
        assert_eq!(
            BUSY[home].load(Ordering::Acquire),
            1,
            "the read freed a slot it never held"
        );
        assert_eq!(
            BUSY[next].load(Ordering::Acquire),
            0,
            "the read kept the slot it ran on"
        );

        // The shims themselves, on the same next slot, where the read path leaves the caller's
        // stack pointer in the top word.
        let top = ARENA_BASE.load(Ordering::Acquire) + (next + 1) * STACK_SIZE;
        // Safety: the word is inside the arena, and nothing else runs on that stack meanwhile.
        let saved = unsafe { &*std::ptr::with_exposed_provenance::<AtomicU64>(top - 8) };
        saved.store(0, Ordering::Relaxed);
        nanotime_through_the_shim();
        assert_ne!(
            saved.load(Ordering::Relaxed),
            0,
            "the nanotime shim took the genuine path"
        );
        walltime_through_the_shim();
        drop(held);
        assert!(!in_flight(), "the read left its thread marked as reading");
    }

    #[test]
    fn a_read_with_every_slot_held_runs_on_a_spare_and_the_next_one_reuses_it() {
        let _alone = alone();
        assert!(install_arena(), "the arena maps");

        let held = hold(0..SLOTS);
        let at = read_through_the_entry();
        assert_ne!(
            at, GENUINE_MARK,
            "a read with no read of its own in flight took the genuine path"
        );
        assert_eq!(
            arena_slot_of(at),
            None,
            "the read ran on a held slot's stack"
        );
        let spare = spare_of(at).expect("the read ran on a spare's stack");
        assert_eq!(
            header_word(spare).load(Ordering::Acquire),
            0,
            "the read kept its spare"
        );

        // A free spare serves the next read without another mapping.
        let mapped = spares().len();
        let again = read_through_the_entry();
        assert!(spare_of(again).is_some(), "the read ran at {again:#x}");
        assert_eq!(
            spares().len(),
            mapped,
            "a read mapped a spare with one free"
        );

        // The shims themselves, on a spare.
        nanotime_through_the_shim();
        walltime_through_the_shim();
        assert_eq!(
            spares().len(),
            mapped,
            "a read mapped a spare with one free"
        );
        drop(held);
        assert!(!in_flight(), "the read left its thread marked as reading");
    }

    #[test]
    fn a_read_with_every_slot_and_spare_held_maps_a_new_spare() {
        let _alone = alone();
        assert!(install_arena(), "the arena maps");

        let held = hold(0..SLOTS);
        // At least one spare exists, and every one is held.
        let first = read_through_the_entry();
        assert!(spare_of(first).is_some(), "the read ran at {first:#x}");
        let before = spares();
        for &header in &before {
            assert_eq!(header_word(header).swap(1, Ordering::AcqRel), 0);
        }

        let at = read_through_the_entry();
        let after = spares();
        assert_eq!(after.len(), before.len() + 1, "no spare was mapped");
        assert_eq!(
            spare_of(at),
            after.first().copied(),
            "the read ran at {at:#x}"
        );
        assert_eq!(
            after.get(1..),
            Some(before.as_slice()),
            "the new spare was not pushed in front of the others"
        );
        assert_eq!(
            after
                .first()
                .map(|&header| header_word(header).load(Ordering::Acquire)),
            Some(0),
            "the read kept its spare"
        );

        for &header in &before {
            header_word(header).store(0, Ordering::Release);
        }
        drop(held);
        assert!(!in_flight(), "the read left its thread marked as reading");
    }

    /// The stacks reads of [`occupy_the_stack`] are running on, by the address of each one's top.
    static OCCUPIED: Mutex<BTreeSet<usize>> = Mutex::new(BTreeSet::new());

    /// Set by a read of [`occupy_the_stack`] that found its stack occupied, or ran on none of ours.
    static SHARED: AtomicBool = AtomicBool::new(false);

    /// An entry that marks the stack it runs on as occupied while it runs, and notes a stack it
    /// finds occupied already.
    unsafe extern "C" fn occupy_the_stack(out: *mut usize, _nanos: *mut i32) {
        let marker = 0_u8;
        let at = std::hint::black_box(&raw const marker).addr();
        let base = ARENA_BASE.load(Ordering::Acquire);
        let top = arena_slot_of(at)
            .map(|slot| base + (slot + 1) * STACK_SIZE)
            .or_else(|| spare_of(at));
        let occupied = || OCCUPIED.lock().unwrap_or_else(PoisonError::into_inner);
        if top.is_none_or(|top| !occupied().insert(top)) {
            SHARED.store(true, Ordering::Relaxed);
        }
        // Lingers, so reads that overlap meet here.
        for _ in 0..64 {
            std::hint::spin_loop();
        }
        if let Some(top) = top {
            occupied().remove(&top);
        }
        // Safety: the harness passes its own result slot.
        unsafe { out.write(at) };
    }

    #[test]
    fn reads_on_many_threads_at_once_never_take_the_genuine_path_or_share_a_stack() {
        let _alone = alone();
        assert!(install_arena(), "the arena maps");

        // Four free slots for many threads, so the walks meet and spill onto spares.
        let held = hold(4..SLOTS);
        let mut readers = Vec::new();
        for _ in 0..32 {
            readers.push(std::thread::spawn(|| {
                (0..2_000).all(|_| {
                    let mut out = 0;
                    // Safety: `out` is live, and both routines are the harness's own.
                    unsafe { tw_test_enter(&raw mut out, occupy_the_stack, tw_test_genuine) };
                    out != GENUINE_MARK
                })
            }));
        }
        let faked = readers.into_iter().fold(true, |all, reader| {
            reader.join().expect("a reader ran") && all
        });
        drop(held);

        assert!(
            faked,
            "a read with no read of its own in flight took the genuine path"
        );
        assert!(
            !SHARED.load(Ordering::Relaxed),
            "two reads ran on one stack at once"
        );
    }

    #[test]
    fn a_nested_read_takes_the_genuine_path_and_claims_no_stack() {
        let _alone = alone();
        assert!(install_arena(), "the arena maps");

        let mut at = 0;
        let still = inside_a_read(|| at = read_through_the_entry());
        assert_eq!(at, GENUINE_MARK, "a nested read ran at {at:#x}");
        assert!(
            still,
            "the genuine path cleared the outer read's in-flight word"
        );
        assert!(
            BUSY.iter().all(|slot| slot.load(Ordering::Acquire) == 0),
            "a nested read claimed a slot"
        );
    }

    #[test]
    fn a_nested_read_sends_the_nanotime_shim_down_its_genuine_path() {
        let _alone = alone();
        assert!(install_arena(), "the arena maps");

        let still = inside_a_read(nanotime_through_the_shim);
        assert!(
            still,
            "the genuine path cleared the outer read's in-flight word"
        );
    }

    #[test]
    fn a_nested_read_sends_the_walltime_shim_down_its_genuine_path() {
        let _alone = alone();
        assert!(install_arena(), "the arena maps");

        let still = inside_a_read(walltime_through_the_shim);
        assert!(
            still,
            "the genuine path cleared the outer read's in-flight word"
        );
    }

    #[test]
    fn every_thread_has_an_in_flight_word_of_its_own() {
        let _alone = alone();
        // Safety: the word is this thread's own.
        let here = unsafe { tw_test_in_flight() }.addr();
        let (there, set) = std::thread::spawn(|| {
            // Safety: as above, on the spawned thread.
            let word = unsafe { tw_test_in_flight() };
            (word.addr(), in_flight())
        })
        .join()
        .unwrap();
        assert_ne!(here, there);
        assert!(!set, "a new thread starts with a read in flight");
    }
}
