"""Exercise storage failures, orphan locks, and evidence-preserving restarts."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

import capture_guard
import native_panel
from test_native_panel import artifact


class CaptureGuardTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def test_probe_creates_reads_and_removes_as_current_user(self):
        result = capture_guard.preflight(self.root, reserve=0)
        self.assertEqual(result["uid"], os.getuid())
        self.assertEqual(list(self.root.iterdir()), [])

    def test_storage_reserve_includes_next_report_budget(self):
        usage = __import__("shutil")._ntuple_diskusage(1000, 900, 100)
        with patch("capture_guard.shutil.disk_usage", return_value=usage):
            with self.assertRaisesRegex(ValueError, "need 101"):
                capture_guard.preflight(self.root, reserve=50, report_budget=51)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_permission_failure_is_not_a_successful_probe(self):
        with patch("capture_guard.tempfile.NamedTemporaryFile", side_effect=PermissionError("denied")):
            with self.assertRaises(PermissionError):
                capture_guard.preflight(self.root, reserve=0)

    def test_active_child_keeps_lock_after_parent_handle_closes(self):
        # A pipe, rather than sleep, deterministically controls child lifetime.
        with capture_guard.capture_lock(self.root) as descriptor:
            child = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.buffer.read(1)"],
                                     stdin=subprocess.PIPE, pass_fds=(descriptor,))
        try:
            with self.assertRaisesRegex(ValueError, "another capture"):
                with capture_guard.capture_lock(self.root):
                    pass
        finally:
            child.communicate(b"x", timeout=10)
        with capture_guard.capture_lock(self.root):
            pass

    def test_retention_preserves_every_interrupted_file_and_budget(self):
        for suffix in (".json.gz", ".log", ".log.gz"):
            (self.root / ("pair" + suffix)).write_bytes(b"evidence")
        capture_guard.retain_attempt(self.root, "pair")
        self.assertEqual(capture_guard.retained_bytes(self.root), 24)
        self.assertFalse((self.root / "pair.json.gz").exists())
        self.assertEqual(len(list(self.root.glob("attempts/*/pair*"))), 3)

    def make_record(self, pair_id="pair"):
        source_path = self.root / "input.pdf"
        source_path.write_bytes(b"pdf")
        source = {"path": "input.pdf", "bytes": 3, "sha256": hashlib.sha256(b"pdf").hexdigest()}
        pair = {"id": pair_id, "old": source, "new": source}
        report = self.root / (pair_id + ".json.gz")
        with gzip.open(report, "wb") as stream:
            stream.write(json.dumps(artifact()).encode())
        with gzip.open(self.root / (pair_id + ".log.gz"), "wb") as stream:
            stream.write(b"")
        row = {"pair": pair_id, "status": "captured", "exit_code": 1,
               "report_bytes": report.stat().st_size, "report_sha256": native_panel.sha256(report),
               "summary": native_panel.read_native_summary(report)}
        return pair, row

    def verify(self, pair, row):
        return capture_guard.verify_saved_row(row, pair, self.root, self.root,
                                              native_panel.check_source, native_panel.read_native_summary)

    def test_resume_revalidates_inputs_report_and_exit_contract(self):
        pair, row = self.make_record()
        self.assertTrue(self.verify(pair, row))
        row["exit_code"] = 0
        with self.assertRaisesRegex(ValueError, "contract"):
            self.verify(pair, row)
        row["exit_code"] = 1
        (self.root / "input.pdf").write_bytes(b"bad")
        with self.assertRaisesRegex(ValueError, "input"):
            self.verify(pair, row)

    def test_corrupt_saved_report_is_never_overwritten_or_reused(self):
        pair, row = self.make_record()
        (self.root / "pair.json.gz").write_bytes(b"corrupt")
        with self.assertRaisesRegex(ValueError, "report"):
            self.verify(pair, row)
        self.assertEqual((self.root / "pair.json.gz").read_bytes(), b"corrupt")

    def test_corrupt_saved_log_is_rejected(self):
        pair, row = self.make_record()
        (self.root / "pair.log.gz").write_bytes(b"corrupt")
        with self.assertRaises(OSError):
            self.verify(pair, row)

    def test_storage_failure_never_launches_engine(self):
        pair, _ = self.make_record()
        with patch("native_panel.capture_guard.preflight", side_effect=ValueError("no room")), \
                patch("native_panel.subprocess.Popen") as launch:
            row = native_panel.capture_pair(Path("unused"), self.root, pair, self.root, 1)
        self.assertEqual(row["status"], "storage_unavailable")
        launch.assert_not_called()

    def test_running_child_is_reaped_when_storage_falls_below_reserve(self):
        pair, _ = self.make_record()
        destination = self.root / "run"
        destination.mkdir()
        process = Mock(pid=123456, returncode=-9)
        process.poll.side_effect = [None, -9]
        usage = __import__("shutil")._ntuple_diskusage(1000, 999, 1)
        with patch("native_panel.capture_guard.preflight", return_value={}), \
                patch("native_panel.shutil.disk_usage", return_value=usage), \
                patch("native_panel.subprocess.Popen", return_value=process) as launch, \
                patch("native_panel.os.killpg") as kill:
            row = native_panel.capture_pair(Path("engine"), self.root, pair, destination, 10, lock_fd=42)
        self.assertEqual(row["status"], "storage_reserve_reached")
        kill.assert_called_once_with(123456, __import__("signal").SIGKILL)
        process.wait.assert_called_once()
        self.assertEqual(launch.call_args.kwargs["pass_fds"], (42,))
        self.assertFalse((destination / "pair.log").exists())
        with gzip.open(destination / "pair.log.gz", "rb") as stream:
            self.assertEqual(stream.read(), b"")

    def test_missing_engine_is_recorded_without_aborting_remaining_pairs(self):
        pair, _ = self.make_record()
        destination = self.root / "run"
        destination.mkdir()
        with patch("native_panel.capture_guard.preflight", return_value={}), \
                patch("native_panel.subprocess.Popen", side_effect=FileNotFoundError("engine disappeared")):
            row = native_panel.capture_pair(Path("engine"), self.root, pair, destination, 10)
        self.assertEqual(row["status"], "execution_failed")
        self.assertIn("engine disappeared", row["error"])


class ResumeTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.binary = self.root / "binary"
        self.binary.write_bytes(b"frozen binary")
        self.panel = self.root / "panel.json"
        self.panel.write_text("{}")
        self.output = self.root / "capture"
        self.args = argparse.Namespace(action="capture", resume=False, retry_failed=False,
                                       output=self.output, binary=self.binary, panel=self.panel, timeout=1)
        self.cgroup = self.root / "cgroup"
        self.cgroup.mkdir()
        (self.cgroup / "memory.events").write_text("oom_kill 0")
        (self.cgroup / "memory.peak").write_text("1000")
        self.guard = {"cgroup": str(self.cgroup), "memory_max": 1000, "memory_swap_max": 0}

    def run_capture(self, pairs, capture):
        with patch("native_panel.require_memory_guard", return_value=self.guard), \
                patch("native_panel.subprocess.check_output", side_effect=["head\n", "tree\n", b""]), \
                patch("native_panel.capture_pair", side_effect=capture), \
                patch("native_panel.capture_guard.preflight", return_value={}), \
                patch("native_panel.capture_guard.verify_saved_row", return_value=True):
            native_panel.run(self.args, self.root, pairs, {p["id"] for p in pairs}, None)

    def test_resume_keeps_committed_rows_and_retries_only_interrupted_pair(self):
        pairs = [{"id": name, "old": {"bytes": 1}, "new": {"bytes": 1}} for name in ("a", "b")]
        row = {"pair": "a", "status": "captured"}
        with self.assertRaisesRegex(RuntimeError, "interrupted"):
            self.run_capture(pairs, [row, RuntimeError("interrupted")])
        self.assertEqual(json.loads((self.output / "summary.json").read_text())["rows"], [row])
        (self.output / "b.json.gz").write_bytes(b"partial")
        self.args.resume = True
        self.run_capture(pairs, [{"pair": "b", "status": "captured"}])
        summary = json.loads((self.output / "summary.json").read_text())
        self.assertEqual([r["pair"] for r in summary["rows"]], ["a", "b"])
        self.assertEqual(next(self.output.glob("attempts/b-*/b.json.gz")).read_bytes(), b"partial")

    def test_changed_binary_cannot_resume_previous_results(self):
        self.run_capture([], [])
        before = (self.output / "summary.json").read_bytes()
        self.binary.write_bytes(b"different binary")
        self.args.resume = True
        with self.assertRaisesRegex(ValueError, "binary_sha256"):
            self.run_capture([], [])
        self.assertEqual((self.output / "summary.json").read_bytes(), before)

    def test_explicit_failures_require_retry_flag_and_keep_prior_summary(self):
        pair = {"id": "a", "old": {"bytes": 1}, "new": {"bytes": 1}}
        self.run_capture([pair], [{"pair": "a", "status": "execution_failed"}])
        self.args.resume = True
        self.run_capture([pair], [])
        self.args.retry_failed = True
        self.run_capture([pair], [{"pair": "a", "status": "captured"}])
        self.assertTrue(list(self.output.glob("attempts/summary-*.json")))
        summary = json.loads((self.output / "summary.json").read_text())
        self.assertEqual(summary["rows"][0]["status"], "captured")


if __name__ == "__main__":
    unittest.main()
