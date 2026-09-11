/*
 * libtimewarp_controller.h -- changing and reading a live libtimewarp session.
 *
 * A session is the shared configuration a faked process tree computes its
 * clocks from. This header is how a program outside that tree reads what the
 * session is doing and changes it. It declares the whole surface of
 * libtimewarp_controller.so; the Python tool shipped with the library is one
 * caller of it.
 *
 * Link with, for example:
 *
 *     cc -o mytool mytool.c -ltimewarp_controller
 *
 * A session is attached to by the path of its session file. Names are
 * generated, so there is no name to type: the path comes from
 * LIBTIMEWARP__SHARED_FILE in the environment of any process of the tree, or
 * from reading the session directory, whose entries are told apart by the
 * creator pid and creation time libtimewarp_info reports.
 *
 * Every call returns a code from enum libtimewarp_error, zero being success,
 * and leaves a message beside it that libtimewarp_last_error hands back.
 *
 * This library offers primitives only. Anything with memory, like a freeze
 * that remembers the rate to come back to, belongs one level up, in the
 * program that drives it.
 *
 * None of the calls below is thread safe on one handle: a handle is used from
 * one thread at a time. Handles of different sessions are independent, and the
 * write lock a transaction holds serializes writers across processes.
 */

#ifndef LIBTIMEWARP_CONTROLLER_H
#define LIBTIMEWARP_CONTROLLER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Long enough for PATH_MAX, which is what the session file reserves. */
#define LIBTIMEWARP_HOOK_PATH_MAX 4096

/*
 * Why a call did not do what it was asked. A number is fixed for good: a code
 * added later takes a new one rather than renumbering its neighbours.
 */
enum libtimewarp_error {
    /* The call did what it was asked. */
    LIBTIMEWARP_OK = 0,
    /* No session file at that path. */
    LIBTIMEWARP_NO_SESSION = 1,
    /* The file exists and is not a session this library can read. */
    LIBTIMEWARP_BAD_SESSION_FILE = 2,
    /* A value out of range: a negative rate, a rate above the ceiling, a
       Realtime value outside its range, an unknown family, or an operation the
       family has no meaning for. */
    LIBTIMEWARP_BAD_ARGUMENT = 3,
    /* A refused backwards move on Monotonic or Boottime. */
    LIBTIMEWARP_WOULD_GO_BACKWARDS = 4,
    /* A change on a session running the null keeper, which has no parameters
       to change. Reads and info work there; every change is refused. */
    LIBTIMEWARP_UNSUPPORTED_FOR_KEEPER = 5,
    /* The file or its directory is owned by another uid, or is the wrong kind
       of file. */
    LIBTIMEWARP_NOT_OURS = 6,
    /* Anything else the operating system refused. */
    LIBTIMEWARP_IO = 7
};

/*
 * A logical grouping of clocks, and the atomic unit of configuration: every
 * change lands on a whole family at once, and every clock of a family reads
 * the family's value. The numbers index the arrays in struct libtimewarp_info.
 */
enum libtimewarp_family {
    /* The wall clock. */
    LIBTIMEWARP_REALTIME = 0,
    /* Nondecreasing, stops across suspend. */
    LIBTIMEWARP_MONOTONIC = 1,
    /* Nondecreasing, keeps counting across suspend. */
    LIBTIMEWARP_BOOTTIME = 2,
    /* Process and thread CPU time. It has one setting, its rate, fixed when
       the session was created, and no single value it shows now. */
    LIBTIMEWARP_CPU = 3
};

/* Which timekeeper computes the families' values. */
enum libtimewarp_keeper {
    /* A line per family over a genuine timebase. This is the one a controller
       can change. */
    LIBTIMEWARP_KEEPER_AFFINE = 1,
    /* Every family reads genuine, which is what a fault leaves behind. */
    LIBTIMEWARP_KEEPER_NULL = 2
};

/* Why a session stopped faking. Zero is not a fault: it is what the slot holds
   while the session is still faking. */
enum libtimewarp_fault {
    /* The environment asked for something the library could not set up. */
    LIBTIMEWARP_FAULT_BAD_VALUE = 1
};

/*
 * An attached session. Opaque: the only thing to do with one is pass it back.
 */
struct libtimewarp_controller;

/*
 * A change in flight. Opaque in the same way, and spent by
 * libtimewarp_commit or libtimewarp_abort.
 */
struct libtimewarp_transaction;

/*
 * What libtimewarp_info fills in.
 *
 * The three families that carry a line are read as one whole copy, so the
 * rates and timebases here belong to one moment rather than to three.
 */
struct libtimewarp_info {
    /* From enum libtimewarp_keeper. */
    uint32_t keeper;
    /* 1 when the session poses as the vDSO on Linux, or substitutes the
       timekeep page on FreeBSD. */
    uint32_t emulate_vdso;
    /* 1 when a process that cannot set up what it was asked for exits rather
       than running on genuine time. */
    uint32_t fail_closed;
    /* The process that created the session. */
    uint32_t creator_pid;
    /* Genuine CLOCK_REALTIME when the session was created, in nanoseconds.
       With the creator pid, this is what tells two files of one directory
       apart after a pid has been recycled. */
    int64_t creation_time;
    /* From enum libtimewarp_fault, or 0 while the session is still faking. */
    uint32_t fault_code;
    /* The process that hit the fault. Meaningful only beside a nonzero code. */
    uint32_t fault_pid;
    /* Each family's rate: fake nanoseconds per nanosecond of its timebase,
       indexed by enum libtimewarp_family. 0 is a frozen family. */
    double rate[4];
    /* Each family's timebase, the genuine clock id passage is measured
       against, indexed the same way. LIBTIMEWARP_CPU tracks no timebase, and
       its slot is 0. */
    int32_t timebase[4];
    /* The hook library every process of the session loads, NUL-terminated.
       Empty when the session has no hook. */
    char hook_path[LIBTIMEWARP_HOOK_PATH_MAX];
};

/*
 * The message beside the code the last call on this thread returned:
 * NUL-terminated, never NULL, and empty after a call that succeeded.
 *
 * The string belongs to the library and stays valid until the next call on
 * this thread. The code is the stable part; the message names the particular
 * value or file and may be reworded.
 */
const char *libtimewarp_last_error(void);

/*
 * Attaches by the path of the session file, applying the same security checks
 * a faked process applies: the directory and the file must be owned by the
 * effective uid, the file must be a regular file, and neither a planted
 * symlink nor a planted FIFO is followed.
 *
 * On success *out holds a session handle, which libtimewarp_close releases.
 * A session running the null keeper opens like any other: reads, info and the
 * fault slot all work, and libtimewarp_begin returns
 * LIBTIMEWARP_UNSUPPORTED_FOR_KEEPER.
 */
int32_t libtimewarp_attach(const char *path, struct libtimewarp_controller **out);

/*
 * Releases a session handle. A NULL handle is nothing to release.
 *
 * A transaction open on the session keeps the session mapped on its own, so
 * closing here first is allowed; the transaction still has to be committed or
 * aborted.
 */
void libtimewarp_close(struct libtimewarp_controller *controller);

/*
 * What the family shows now, in nanoseconds, written to *out.
 *
 * This is the read path a faked process takes, so the answer is the one every
 * process of the session is getting at this instant. A session on the null
 * keeper answers with the genuine reading.
 *
 * LIBTIMEWARP_CPU has no single value it shows now, because each of its clocks
 * scales its own consumption, so it returns LIBTIMEWARP_BAD_ARGUMENT.
 */
int32_t libtimewarp_get(const struct libtimewarp_controller *controller,
                        uint32_t family, int64_t *out);

/*
 * How the session was set up, what each family is doing, and what went wrong
 * with it, written to *out.
 *
 * This is where a fault is read: a process that could not set up what the
 * environment asked for still publishes or joins a session, on the null keeper
 * with no hook, with the fault recorded in it.
 */
int32_t libtimewarp_info(const struct libtimewarp_controller *controller,
                         struct libtimewarp_info *out);

/*
 * Takes the write lock, and nothing else. On success *out holds a transaction
 * handle.
 *
 * Readers keep reading the old parameters for as long as the transaction is
 * open; only other writers wait, and that wait has no bound, because a held
 * lock means a live writer that will finish. A controller that dies between
 * begin and commit releases the lock through the kernel and leaves nothing
 * behind.
 */
int32_t libtimewarp_begin(struct libtimewarp_controller *controller,
                          struct libtimewarp_transaction **out);

/*
 * Stages a move of the family by a duration in nanoseconds. Nothing is written
 * to the session until the commit.
 */
int32_t libtimewarp_shift(struct libtimewarp_transaction *transaction,
                          uint32_t family, int64_t nanos);

/*
 * Stages a value in nanoseconds for the family to show now. Nothing is written
 * to the session until the commit.
 */
int32_t libtimewarp_set_time(struct libtimewarp_transaction *transaction,
                             uint32_t family, int64_t nanos);

/*
 * Stages how fast the family runs: fake nanoseconds per nanosecond of its
 * timebase, with 0 freezing it. A negative rate, one that is not a number, and
 * one at or above the ceiling of 2^32 are refused. Nothing is written to the
 * session until the commit.
 *
 * LIBTIMEWARP_CPU refuses this, and shift and set_time with it: its rate is
 * fixed when the session is created.
 */
int32_t libtimewarp_set_rate(struct libtimewarp_transaction *transaction,
                             uint32_t family, double rate);

/*
 * Applies the staged operations in order against one anchor sampled inside the
 * write section, and publishes once. Every process of the session sees all of
 * the transaction or none of it, on its next read.
 *
 * A refused operation aborts the whole transaction with nothing written, and
 * the code returned is the one that operation refused with. The handle is
 * spent either way, and the write lock is released either way.
 */
int32_t libtimewarp_commit(struct libtimewarp_transaction *transaction);

/*
 * Discards the staged operations and releases the write lock, having written
 * nothing. A NULL handle is nothing to discard.
 */
void libtimewarp_abort(struct libtimewarp_transaction *transaction);

#ifdef __cplusplus
}
#endif

#endif /* LIBTIMEWARP_CONTROLLER_H */
