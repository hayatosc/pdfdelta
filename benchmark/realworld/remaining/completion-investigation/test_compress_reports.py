"""Migration regressions for capture report compression."""

import gzip
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
import time
import unittest

import compress_reports


def write_report(path, payload):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(payload)


class CompressionTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="pdfdelta-compress-test-"))

    def tearDown(self):
        shutil.rmtree(self.root, ignore_errors=True)

    def owned_fixture(self, payload=b"{\"report\": true}\n"):
        run = self.root / "run-001-native"
        report = run / "pair-001-native.json"
        write_report(report, payload.decode())
        (run / "summary.json").write_text(
            json.dumps({"rows": [{"pair": "pair-001", "report": {"path": str(report)}}]})
        )
        return report

    def test_dry_run_lists_without_changing_files(self):
        report = self.owned_fixture()
        owned = compress_reports.owned_reports(self.root)
        self.assertIn(report.resolve(), owned)
        self.assertFalse(Path(f"{report}.gz").exists())
        self.assertTrue(report.exists())

    def test_outside_root_and_symlink_reports_are_rejected(self):
        inside = self.owned_fixture()
        outside = Path(tempfile.mkdtemp(prefix="pdfdelta-outside-")) / "outside-native.json"
        write_report(outside, "{}\n")
        link = self.root / "linked-native.json"
        link.symlink_to(outside)
        (self.root / "run-link").mkdir()
        (self.root / "run-link" / "summary.json").write_text(
            json.dumps({"rows": [{"report": {"path": str(outside)}}]})
        )
        owned = compress_reports.owned_reports(self.root)
        self.assertIn(inside.resolve(), owned)
        self.assertNotIn(outside.resolve(), owned)
        self.assertNotIn(link.resolve(), owned)

    def test_apply_compresses_verifies_and_records_manifest(self):
        report = self.owned_fixture()
        original = report.read_bytes()
        manifest = self.root / "compression-manifest.json"
        entries = []
        entry = compress_reports.compress_one(report)
        self.assertEqual(entry["state"], "published")
        self.assertTrue(report.exists())
        compress_reports.upsert(entries, entry)
        compress_reports.write_manifest(manifest, entries)
        compress_reports.finalize(entry, manifest, entries)
        self.assertFalse(report.exists())
        archive = Path(f"{report}.gz")
        self.assertEqual(
            hashlib.sha256(gzip.decompress(archive.read_bytes())).hexdigest(),
            entry["logical_sha256"],
        )
        payload = json.loads(manifest.read_text())
        self.assertEqual(payload["version"], compress_reports.MANIFEST_VERSION)
        self.assertEqual(payload["entries"][0]["state"], "deleted")

    def test_conflicting_existing_archive_fails_without_removing_source(self):
        report = self.owned_fixture()
        archive = Path(f"{report}.gz")
        with gzip.open(archive, "wb") as stream:
            stream.write(b"different content")
        with self.assertRaises(ValueError):
            compress_reports.compress_one(report)
        self.assertTrue(report.exists())

    def test_matching_existing_archive_is_adopted(self):
        report = self.owned_fixture()
        archive = Path(f"{report}.gz")
        with gzip.open(archive, "wb") as stream:
            stream.write(report.read_bytes())
        entry = compress_reports.compress_one(report)
        self.assertEqual(entry["state"], "published")
        manifest = self.root / "compression-manifest.json"
        entries = [entry]
        compress_reports.finalize(entry, manifest, entries)
        self.assertFalse(report.exists())
        self.assertTrue(archive.exists())

    def test_resume_finishes_published_entries_and_detects_changed_sources(self):
        report = self.owned_fixture()
        archive = Path(f"{report}.gz")
        with gzip.open(archive, "wb") as stream:
            stream.write(report.read_bytes())
        entry = compress_reports.compress_one(report)
        manifest = self.root / "compression-manifest.json"
        entries = [entry]
        compress_reports.write_manifest(manifest, entries)
        failures = compress_reports.resume(entries, apply=True, root=self.root)
        self.assertEqual(failures, 0)
        self.assertFalse(report.exists())
        self.assertEqual(entries[0]["state"], "deleted")

        # A changed source is not deleted.
        report2 = self.root / "run-002-native" / "pair-002-native.json"
        write_report(report2, "{}\n")
        archive2 = Path(f"{report2}.gz")
        with gzip.open(archive2, "wb") as stream:
            stream.write(report2.read_bytes())
        entry2 = compress_reports.compress_one(report2)
        time.sleep(0.01)
        report2.write_text("{\"changed\": true}\n")
        failures = compress_reports.resume([entry2], apply=True, root=self.root)
        self.assertEqual(failures, 1)
        self.assertTrue(report2.exists())

        # Manifest entries outside the requested root are never deleted.
        outside = Path(tempfile.mkdtemp(prefix="pdfdelta-manifest-outside-"))
        try:
            outside_report = outside / "outside-native.json"
            write_report(outside_report, "{}\n")
            outside_archive = Path(f"{outside_report}.gz")
            with gzip.open(outside_archive, "wb") as stream:
                stream.write(outside_report.read_bytes())
            unsafe = {
                "old_path": str(outside_report),
                "new_path": str(outside_archive),
                "logical_sha256": hashlib.sha256(outside_report.read_bytes()).hexdigest(),
                "logical_bytes": outside_report.stat().st_size,
                "state": "published",
            }
            failures = compress_reports.resume([unsafe], apply=True, root=self.root)
            self.assertEqual(failures, 1)
            self.assertTrue(outside_report.exists())
        finally:
            shutil.rmtree(outside, ignore_errors=True)

    def test_source_mutated_during_read_is_not_published(self):
        report = self.owned_fixture()
        original = compress_reports.logical_digest

        def mutating(path):
            report.write_text("{\"mutated\": true}\n")
            return original(path)

        compress_reports.logical_digest = mutating
        try:
            with self.assertRaises(compress_reports.SourceChanged):
                compress_reports.compress_one(report)
        finally:
            compress_reports.logical_digest = original
        self.assertTrue(report.exists())
        self.assertFalse(Path(f"{report}.gz").exists())

    def test_finalize_rejects_source_changed_after_publish(self):
        report = self.owned_fixture()
        entry = compress_reports.compress_one(report)
        manifest = self.root / "compression-manifest.json"
        entries = [entry]
        time.sleep(0.01)
        report.write_text("{\"changed\": true}\n")
        with self.assertRaises(compress_reports.SourceChanged):
            compress_reports.finalize(entry, manifest, entries)
        self.assertTrue(report.exists())

    def test_manifest_write_is_atomic_and_versioned(self):
        manifest = self.root / "compression-manifest.json"
        compress_reports.write_manifest(manifest, [])
        payload = json.loads(manifest.read_text())
        self.assertEqual(payload, {"version": 1, "entries": []})
        self.assertFalse(list(self.root.glob("compression-manifest.json.tmp.*")))


if __name__ == "__main__":
    unittest.main()
