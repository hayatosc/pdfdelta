"""Evaluator regressions; constructed records never count as real PDF results."""

import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

import verify


class EvidenceTests(unittest.TestCase):
    @staticmethod
    def report(category="B", extra=False):
        review = {
            "old_sources": [{"origin": "native", "glyph": 1}],
            "new_sources": [{"origin": "native", "glyph": 2}],
            "old_boundaries": [[], []], "new_boundaries": [[], []], "convention": "fixture",
            "comparison": {"interpretation": "conditional_on_correspondence" if category == "B" else "inferred",
                           "compared": True, "unresolved": [],
                           "operation": {"kind": "text_changed", "old": "old", "new": "new"}},
        }
        if extra:
            review["new_sources"].append({"origin": "native", "glyph": 3})
        return {"typed_changes": 0, "scope_content_changes": int(category == "B"),
                "inferred_scope_changes": int(category == "C"),
                "comparison": {"scopes": [{"result": {"comparisons": [], "text_scope_reviews": [review]}}]}}

    @staticmethod
    def adjudication(report):
        event = verify.events(report)[0]
        return [{"pointer": event["pointer"], "event_sha256": verify.event_digest(event),
                 "verdict": "source_supported", "source_content_rationale": "Synthetic source match",
                 "correspondence_rationale": "Synthetic closed boundary match"}]

    def test_range_recovery_needs_a_new_finite_adjudicated_b(self):
        core = {("old", 1), ("new", 2)}
        report = self.report()
        reviews = self.adjudication(report)
        score = verify.pair_recovery(None, report, core, core, set(), reviews)
        self.assertEqual(score["additional_categories"], ["B"])
        self.assertIsNone(score["strict_event_precision"])
        self.assertFalse(verify.pair_recovery(None, report, core, core, set(), [])["correct"])
        self.assertEqual(verify.pair_recovery(report, report, core, core, set(), [])["additional_categories"], [])
        oversized = self.report(extra=True)
        self.assertEqual(verify.pair_recovery(None, oversized, core, core, set(),
                                            self.adjudication(oversized))["additional_categories"], [])
        inferred = self.report(category="C")
        self.assertEqual(verify.pair_recovery(None, inferred, core, core, set(), [])["additional_categories"], [])

    def test_gate_reduction_can_pass_without_counting_c_or_duplicate_pairs(self):
        development = {f"dev-{i}": f"publisher-{i % 3}" for i in range(6)}
        blind = {f"blind-{i}": f"publisher-{i % 2}" for i in range(3)}
        gates = verify.gate_summary(development, blind, development, blind,
                                    {"kept"}, {"kept", "gain-1", "gain-2"}, True, True)
        self.assertTrue(all(gate["passed"] for gate in gates.values()))
        duplicated = verify.gate_summary(["dev-0"] * 6, [], development, blind,
                                         {"kept"}, {"gain-1", "gain-2"}, True, False)
        for gate in ("G1", "G2", "G3", "G5"):
            self.assertFalse(duplicated[gate]["passed"], gate)

    def test_complete_requires_finished_search_and_nonempty_scopes(self):
        coverage = {"channel": "text", "complete": True}
        for side in ("old", "new"):
            coverage.update({side + "_inventory_complete": True,
                             side + "_discovered_sources": 2,
                             side + "_compared_sources": 2,
                             side + "_presence_sources": 0,
                             side + "_uncompared_sources": 0})
        scope = {"unresolved": [], "candidates": {"exhaustive": True},
                 "text_search": {"exhaustive": True},
                 "matching": {"conflict_search_complete": True,
                              "components": [{"exhaustive": True}]}}
        report = {"comparison_complete": True, "coverage": [coverage],
                  "contract": {"version": 1, "channels": ["text"]},
                  "comparison": {"scopes": [{"result": scope}]}}
        run = {"status": "captured", "exit_code": 0}
        self.assertTrue(verify.complete(report, run))
        for key in ("candidates", "text_search"):
            invalid = copy.deepcopy(report)
            invalid["comparison"]["scopes"][0]["result"][key]["exhaustive"] = False
            with self.subTest(key=key), self.assertRaises(ValueError):
                verify.complete(invalid, run)
        invalid = copy.deepcopy(report)
        invalid["comparison"]["scopes"] = []
        with self.assertRaises(ValueError):
            verify.complete(invalid, run)
        self.assertFalse(verify.complete(report, {"status": "failed", "exit_code": 124}))

    def test_source_review_cannot_override_exact_strict_gold(self):
        report = self.report()
        scope = report["comparison"]["scopes"][0]["result"]
        scope["text_scope_reviews"] = []
        scope["comparisons"] = [{
            "operation": {"kind": "text_changed", "old": "a", "new": "b"},
            "interpretation": "conditional_on_correspondence", "compared": True, "unresolved": [],
            "text_mask": {"old": [{"sources": [{"origin": "native", "glyph": 1}]}],
                          "new": [{"sources": [{"origin": "native", "glyph": 2},
                                               {"origin": "native", "glyph": 3}]}]},
        }]
        report.update(typed_changes=1, scope_content_changes=0)
        core = {("old", 1), ("new", 2)}
        score = verify.pair_recovery(None, report, core, core | {("new", 3)}, set(),
                                     self.adjudication(report),
                                     {"sources": core, "operation": scope["comparisons"][0]["operation"]})
        self.assertFalse(score["correct"])
        self.assertEqual(score["additional_categories"], [])

    def test_native_candidates_and_unlocalized_regions_are_not_recovery(self):
        span = {"sources": [{"kind": "glyph", "glyph_id": 1}]}
        candidate = {"kind": "replacement", "occurrences": [{"old_span": span, "new_span": span}]}
        report = {"changes": [], "change_candidates": [candidate], "proven_changed_regions": [
            {"old_span": span, "new_span": span, "proof": "exact_token_multiset_mismatch"}]}
        events = verify.events(report)
        self.assertEqual([event["category"] for event in events], ["C", "C"])
        core = {("old", 1), ("new", 1)}
        self.assertEqual(verify.pair_recovery(None, report, core, core, set(), [])["additional_categories"], [])

    def test_observations_bind_bytes_routes_budgets_and_repetitions(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)

            def save(name, value):
                path = directory / name
                path.write_text(json.dumps(value))
                return {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}

            binary = save("binary", "synthetic executable")
            report = self.report()
            report.update(comparison_complete=False, contract={"version": 1, "channels": ["text"]})
            reference = save("report.json", report)
            pair = {"id": "fixture", "old": {"sha256": "old"}, "new": {"sha256": "new"}}
            run = {"pair": "fixture", "route": "text", "status": "captured", "exit_code": 3,
                   "old_sha256": "old", "new_sha256": "new", "wall_seconds": 1, "peak_rss_kib": 10,
                   "report_sha256": reference["sha256"], "report_bytes": (directory / "report.json").stat().st_size}
            capture = {"binary_sha256": binary["sha256"], "timeout_seconds": 180, "limit_scale": 1,
                       "runs": [run, dict(run, wall_seconds=2)]}
            capture_reference = save("capture.json", capture)
            index = {"observations": [{"pair": "fixture", "repetition": repeat, "run_index": repeat - 1,
                                       "capture": capture_reference, "report": reference} for repeat in (1, 2)]}
            observed = verify.observations(index, {"pairs": [pair]}, binary)
            self.assertEqual(len(observed), 2)
            self.assertFalse(any(row["complete"] for row in observed.values()))
            invalid = copy.deepcopy(index)
            invalid["observations"].append(invalid["observations"][0])
            with self.assertRaisesRegex(ValueError, "duplicate"):
                verify.observations(invalid, {"pairs": [pair]}, binary)
            invalid = copy.deepcopy(index)
            invalid["observations"][1]["run_index"] = 0
            with self.assertRaisesRegex(ValueError, "reused as a repetition"):
                verify.observations(invalid, {"pairs": [pair]}, binary)
            for field, value in (("limit_scale", 2), ("timeout_seconds", 181)):
                invalid_capture = copy.deepcopy(capture)
                invalid_capture[field] = value
                invalid = copy.deepcopy(index)
                invalid["observations"][0]["capture"] = save("invalid-capture.json", invalid_capture)
                with self.subTest(field=field), self.assertRaisesRegex(ValueError, "budget changed"):
                    verify.observations(invalid, {"pairs": [pair]}, binary)
            (directory / "report.json").write_text("{}")
            with self.assertRaisesRegex(ValueError, "stale evidence"):
                verify.observations(index, {"pairs": [pair]}, binary)
            (directory / "report.json").unlink()
            with self.assertRaises(FileNotFoundError):
                verify.observations(index, {"pairs": [pair]}, binary)


if __name__ == "__main__":
    unittest.main()
