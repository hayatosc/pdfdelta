"""Guard against empty-text success and incomplete panel evidence."""

import gzip
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import native_panel


def artifact12(tokens=4, engine="flow"):
    """Current flow-native schema 12 compact artifact."""
    scope = {"supported_text": True, "images_compared": False}
    summary = {"comparison_complete": True, "difference_status": "detected",
               "content_changes": 1, "formatting_only_changes": 0,
               "uncertain_changes": 0, "unsupported_extraction_issues": 0,
               "unresolved_extraction_issues": 0, "unexamined_paint_issues": 0,
               "old_tokens": tokens, "new_tokens": tokens}
    return {"artifact_format": "pdfdelta-native-compact", "artifact_version": 3,
            "source_schema_version": 12, "engine": engine,
            "report": {"schema_version": 12, "engine": engine,
                       "comparison_scope": scope, "difference_status": "detected",
                       "summary": summary,
                       "extraction": {"old_complete": True, "new_complete": True,
                                      "issues": []}}}


def artifact(tokens=4, engine="flow"):
    """Historical flow schema 11 compact artifact with compatibility positions."""
    scope = {"supported_text": True, "images_compared": False}
    coverage = {"total_tokens": tokens, "resolved_tokens": tokens, "ratio": 1.0}
    summary = {"comparison_complete": True, "difference_status": "detected",
               "comparison_scope": scope, "established_changes": 1, "content_changes": 1,
               "proven_changed_regions": 0, "formatting_only_changes": 0, "uncertain_changes": 0,
               "unresolved_regions": 0, "tentative_candidates": 0,
               "unsupported_extraction_issues": 0, "unresolved_extraction_issues": 0,
               "old_alignment_coverage": dict(coverage), "new_alignment_coverage": dict(coverage)}
    payload = {"artifact_format": "pdfdelta-native-compact", "artifact_version": 2,
               "source_schema_version": 11, "report": {"schema_version": 11,
               "comparison_scope": scope, "difference_status": "detected", "summary": summary,
               "assessment": None,
               "extraction": {"old_complete": True, "new_complete": True, "issues": []}}}
    payload["engine"] = engine
    payload["report"]["engine"] = engine
    payload["report"]["not_produced_by_engine"] = ["change_candidates"]
    payload["report"]["summary"]["unexamined_paint_issues"] = 0
    return payload


class NativePanelTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.report = self.root / "report.json.gz"

    def read(self, payload):
        with gzip.open(self.report, "wb") as stream:
            stream.write(json.dumps(payload).encode())
        return native_panel.read_native_summary(self.report)

    def test_nonempty_complete_report(self):
        self.assertTrue(self.read(artifact12())["meaningful_complete"])

    def test_current_schema_is_accepted(self):
        result = self.read(artifact12())
        self.assertEqual(result["engine"], "flow")
        self.assertEqual(result["old_tokens"], 4)
        self.assertEqual(result["new_tokens"], 4)
        self.assertTrue(result["meaningful_complete"])

    def test_historical_schema_11_is_still_accepted(self):
        result = self.read(artifact())
        self.assertEqual(result["engine"], "flow")
        self.assertTrue(result["meaningful_complete"])

    def test_flow_engine_report_is_accepted(self):
        result = self.read(artifact())
        self.assertEqual(result["engine"], "flow")
        self.assertTrue(result["meaningful_complete"])

    def test_historical_proof_report_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "unsupported text engine"):
            self.read(artifact(engine="proof"))

    def test_either_empty_side_is_excluded_even_when_engine_complete(self):
        for side in ("old", "new"):
            payload = artifact12()
            payload["report"]["summary"][side + "_tokens"] = 0
            result = self.read(payload)
            self.assertTrue(result["comparison_complete"])
            self.assertFalse(result["meaningful_complete"])
            self.assertTrue(result["empty_native_text"])
        for side in ("old", "new"):
            payload = artifact()
            payload["report"]["summary"][side + "_alignment_coverage"].update(total_tokens=0, resolved_tokens=0)
            result = self.read(payload)
            self.assertTrue(result["comparison_complete"])
            self.assertFalse(result["meaningful_complete"])
            self.assertTrue(result["empty_native_text"])

    def test_false_completion_is_rejected(self):
        for field in ("tentative_candidates", "proven_changed_regions", "unresolved_regions"):
            payload = artifact()
            payload["report"]["summary"][field] = 1
            with self.assertRaisesRegex(ValueError, "completeness"):
                self.read(payload)
        payload = artifact12()
        payload["report"]["extraction"]["old_complete"] = False
        with self.assertRaisesRegex(ValueError, "completeness"):
            self.read(payload)

    def test_extraction_issue_and_coverage_must_agree(self):
        payload = artifact12()
        payload["report"]["extraction"]["issues"] = [{"kind": "unsupported"}]
        with self.assertRaisesRegex(ValueError, "issue count"):
            self.read(payload)
        payload = artifact()
        payload["report"]["extraction"]["issues"] = [{"kind": "unsupported"}]
        with self.assertRaisesRegex(ValueError, "issue count"):
            self.read(payload)
        payload = artifact()
        payload["report"]["summary"]["old_alignment_coverage"]["ratio"] = 0.5
        with self.assertRaisesRegex(ValueError, "ratio"):
            self.read(payload)

    def test_flow_paint_issues_are_not_extraction_issues(self):
        payload = artifact12(engine="flow")
        payload["report"]["extraction"]["issues"] = [
            {"kind": "unresolved", "message": "non-text paint differs; text drawn as paths or images was not examined"},
            {"kind": "unresolved", "message": "a glyph is missing"},
        ]
        payload["report"]["summary"]["unexamined_paint_issues"] = 1
        payload["report"]["summary"]["unresolved_extraction_issues"] = 1
        result = self.read(payload)
        self.assertEqual(result["unexamined_paint_issues"], 1)
        payload["report"]["summary"]["unexamined_paint_issues"] = 2
        with self.assertRaisesRegex(ValueError, "paint issue count"):
            self.read(payload)

        payload = artifact(engine="flow")
        payload["report"]["extraction"]["issues"] = [
            {"kind": "unresolved", "description": "non-text paint differs; text drawn as paths or images was not examined"},
            {"kind": "unresolved", "description": "a glyph is missing"},
        ]
        payload["report"]["summary"]["unexamined_paint_issues"] = 1
        payload["report"]["summary"]["unresolved_extraction_issues"] = 1
        result = self.read(payload)
        self.assertEqual(result["unexamined_paint_issues"], 1)
        # A pre-counter report that included paint in the unresolved count is
        # still readable, and the paint count is recovered from its messages.
        payload["report"]["summary"].pop("unexamined_paint_issues")
        payload["report"]["summary"]["unresolved_extraction_issues"] = 2
        result = self.read(payload)
        self.assertEqual(result["unexamined_paint_issues"], 1)
        # A new report that counts paint twice is rejected.
        payload["report"]["summary"]["unexamined_paint_issues"] = 1
        with self.assertRaisesRegex(ValueError, "extraction issue count"):
            self.read(payload)
        payload["report"]["summary"]["unresolved_extraction_issues"] = 1
        payload["report"]["summary"]["unexamined_paint_issues"] = 2
        with self.assertRaisesRegex(ValueError, "paint issue count"):
            self.read(payload)

    def test_negative_boolean_and_missing_counts_are_rejected(self):
        for value in (-1, True, 1.5):
            payload = artifact()
            payload["report"]["summary"]["tentative_candidates"] = value
            with self.assertRaisesRegex(ValueError, "nonnegative integer"):
                self.read(payload)
        payload = artifact12()
        payload["report"]["summary"]["old_tokens"] = -1
        with self.assertRaisesRegex(ValueError, "nonnegative integer"):
            self.read(payload)
        payload = artifact()
        del payload["report"]["summary"]["unresolved_regions"]
        with self.assertRaisesRegex(ValueError, "missing contract"):
            self.read(payload)
        payload = artifact12()
        del payload["report"]["summary"]["new_tokens"]
        with self.assertRaisesRegex(ValueError, "missing contract"):
            self.read(payload)

    def test_truncated_and_trailing_json_are_rejected(self):
        data = json.dumps(artifact12()).encode()
        for invalid in (data[:-2], data + b"{}"):
            with gzip.open(self.report, "wb") as stream:
                stream.write(invalid)
            with self.assertRaises(ValueError):
                native_panel.read_native_summary(self.report)

    def test_capture_command_names_the_flow_engine_explicitly(self):
        pair = {"id": "pair", "old": {"path": "old.pdf"}, "new": {"path": "new.pdf"}}
        command = native_panel.capture_command(Path("pdfdelta"), self.root, pair,
                                               self.root / "report.json.gz")
        self.assertIn("--engine", command)
        self.assertEqual(command[command.index("--engine") + 1], "flow")
        self.assertEqual(command.count("--engine"), 1)

    def test_missing_or_mismatched_input_never_runs_engine(self):
        source = {"path": "source.pdf", "bytes": 3, "sha256": hashlib.sha256(b"pdf").hexdigest()}
        pair = {"id": "pair", "old": source, "new": source}
        with patch("native_panel.subprocess.Popen") as process:
            self.assertEqual(native_panel.capture_pair(Path("engine"), self.root, pair, self.root, 1)["status"], "input_unavailable")
            self.root.joinpath("source.pdf").write_bytes(b"bad")
            result = native_panel.capture_pair(Path("engine"), self.root, pair, self.root, 1)
            self.assertEqual(result["inputs"]["old"]["status"], "hash_mismatch")
            process.assert_not_called()

    def test_mismatched_existing_input_is_preserved(self):
        path = self.root / "source.pdf"
        path.write_bytes(b"bad")
        source = {"path": "source.pdf", "bytes": 3, "sha256": hashlib.sha256(b"pdf").hexdigest()}
        with patch("native_panel.urlopen") as request:
            self.assertEqual(native_panel.fetch_source(self.root, source, 1)["status"], "hash_mismatch")
            request.assert_not_called()
        self.assertEqual(path.read_bytes(), b"bad")

    def test_download_hash_includes_buffered_tail(self):
        data = b"small public PDF fixture"
        source = {"path": "source.pdf", "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(),
                  "url": "https://example.org/fixture.pdf"}
        response = io.BytesIO(data)
        response.status = 200
        response.url = source["url"]
        with patch("native_panel.urlopen", return_value=response):
            result = native_panel.fetch_source(self.root, source, 1)
        self.assertEqual(result["status"], "verified")
        self.assertEqual(result["sha256"], source["sha256"])
        self.assertEqual(native_panel.check_source(self.root, source)["status"], "verified")

    def test_completed_interrupted_download_is_verified_before_reuse(self):
        source = {"path": "source.pdf", "bytes": 3, "sha256": hashlib.sha256(b"pdf").hexdigest()}
        partial = self.root / "source.download"
        for data, status in ((b"bad", "unfinished_download"), (b"pdf", "verified")):
            partial.write_bytes(data)
            with patch("native_panel.urlopen") as request:
                self.assertEqual(native_panel.fetch_source(self.root, source, 1)["status"], status)
                request.assert_not_called()

    def test_retry_retains_previous_failed_bytes(self):
        source = {"path": "source.pdf", "bytes": 3, "sha256": hashlib.sha256(b"pdf").hexdigest(),
                  "url": "https://example.org/fixture.pdf"}
        self.root.joinpath("source.download").write_bytes(b"bad")
        response = io.BytesIO(b"pdf")
        response.status, response.url = 200, source["url"]
        with patch("native_panel.urlopen", return_value=response):
            result = native_panel.fetch_source(self.root, source, 1, retry=True)
        self.assertEqual(result["status"], "verified")
        self.assertEqual(Path(result["previous_download"]["path"]).read_bytes(), b"bad")

    def test_memory_guard_rejects_unbounded_or_swapping_workloads(self):
        for memory, swap in (("max", "0"), ("6000000001", "0"), ("6000000000", "1")):
            self.root.joinpath("memory.max").write_text(memory)
            self.root.joinpath("memory.swap.max").write_text(swap)
            with self.assertRaisesRegex(ValueError, "shared memory"):
                native_panel.require_memory_guard(self.root)

    def test_denominator_keeps_failures_and_not_run_rows(self):
        rows = [{"status": "input_unavailable"}, {"status": "execution_failed"},
                {"status": "not_run"}, {"status": "captured", "summary": {"meaningful_complete": True, "comparison_complete": True}}]
        self.assertEqual(native_panel.totals(rows)["fixed_denominator"], 36)
        self.assertEqual(native_panel.totals(rows)["meaningful_complete"], 1)


if __name__ == "__main__":
    unittest.main()
