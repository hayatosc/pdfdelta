"""Retention audit regressions on independent serde-style fixtures."""

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


if __name__ == "__main__":
    unittest.main()
