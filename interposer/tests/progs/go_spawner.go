// Starts a program the way every Go program does, through os/exec. The child gets the environment
// the Go runtime copied from the array the kernel laid out when this program started, never the
// one libc keeps.
//
// argv[1] is the directory the child runs in, `.` for this program's own, and argv[2:] is the
// child and its arguments. Before starting it, the program reports its pid and every variable of
// its own environment, then waits for a line on stdin, so a case can change the session first.
package main

import (
	"bufio"
	"fmt"
	"os"
	"os/exec"
)

func main() {
	if len(os.Args) < 3 {
		fmt.Fprintln(os.Stderr, "usage: go_spawner <dir> <program> [argument...]")
		os.Exit(2)
	}
	fmt.Printf("go.pid=%d\n", os.Getpid())
	fmt.Printf("go.shared=%s\n", os.Getenv("LIBTIMEWARP__SHARED_FILE"))
	for _, entry := range os.Environ() {
		fmt.Printf("go.env.%s\n", entry)
	}
	fmt.Println("go.ready=1")

	if _, err := bufio.NewReader(os.Stdin).ReadString('\n'); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	cmd := exec.Command(os.Args[2], os.Args[3:]...)
	if os.Args[1] != "." {
		cmd.Dir = os.Args[1]
	}
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
