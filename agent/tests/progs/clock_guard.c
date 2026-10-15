/* Runs argv[1..] under a seccomp filter that refuses with EPERM every system call that could move
   a Linux clock: clock_settime, clock_adjtime, adjtimex, and settimeofday with either pointer set.
   A settimeofday with both pointers null sets nothing and is let through, so the kernel's own
   privilege check still answers the library's probe.

   This is what lets a target hold CAP_SYS_TIME without any way to use it: whatever the library
   does, or fails to do, a genuine set from the target or anything it starts is refused before the
   kernel looks at it. The filter refuses every call of any other system call ABI, which is how a
   64-bit process could otherwise reach the same calls under other numbers.

   Exits 126 without running anything if the filter cannot be installed. */
#ifdef __linux__
#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <stdio.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>

#if defined(__x86_64__)
#define NATIVE AUDIT_ARCH_X86_64
/* The x32 ABI shares the arch value and marks its numbers with this bit. */
#define FOREIGN_NUMBERS 0x40000000
#elif defined(__aarch64__)
#define NATIVE AUDIT_ARCH_AARCH64
#define FOREIGN_NUMBERS 0xffffffff
#else
#error "clock_guard knows the system call ABI of x86_64 and aarch64 only"
#endif

#define REFUSE (SECCOMP_RET_ERRNO | (EPERM & SECCOMP_RET_DATA))
#define ARG_LOW(n) (offsetof(struct seccomp_data, args) + (n) * sizeof(__u64))
#define ARG_HIGH(n) (ARG_LOW(n) + sizeof(__u32))

static struct sock_filter filter[] = {
    /* 0 */ BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
    /* 1 */ BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, NATIVE, 1, 0),
    /* 2 */ BPF_STMT(BPF_RET | BPF_K, REFUSE),
    /* 3 */ BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
    /* 4 */ BPF_JUMP(BPF_JMP | BPF_JGE | BPF_K, FOREIGN_NUMBERS, 13, 0),
    /* 5 */ BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_clock_settime, 12, 0),
    /* 6 */ BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_clock_adjtime, 11, 0),
    /* 7 */ BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_adjtimex, 10, 0),
    /* 8 */ BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_settimeofday, 1, 0),
    /* 9 */ BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    /* settimeofday: through only when all four halves of its two pointers are zero. */
    /* 10 */ BPF_STMT(BPF_LD | BPF_W | BPF_ABS, ARG_LOW(0)),
    /* 11 */ BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 6),
    /* 12 */ BPF_STMT(BPF_LD | BPF_W | BPF_ABS, ARG_HIGH(0)),
    /* 13 */ BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 4),
    /* 14 */ BPF_STMT(BPF_LD | BPF_W | BPF_ABS, ARG_LOW(1)),
    /* 15 */ BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 2),
    /* 16 */ BPF_STMT(BPF_LD | BPF_W | BPF_ABS, ARG_HIGH(1)),
    /* 17 */ BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 0, 1, 0),
    /* 18 */ BPF_STMT(BPF_RET | BPF_K, REFUSE),
    /* 19 */ BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
};

int main(int argc, char **argv) {
    struct sock_fprog program = {
        .len = (unsigned short)(sizeof filter / sizeof filter[0]),
        .filter = filter,
    };

    if (argc < 2) {
        fprintf(stderr, "clock_guard: no command to run\n");
        return 126;
    }
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) {
        perror("clock_guard: PR_SET_NO_NEW_PRIVS");
        return 126;
    }
    if (prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program) != 0) {
        perror("clock_guard: PR_SET_SECCOMP");
        return 126;
    }
    execvp(argv[1], argv + 1);
    perror("clock_guard: exec");
    return 126;
}
#else
#include <stdio.h>

int main(void) {
    fprintf(stderr, "clock_guard: only Linux has the filter this needs\n");
    return 126;
}
#endif
