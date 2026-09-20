"""Compressed capture storage regressions for the capture entry point."""

import gzip
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import capture


class CaptureStorageTests(unittest.TestCase):
    def setUp(self):
        self.directory = Path(tempfile.mkdtemp(prefix="pdfdelta-capture-storage-"))

    def tearDown(self):
        import shutil

        shutil.rmtree(self.directory, ignore_errors=True)

    def test_report_reference_preserves_logical_digest_and_encoding(self):
        payload = b'{"schema_version": 11, "summary": {}}\n'
        report = self.directory / "pair-native.json.gz"
        with gzip.open(report, "wb") as stream:
            stream.write(payload)
        reference = capture.report_reference(report)
        self.assertEqual(reference["sha256"], hashlib.sha256(payload).hexdigest())
        self.assertEqual(reference["logical_bytes"], len(payload))
        self.assertEqual(reference["file_sha256"], hashlib.sha256(report.read_bytes()).hexdigest())
        self.assertEqual(reference["encoding"], "gzip")
        self.assertEqual(reference["bytes"], report.stat().st_size)

    def test_open_report_reads_plain_and_compressed_content(self):
        payload = b"content"
        plain = self.directory / "plain.json"
        plain.write_bytes(payload)
        compressed = self.directory / "compressed.json.gz"
        with gzip.open(compressed, "wb") as stream:
            stream.write(payload)
        self.assertEqual(capture.open_report(plain).read(), payload)
        self.assertEqual(capture.open_report(compressed).read(), payload)

    def test_truncated_archive_fails_closed(self):
        report = self.directory / "truncated-native.json.gz"
        with gzip.open(report, "wb") as stream:
            stream.write(b'{"schema_version": 11, "difference_status": "detected"}\n')
        raw = report.read_bytes()
        report.write_bytes(raw[: len(raw) // 2])
        with self.assertRaises(capture.NativeReportError):
            capture.read_native_report(report)

    def test_marker_transition_preserves_existing_pin_and_reason(self):
        record = {
            "head": "a" * 40,
            "binary": {"sha256": "b" * 64},
            "panel": {"sha256": "c" * 64},
        }
        args = type("Args", (), {"route": "native"})()
        created = "2026-09-20T00:00:00+00:00"
        active = capture.run_marker_fields(
            record, args, 36, "active", created,
            existing={"pinned": True, "reason": "final audit", "created_utc": created},
        )
        completed = capture.run_marker_fields(
            record, args, 36, "completed", created, existing=active
        )
        self.assertTrue(completed["pinned"])
        self.assertEqual(completed["reason"], "final audit")
        self.assertEqual(completed["created_utc"], created)
        self.assertEqual(completed["state"], "completed")

    def test_transition_marker_preserves_on_disk_pin(self):
        record = {
            "head": "a" * 40,
            "binary": {"sha256": "b" * 64},
            "panel": {"sha256": "c" * 64},
        }
        args = type("Args", (), {"route": "native"})()
        marker = self.directory / ".capture-run.json"
        capture.write_run_marker(
            marker,
            {
                "version": 1,
                "kind": "panel-capture",
                "state": "active",
                "pinned": True,
                "reason": "hold during capture",
                "created_utc": "2026-09-20T00:00:00+00:00",
                "updated_utc": "2026-09-20T00:00:00+00:00",
                "head": "a" * 40,
                "binary_sha256": "b" * 64,
                "panel_sha256": "c" * 64,
                "route": "native",
                "fixed_denominator": 36,
                "selected_pairs": 36,
            },
        )
        fields = capture.transition_run_marker(
            marker, record, args, 36, "completed", "2026-09-20T01:00:00+00:00"
        )
        self.assertTrue(fields["pinned"])
        self.assertEqual(fields["reason"], "hold during capture")
        self.assertEqual(fields["created_utc"], "2026-09-20T00:00:00+00:00")
        self.assertEqual(fields["state"], "completed")
        on_disk = json.loads(marker.read_text())
        self.assertTrue(on_disk["pinned"])
        self.assertEqual(on_disk["state"], "completed")

    def test_report_reference_resolves_migrated_plaintext_path(self):
        payload = b'{"schema_version": 11}\n'
        old = self.directory / "pair-native.json"
        archive = self.directory / "pair-native.json.gz"
        with gzip.open(archive, "wb") as stream:
            stream.write(payload)
        reference = capture.report_reference(old)
        self.assertEqual(reference["path"], str(archive))
        self.assertEqual(reference["sha256"], hashlib.sha256(payload).hexdigest())
        self.assertEqual(reference["encoding"], "gzip")
        self.assertEqual(reference["bytes"], archive.stat().st_size)

    def test_resolve_report_path_handles_gzip_and_manifest(self):
        exact = self.directory / "exact-native.json"
        exact.write_text("{}")
        self.assertEqual(capture.resolve_report_path(exact), exact)
        compressed_base = self.directory / "migrated-native.json"
        archive = self.directory / "migrated-native.json.gz"
        with gzip.open(archive, "wb") as stream:
            stream.write(b"{}")
        self.assertEqual(capture.resolve_report_path(compressed_base), archive)
        manifest_base = self.directory / "old" / "manifest-native.json"
        manifest_base.parent.mkdir()
        archive2 = manifest_base.parent / "manifest-native.json.gz"
        with gzip.open(archive2, "wb") as stream:
            stream.write(b"{}")
        (self.directory / "compression-manifest.json").write_text(
            json.dumps(
                {
                    "version": 1,
                    "entries": [
                        {
                            "old_path": str(manifest_base),
                            "new_path": str(archive2),
                        }
                    ],
                }
            )
        )
        self.assertEqual(capture.resolve_report_path(manifest_base), archive2)
        with self.assertRaises(FileNotFoundError):
            capture.resolve_report_path(self.directory / "missing-native.json")

    def test_preflight_free_space_fails_below_reserve(self):
        with mock.patch.object(
            capture.shutil,
            "disk_usage",
            return_value=type("Usage", (), {"free": capture.MIN_FREE_BYTES - 1})(),
        ):
            with self.assertRaises(RuntimeError):
                capture.require_free_space(self.directory)

    def test_run_marker_records_state_and_identity(self):
        record = {
            "head": "a" * 40,
            "binary": {"sha256": "b" * 64},
            "panel": {"sha256": "c" * 64},
        }
        args = type("Args", (), {"route": "native"})()
        fields = capture.run_marker_fields(record, args, 13, "active", "2026-09-20T00:00:00+00:00")
        self.assertEqual(fields["version"], 1)
        self.assertEqual(fields["state"], "active")
        self.assertFalse(fields["pinned"])
        self.assertEqual(fields["selected_pairs"], 13)
        path = self.directory / ".capture-run.json"
        capture.write_run_marker(path, fields)
        self.assertEqual(json.loads(path.read_text())["head"], "a" * 40)


if __name__ == "__main__":
    unittest.main()
