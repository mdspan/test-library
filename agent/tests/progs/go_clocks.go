// Reads the clocks a Go program reads. The runtime skips libc for all of them, so nothing here is
// caught by interposing a symbol: `time.Now` goes through the object the auxiliary vector names,
// and `os.Stat` and the kernel side of a sleep are raw system calls.
//
// argv[1] picks what to do. `now` reports the wall clock and a monotonic span; `stat <dir>` writes
// a file and reports the timestamp it reads back beside the clock; `sleep <ms>` sleeps and reports
// what the runtime made of the span.
package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"time"
)

func main() {
	mode := "now"
	if len(os.Args) > 1 {
		mode = os.Args[1]
	}
	switch mode {
	case "now":
		now()
	case "stat":
		stat(argument(2))
	case "sleep":
		sleep(argument(2))
	default:
		fmt.Fprintf(os.Stderr, "unknown mode %q\n", mode)
		os.Exit(2)
	}
}

func argument(at int) string {
	if len(os.Args) <= at {
		fmt.Fprintf(os.Stderr, "this mode wants an argument\n")
		os.Exit(2)
	}
	return os.Args[at]
}

// The wall clock, and a monotonic span the runtime measures for itself.
func now() {
	start := time.Now()
	spin := 0
	for time.Since(start) < time.Millisecond {
		spin++
	}
	fmt.Printf("go.now=%d\n", time.Now().UnixNano())
	fmt.Printf("go.since=%d\n", time.Since(start).Nanoseconds())
}

// A file written just now, and what the runtime says its modification time is.
func stat(dir string) {
	path := filepath.Join(dir, "written-by-go")
	if err := os.WriteFile(path, []byte("written\n"), 0o600); err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}
	info, err := os.Stat(path)
	if err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}
	fmt.Printf("go.now=%d\n", time.Now().UnixNano())
	fmt.Printf("go.stat=%d\n", info.ModTime().UnixNano())
}

// A sleep, and the span the runtime measured across it on its own clock.
func sleep(milliseconds string) {
	asked, err := strconv.Atoi(milliseconds)
	if err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}
	start := time.Now()
	time.Sleep(time.Duration(asked) * time.Millisecond)
	fmt.Printf("go.sleep.asked=%d\n", int64(asked)*int64(time.Millisecond))
	fmt.Printf("go.sleep.measured=%d\n", time.Since(start).Nanoseconds())
	fmt.Printf("go.now=%d\n", time.Now().UnixNano())
}
