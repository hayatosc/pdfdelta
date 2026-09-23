import hashlib
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import dedup_reports  # noqa: E402
import housekeeping  # noqa: E402
import rotate_runs  # noqa: E402


def digest(data):
    return hashlib.sha256(data).hexdigest()


class DedupReportsTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name) / "cache"
        self.root.mkdir()

    def tearDown(self):
        self.temporary.cleanup()

    def make_run(self, name, state="completed", pinned=False):
        run = self.root / name
        run.mkdir()
        marker = {"version": 1, "state": state, "pinned": pinned}
        (run / ".capture-run.json").write_text(json.dumps(marker))
        return run

    def write_report(self, run, name, payload, claimed=None, logical=None):
        path = run / name
        path.write_bytes(payload)
        report = {
            "path": str(path),
            "encoding": "gzip",
            "sha256": logical or ("f" * 64),
            "file_sha256": claimed or digest(payload),
            "bytes": len(payload),
        }
        (run / "summary.json").write_text(json.dumps({"rows": [{"report": report}]}))
        return path

    def test_identical_reports_are_shared_once(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"identical\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload)
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        self.assertEqual(len(groups), 1)
        replaced, skipped = dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), True, [], binding)
        self.assertEqual((replaced, skipped), (1, 0))
        self.assertEqual(os.stat(path_a).st_ino, os.stat(path_b).st_ino)
        self.assertEqual(path_a.read_bytes(), payload)
        self.assertFalse(os.stat(path_a).st_mode & 0o222)
        replaced, skipped = dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), True, [], binding)
        self.assertEqual((replaced, skipped), (0, 1))

    def test_forged_metadata_hash_rejected(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"forged\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload, claimed="0" * 64)
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertEqual(len(groups), 0)
        self.assertEqual(len(errors), 1)
        self.assertNotEqual(os.stat(path_a).st_ino, os.stat(path_b).st_ino)

    def test_relative_root_and_binding_validation(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"relative\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload)
        original = os.getcwd()
        os.chdir(self.root)
        try:
            groups, errors, binding = dedup_reports.discover_groups(".")
        finally:
            os.chdir(original)
        self.assertFalse(errors)
        self.assertEqual(len(groups), 1)
        self.assertEqual(
            {Path(value).resolve() for value in binding.values()},
            {first.resolve(), second.resolve()},
        )
        errors = []
        replaced, _ = dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), True, errors, binding
        )
        self.assertEqual((replaced, len(errors)), (1, 0))
        self.assertEqual(os.stat(path_a).st_ino, os.stat(path_b).st_ino)

    def test_malformed_claimed_metadata_reported(self):
        run = self.make_run("run-a")
        payload = b"malformed\n"
        path = run / "pair-native.json.gz"
        path.write_bytes(payload)
        report = {
            "path": str(path),
            "encoding": "gzip",
            "sha256": "f" * 64,
            "file_sha256": "not-a-digest",
            "bytes": len(payload),
        }
        (run / "summary.json").write_text(json.dumps({"rows": [{"report": report}]}))
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertEqual(len(groups), 0)
        self.assertTrue(any("malformed claimed report metadata" in error for error in errors))

    def test_canonical_change_noticed_in_dry_run(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"dry canonical\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        self.write_report(second, "pair-native.json.gz", payload)
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        os.chmod(path_a, 0o644)
        path_a.write_bytes(b"changed later\n")
        errors = []
        replaced, _ = dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), False, errors, binding
        )
        self.assertEqual(replaced, 0)
        self.assertTrue(any("canonical" in error for error in errors))

    def test_chmod_failure_aborts_without_replacement(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"chmod\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload)
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        original = dedup_reports.make_readonly

        def failing_readonly(path):
            raise OSError("simulated chmod failure")

        dedup_reports.make_readonly = failing_readonly
        errors = []
        try:
            replaced, _ = dedup_reports.apply_group(
                next(iter(groups)), next(iter(groups.values())), True, errors, binding
            )
        finally:
            dedup_reports.make_readonly = original
        self.assertEqual(replaced, 0)
        self.assertEqual(path_a.read_bytes(), payload)
        self.assertEqual(path_b.read_bytes(), payload)
        self.assertNotEqual(os.stat(path_a).st_ino, os.stat(path_b).st_ino)
        self.assertTrue(any("readonly failed" in error for error in errors))

    def test_marker_change_before_apply_rejected(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"marker\n"
        self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload)
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        (first / ".capture-run.json").write_text(
            json.dumps({"version": 1, "state": "active", "pinned": False})
        )
        errors = []
        replaced, _ = dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), True, errors, binding
        )
        self.assertEqual(replaced, 0)
        self.assertEqual(path_b.read_bytes(), payload)
        self.assertTrue(any("completed" in error for error in errors))

    def test_canonical_replaced_at_link_time_preserves_destination(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"link time\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload)
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        original_link = os.link

        def replacing_link(source, destination):
            replacement = path_a.with_name("replacement.tmp")
            replacement.write_bytes(b"replaced at link time\n")
            os.chmod(path_a, 0o644)
            os.replace(replacement, source)
            return original_link(source, destination)

        os.link = replacing_link
        errors = []
        try:
            replaced, _ = dedup_reports.apply_group(
                next(iter(groups)), next(iter(groups.values())), True, errors, binding
            )
        finally:
            os.link = original_link
        self.assertEqual(replaced, 0)
        self.assertEqual(path_b.read_bytes(), payload)
        self.assertTrue(any("differs from verified canonical" in error for error in errors))
        self.assertFalse(list(self.root.rglob("*.dedup-*")))

    def rotation_log(self, **kwargs):
        import contextlib
        import io

        buffer = io.StringIO()
        lines = []
        with contextlib.redirect_stdout(buffer):
            failures = rotate_runs.run_retention(self.root, log=lines.append, **kwargs)
        reclaimed_lines = [
            line
            for line in lines + buffer.getvalue().splitlines()
            if line.startswith("reclaimed ")
        ]
        self.assertEqual(len(reclaimed_lines), 1, reclaimed_lines)
        return failures, int(reclaimed_lines[0].split()[1]), lines

    def age_runs(self, runs):
        for index, run in enumerate(runs):
            stamp = 1_000_000 + index * 10_000
            os.utime(run, (stamp, stamp))

    def independent_bytes(self, runs):
        seen = set()
        total = 0
        for run in runs:
            for path in run.rglob("*"):
                if path.is_symlink() or not path.is_file():
                    continue
                stat = os.lstat(path)
                if (stat.st_dev, stat.st_ino) in seen:
                    continue
                seen.add((stat.st_dev, stat.st_ino))
                total += stat.st_size
        return total

    def test_rotation_success_counts_exact_bytes(self):
        runs = [self.make_run(name) for name in ("run-a", "run-b", "run-c")]
        for index, run in enumerate(runs):
            self.write_report(run, "pair-native.json.gz", f"payload {index}\n".encode())
        self.age_runs(runs)
        expected = self.independent_bytes([runs[0], runs[1]])
        failures, reclaimed, _lines = self.rotation_log(keep=1, apply=True)
        self.assertEqual(failures, 0)
        self.assertFalse(runs[0].exists())
        self.assertFalse(runs[1].exists())
        self.assertTrue(runs[2].exists())
        self.assertGreater(reclaimed, 0)
        self.assertEqual(reclaimed, expected)

    def test_rotation_failure_preserves_files_and_counts_zero(self):
        runs = [self.make_run(name) for name in ("run-a", "run-b", "run-c")]
        paths = [
            self.write_report(run, "pair-native.json.gz", b"keep failure\n")
            for run in runs
        ]
        self.age_runs(runs)
        original = rotate_runs.shutil.rmtree

        def failing_rmtree(directory, *args, **kwargs):
            raise OSError("simulated rmtree failure")

        rotate_runs.shutil.rmtree = failing_rmtree
        try:
            failures, reclaimed, _lines = self.rotation_log(keep=1, apply=True)
        finally:
            rotate_runs.shutil.rmtree = original
        self.assertGreaterEqual(failures, 1)
        self.assertEqual(reclaimed, 0)
        for path in paths:
            self.assertEqual(path.read_bytes(), b"keep failure\n")

    def test_rotation_two_links_same_inode_counted_once(self):
        runs = [self.make_run(name) for name in ("run-a", "run-b", "run-c")]
        payload = b"shared rotation\n"
        path_a = self.write_report(runs[0], "pair-native.json.gz", payload)
        self.write_report(runs[1], "pair-native.json.gz", payload)
        self.write_report(runs[2], "pair-native.json.gz", b"separate\n")
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), True, errors, binding
        )
        self.assertEqual(
            os.stat(path_a).st_ino, os.stat(runs[1] / "pair-native.json.gz").st_ino
        )
        self.age_runs(runs)
        expected = self.independent_bytes([runs[0], runs[1]])
        failures, reclaimed, _lines = self.rotation_log(keep=1, apply=True)
        self.assertEqual(failures, 0)
        self.assertFalse(runs[0].exists())
        self.assertFalse(runs[1].exists())
        self.assertTrue(runs[2].exists())
        self.assertEqual(reclaimed, expected)
        self.assertGreaterEqual(reclaimed, len(payload))

    def test_post_share_dry_run_has_zero_candidate_bytes(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"zero candidate\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        self.write_report(second, "pair-native.json.gz", payload)
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        self.assertGreater(dedup_reports.candidate_savings(groups), 0)
        dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), True, errors, binding
        )
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        self.assertEqual(dedup_reports.candidate_savings(groups), 0)

    def test_binding_none_is_rejected(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"binding\n"
        self.write_report(first, "pair-native.json.gz", payload)
        self.write_report(second, "pair-native.json.gz", payload)
        groups, errors, _binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        with self.assertRaises(ValueError):
            dedup_reports.apply_group(
                next(iter(groups)), next(iter(groups.values())), True, [], None
            )

    def test_removing_one_link_preserves_survivor(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"survivor\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload)
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), True, errors, binding
        )
        self.assertEqual(os.stat(path_a).st_ino, os.stat(path_b).st_ino)
        local_before = rotate_runs.reclaimable_bytes([first])
        path_b.unlink()
        self.assertEqual(path_a.read_bytes(), payload)
        self.assertEqual(os.stat(path_a).st_nlink, 1)
        self.assertGreaterEqual(
            rotate_runs.reclaimable_bytes([first]) - local_before, len(payload)
        )

    def test_forced_temporary_collision_preserves_preexisting_file(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"collision\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload)
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        existing = path_b.with_name(path_b.name + ".dedup-0-" + "0" * 16)
        existing.write_bytes(b"keep\n")
        errors = []
        dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), True, errors, binding)
        self.assertEqual(existing.read_bytes(), b"keep\n")
        self.assertEqual(os.stat(path_a).st_ino, os.stat(path_b).st_ino)

    def test_same_run_symlink_parent_rejected(self):
        run = self.make_run("run-a")
        real = run / "real"
        real.mkdir()
        payload = b"symlink parent\n"
        target = real / "pair-native.json.gz"
        target.write_bytes(payload)
        linked_dir = run / "linked"
        linked_dir.symlink_to(real)
        report = {
            "path": str(linked_dir / "pair-native.json.gz"),
            "encoding": "gzip",
            "sha256": "f" * 64,
            "file_sha256": digest(payload),
            "bytes": len(payload),
        }
        (run / "summary.json").write_text(json.dumps({"rows": [{"report": report}]}))
        reports = dedup_reports.expected_reports(run, [])
        self.assertEqual(reports, [])

    def test_failed_replace_preserves_original(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"replace failure\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload)
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        original_replace = os.replace

        def failing_replace(source, destination):
            raise OSError("simulated replace failure")

        os.replace = failing_replace
        errors = []
        try:
            replaced, _ = dedup_reports.apply_group(
                next(iter(groups)), next(iter(groups.values())), True, errors, binding)
        finally:
            os.replace = original_replace
        self.assertEqual(replaced, 0)
        self.assertEqual(path_b.read_bytes(), payload)
        self.assertNotEqual(os.stat(path_a).st_ino, os.stat(path_b).st_ino)
        self.assertTrue(errors)
        self.assertFalse(list(self.root.rglob("*.dedup-*")))

    def test_different_bytes_never_shared(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        path_a = self.write_report(first, "pair-native.json.gz", b"aaaa\n")
        path_b = self.write_report(second, "pair-native.json.gz", b"bbbb\n")
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        self.assertEqual(len(groups), 0)
        self.assertNotEqual(os.stat(path_a).st_ino, os.stat(path_b).st_ino)

    def test_external_hard_link_refused(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"shared elsewhere\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload)
        external = self.root / "external.json.gz"
        os.link(path_a, external)
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        errors = []
        replaced, _ = dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), True, errors, binding)
        self.assertEqual(replaced, 0)
        self.assertTrue(any("external hard link" in error for error in errors))
        self.assertNotEqual(os.stat(path_a).st_ino, os.stat(path_b).st_ino)

    def test_preexisting_temporary_path_preserved(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"temp\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload)
        fixed = path_b.with_name(path_b.name + ".dedup.tmp")
        fixed.write_bytes(b"keep me\n")
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), True, [], binding)
        self.assertEqual(fixed.read_bytes(), b"keep me\n")
        self.assertEqual(os.stat(path_a).st_ino, os.stat(path_b).st_ino)

    def test_canonical_change_between_discovery_and_apply_preserves_destination(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"before\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload)
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        os.chmod(path_a, 0o644)
        path_a.write_bytes(b"after!\n")
        errors = []
        replaced, _ = dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), True, errors, binding)
        self.assertEqual(replaced, 0)
        self.assertEqual(path_b.read_bytes(), payload)
        self.assertTrue(errors)

    def test_active_unmarked_and_symlink_runs_skipped(self):
        active = self.make_run("run-active", state="active")
        unmarked = self.root / "run-unmarked"
        unmarked.mkdir()
        payload = b"skip\n"
        path_active = self.write_report(active, "pair-native.json.gz", payload)
        (unmarked / "pair-native.json.gz").write_bytes(payload)
        outside = Path(self.temporary.name) / "outside"
        outside.mkdir()
        (outside / "pair-native.json.gz").write_bytes(payload)
        symlink = self.root / "run-link"
        symlink.symlink_to(outside)
        runs = list(dedup_reports.completed_runs(self.root))
        self.assertEqual(len(runs), 0)
        groups, errors, binding = dedup_reports.discover_groups(self.root)
        self.assertFalse(errors)
        self.assertEqual(len(groups), 0)
        self.assertEqual(path_active.read_bytes(), payload)

    def test_managed_cache_counts_hard_links_once(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"count\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload)
        groups, _, binding = dedup_reports.discover_groups(self.root)
        dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), True, [], binding)
        self.assertEqual(os.stat(path_a).st_ino, os.stat(path_b).st_ino)
        seen = set()
        expected = 0
        for path in self.root.rglob("*"):
            if not path.is_file():
                continue
            stat = os.stat(path)
            if (stat.st_dev, stat.st_ino) in seen:
                continue
            seen.add((stat.st_dev, stat.st_ino))
            expected += stat.st_size
        self.assertEqual(housekeeping.managed_cache_bytes(self.root), expected)
        self.assertLess(expected, 2 * (len(payload) + path_a.parent.joinpath("summary.json").stat().st_size + path_a.parent.joinpath(".capture-run.json").stat().st_size))

    def test_rotation_reclaims_shared_inode_once(self):
        first = self.make_run("run-a")
        second = self.make_run("run-b")
        payload = b"rotate\n"
        path_a = self.write_report(first, "pair-native.json.gz", payload)
        path_b = self.write_report(second, "pair-native.json.gz", payload)
        groups, _, binding = dedup_reports.discover_groups(self.root)
        dedup_reports.apply_group(
            next(iter(groups)), next(iter(groups.values())), True, [], binding)
        local_first = rotate_runs.reclaimable_bytes([first])
        local_second = rotate_runs.reclaimable_bytes([second])
        combined = rotate_runs.reclaimable_bytes([first, second])
        self.assertEqual(combined, local_first + local_second + len(payload))
        (second / "summary.json").unlink()
        (second / ".capture-run.json").unlink()
        self.assertEqual(
            rotate_runs.reclaimable_bytes([second]), 0
        )


if __name__ == "__main__":
    unittest.main()
