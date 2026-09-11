"""Small failure-mode tests for the evidence checker; these are not PDF results."""

import copy
import hashlib
from pathlib import Path
import tempfile
import unittest

import verify


class EvidenceTests(unittest.TestCase):
    def test_missing_and_stale_evidence_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "evidence.json"
            reference = {"path": str(path), "sha256": hashlib.sha256(b"{}").hexdigest()}
            with self.assertRaises(FileNotFoundError):
                verify.checked_path(reference)
            path.write_bytes(b"{}")
            self.assertEqual(verify.checked_path(reference), path)
            path.write_bytes(b"[]")
            with self.assertRaises(ValueError):
                verify.checked_path(reference)

    def test_duplicate_pair_cannot_increase_denominator(self):
        with self.assertRaises(ValueError):
            verify.unique_by([{"pair": "same"}, {"pair": "same"}], "pair")

    def test_scalar_source_multiplicity_is_preserved(self):
        atom = {"id": 8, "kind": "glyph"}
        self.assertEqual(verify.source_rows({"source_rows": [[0, 0, [atom]], [1, 1, [atom]]]}),
                         [[atom], [atom]])

    def test_complete_requires_full_nonempty_source_coverage(self):
        run = {"status": "captured", "exit_code": 1}
        coverage = {"channel": "text", "complete": True}
        for side in ("old", "new"):
            coverage.update({side + "_inventory_complete": True,
                             side + "_discovered_sources": 3,
                             side + "_compared_sources": 2,
                             side + "_presence_sources": 1,
                             side + "_uncompared_sources": 0})
        report = {"comparison_complete": True, "contract": {"channels": ["text"]},
                  "coverage": [coverage]}
        self.assertTrue(verify.common_complete(report, run))
        for field, value in (("old_discovered_sources", 0), ("old_inventory_complete", False),
                             ("new_uncompared_sources", 1), ("new_compared_sources", 0)):
            invalid = copy.deepcopy(report)
            invalid["coverage"][0][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                verify.common_complete(invalid, run)
        self.assertFalse(verify.common_complete(report, {"status": "failed", "exit_code": 124}))


if __name__ == "__main__":
    unittest.main()
