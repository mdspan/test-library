/* A process of a faked tree for the tool to find: it carries the session path in its environment
   and does nothing else until the case that started it kills it.

   A program of the suite's own rather than one of the system's, because macOS keeps the
   environment of a protected binary out of what `ps` will print, and that is how the tool reads
   the session path out of a process there. */
#include <unistd.h>

int main(void) {
    sleep(30);
    return 0;
}
