#include "report.h"

/* Execs argv[1] with the environment `main` was handed. musl, FreeBSD, and macOS hand over the
   array the kernel laid out, glibc the one its `setenv` keeps. */
int main(int argc, char **argv, char **envp) {
    char *args[3];

    report("before");
    if (argc < 2) {
        return 1;
    }
    args[0] = argv[1];
    args[1] = (char *)"after";
    args[2] = NULL;
    execve(argv[1], args, envp);
    return 1;
}
