"""Retention audit regressions on independent serde-style fixtures."""
import json

import gzip
import hashlib
from pathlib import Path
import shutil
import tempfile
import unittest

import audit_retention


BASE = """{
  "schema_version": 11,
  "assessment": {
    "policy_version": 1,
    "work_limit": 10,
    "work_used": %(work_used)d,
    "work_by_stage": {
      "localization": %(localization)d
    },
    "candidates_truncated": false,
    "old_resolution": [
      {
        "block": 0,
        "state": "equal",
        "bbox": {
          "x": %(x)s
        }
      }
    ],
    "new_resolution": [],
    "relations": [
      {
        "relation": 1,
        "sources": [
          {
            "glyph_id": %(glyph_id)d
          }
        ]
      }
    ],
    "review_units": []
  },
  "summary": {
    "difference_status": "detected",
    "content_changes": %(content_changes)d
  },
  "changes": [],
  "change_candidates": [],
  "proven_changed_regions": [],
  "formatting_only_changes": [],
  "unresolved_regions": [],
  "extraction": {
    "old_complete": %(old_complete)s,
    "new_complete": true,
    "issues": []
  }
}
"""


def report_text(**overrides):
    fields = {
        "work_used": 5,
        "localization": 5,
        "x": "1.0",
        "glyph_id": 7,
        "content_changes": 1,
        "old_complete": "true",
    }
    fields.update(overrides)
    return BASE % fields


class RetentionAuditTests(unittest.TestCase):
    def setUp(self):
        self.directory = Path(tempfile.mkdtemp(prefix="pdfdelta-audit-test-"))

    def tearDown(self):
        shutil.rmtree(self.directory, ignore_errors=True)

    def write(self, name, text):
        path = self.directory / name
        path.write_text(text)
        return path

    def test_work_counters_do_not_change_member_digests(self):
        left = self.write("left.json", report_text(work_used=5, localization=5))
        right = self.write("right.json", report_text(work_used=9, localization=9))
        result = audit_retention.audit_pair(left, right)
        self.assertEqual(result["different_members"], [])
        self.assertEqual(len(result["left_members"]), 11)

    def test_nested_child_values_change_their_own_member(self):
        left = self.write("left.json", report_text())
        summary_changed = self.write("summary.json", report_text(content_changes=2))
        extraction_changed = self.write("extraction.json", report_text(old_complete="false"))
        relations_changed = self.write("relations.json", report_text(glyph_id=8))

        summary = audit_retention.audit_pair(left, summary_changed)
        self.assertEqual(summary["different_members"], ["summary"])
        extraction = audit_retention.audit_pair(left, extraction_changed)
        self.assertEqual(extraction["different_members"], ["extraction"])
        relations = audit_retention.audit_pair(left, relations_changed)
        self.assertEqual(relations["different_members"], ["relations"])

    def test_resolution_coordinates_are_hashed(self):
        left = self.write("left.json", report_text())
        moved = self.write("moved.json", report_text(x="2.5"))
        result = audit_retention.audit_pair(left, moved)
        self.assertEqual(result["different_members"], ["old_resolution"])

    def test_duplicate_member_key_fails(self):
        text = report_text().replace(
            '  "changes": [],',
            '  "changes": [],\n  "summary": {\n    "content_changes": 9\n  },',
        )
        duplicate = self.write("duplicate.json", text)
        left = self.write("left.json", report_text())
        with self.assertRaises(audit_retention.AuditError):
            audit_retention.audit_pair(left, duplicate)

    def test_missing_member_fails(self):
        text = report_text().replace('  "extraction": {', '  "unused": {')
        missing = self.write("missing.json", text.replace('"issues": []', '"issues": []'))
        left = self.write("left.json", report_text())
        with self.assertRaises(audit_retention.AuditError):
            audit_retention.audit_pair(left, missing)

    def test_truncated_report_fails(self):
        text = report_text()
        truncated = self.write("truncated.json", text[: len(text) // 2])
        left = self.write("left.json", report_text())
        with self.assertRaises(audit_retention.AuditError):
            audit_retention.audit_pair(left, truncated)

    def test_removed_final_root_close_fails_even_against_itself(self):
        text = report_text()
        # Remove only the final unindented root close line; every member and
        # their indented closing braces remain.
        assert text.rstrip().endswith("\n}")
        without_root_close = text.rstrip()[: -len("\n}")] + "\n"
        truncated = self.write("no-root-close.json", without_root_close)
        with self.assertRaises(audit_retention.AuditError):
            audit_retention.audit_pair(truncated, truncated)
        digests, seen, complete = audit_retention.scan_member_digests(truncated)
        self.assertFalse(complete)
        self.assertEqual(seen["extraction"], 1)


class ReferenceModeTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="pdfdelta-audit-modes-"))
        self.left = self.root / "left"
        self.right = self.root / "right"
        self.baseline_path = self.root / "baseline.json"

    def tearDown(self):
        shutil.rmtree(self.root, ignore_errors=True)

    def capture(self, root, pair, payload, **overrides):
        run = root / pair
        run.mkdir(parents=True)
        report = run / f"{pair}-native.json.gz"
        with gzip.open(report, "wb") as stream:
            stream.write(payload)
        logical = hashlib.sha256(payload).hexdigest()
        summary = {
            "rows": [
                {"pair": pair, "report": {"path": str(report), "sha256": logical}}
            ],
            "head": "h" * 40,
            "binary": {"sha256": "b" * 64},
            "panel": {"sha256": audit_retention.PANEL_SHA256},
            "fixed_denominator": 36,
            "route": "native",
            "limit_scale": 1,
            "timeout_seconds": 180,
        }
        summary.update(overrides)
        (root / "summary.json").write_text(json.dumps(summary))
        return logical

    def baseline_file(self, pair, logical):
        self.baseline_path.write_text(
            json.dumps({"records": [{"pair": pair, "report_sha256": logical}]})
        )

    def test_baseline_mode_rejects_changed_non_proxy(self):
        payload = report_text().encode()
        left_logical = self.capture(self.left, "pair-a", payload)
        right_logical = self.capture(self.right, "pair-a", report_text(content_changes=2).encode())
        self.baseline_file("pair-a", left_logical)
        with self.assertRaises(audit_retention.AuditError):
            audit_retention.run_audit(
                [], self.left, self.right, "baseline", baseline_path=self.baseline_path
            )
        self.assertNotEqual(left_logical, right_logical)

    def test_accepted_mode_rejects_changed_non_proxy(self):
        payload = report_text().encode()
        self.capture(self.left, "pair-a", payload)
        self.capture(self.right, "pair-a", report_text(content_changes=2).encode())
        self.baseline_file("pair-a", hashlib.sha256(payload).hexdigest())
        with self.assertRaises(audit_retention.AuditError):
            audit_retention.run_audit(
                [], self.left, self.right, "accepted", baseline_path=self.baseline_path
            )

    def test_accepted_mode_rejects_tampered_left(self):
        payload = report_text().encode()
        self.capture(self.left, "pair-a", payload)
        self.capture(self.right, "pair-a", payload)
        self.baseline_file("pair-a", hashlib.sha256(payload).hexdigest())
        report = self.left / "pair-a" / "pair-a-native.json.gz"
        with gzip.open(report, "wb") as stream:
            stream.write(report_text(content_changes=5).encode())
        with self.assertRaises(audit_retention.AuditError):
            audit_retention.run_audit(
                [], self.left, self.right, "accepted", baseline_path=self.baseline_path
            )

    def test_missing_capture_pair_metadata_fails(self):
        payload = report_text().encode()
        self.capture(self.left, "pair-a", payload)
        self.capture(self.right, "pair-a", payload)
        self.baseline_file("pair-a", hashlib.sha256(payload).hexdigest())
        summary = json.loads((self.right / "summary.json").read_text())
        summary["rows"] = []
        (self.right / "summary.json").write_text(json.dumps(summary))
        with self.assertRaises(audit_retention.AuditError):
            audit_retention.run_audit(
                [], self.left, self.right, "accepted", baseline_path=self.baseline_path
            )

    def test_baseline_mode_proxy_requires_left_baseline_match(self):
        left_payload = report_text().encode()
        right_payload = report_text(content_changes=2).encode()
        self.capture(self.left, "pair-a", left_payload)
        self.capture(self.right, "pair-a", right_payload)
        self.baseline_file("pair-a", hashlib.sha256(right_payload).hexdigest())
        with self.assertRaises(audit_retention.AuditError):
            audit_retention.run_audit(
                ["pair-a"], self.left, self.right, "baseline",
                baseline_path=self.baseline_path,
            )

    def test_accepted_mode_uses_verified_left_reference(self):
        left_payload = report_text().encode()
        self.capture(self.left, "pair-a", left_payload)
        self.capture(self.right, "pair-a", left_payload)
        # The baseline record differs from the accepted left reference; the
        # accepted mode compares against the verified left capture instead.
        self.baseline_file("pair-a", "f" * 64)
        report = audit_retention.run_audit(
            ["pair-a"], self.left, self.right, "accepted",
            baseline_path=self.baseline_path,
        )
        self.assertEqual(report["proxy_member_compared"], ["pair-a"])
        self.assertEqual(report["different"], [])
        self.assertEqual(report["unexpected_different"], [])

    def test_accepted_mode_requires_native_route(self):
        payload = report_text().encode()
        self.capture(self.left, "pair-a", payload, route="text")
        self.capture(self.right, "pair-a", payload)
        self.baseline_file("pair-a", hashlib.sha256(payload).hexdigest())
        with self.assertRaises(audit_retention.AuditError):
            audit_retention.run_audit(
                ["pair-a"], self.left, self.right, "accepted",
                baseline_path=self.baseline_path,
            )

    def test_accepted_mode_requires_fixed_limits(self):
        payload = report_text().encode()
        self.capture(self.left, "pair-a", payload, limit_scale=2)
        self.capture(self.right, "pair-a", payload)
        self.baseline_file("pair-a", hashlib.sha256(payload).hexdigest())
        with self.assertRaises(audit_retention.AuditError):
            audit_retention.run_audit(
                ["pair-a"], self.left, self.right, "accepted",
                baseline_path=self.baseline_path,
            )

    def test_accepted_mode_requires_binary_identity(self):
        payload = report_text().encode()
        self.capture(self.left, "pair-a", payload, binary={})
        self.capture(self.right, "pair-a", payload)
        self.baseline_file("pair-a", hashlib.sha256(payload).hexdigest())
        with self.assertRaises(audit_retention.AuditError):
            audit_retention.run_audit(
                ["pair-a"], self.left, self.right, "accepted",
                baseline_path=self.baseline_path,
            )

    def test_baseline_mode_ignores_accepted_metadata(self):
        payload = report_text().encode()
        self.capture(self.left, "pair-a", payload, route="text", limit_scale=2)
        self.capture(self.right, "pair-a", payload, route="text", limit_scale=2)
        self.baseline_file("pair-a", hashlib.sha256(payload).hexdigest())
        audit_retention.run_audit(
            [], self.left, self.right, "baseline", baseline_path=self.baseline_path
        )

    def test_parse_cli_defaults_to_baseline_reference(self):
        parsed = audit_retention.parse_cli(["p.json", "left", "right"])
        self.assertEqual(parsed.reference, "baseline")
        self.assertEqual(
            str(parsed.output),
            "benchmark/realworld/cache/native-12-of-36-2026-09-20/retention-audit.json",
        )

    def test_expected_proxy_difference_is_reported_separately(self):
        left_payload = report_text().encode()
        right_payload = report_text(content_changes=2).encode()
        self.capture(self.left, "pair-a", left_payload)
        self.capture(self.right, "pair-a", right_payload)
        self.baseline_file("pair-a", hashlib.sha256(left_payload).hexdigest())
        report = audit_retention.run_audit(
            ["pair-a"], self.left, self.right, "accepted",
            baseline_path=self.baseline_path,
            expected_differences=["pair-a"],
        )
        self.assertEqual(report["unexpected_different"], [])
        self.assertEqual(
            [item["pair"] for item in report["expected_different"]], ["pair-a"]
        )
        self.assertIn("summary", report["expected_different"][0]["members"])

    def test_accepted_mode_proxy_pair_compares_members(self):
        payload = report_text().encode()
        self.capture(self.left, "pair-a", payload)
        self.capture(self.right, "pair-a", payload)
        self.baseline_file("pair-a", hashlib.sha256(payload).hexdigest())
        report = audit_retention.run_audit(
            ["pair-a"], self.left, self.right, "accepted", baseline_path=self.baseline_path
        )
        self.assertEqual(report["proxy_member_compared"], ["pair-a"])
        self.assertEqual(report["different"], [])


if __name__ == "__main__":
    unittest.main()
