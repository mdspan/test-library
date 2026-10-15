#include "report.h"

int main(int argc, char **argv) {
    report(argc > 1 ? argv[1] : "probe");
    return 0;
}
