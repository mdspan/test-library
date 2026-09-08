#include <sys/wait.h>

#include "report.h"

int main(void) {
    pid_t child = fork();
    if (child == 0) {
        report("child");
        _exit(0);
    }
    waitpid(child, NULL, 0);
    report("parent");
    return 0;
}
