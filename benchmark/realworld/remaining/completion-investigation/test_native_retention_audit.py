"""Regression tests for the native retention verifier on synthetic fixtures."""

import unittest

import native_retention_audit as audit


def entry(block, start, end, state, glyphs=()):
    return {
        "block": block,
        "canonical_range": {"start": start, "end": end},
        "comparable_range": {"start": start, "end": end},
        "state": state,
        "sources": [{"kind": "glyph", "glyph_id": glyph} for glyph in glyphs],
    }


def report(old, new, regions=(), **members):
    return {
        "assessment": {"old_resolution": list(old), "new_resolution": list(new)},
        "unresolved_regions": list(regions),
        "changes": [],
        "formatting_only_changes": [],
        "proven_changed_regions": [],
        "change_candidates": [],
        **members,
    }


def region(old_blocks=(), new_blocks=()):
    def span(blocks):
        return {"blocks": list(blocks), "canonical_range": {"start": 0, "end": 1}}

    return {
        "old_span": span(old_blocks) if old_blocks else None,
        "new_span": span(new_blocks) if new_blocks else None,
    }


def canonical(result):
    return result["coordinates"]["canonical_range"]


class SideAuditTests(unittest.TestCase):
    def audit(self, before, after, side="old_resolution"):
        problems = []
        result = audit.audit_side(before, after, side, problems)
        return result, problems

    def test_partial_partition_passes(self):
        before = report(
            [entry(0, 0, 2, "equal", [1, 2]), entry(0, 2, 4, "unresolved", [3, 4])],
            [],
        )
        after = report(
            [
                entry(0, 0, 2, "equal", [1, 2]),
                entry(0, 2, 3, "unresolved", [3]),
                entry(0, 3, 4, "changed", [4]),
            ],
            [],
        )
        result, problems = self.audit(before, after)
        self.assertEqual(problems, [])
        self.assertEqual(canonical(result)["prior_resolved_lost"], 0)
        self.assertEqual(canonical(result)["newly_resolved"], 1)
        self.assertEqual(canonical(result)["transitions"]["equal->equal"], 2)
        self.assertEqual(canonical(result)["transitions"]["unresolved->changed"], 1)

    def test_new_resolution_from_uncertainty_only(self):
        before = report([entry(0, 0, 2, "unresolved", [1, 2])], [])
        after = report([entry(0, 0, 1, "equal", [1]), entry(0, 1, 2, "unresolved", [2])], [])
        result, problems = self.audit(before, after)
        self.assertEqual(problems, [])
        self.assertEqual(canonical(result)["newly_resolved"], 1)

    def test_prior_resolved_becomes_unresolved_fails(self):
        before = report([entry(0, 0, 2, "equal", [1, 2])], [])
        after = report([entry(0, 0, 2, "unresolved", [1, 2])], [])
        result, problems = self.audit(before, after)
        self.assertEqual(canonical(result)["prior_resolved_lost"], 2)
        self.assertTrue(any("prior resolved tokens" in problem for problem in problems))

    def test_equal_to_changed_is_reported_for_review(self):
        before = report([entry(0, 0, 2, "equal", [1, 2])], [])
        after = report([entry(0, 0, 2, "changed", [1, 2])], [])
        result, problems = self.audit(before, after)
        self.assertEqual(problems, [])
        self.assertEqual(canonical(result)["review_transitions"], {"equal->changed": 2})

    def test_token_universe_change_fails(self):
        before = report([entry(0, 0, 2, "unresolved", [1, 2])], [])
        after = report([entry(0, 0, 3, "unresolved", [1, 2, 3])], [])
        _, problems = self.audit(before, after)
        self.assertTrue(any("token universe changed" in problem or "tokens new after" in problem for problem in problems))

    def test_overlapping_ranges_are_rejected(self):
        before = report(
            [entry(0, 0, 2, "unresolved", [1, 2]), entry(0, 1, 3, "unresolved", [2, 3])],
            [],
        )
        after = report([entry(0, 0, 3, "unresolved", [1, 2, 3])], [])
        _, problems = self.audit(before, after)
        self.assertTrue(any("duplicate or overlapping token" in problem for problem in problems))

    def test_invalid_state_is_rejected(self):
        before = report([entry(0, 0, 1, "resolved", [1])], [])
        after = report([entry(0, 0, 1, "unresolved", [1])], [])
        _, problems = self.audit(before, after)
        self.assertTrue(any("invalid state" in problem for problem in problems))

    def test_source_glyph_loss_fails(self):
        before = report([entry(0, 0, 2, "unresolved", [1, 2])], [])
        after = report([entry(0, 0, 2, "unresolved", [1])], [])
        result, problems = self.audit(before, after)
        self.assertEqual(result["source_glyphs_lost"], 1)
        self.assertTrue(any("source glyph events changed" in problem for problem in problems))

    def test_source_glyph_invented_fails(self):
        before = report([entry(0, 0, 2, "unresolved", [1])], [])
        after = report([entry(0, 0, 2, "unresolved", [1, 2])], [])
        result, problems = self.audit(before, after)
        self.assertEqual(result["source_glyphs_invented"], 1)
        self.assertTrue(any("source glyph events changed" in problem for problem in problems))

    def test_dedup_across_split_entries_passes(self):
        before = report([entry(0, 0, 2, "unresolved", [1])], [])
        after = report(
            [entry(0, 0, 1, "unresolved", [1]), entry(0, 1, 2, "unresolved", [1])],
            [],
        )
        result, problems = self.audit(before, after)
        self.assertEqual(problems, [])
        self.assertEqual(result["source_glyphs_invented"], 0)

    def test_synthetic_split_preserves_projection(self):
        synthetic = {
            "block": 0,
            "canonical_range": {"start": 0, "end": 2},
            "comparable_range": {"start": 0, "end": 2},
            "state": "unresolved",
            "sources": [
                {"kind": "synthetic_space", "preceding_glyph_id": 1, "following_glyph_id": 2}
            ],
        }
        before = report([synthetic], [])
        after = report(
            [
                dict(
                    synthetic,
                    canonical_range={"start": 0, "end": 1},
                    comparable_range={"start": 0, "end": 1},
                ),
                entry(0, 1, 2, "unresolved", [1, 2]),
            ],
            [],
        )
        _, problems = self.audit(before, after)
        self.assertEqual(problems, [])

    def test_malformed_synthetic_endpoints_are_rejected(self):
        before = report([entry(0, 0, 2, "unresolved", [1, 2])], [])
        after = report([entry(0, 0, 2, "unresolved", [1, 2])], [])
        after["assessment"]["old_resolution"][0]["sources"].append(
            {"kind": "line_break", "preceding_glyph_id": 1}
        )
        _, problems = self.audit(before, after)
        self.assertTrue(any("malformed endpoints" in problem for problem in problems))

    def test_structural_event_change_is_review_only(self):
        line_break = {
            "kind": "line_break",
            "preceding_glyph_id": 1,
            "following_glyph_id": 2,
        }
        before = report([entry(0, 0, 2, "unresolved", [1, 2])], [])
        before["assessment"]["old_resolution"][0]["sources"].insert(1, line_break)
        after = report([entry(0, 0, 2, "unresolved", [1, 2])], [])
        result, problems = self.audit(before, after)
        self.assertEqual(problems, [])
        self.assertEqual(result["structural_event_differences"], 1)
        self.assertEqual(result["structural_review"], [0])

    def test_new_unresolved_block_fails(self):
        before = report([entry(0, 0, 1, "unresolved", [1])], [], regions=[region(old_blocks=[0])])
        after = report(
            [entry(0, 0, 1, "unresolved", [1])],
            [],
            regions=[region(old_blocks=[0]), region(old_blocks=[9])],
        )
        result, problems = self.audit(before, after)
        self.assertEqual(result["region_blocks"]["old_new_uncertainty"], 1)
        self.assertTrue(any("became unresolved" in problem for problem in problems))

    def test_region_block_namespaces_stay_separate(self):
        before = report(
            [entry(0, 0, 1, "unresolved", [1])],
            [],
            regions=[region(old_blocks=[0], new_blocks=[0])],
        )
        after = report(
            [entry(0, 0, 1, "unresolved", [1])],
            [],
            regions=[region(old_blocks=[0], new_blocks=[0])],
        )
        result, _ = self.audit(before, after)
        self.assertEqual(result["region_blocks"]["old_before"], 1)
        self.assertEqual(result["region_blocks"]["new_before"], 1)


class MembershipTests(unittest.TestCase):
    def test_certified_payload_loss_fails(self):
        before = report([], [], changes=[{"kind": "replacement", "occurrences": []}])
        after = report([], [])
        problems = []
        result = audit.exact_retention(problems, "changes", before, after)
        self.assertEqual(result["retained"], 0)
        self.assertTrue(any("prior payloads lost" in problem for problem in problems))

    def test_candidate_regeneration_accounted_by_assessment(self):
        source = {"kind": "glyph", "glyph_id": 7}
        before = report(
            [entry(0, 0, 1, "unresolved", [7])],
            [],
            change_candidates=[{"occurrences": [{"old_span": {"sources": [source]}}]}],
        )
        after = report([entry(0, 0, 1, "unresolved", [7])], [])
        result = audit.tentative_accounting(before, after, "old_resolution", "old_span")
        self.assertEqual(result["sources_missing"], 0)

    def test_candidate_side_evidence_is_not_satisfied_by_other_side(self):
        source = {"kind": "glyph", "glyph_id": 7, "page": 0}
        before = report(
            [],
            [],
            change_candidates=[{"occurrences": [{"new_span": {"sources": [source]}}]}],
        )
        after = report([entry(0, 0, 1, "unresolved", [7])], [])
        result = audit.tentative_accounting(before, after, "new_resolution", "new_span")
        self.assertEqual(result["sources_missing"], 1)

    def test_candidate_structural_events_are_not_required(self):
        glyph = {"kind": "glyph", "glyph_id": 7}
        before = report(
            [entry(0, 0, 1, "unresolved", [7])],
            [],
            change_candidates=[
                {
                    "occurrences": [
                        {
                            "old_span": {
                                "sources": [{"kind": "block_separator_space"}, glyph]
                            }
                        }
                    ]
                }
            ],
        )
        after = report([entry(0, 0, 1, "unresolved", [7])], [])
        result = audit.tentative_accounting(before, after, "old_resolution", "old_span")
        self.assertEqual(result["sources_missing"], 0)

    def test_candidate_source_unaccounted_fails(self):
        source = {"kind": "glyph", "glyph_id": 7}
        before = report(
            [entry(0, 0, 1, "unresolved", [8])],
            [],
            change_candidates=[{"occurrences": [{"old_span": {"sources": [source]}}]}],
        )
        after = report([entry(0, 0, 1, "unresolved", [8])], [])
        result = audit.tentative_accounting(before, after, "old_resolution", "old_span")
        self.assertEqual(result["sources_missing"], 1)


class ClassificationTests(unittest.TestCase):
    def test_clean_run_passes(self):
        self.assertEqual(audit.classify_retention([], [], 0), "pass")

    def test_problems_fail(self):
        self.assertEqual(audit.classify_retention(["problem"], [], 0), "fail")

    def test_review_is_not_a_clean_pass(self):
        self.assertEqual(audit.classify_retention([], ["review"], 0), "needs-review")

    def test_sequence_mismatch_is_not_a_clean_pass(self):
        self.assertEqual(audit.classify_retention([], [], 1), "needs-review")

    def test_review_and_problems_fail(self):
        self.assertEqual(audit.classify_retention(["problem"], ["review"], 1), "fail")




class LoaderProjectionTests(unittest.TestCase):
    def _report(self):
        return {
            "unresolved_regions": [
                {"old_span": {"blocks": [1], "sources": [{"kind": "glyph", "glyph_id": 9}]},
                 "new_span": {"blocks": [2]}, "text": "irrelevant"},
            ],
            "assessment": {
                "old_resolution": [
                    {"block": 1, "state": "changed", "canonical_range": {"start": 1, "end": 2},
                     "comparable_range": {"start": 1, "end": 2},
                     "sources": [{"kind": "glyph", "glyph_id": 5}], "text": "x" * 100000},
                ],
                "new_resolution": [
                    {"block": 2, "state": "changed", "canonical_range": {"start": 0, "end": 1},
                     "comparable_range": {"start": 0, "end": 1},
                     "sources": [{"kind": "glyph", "glyph_id": 6}]},
                ],
            },
            "change_candidates": [
                {"occurrences": [{"old_span": {"blocks": [1], "sources": [{"kind": "glyph", "glyph_id": 7}]},
                                  "new_span": {"blocks": [2], "sources": [{"kind": "glyph", "glyph_id": 8}]}}]},
            ],
            "changes": [{"payload": 1.5}],
            "formatting_only_changes": [{"payload": "B"}],
            "proven_changed_regions": [{"payload": "C"}],
        }

    def _write(self, directory, pair, report):
        target = directory / pair
        target.mkdir(parents=True, exist_ok=True)
        import gzip, json
        with gzip.open(target / f"{pair}-native.json.gz", "wt") as stream:
            json.dump(report, stream)

    def test_projection_retains_exact_semantics(self):
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            report = self._report()
            self._write(directory, "pair", report)
            loaded = audit.load_report(directory, "pair")
            self.assertNotIn("text", loaded["unresolved_regions"][0])
            self.assertEqual(loaded["unresolved_regions"][0]["old_span"]["blocks"], [1])
            self.assertNotIn("sources", loaded["unresolved_regions"][0]["old_span"])
            self.assertNotIn("text", loaded["assessment"]["old_resolution"][0])
            self.assertIsInstance(loaded["changes"][0]["payload"], float)
            self.assertEqual(loaded["changes"][0]["payload"], 1.5)
            self.assertEqual(loaded["_candidate_sources"]["old_span"],
                             {audit.canonical({"kind": "glyph", "glyph_id": 7})})
            problems = []
            retained = audit.exact_retention(problems, "changes", loaded, loaded)
            self.assertTrue(retained["identical"])
            self.assertEqual(problems, [])

    def test_removed_and_duplicated_certified_payloads_fail(self):
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            before = self._report()
            after = self._report()
            after["changes"] = []
            self._write(directory, "pair", before)
            before_loaded = audit.load_report(directory, "pair")
            self._write(directory, "pair", after)
            after_loaded = audit.load_report(directory, "pair")
            problems = []
            retained = audit.exact_retention(problems, "changes", before_loaded, after_loaded)
            self.assertFalse(retained["identical"])
            self.assertTrue(problems)
            # Actual count loss: two identical payloads before, one after.
            before2 = self._report()
            before2["changes"] = [{"payload": "A"}, {"payload": "A"}]
            after2 = self._report()
            after2["changes"] = [{"payload": "A"}]
            self._write(directory, "pair", before2)
            before2_loaded = audit.load_report(directory, "pair")
            self._write(directory, "pair", after2)
            after2_loaded = audit.load_report(directory, "pair")
            problems = []
            retained = audit.exact_retention(problems, "changes", before2_loaded, after2_loaded)
            self.assertFalse(retained["identical"])
            self.assertTrue(problems)

    def test_missing_candidate_source_is_reported(self):
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            before = self._report()
            after = self._report()
            after["assessment"]["old_resolution"] = []
            after["changes"] = []
            after["formatting_only_changes"] = []
            after["proven_changed_regions"] = []
            after["change_candidates"] = []
            self._write(directory, "pair", before)
            before_loaded = audit.load_report(directory, "pair")
            self._write(directory, "pair", after)
            after_loaded = audit.load_report(directory, "pair")
            accounting = audit.tentative_accounting(before_loaded, after_loaded, "old_resolution", "old_span")
            self.assertEqual(accounting["sources_missing"], 1)

    def test_missing_required_key_is_an_error(self):
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            report = self._report()
            del report["changes"]
            self._write(directory, "pair", report)
            with self.assertRaises(ValueError):
                audit.load_report(directory, "pair")



    def test_tentative_accounting_parity_and_self_consistency(self):
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            report = self._report()
            self._write(directory, "pair", report)
            loaded = audit.load_report(directory, "pair")
            self.assertEqual(
                audit.tentative_accounting(loaded, loaded, "old_resolution", "old_span")["sources_missing"],
                0,
            )
            legacy = report
            legacy_accounting = audit.tentative_accounting(legacy, legacy, "old_resolution", "old_span")
            projected_accounting = audit.tentative_accounting(loaded, loaded, "old_resolution", "old_span")
            self.assertEqual(legacy_accounting, projected_accounting)

    def test_wrong_array_type_is_an_error(self):
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            report = self._report()
            report["changes"] = {"payload": "not an array"}
            self._write(directory, "pair", report)
            with self.assertRaises(ValueError):
                audit.load_report(directory, "pair")

if __name__ == "__main__":
    unittest.main()
