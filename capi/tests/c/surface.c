/*
 * Drives the whole of libtimewarp_controller.h from C: the reads, the
 * transaction calls, and the error codes. Everything it learns goes to stdout
 * as key=value lines, which the case around it checks.
 *
 * Usage: surface <affine.session> <null.session> <missing.session>
 */

#include "libtimewarp_controller.h"

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define HOUR 3600000000000LL

static void say(const char *key, long long value)
{
    printf("%s=%lld\n", key, value);
}

/* The code, and whether a message came back beside it. */
static void code(const char *key, int32_t status)
{
    say(key, status);
    printf("%s.message=%s\n", key, libtimewarp_last_error());
}

/* The shape of the struct, so a header that drifted from the library is caught
   here rather than by a field that silently reads the wrong bytes. */
static void layout(void)
{
    say("layout.sizeof", (long long)sizeof(struct libtimewarp_info));
    say("layout.keeper", (long long)offsetof(struct libtimewarp_info, keeper));
    say("layout.emulate_vdso", (long long)offsetof(struct libtimewarp_info, emulate_vdso));
    say("layout.fail_closed", (long long)offsetof(struct libtimewarp_info, fail_closed));
    say("layout.creator_pid", (long long)offsetof(struct libtimewarp_info, creator_pid));
    say("layout.creation_time", (long long)offsetof(struct libtimewarp_info, creation_time));
    say("layout.fault_code", (long long)offsetof(struct libtimewarp_info, fault_code));
    say("layout.fault_pid", (long long)offsetof(struct libtimewarp_info, fault_pid));
    say("layout.rate", (long long)offsetof(struct libtimewarp_info, rate));
    say("layout.timebase", (long long)offsetof(struct libtimewarp_info, timebase));
    say("layout.hook_path", (long long)offsetof(struct libtimewarp_info, hook_path));
}

static void report(const char *tag, const struct libtimewarp_info *info)
{
    printf("%s.keeper=%u\n", tag, info->keeper);
    printf("%s.emulate_vdso=%u\n", tag, info->emulate_vdso);
    printf("%s.fail_closed=%u\n", tag, info->fail_closed);
    printf("%s.creator_pid=%u\n", tag, info->creator_pid);
    printf("%s.creation_time=%lld\n", tag, (long long)info->creation_time);
    printf("%s.fault_code=%u\n", tag, info->fault_code);
    printf("%s.fault_pid=%u\n", tag, info->fault_pid);
    printf("%s.hook_path=%s\n", tag, info->hook_path);
    for (int family = 0; family < 4; family++) {
        printf("%s.rate.%d=%.6f\n", tag, family, info->rate[family]);
        printf("%s.timebase.%d=%d\n", tag, family, info->timebase[family]);
    }
}

/* The reads, over an affine session. */
static int reads(struct libtimewarp_controller *session)
{
    struct libtimewarp_info info;
    int64_t showing = 0;

    memset(&info, 0, sizeof info);
    code("info.status", libtimewarp_info(session, &info));
    report("info", &info);

    code("get.realtime.status", libtimewarp_get(session, LIBTIMEWARP_REALTIME, &showing));
    say("get.realtime", (long long)showing);
    code("get.monotonic.status", libtimewarp_get(session, LIBTIMEWARP_MONOTONIC, &showing));
    say("get.monotonic", (long long)showing);
    code("get.boottime.status", libtimewarp_get(session, LIBTIMEWARP_BOOTTIME, &showing));
    say("get.boottime", (long long)showing);

    /* Cpu has no single value it shows now, and no family carries the number
       after the last one. */
    code("get.cpu.status", libtimewarp_get(session, LIBTIMEWARP_CPU, &showing));
    code("get.nofamily.status", libtimewarp_get(session, 4, &showing));
    code("get.null_out.status", libtimewarp_get(session, LIBTIMEWARP_REALTIME, NULL));
    return 0;
}

/* The transaction calls: one that lands, one that is discarded, and two that
   are refused. */
static int changes(struct libtimewarp_controller *session)
{
    struct libtimewarp_transaction *transaction = NULL;
    struct libtimewarp_info info;
    int64_t showing = 0;

    code("begin.status", libtimewarp_begin(session, &transaction));
    code("shift.status", libtimewarp_shift(transaction, LIBTIMEWARP_REALTIME, HOUR));
    code("set_rate.status", libtimewarp_set_rate(transaction, LIBTIMEWARP_MONOTONIC, 2.0));
    /* Staged, and so not yet visible to anyone. */
    libtimewarp_get(session, LIBTIMEWARP_REALTIME, &showing);
    say("staged.realtime", (long long)showing);
    code("commit.status", libtimewarp_commit(transaction));
    libtimewarp_get(session, LIBTIMEWARP_REALTIME, &showing);
    say("committed.realtime", (long long)showing);

    memset(&info, 0, sizeof info);
    libtimewarp_info(session, &info);
    printf("committed.rate.monotonic=%.6f\n", info.rate[LIBTIMEWARP_MONOTONIC]);

    /* An abort writes nothing. */
    code("begin2.status", libtimewarp_begin(session, &transaction));
    code("set_time.status",
         libtimewarp_set_time(transaction, LIBTIMEWARP_REALTIME, 2000000000000000000LL));
    libtimewarp_abort(transaction);
    libtimewarp_get(session, LIBTIMEWARP_REALTIME, &showing);
    say("aborted.realtime", (long long)showing);

    /* Cpu's rate is fixed at creation, and a negative rate is no rate. */
    code("begin3.status", libtimewarp_begin(session, &transaction));
    code("cpu_rate.status", libtimewarp_set_rate(transaction, LIBTIMEWARP_CPU, 2.0));
    code("negative_rate.status", libtimewarp_set_rate(transaction, LIBTIMEWARP_REALTIME, -1.0));
    libtimewarp_abort(transaction);

    /* A backwards move on a nondecreasing family is judged at the commit. */
    code("begin4.status", libtimewarp_begin(session, &transaction));
    code("backwards.status",
         libtimewarp_shift(transaction, LIBTIMEWARP_MONOTONIC, -HOUR));
    code("backwards.commit.status", libtimewarp_commit(transaction));
    return 0;
}

/* A session on the null keeper: reads, info and the fault slot work, and every
   change is refused. */
static int null_keeper(const char *path)
{
    struct libtimewarp_controller *session = NULL;
    struct libtimewarp_transaction *transaction = NULL;
    struct libtimewarp_info info;
    int64_t showing = 0;

    code("null.attach.status", libtimewarp_attach(path, &session));
    memset(&info, 0, sizeof info);
    code("null.info.status", libtimewarp_info(session, &info));
    report("null.info", &info);
    code("null.get.status", libtimewarp_get(session, LIBTIMEWARP_REALTIME, &showing));
    say("null.get.realtime", (long long)showing);
    code("null.begin.status", libtimewarp_begin(session, &transaction));
    libtimewarp_close(session);
    return 0;
}

int main(int argc, char **argv)
{
    struct libtimewarp_controller *session = NULL;

    if (argc != 4) {
        fprintf(stderr, "usage: %s <affine> <null> <missing>\n", argv[0]);
        return 2;
    }
    layout();

    /* Nothing at that path, and a pointer where a path should be. */
    code("missing.status", libtimewarp_attach(argv[3], &session));
    code("null_path.status", libtimewarp_attach(NULL, &session));

    code("attach.status", libtimewarp_attach(argv[1], &session));
    if (session == NULL) {
        fprintf(stderr, "attaching to %s gave no handle\n", argv[1]);
        return 2;
    }
    reads(session);
    changes(session);
    libtimewarp_close(session);

    null_keeper(argv[2]);

    /* A null handle is nothing to release, and nothing to discard. */
    libtimewarp_close(NULL);
    libtimewarp_abort(NULL);
    printf("done=1\n");
    return 0;
}
