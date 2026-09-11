// Reads the clock the two ways one process can on FreeBSD, over and over: through the Go runtime,
// which computes it from the timekeep page the auxiliary vector names, and through libc, which the
// library interposes. A case that changes the session underneath can then watch what each reader
// sees, and whether the two ever disagree in a way that runs a clock backwards.
//
// The libc reading goes through cgo, because that is the only way a Go program calls libc at all:
// every clock the runtime reads for itself skips it.
//
// argv[1] is how many pairs to print at most, argv[2] how many microseconds to pause between them.
// The pause is faketime like everything else the runtime measures, which is what a case watching
// for a genuine duration allows for.
package main

/*
#include <time.h>

static long long libc_realtime(void) {
    struct timespec ts;

    if (clock_gettime(CLOCK_REALTIME, &ts) != 0) {
        return 0;
    }
    return (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}
*/
import "C"

import (
	"bufio"
	"fmt"
	"os"
	"strconv"
	"time"
)

func main() {
	count := argument(1, 20000)
	pause := time.Duration(argument(2, 200)) * time.Microsecond
	out := bufio.NewWriter(os.Stdout)

	for taken := 0; taken < count; taken++ {
		page := time.Now().UnixNano()
		libc := int64(C.libc_realtime())
		fmt.Fprintf(out, "pair=%d %d\n", page, libc)
		// Flushed as it goes, so a case watching from outside sees the readings while they are
		// being taken rather than when the program ends.
		if out.Flush() != nil {
			return
		}
		time.Sleep(pause)
	}
}

func argument(at int, fallback int) int {
	if len(os.Args) <= at {
		return fallback
	}
	value, err := strconv.Atoi(os.Args[at])
	if err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}
	return value
}
