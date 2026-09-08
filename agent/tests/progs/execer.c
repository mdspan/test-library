#include "report.h"

/* argv[1] is the probe to exec. */
int main(int argc, char **argv) {
    char *args[3];

    report("before");
    if (argc < 2) {
        return 1;
    }
    args[0] = argv[1];
    args[1] = (char *)"after";
    args[2] = NULL;
    execv(argv[1], args);
    return 1;
}
