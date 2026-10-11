"""Finding the session to act on, without a session file in sight.

The cases that need a real session live in the Rust suite beside the C library, because that is
where one can be created.
"""

import os
import subprocess
import sys
import unittest

from libtimewarp import discovery


class SessionDirectory(unittest.TestCase):
    """The same three rules the library follows, so both sides compute the same path."""

    def test_the_session_dir_variable_wins(self):
        env = {"LIBTIMEWARP_SESSION_DIR": "/run/user/1000", "XDG_RUNTIME_DIR": "/xdg"}
        self.assertEqual(str(discovery.session_dir(env)), "/run/user/1000/libtimewarp")

    def test_xdg_runtime_dir_comes_second(self):
        self.assertEqual(str(discovery.session_dir({"XDG_RUNTIME_DIR": "/xdg"})), "/xdg/libtimewarp")

    def test_the_default_separates_users_by_euid(self):
        self.assertEqual(str(discovery.session_dir({}, euid=1000)), "/tmp/libtimewarp-1000")
        self.assertNotEqual(discovery.session_dir({}, euid=1000), discovery.session_dir({}, euid=1001))

    def test_an_empty_variable_is_unset(self):
        env = {"LIBTIMEWARP_SESSION_DIR": "", "XDG_RUNTIME_DIR": ""}
        self.assertEqual(str(discovery.session_dir(env, euid=5)), "/tmp/libtimewarp-5")

    def test_the_temp_directory_is_the_one_the_environment_names(self):
        self.assertEqual(
            str(discovery.session_dir({"TMPDIR": "/scratch"}, euid=5)),
            "/scratch/libtimewarp-5",
        )


class ProcessEnvironment(unittest.TestCase):
    def started(self, env):
        """A child that has reported in from its own code, so it is past exec and its environment
        is the one it was given."""
        child = subprocess.Popen(
            [sys.executable, "-c", "import time; print('up', flush=True); time.sleep(30)"],
            env=env,
            stdout=subprocess.PIPE,
        )

        def stop():
            child.kill()
            child.wait()
            child.stdout.close()

        self.addCleanup(stop)
        if child.stdout.readline() != b"up\n":
            self.fail(f"the child exited with status {child.wait()} before it reported in")
        return child

    def test_a_process_of_the_tree_carries_the_path(self):
        child = self.started(
            dict(os.environ, LIBTIMEWARP__SHARED_FILE="/tmp/libtimewarp/pid-1-x.session")
        )
        try:
            found = discovery.shared_file_of(child.pid)
        except LookupError as why:
            status = child.poll()
            state = "still running" if status is None else f"gone, with status {status}"
            self.fail(f"{why}; the child is {state}")
        self.assertEqual(str(found), "/tmp/libtimewarp/pid-1-x.session")

    def test_a_process_in_no_session_of_ours_says_so(self):
        child = self.started(
            {key: value for key, value in os.environ.items() if key != "LIBTIMEWARP__SHARED_FILE"}
        )
        with self.assertRaises(LookupError):
            discovery.shared_file_of(child.pid)

    def test_a_process_that_is_not_there_says_so(self):
        with self.assertRaises(LookupError):
            discovery.shared_file_of(0x7FFFFFFF)


if __name__ == "__main__":
    unittest.main()
