"""Small housekeeping tests: protections, failed rotation and cap refusal."""

import json
import tempfile
import unittest
from pathlib import Path

import housekeeping
import rotate_runs


def marker(directory, state, pinned, timestamp):
    (directory / rotate_runs.MARKER).write_text(
        json.dumps(
            {
                "version": 1,
                "state": state,
                "pinned": pinned,
                "reason": None,
                "created_utc": timestamp,
                "updated_utc": timestamp,
            }
        )
    )


class HousekeepingTests(unittest.TestCase):
    def test_protections_and_failed_rotation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            pinned = root / "pinned-run"
            active = root / "active-run"
            unmarked = root / "unmarked-run"
            newest = root / "newest-run"
            failed = root / "failed-run"
            for directory in (pinned, active, unmarked, newest, failed):
                directory.mkdir()
            marker(pinned, "completed", True, "2026-09-01T00:00:00+00:00")
            marker(active, "active", False, "2026-09-01T00:00:00+00:00")
            marker(newest, "completed", False, "2026-09-03T00:00:00+00:00")
            marker(failed, "failed", False, "2026-09-02T00:00:00+00:00")
            (unmarked / "data").write_text("x")
            symlink = root / "linked-run"
            symlink.symlink_to(newest)
            failures, lines = housekeeping.cleanup(root, keep=1, protect=[])
            self.assertEqual(failures, 0, lines)
            self.assertTrue(pinned.exists())
            self.assertTrue(active.exists())
            self.assertTrue(unmarked.exists())
            self.assertTrue(symlink.exists())
            self.assertTrue(newest.exists())
            self.assertFalse(failed.exists(), "failed runs rotate when requested")

    def test_cap_refusal(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            run = root / "run"
            run.mkdir()
            marker(run, "completed", False, "2026-09-03T00:00:00+00:00")
            (run / "blob").write_text("x" * 1024)
            free_now = housekeeping.free_bytes(Path(tmp))
            ok, reason = housekeeping.check_caps(root, free_now + 1, 20 * housekeeping.GIB, Path(tmp))
            self.assertFalse(ok)
            self.assertIn("free space", reason)
            ok, reason = housekeeping.check_caps(root, 0, 1, Path(tmp))
            self.assertFalse(ok)
            self.assertIn("managed cache", reason)
            ok, _ = housekeeping.check_caps(root, 0, 10**12, Path(tmp))
            self.assertTrue(ok)


    def test_check_only_never_rotates(self):
        import subprocess
        import sys
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            run = root / "run"
            run.mkdir()
            marker(run, "completed", False, "2026-09-01T00:00:00+00:00")
            script = Path(__file__).resolve().parent / "housekeeping.py"
            proc = subprocess.run(
                [sys.executable, str(script), str(root), "--keep", "0", "--check-only"],
                capture_output=True, text=True,
                env={"PYTHONPATH": str(script.parent), "PATH": "/usr/bin:/bin"},
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertTrue(run.exists(), "check-only must not rotate")

    def test_relative_root_sizes_correctly(self):
        import os
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            run = root / "run"
            run.mkdir()
            marker(run, "completed", False, "2026-09-01T00:00:00+00:00")
            (run / "blob").write_text("x" * 2048)
            previous = Path.cwd()
            os.chdir(root)
            try:
                size = housekeeping.managed_cache_bytes(".")
                self.assertGreaterEqual(size, 2048)
            finally:
                os.chdir(previous)

    def test_run_job_cleans_pre_and_post_and_preserves_exit(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "cache"
            root.mkdir()
            for index in range(4):
                run = root / f"disposable-{index}"
                run.mkdir()
                marker(run, "completed", False, f"2026-09-0{index + 1}T00:00:00+00:00")
                (run / "blob").write_text("x" * 256)
            target = Path(tmp) / "target"
            tmp_dir = Path(tmp) / "tmp"
            target.mkdir()
            tmp_dir.mkdir()
            command = [
                "python3",
                "-c",
                f"import pathlib, sys; r=pathlib.Path({str(root)!r}); "
                f"sys.exit(9) if len(list(r.glob('disposable-*'))) != 1 else None; "
                f"p=r/'created-during'; p.mkdir(); "
                f"import json; (p/'.capture-run.json').write_text(json.dumps({{'version':1,'state':'completed','pinned':False,'reason':None,'created_utc':'2026-09-09T00:00:00+00:00','updated_utc':'2026-09-09T00:00:00+00:00'}})); "
                "raise SystemExit(7)",
            ]
            lines = []
            status = housekeeping.run_job(
                root, command, keep=1, protect=[],
                free_min_bytes=0, cache_max_bytes=10**12, free_path=Path(tmp),
                target_dir=target, tmp_dir=tmp_dir,
                target_max_bytes=4 * housekeeping.GIB, tmp_max_bytes=512 * 1024**2,
                log=lines.append,
            )
            self.assertEqual(status, 7, lines)
            self.assertTrue(any("removed" in line for line in lines), lines)
            self.assertTrue((root / "created-during").exists(), "newest kept")
            self.assertFalse((root / "disposable-3").exists(), "post cleanup rotated the older run")


    def test_tmp_only_explicit_artifacts_are_cleaned(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "target"
            tmp_dir = Path(tmp) / "task"
            target.mkdir(); tmp_dir.mkdir()
            (tmp_dir / ".task-owned").write_text("")
            (tmp_dir / "heavy.lock").write_text("")
            keep_open = tmp_dir / "unarchived-evidence.txt"
            keep_open.write_text("evidence")
            disposable = tmp_dir / "old.disposable"
            disposable.write_text("x" * 1024)
            completed = tmp_dir / "old-run"
            completed.mkdir()
            marker(completed, "completed", False, "2026-09-01T00:00:00+00:00")
            removed = housekeeping.cleanup_reproducible(target, tmp_dir, False, True)
            self.assertTrue(keep_open.exists(), "unmarked evidence preserved")
            self.assertTrue((tmp_dir / "heavy.lock").exists())
            self.assertTrue((tmp_dir / ".task-owned").exists())
            self.assertFalse(disposable.exists())
            self.assertFalse(completed.exists())
            self.assertGreaterEqual(removed, 1024)

    def test_target_tmp_cap_refusal(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "cache"; root.mkdir()
            target = Path(tmp) / "target"; target.mkdir()
            tmp_dir = Path(tmp) / "task"; tmp_dir.mkdir()
            (tmp_dir / ".task-owned").write_text("")
            (target / "big.bin").write_text("x" * 4096)
            lines = []
            status = housekeeping.run_job(
                root, ["python3", "-c", "raise SystemExit(0)"], keep=1, protect=[],
                free_min_bytes=0, cache_max_bytes=10**12, free_path=Path(tmp),
                target_dir=target, tmp_dir=tmp_dir,
                target_max_bytes=1024, tmp_max_bytes=512 * 1024**2,
                log=lines.append,
            )
            self.assertEqual(status, 75, lines)
            self.assertTrue(any("cap refusal" in line for line in lines), lines)


    def test_tmp_active_pinned_and_malformed_runs_are_kept(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "target"; target.mkdir()
            tmp_dir = Path(tmp) / "task"; tmp_dir.mkdir()
            (tmp_dir / ".task-owned").write_text("")
            active = tmp_dir / "active-run"; active.mkdir()
            marker(active, "active", False, "2026-09-01T00:00:00+00:00")
            pinned = tmp_dir / "pinned-run"; pinned.mkdir()
            marker(pinned, "completed", True, "2026-09-01T00:00:00+00:00")
            malformed = tmp_dir / "malformed-run"; malformed.mkdir()
            (malformed / ".capture-run.json").write_text("not json")
            baseline = tmp_dir / "Baseline-run"; baseline.mkdir()
            marker(baseline, "completed", False, "2026-09-01T00:00:00+00:00")
            completed = tmp_dir / "completed-run"; completed.mkdir()
            marker(completed, "completed", False, "2026-09-01T00:00:00+00:00")
            removed = housekeeping.cleanup_reproducible(target, tmp_dir, False, True)
            self.assertTrue(active.exists())
            self.assertTrue(pinned.exists())
            self.assertTrue(malformed.exists())
            self.assertTrue(baseline.exists(), "protected marker name kept")
            self.assertFalse(completed.exists())


    def test_cli_child_argv_roundtrip_with_embedded_separator(self):
        import subprocess
        import sys
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "cache"; root.mkdir()
            target = Path(tmp) / "target"; target.mkdir()
            tmp_dir = Path(tmp) / "task"; tmp_dir.mkdir()
            (tmp_dir / ".task-owned").write_text("")
            script = Path(__file__).resolve().parent / "housekeeping.py"
            child = [sys.executable, "-c", "import sys,json;print(json.dumps(sys.argv[1:]))", "--", "a b", "", "--check", "x--y"]
            proc = subprocess.run(
                [sys.executable, str(script), str(root), "--target", str(target), "--tmp", str(tmp_dir), "--run"] + child,
                capture_output=True, text=True,
                env={"PYTHONPATH": str(script.parent), "PATH": "/usr/bin:/bin"},
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            import json
            printed = json.loads([line for line in proc.stdout.splitlines() if line.startswith("[")][-1])
            self.assertEqual(printed, child[3:], "child argv preserved exactly")

if __name__ == "__main__":
    unittest.main()
