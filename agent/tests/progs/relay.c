#include "report.h"

/* argv[1] is this run's own report tag. When more follow, argv[2] is the next program to exec
   into and argv[3..] become its own arguments, argv[0] included. */
int main(int argc, char **argv) {
    if (argc < 2) {
        return 1;
    }
    report(argv[1]);
    if (argc < 3) {
        return 0;
    }
    execv(argv[2], &argv[2]);
    return 1;
}
