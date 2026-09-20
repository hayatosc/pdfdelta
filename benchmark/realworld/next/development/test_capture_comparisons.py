"""Stream-compressed log regressions for the comparison capture driver."""

import gzip
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "capture_comparisons", HERE / "capture-comparisons.py"
)
driver = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(driver)

BIG_OUTPUT = (
    "import sys\n"
    "sys.stdout.write('o' * 300000)\n"
    "sys.stdout.flush()\n"
    "sys.stderr.write('e' * 300000)\n"
    "sys.stderr.flush()\n"
)


class CompressedLogTests(unittest.TestCase):
    def setUp(self):
        self.directory = Path(tempfile.mkdtemp(prefix="pdfdelta-log-test-"))

    def tearDown(self):
        shutil.rmtree(self.directory, ignore_errors=True)

    def test_pumps_beyond_pipe_capacity_to_both_streams(self):
        stdout_path = self.directory / "run.stdout.gz"
        stderr_path = self.directory / "run.stderr.gz"
        exit_code, errors, prefix = driver.run_with_compressed_logs(
            [sys.executable, "-c", BIG_OUTPUT], stdout_path, stderr_path
        )
        self.assertEqual(exit_code, 0)
        self.assertEqual(errors, [])
        self.assertEqual(prefix, b"e" * 4096)
        self.assertEqual(gzip.decompress(stdout_path.read_bytes()), b"o" * 300000)
        self.assertEqual(gzip.decompress(stderr_path.read_bytes()), b"e" * 300000)

    def test_small_command_returns_its_exit_code(self):
        stdout_path = self.directory / "small.stdout.gz"
        stderr_path = self.directory / "small.stderr.gz"
        exit_code, errors, prefix = driver.run_with_compressed_logs(
            [sys.executable, "-c", "import sys; print('out'); print('err', file=sys.stderr)"],
            stdout_path,
            stderr_path,
        )
        self.assertEqual(exit_code, 0)
        self.assertEqual(errors, [])
        self.assertEqual(prefix, b"err\n")
        self.assertEqual(gzip.decompress(stdout_path.read_bytes()), b"out\n")

    def test_pump_write_failure_is_reported_and_kills_the_child(self):
        # A directory at the target path makes the gzip sink fail immediately.
        stdout_path = self.directory / "blocked.stdout.gz"
        stdout_path.mkdir()
        stderr_path = self.directory / "blocked.stderr.gz"
        started = time.monotonic()
        exit_code, errors, prefix = driver.run_with_compressed_logs(
            [sys.executable, "-c", BIG_OUTPUT], stdout_path, stderr_path
        )
        elapsed = time.monotonic() - started
        self.assertTrue(errors, "the pump failure must be reported")
        self.assertLess(elapsed, 15.0, "a failed pump must not deadlock the wait")
        self.assertNotEqual(exit_code, 0)
        del prefix

    def test_pump_failure_terminates_its_direct_child(self):
        # A real wrapper copy with a sleeping grandchild: the direct child must
        # be gone after a pump failure, and the call must not block on pipes.
        pid_file = self.directory / "tree-child.pid"
        blocked = self.directory / "blocked.stdout.gz"
        blocked.mkdir()
        stderr_path = self.directory / "tree.stderr.gz"
        script = (
            "import sys, time;"
            f"open({str(pid_file)!r}, 'w').write(str(__import__('os').getpid()));"
            "sys.stdout.write('o' * 300000);"
            "time.sleep(300)"
        )
        command = [
            "/usr/bin/time",
            "-f",
            "%e %M",
            "-o",
            str(self.directory / "tree.time"),
            "timeout",
            "--foreground",
            "--kill-after=5",
            "180",
            sys.executable,
            "-c",
            script,
        ]
        started = time.monotonic()
        exit_code, errors, _ = driver.run_with_compressed_logs(command, blocked, stderr_path)
        elapsed = time.monotonic() - started
        self.assertTrue(errors)
        self.assertLess(elapsed, 15.0)
        self.assertNotEqual(exit_code, 0)

    def test_wrapper_exit_with_grandchild_holding_pipe_is_cleaned(self):
        # The direct wrapper exits 0 while `sleep` keeps the inherited pipes
        # open; the bounded shutdown must kill the group and record a failure
        # instead of reporting a captured log.
        pid_file = self.directory / "holding.pid"
        stdout_path = self.directory / "holding.stdout.gz"
        stderr_path = self.directory / "holding.stderr.gz"
        started = time.monotonic()
        exit_code, errors, _ = driver.run_with_compressed_logs(
            ["sh", "-c", f"sleep 300 & echo $! > {pid_file}; exit 0"],
            stdout_path,
            stderr_path,
            shutdown_seconds=1.0,
        )
        elapsed = time.monotonic() - started
        self.assertEqual(exit_code, 0)
        self.assertTrue(any("did not finish" in error for error in errors), errors)
        self.assertLess(elapsed, 15.0)
        grandchild = int(pid_file.read_text())
        self._assert_gone(grandchild)

    def test_timeout_wrapper_cleans_up_descendants(self):
        # `timeout --foreground` signals only its direct child; the sleeping
        # grandchild survives, so the driver must clean the owned group after
        # the timeout exit status.
        pid_file = self.directory / "timeout.pid"
        stdout_path = self.directory / "timeout.stdout.gz"
        stderr_path = self.directory / "timeout.stderr.gz"
        started = time.monotonic()
        exit_code, errors, _ = driver.run_with_compressed_logs(
            [
                "timeout",
                "--foreground",
                "--kill-after=5",
                "1",
                "sh",
                "-c",
                f"sleep 300 & echo $! > {pid_file}; wait",
            ],
            stdout_path,
            stderr_path,
            shutdown_seconds=2.0,
        )
        elapsed = time.monotonic() - started
        self.assertEqual(exit_code, 124)
        self.assertLess(elapsed, 15.0)
        self.assertTrue(pid_file.is_file())
        self._assert_gone(int(pid_file.read_text()))
        del errors

    def _assert_gone(self, pid):
        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.1)
        self.fail(f"process {pid} survived group termination")

    def test_terminate_process_group_kills_descendants(self):
        # Group termination must reap grandchildren that outlive the direct
        # child, which is the real /usr/bin/time -> timeout -> pdfdelta shape.
        pid_file = self.directory / "grandchild.pid"
        process = subprocess.Popen(
            [
                "sh",
                "-c",
                f"sleep 300 & echo $! > {pid_file}; wait",
            ],
            start_new_session=True,
        )
        deadline = time.monotonic() + 5.0
        while not pid_file.is_file() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertTrue(pid_file.is_file(), "the grandchild pid must be recorded")
        grandchild = int(pid_file.read_text())
        driver.terminate_process_group(process)
        process.wait()
        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline:
            try:
                os.kill(grandchild, 0)
            except ProcessLookupError:
                break
            time.sleep(0.1)
        else:
            self.fail("the grandchild survived the group termination")


if __name__ == "__main__":
    unittest.main()
