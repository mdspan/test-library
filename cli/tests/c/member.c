/* A process of a faked tree for the tool to find: it carries the session path in its environment
   and does nothing else until the case that started it kills it.

   Built by the suite: macOS withholds the environment of a protected system binary from other
   processes. */
#include <unistd.h>

int main(void) {
    sleep(30);
    return 0;
}
