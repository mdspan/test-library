/* Reports the errno `main` starts with, which C requires to be zero. */
#include <errno.h>
#include <stdio.h>

int main(void) {
    int at_start = errno;

    printf("main.errno=%d\n", at_start);
    fflush(stdout);
    return 0;
}
