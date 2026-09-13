"""Evaluator regressions; constructed records never count as real PDF results."""

import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import verify


class EvidenceTests(unittest.TestCase):
    def test_native_regions_bind_endpoints_and_transition_identity(self):
        report = self.report()
        result = report["comparison"]["scopes"][0]["result"]
        review = result["text_scope_reviews"][0]
        review["boundaries"] = [0, 1]
        review["comparison"].update(old=[2], new=[2])
        result.update(accepted_correspondences=[0, 1], text_boundary_correspondences=[],
                      matching={"source_only_mandatory": [0, 1], "inferred_proposals": []},
                      candidates={"proposals": [{"old": [1], "new": [1]}, {"old": [3], "new": [3]}]})
        review["native_regions"] = {"old": None, "new": {
            "convention": "native-tag-ordered-page-regions-v1",
            "regions": [{"page": 0, "nodes": [1, 2], "bounded_paint": False},
                        {"page": 1, "nodes": [3], "bounded_paint": False}],
            "transitions": [{"before": {"origin": "native", "glyph": 12},
                             "after": {"origin": "native", "glyph": 13},
                             "structure": {"origin": "structured", "element": 7}, "position": 13}],
        }}
        with patch.object(verify, "CONTRACT", "historical"), self.assertRaises(ValueError):
            verify.events(report)
        with patch.object(verify, "CONTRACT", "source-boundaries-v1"):
            event = verify.events(report)[0]
            self.assertEqual(event["source_projection"]["native_regions"], review["native_regions"])
            native = copy.deepcopy(report)
            native["comparison"]["scopes"][0]["result"]["text_scope_reviews"][0]["native_regions"]["new"]["convention"] = "native-k-parent-bound-page-regions-v1"
            self.assertEqual(verify.events(native)[0]["category"], "B")
            for mutation in ("missing", "duplicate", "order", "unaccepted", "inferred"):
                invalid = copy.deepcopy(report)
                scope = invalid["comparison"]["scopes"][0]["result"]
                chain = scope["text_scope_reviews"][0]["native_regions"]["new"]
                if mutation == "missing":
                    chain["transitions"] = []
                elif mutation == "duplicate":
                    chain["regions"][1]["nodes"] = [2, 3]
                elif mutation == "order":
                    chain["transitions"][0]["position"] = 0
                elif mutation == "unaccepted":
                    scope["accepted_correspondences"] = [0]
                else:
                    chain["convention"] = "inferred-page-order"
                with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                    verify.events(invalid)

    def test_local_presence_needs_an_empty_interval_and_scoped_gold(self):
        report = self.report()
        result = report["comparison"]["scopes"][0]["result"]
        review = result["text_scope_reviews"][0]
        review["old_sources"] = []
        review["boundaries"] = [0, 1]
        review["presence"] = {"convention": "closed-native-interval-presence-v1", "present": "new"}
        review["comparison"].update(old=[], new=[2], operation={"kind": "text_changed", "old": "", "new": "new"})
        for side in ("old", "new"):
            review[side + "_boundaries"] = [[{"origin": "native", "glyph": 10}], [{"origin": "native", "glyph": 11}]]
        result.update(accepted_correspondences=[0, 1], text_boundary_correspondences=[],
                      matching={"source_only_mandatory": [0, 1], "inferred_proposals": []},
                      candidates={"proposals": [{"old": [1], "new": [1]}, {"old": [3], "new": [3]}]})
        with patch.object(verify, "CONTRACT", "historical"), self.assertRaises(ValueError):
            verify.events(report)
        with patch.object(verify, "CONTRACT", "source-boundaries-v1"):
            event = verify.events(report)[0]
            self.assertEqual(event["source_projection"]["presence"], review["presence"])
            core = {("new", 2)}
            self.assertTrue(verify.range_recovery(review, core, core)["source_range_hit"])
            self.assertFalse(verify.range_recovery(review, core | {("old", 1)}, core | {("old", 1)})["source_range_hit"])
            for mutation in ("not_empty", "no_endpoint", "unaccepted", "global_claim"):
                invalid = copy.deepcopy(report)
                scope = invalid["comparison"]["scopes"][0]["result"]
                row = scope["text_scope_reviews"][0]
                if mutation == "not_empty":
                    row["old_sources"] = [{"origin": "native", "glyph": 1}]
                elif mutation == "no_endpoint":
                    row["old_boundaries"][1] = []
                elif mutation == "unaccepted":
                    scope["accepted_correspondences"] = [0]
                else:
                    row["presence"]["convention"] = "document-wide-insertion"
                with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                    verify.events(invalid)

    def test_source_cut_identity_binds_its_accepted_population(self):
        report = self.report()
        result = report["comparison"]["scopes"][0]["result"]
        review = result["text_scope_reviews"][0]
        review["boundaries"] = []
        review["comparison"].update(old=[2], new=[5])
        result.update(accepted_correspondences=[0, 1], text_boundary_correspondences=[],
                      matching={"source_only_mandatory": [0, 1], "inferred_proposals": []},
                      candidates={"proposals": [{"old": [1], "new": [4]}, {"old": [3], "new": [6]}]})
        review["source_cuts"] = {
            "convention": "unique-native-fragment-cuts-v2",
            "projection": "retained-glyph-ligatures-spacing-v1",
            "population": {"kind": "matched_interval", "boundaries": [0, 1], "old": [1, 2, 3], "new": [4, 5, 6]},
            "entry": {"old": {"node": 2, "token_boundary": 1}, "new": {"node": 5, "token_boundary": 1},
                      "evidence": {"kind": "unique_native_fragment", **{
                          side: {"node": node, "tokens": [0, 1], "sources": [{"origin": "native", "glyph": 10}]}
                          for side, node in (("old", 2), ("new", 5))}}},
            "exit": {"old": {"node": 3, "token_boundary": 0}, "new": {"node": 6, "token_boundary": 0},
                     "evidence": {"kind": "accepted_boundary", "proposal": 1}},
        }
        with patch.object(verify, "CONTRACT", "historical"), self.assertRaises(ValueError):
            verify.events(report)
        with patch.object(verify, "CONTRACT", "source-boundaries-v1"):
            event = verify.events(report)[0]
            self.assertEqual(event["source_projection"]["source_cuts"], review["source_cuts"])
            for mutation in ("inferred", "unaccepted", "escaped", "split", "profile"):
                invalid = copy.deepcopy(report)
                result = invalid["comparison"]["scopes"][0]["result"]
                row = result["text_scope_reviews"][0]
                if mutation == "inferred":
                    result["matching"]["inferred_proposals"] = [0]
                elif mutation == "unaccepted":
                    result["accepted_correspondences"] = [0]
                elif mutation == "escaped":
                    row["comparison"]["old"] = [99]
                elif mutation == "split":
                    row["source_cuts"]["entry"]["old"]["token_boundary"] = 2
                else:
                    row["source_cuts"]["projection"] = "unknown"
                with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                    verify.events(invalid)

        row_ordered = copy.deepcopy(report)
        row = row_ordered["comparison"]["scopes"][0]["result"]["text_scope_reviews"][0]
        row["source_cuts"]["population"]["row_order"] = {
            "convention": "horizontal-row-boundaries-v1",
            "old": [{"node": node, "sources": [{"origin": "native", "glyph": glyph}]}
                    for node, glyph in ((1, 21), (3, 23))], "new": None,
        }
        row_proof = row["source_cuts"]["population"]["row_order"]
        for convention in ("horizontal-row-boundaries-v1", "horizontal-paint-row-boundaries-v1"):
            row_proof["convention"] = convention
            with patch.object(verify, "CONTRACT", "source-boundaries-v1"):
                self.assertEqual(verify.events(row_ordered)[0]["category"], "B")
                for mutation in ("node", "source", "profile", "body"):
                    invalid = copy.deepcopy(row_ordered)
                    row = invalid["comparison"]["scopes"][0]["result"]["text_scope_reviews"][0]
                    proof = row["source_cuts"]["population"]["row_order"]
                    if mutation == "node":
                        proof["old"][0]["node"] = 2
                    elif mutation == "source":
                        proof["old"][0]["sources"] = []
                    elif mutation == "profile":
                        proof["convention"] = "unknown"
                    else:
                        proof["old"][0]["sources"] = copy.deepcopy(row["old_sources"])
                    with self.subTest(row_mutation=mutation), self.assertRaises(ValueError):
                        verify.events(invalid)

        padded = copy.deepcopy(report)
        row = padded["comparison"]["scopes"][0]["result"]["text_scope_reviews"][0]
        row["source_cuts"]["projection"] = "retained-glyph-boundary-padding-v1"
        row["source_cuts"]["population"]["boundary_padding"] = {
            "convention": "optional-clipped-boundary-padding-v1", "old": [],
            "new": [{"origin": "native", "glyph": 99}],
        }
        with patch.object(verify, "CONTRACT", "source-boundaries-v1"):
            self.assertEqual(verify.events(padded)[0]["category"], "B")
            for mutation in ("body", "boundary", "missing", "profile"):
                invalid = copy.deepcopy(padded)
                row = invalid["comparison"]["scopes"][0]["result"]["text_scope_reviews"][0]
                if mutation == "body":
                    row["new_sources"].append({"origin": "native", "glyph": 99})
                elif mutation == "boundary":
                    row["source_cuts"]["entry"]["evidence"]["new"]["sources"][0]["glyph"] = 99
                elif mutation == "missing":
                    del row["source_cuts"]["population"]["boundary_padding"]
                else:
                    row["source_cuts"]["projection"] = "retained-glyph-ligatures-spacing-v1"
                with self.subTest(padding_mutation=mutation), self.assertRaises(ValueError):
                    verify.events(invalid)

        refined = copy.deepcopy(report)
        result = refined["comparison"]["scopes"][0]["result"]
        parent = result["text_scope_reviews"][0]
        inner = copy.deepcopy(parent)
        for side in ("old", "new"):
            parent[side + "_sources"].append({"origin": "native", "glyph": 99})
            parent["comparison"]["operation"][side] += " "
        inner["source_cuts"]["edge_refinement"] = {
            "convention": "mandatory-literal-space-content-edges-v1",
            "enclosing": [copy.deepcopy(parent["source_cuts"][name]) for name in ("entry", "exit")],
            **{side + "_padding": [[], [{"node": node, "tokens": [2, 3],
                                         "sources": [{"origin": "native", "glyph": 99}]}]]
               for side, node in (("old", 2), ("new", 5))},
        }
        inner["source_cuts"]["exit"] = {
            "old": {"node": 2, "token_boundary": 2}, "new": {"node": 5, "token_boundary": 2},
            "evidence": {"kind": "corresponding_content_edge"},
        }
        result["text_scope_reviews"].append(inner)
        with patch.object(verify, "CONTRACT", "source-boundaries-v1"):
            verify.checked_source_cuts(inner, result)
            for mutation in ("parent", "overlap", "text", "certificate", "empty"):
                invalid = copy.deepcopy(result)
                row = invalid["text_scope_reviews"][1]
                if mutation == "parent":
                    invalid["text_scope_reviews"].pop(0)
                elif mutation == "overlap":
                    row["old_sources"].append({"origin": "native", "glyph": 99})
                elif mutation == "text":
                    row["comparison"]["operation"]["new"] += "x"
                elif mutation == "certificate":
                    del row["source_cuts"]["edge_refinement"]
                else:
                    row["source_cuts"]["edge_refinement"]["old_padding"] = [[], []]
                with self.subTest(edge_mutation=mutation), self.assertRaises(ValueError):
                    verify.checked_source_cuts(row, invalid)

    def test_partial_spacing_needs_a_versioned_independent_change_proof(self):
        report = self.report()
        review = report["comparison"]["scopes"][0]["result"]["text_scope_reviews"][0]
        comparison = review["comparison"]
        comparison["unresolved"] = ["spacing and exact masks remain unresolved"]
        comparison["operation"] = {"kind": "text_changed", "old": "a 1", "new": "a 2"}
        comparison["text_change_proof"] = {
            "token": {"Scalar": "1"}, "old_required": 1, "old_possible": 1,
            "new_required": 0, "new_possible": 0,
        }
        review["spacing"] = {"convention": "source-space-interpretations-v1", **{
            side: [{"position": 1, "origin": "reconstructed_gap", "sources": copy.deepcopy(review[side + "_sources"])}]
            for side in ("old", "new")}}
        with patch.object(verify, "CONTRACT", "historical"), self.assertRaises(ValueError):
            verify.events(report)
        with patch.object(verify, "CONTRACT", "source-boundaries-v1"):
            self.assertEqual(verify.events(report)[0]["category"], "B")
            core = {("old", 1), ("new", 2)}
            self.assertTrue(verify.range_recovery(review, core, core)["source_range_hit"])
            self.assertFalse(verify.range_recovery(review, core, {("old", 1)})["source_range_hit"])
            score = verify.pair_recovery(None, report, core, core, set(), self.adjudication(report))
            self.assertEqual(score["additional_categories"], ["B"])
            for mutation in ("no_witness", "wrong_count", "no_change", "missing_space", "external_source"):
                invalid = copy.deepcopy(report)
                row = invalid["comparison"]["scopes"][0]["result"]["text_scope_reviews"][0]
                if mutation == "no_witness":
                    del row["comparison"]["text_change_proof"]
                elif mutation == "wrong_count":
                    row["comparison"]["text_change_proof"]["old_possible"] = 2
                elif mutation == "no_change":
                    row["comparison"]["text_change_proof"]["old_required"] = 0
                elif mutation == "missing_space":
                    row["spacing"]["old"] = []
                else:
                    row["spacing"]["old"][0]["sources"][0]["glyph"] = 999
                with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                    verify.events(invalid)

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
