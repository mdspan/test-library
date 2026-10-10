// Reads the clock the two ways one process can on FreeBSD, over and over: through the Go runtime,
// which reads it through its own clock fallback routines, and through libc, which the library
// interposes. With the timekeep auxiliary-vector entry withheld, both reach the library's one read
// path, so a case that changes the session underneath can watch that the two readers move together.
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
	"io"
	"os"
	"runtime/pprof"
	"strconv"
	"time"
)

func main() {
	// A profiling run turns on CPU profiling, so SIGPROF fires while the loop below reads the
	// clock and a sample taken mid-read exercises the shim's reentrancy guard. The profile is
	// discarded; running under it is the point.
	if os.Getenv("TIMEWARP_TEST_GO_PROFILE") != "" {
		if pprof.StartCPUProfile(io.Discard) == nil {
			defer pprof.StopCPUProfile()
		}
	}

	count := argument(1, 20000)
	pause := time.Duration(argument(2, 200)) * time.Microsecond
	out := bufio.NewWriter(os.Stdout)

	for taken := 0; taken < count; taken++ {
		fallback := time.Now().UnixNano()
		libc := int64(C.libc_realtime())
		fmt.Fprintf(out, "pair=%d %d\n", fallback, libc)
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
