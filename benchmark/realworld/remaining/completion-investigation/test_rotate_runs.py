"""Retention regressions for marker-based capture-run rotation."""

from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import shutil
import tempfile
import time
import types
import unittest
from unittest import mock

import rotate_runs


class ParserStub:
    def error(self, message):
        raise ValueError(message)


def make_run(root, name, state="completed", pinned=False, age_hours=0.0, legacy_nested=False):
    run = root / name
    run.mkdir(parents=True)
    stamp = datetime.now(timezone.utc) - timedelta(hours=age_hours)
    marker = {
        "version": 1,
        "kind": "panel-capture",
        "state": state,
        "pinned": pinned,
        "reason": "fixture" if pinned else None,
        "created_utc": stamp.isoformat(),
        "updated_utc": stamp.isoformat(),
    }
    (run / rotate_runs.MARKER).write_text(json.dumps(marker))
    if legacy_nested:
        nested = run / "raw-child"
        nested.mkdir()
        (nested / "summary.json").write_text(
            json.dumps({"rows": [], "fixed_denominator": 36, "binary": {}, "panel": {}})
        )
        (nested / "runs.json").write_text(
            json.dumps({"runs": [], "binary_sha256": "0" * 64, "implementation_commit": "0" * 40})
        )
    return run


def args_for(root, **overrides):
    base = {
        "root": root,
        "keep": 3,
        "max_age_days": None,
        "protect": [],
        "rotate_failed": False,
        "apply": True,
    }
    base.update(overrides)
    return types.SimpleNamespace(**base)


class RotationTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="pdfdelta-rotate-test-"))

    def tearDown(self):
        shutil.rmtree(self.root, ignore_errors=True)

    def test_discovery_is_marker_only_and_direct_children(self):
        marked = make_run(self.root, "iteration-001-native", legacy_nested=True)
        plain = self.root / "legacy-plain"
        plain.mkdir()
        (plain / "summary.json").write_text(json.dumps({"rows": []}))
        legacy_nested_metadata = make_run(self.root, "iteration-002-native")
        (legacy_nested_metadata / rotate_runs.MARKER).unlink()
        runs = rotate_runs.discover(self.root)
        self.assertEqual([directory for directory, _ in runs], [marked])

    def test_protected_parent_with_nested_raw_metadata_is_never_rotated(self):
        parent = make_run(self.root, "baseline-native", age_hours=100, legacy_nested=True)
        rotate_runs.rotate(args_for(self.root, keep=0), ParserStub())
        self.assertTrue(parent.exists())
        self.assertTrue((parent / "raw-child" / "summary.json").exists())

    def test_active_and_failed_runs_are_not_rotated_automatically(self):
        active = make_run(self.root, "iteration-active-native", state="active", age_hours=100)
        failed = make_run(self.root, "iteration-failed-native", state="failed", age_hours=100)
        rotate_runs.rotate(args_for(self.root, keep=0), ParserStub())
        self.assertTrue(active.exists())
        self.assertTrue(failed.exists())
        rotate_runs.rotate(args_for(self.root, keep=0, rotate_failed=True), ParserStub())
        self.assertTrue(active.exists())
        self.assertFalse(failed.exists())

    def test_newest_generations_are_kept(self):
        runs = [
            make_run(self.root, f"iteration-{index:03d}-native", age_hours=index + 1)
            for index in range(5)
        ]
        failures = rotate_runs.rotate(args_for(self.root, keep=3), ParserStub())
        self.assertEqual(failures, 0)
        remaining = sorted(directory.name for directory, _ in rotate_runs.discover(self.root))
        self.assertEqual(
            remaining,
            ["iteration-000-native", "iteration-001-native", "iteration-002-native"],
        )
        del runs

    def test_pinned_runs_persist_until_unpinned(self):
        run = make_run(self.root, "iteration-pin-native", age_hours=100)
        rotate_runs.update_pin(run, True, "hold for final audit", ParserStub())
        rotate_runs.rotate(args_for(self.root, keep=0), ParserStub())
        self.assertTrue(run.exists())
        rotate_runs.update_pin(run, False, None, ParserStub())
        rotate_runs.rotate(args_for(self.root, keep=0), ParserStub())
        self.assertFalse(run.exists())

    def test_unmarked_legacy_directories_are_ignored(self):
        legacy = self.root / "old-legacy-run"
        legacy.mkdir()
        (legacy / "runs.json").write_text(
            json.dumps({"runs": [], "binary_sha256": "0" * 64, "implementation_commit": "0" * 40})
        )
        rotate_runs.rotate(args_for(self.root, keep=0), ParserStub())
        self.assertTrue(legacy.exists())

    def test_outside_root_is_not_discovered(self):
        outside = Path(tempfile.mkdtemp(prefix="pdfdelta-rotate-outside-"))
        try:
            make_run(outside, "iteration-outside-native")
            self.assertEqual(rotate_runs.discover(self.root), [])
        finally:
            shutil.rmtree(outside, ignore_errors=True)

    def test_removal_failure_returns_nonzero(self):
        make_run(self.root, "iteration-fail-native", age_hours=100)
        with mock.patch.object(rotate_runs.shutil, "rmtree", side_effect=OSError("locked")):
            failures = rotate_runs.rotate(args_for(self.root, keep=0), ParserStub())
        self.assertEqual(failures, 1)


if __name__ == "__main__":
    unittest.main()
