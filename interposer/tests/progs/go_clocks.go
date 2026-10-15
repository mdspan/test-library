// Reads the clocks a Go program reads. The runtime skips libc for all of them, so nothing here is
// caught by interposing a symbol: `time.Now` reads the clock the runtime's own way, and `os.Stat`
// and the kernel side of a sleep are raw system calls.
//
// argv[1] picks what to do. `now` reports the wall clock and a monotonic span; `stat <dir>` writes
// a file and reports the timestamp it reads back beside the clock; `sleep <ms>` sleeps and reports
// what the runtime made of the span; `rates <ms>` reports how far the wall clock and the monotonic
// clock each move across one faked span, which differ when the two families run at rates of their
// own; `threads <n> <reads>` reads the clock on n threads at once and reports the range of the
// wall readings and how often a thread's monotonic clock stepped back; `signals <reads>` reads the
// wall clock that many times while the process signals itself without pause.
package main

import (
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"runtime/pprof"
	"strconv"
	"sync"
	"syscall"
	"time"
)

// The signals mode reads on the main thread, which is the thread the kernel hands a signal sent to
// the process whenever that thread is running and not blocking it. A goroutine is only certain to
// run there when it is locked before main starts.
func init() {
	if len(os.Args) > 1 && os.Args[1] == "signals" {
		runtime.LockOSThread()
	}
}

func main() {
	// A profiling run turns on CPU profiling for the whole program. SIGPROF then fires about a
	// hundred times a second and the runtime's handler reads the clock, so a sample taken during
	// an in-flight read exercises the shim's reentrancy guard. The profile is discarded; running
	// under it is the point.
	if os.Getenv("TIMEWARP_TEST_GO_PROFILE") != "" {
		if pprof.StartCPUProfile(io.Discard) == nil {
			defer pprof.StopCPUProfile()
		}
	}

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
	case "rates":
		rates(argument(2))
	case "threads":
		threads(argument(2), argument(3))
	case "signals":
		signals(argument(2))
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

func number(text string) int {
	value, err := strconv.Atoi(text)
	if err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}
	return value
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

// How far the wall clock and the runtime's own monotonic clock each move across one faked span.
// The loop runs until the monotonic clock shows the span elapsed, so the wall clock covers the
// same genuine interval; with the two families on rates of their own, the distances differ by the
// ratio of those rates. Both clocks skip libc: `time.Now` for the wall reading and the monotonic
// reading `time.Since` subtracts both reach the runtime's clock fallback.
func rates(milliseconds string) {
	span := time.Duration(number(milliseconds)) * time.Millisecond
	start := time.Now()
	wall := time.Now().UnixNano()
	for time.Since(start) < span {
	}
	fmt.Printf("go.rate.mono=%d\n", time.Since(start).Nanoseconds())
	fmt.Printf("go.rate.wall=%d\n", time.Now().UnixNano()-wall)
}

// A sleep, and the span the runtime measured across it on its own clock.
func sleep(milliseconds string) {
	asked := number(milliseconds)
	start := time.Now()
	time.Sleep(time.Duration(asked) * time.Millisecond)
	fmt.Printf("go.sleep.asked=%d\n", int64(asked)*int64(time.Millisecond))
	fmt.Printf("go.sleep.measured=%d\n", time.Since(start).Nanoseconds())
	fmt.Printf("go.now=%d\n", time.Now().UnixNano())
}

// The clock read on many threads at once: each goroutine is locked to a thread of its own, and
// they all start reading together. Reports the earliest and latest wall reading any of them took,
// and how many times a reading came before the one its thread took just ahead of it on the
// monotonic clock.
func threads(count, reads string) {
	n, r := number(count), number(reads)
	runtime.GOMAXPROCS(n)
	lows := make([]int64, n)
	highs := make([]int64, n)
	backs := make([]int, n)
	start := make(chan struct{})
	var ready, done sync.WaitGroup
	ready.Add(n)
	done.Add(n)
	for i := 0; i < n; i++ {
		go func(i int) {
			defer done.Done()
			runtime.LockOSThread()
			ready.Done()
			<-start
			last := time.Now()
			low, high := last.UnixNano(), last.UnixNano()
			for j := 0; j < r; j++ {
				now := time.Now()
				if now.Before(last) {
					backs[i]++
				}
				low, high = min(low, now.UnixNano()), max(high, now.UnixNano())
				last = now
			}
			lows[i], highs[i] = low, high
		}(i)
	}
	ready.Wait()
	close(start)
	done.Wait()
	low, high, back := lows[0], highs[0], 0
	for i := 0; i < n; i++ {
		low, high, back = min(low, lows[i]), max(high, highs[i]), back+backs[i]
	}
	fmt.Printf("go.threads.low=%d\n", low)
	fmt.Printf("go.threads.high=%d\n", high)
	fmt.Printf("go.threads.back=%d\n", back)
}

// The wall clock read again and again while another goroutine sends the process SIGURG without
// pause, so signals arrive in the middle of reads. SIGURG is the runtime's own preemption signal,
// which it handles on every thread and otherwise ignores.
func signals(count string) {
	reads := number(count)
	done := make(chan struct{})
	sent := make(chan int)
	go func() {
		pid := os.Getpid()
		total := 0
		for {
			select {
			case <-done:
				sent <- total
				return
			default:
			}
			if syscall.Kill(pid, syscall.SIGURG) == nil {
				total++
			}
		}
	}()
	var last time.Time
	for i := 0; i < reads; i++ {
		last = time.Now()
	}
	close(done)
	fmt.Printf("go.signals.sent=%d\n", <-sent)
	fmt.Printf("go.signals.reads=%d\n", reads)
	fmt.Printf("go.now=%d\n", last.UnixNano())
}
