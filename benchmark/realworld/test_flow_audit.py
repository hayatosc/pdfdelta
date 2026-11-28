"""Unit tests for the differential audit helpers (stdlib only)."""

from __future__ import annotations

import collections
import sys
import pathlib
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import flow_audit


class AuditHelperTests(unittest.TestCase):
    def test_normalise_splits_digit_letter_edges_and_folds_quotes(self) -> None:
        self.assertEqual(flow_audit.normalise(["10th"]), ["10", "th"])
        self.assertEqual(
            flow_audit.normalise_text("\u201cquoted\u201d"), ['"', "quoted", '"']
        )
        self.assertEqual(flow_audit.normalise(["\u00ad"]), [])

    def test_minimal_diff_marks_changed_tokens_on_both_sides(self) -> None:
        old_flags, new_flags, old_counter, new_counter, status = (
            flow_audit.diff_changed(["alpha", "beta", "gamma"], ["alpha", "delta", "gamma"], 60)
        )
        self.assertEqual(status, "ok")
        self.assertEqual(list(old_flags), [0, 1, 0])
        self.assertEqual(list(new_flags), [0, 1, 0])
        self.assertEqual(old_counter, collections.Counter({"beta": 1}))
        self.assertEqual(new_counter, collections.Counter({"delta": 1}))

    def test_flow_changed_counts_match_the_report_ranges(self) -> None:
        report = {
            "changes": [
                {"kind": "delete", "old": [1, 3], "new": [0, 0]},
                {"kind": "move", "old": [3, 4], "new": [0, 0]},
                {"kind": "layout_only", "old": [0, 1], "new": [0, 1]},
            ]
        }
        old = ["alpha", "beta", "gamma", "delta"]
        new = ["alpha", "beta", "gamma", "delta"]
        old_flags, new_flags, old_counter, new_counter = flow_audit.flow_changes(
            report, old, new
        )
        self.assertEqual(list(old_flags), [0, 1, 1, 0])
        self.assertEqual(list(new_flags), [0, 0, 0, 0])
        self.assertEqual(old_counter, collections.Counter({"beta": 1, "gamma": 1}))
        self.assertEqual(new_counter, collections.Counter())

    def test_only_flags_and_runs_follow_the_exclusive_multiset(self) -> None:
        changed = bytearray([1, 1, 1, 0])
        tokens = ["alpha", "beta", "gamma", "alpha"]
        flags = flow_audit.only_flags(changed, tokens, collections.Counter({"beta": 1}))
        self.assertEqual(list(flags), [0, 1, 0, 0])
        self.assertEqual(flow_audit.maximal_runs(flags, context=1), [(0, 3)])

    def test_candidate_runs_split_at_page_boundaries(self) -> None:
        # A flag run that crosses from a page-2 footer into a page-3 heading
        # must become two single-page regions, so the clip of one page is never
        # compared with the other page's text.
        flags = bytearray([0, 0, 1, 1, 1, 1, 0, 0])
        pages = [2, 2, 2, 3, 3, 3, 3, 3]
        regions = flow_audit.split_run_at_pages(0, 8, flags, pages)
        self.assertEqual(regions, [(0, 3), (3, 8)])
        self.assertTrue(all(
            len({pages[index] for index in range(start, end)}) == 1
            for start, end in regions
        ))

    def test_sampling_is_deterministic_and_stratified(self) -> None:
        candidates = [
            {"pair": "b", "side": "old", "tool": "flow-only", "start": 0, "end": 1},
            {"pair": "a", "side": "old", "tool": "flow-only", "start": 5, "end": 6},
            {"pair": "a", "side": "new", "tool": "oracle-only", "start": 1, "end": 2},
            {"pair": "a", "side": "old", "tool": "flow-only", "start": 9, "end": 10},
        ]
        first = flow_audit.sample_candidates(candidates, 3)
        second = flow_audit.sample_candidates(candidates, 3)
        self.assertEqual(
            [(item["pair"], item["side"], item["tool"], item["start"]) for item in first],
            [("a", "new", "oracle-only", 1), ("a", "old", "flow-only", 5), ("b", "old", "flow-only", 0)],
        )
        self.assertEqual(first, second)

    def test_multiset_compare_reports_both_exclusive_sets(self) -> None:
        flow = collections.Counter({"a": 2, "b": 1})
        oracle = collections.Counter({"a": 2, "c": 1})
        result = flow_audit.multiset_compare(flow, oracle)
        self.assertEqual(result["common"], 2)
        self.assertEqual(result["flow_only"], 1)
        self.assertEqual(result["oracle_only"], 1)

    def test_verdict_categories_use_the_clipped_content(self) -> None:
        self.assertEqual(
            flow_audit.classify(
                "flow-only",
                2,
                2,
                "old text",
                "new text",
                ["new"],
                ["new"],
                "",
                "",
                collections.Counter(),
                collections.Counter({"new": 1}),
                False,
            )[0],
            "flow correct",
        )
        # The same token on another page is a re-pairing, not a missed change:
        # the single-page clip cannot see where the oracle's reading order put
        # the region. This is the #114 bis-operational-risk case.
        self.assertEqual(
            flow_audit.classify(
                "oracle-only",
                2,
                2,
                "",
                "provided the source is",
                ["8", "Contents", "to"],
                ["the", "8", "Contents", "to"],
                "",
                "",
                collections.Counter(),
                collections.Counter({"8": 1, "Contents": 1, "to": 1}),
                False,
            )[0],
            "both acceptable (text exists in the other document)",
        )
        # A token that occurs nowhere in the other document and in no flow
        # change is a real oracle-only difference.
        self.assertEqual(
            flow_audit.classify(
                "oracle-only",
                2,
                2,
                "old text",
                "invented text",
                ["invented"],
                ["old"],
                "",
                "",
                collections.Counter(),
                collections.Counter({"invented": 1}),
                False,
            )[0],
            "oracle correct",
        )
        self.assertEqual(
            flow_audit.classify(
                "oracle-only",
                2,
                2,
                "old text",
                "new text",
                ["old"],
                ["old"],
                "",
                "",
                collections.Counter({"old": 1}),
                collections.Counter({"old": 1}),
                False,
            )[0],
            "both acceptable (flow reports a move/layout change)",
        )
        self.assertEqual(
            flow_audit.classify(
                "flow-only",
                2,
                2,
                "same",
                "same",
                ["moved", "word"],
                ["moved", "word", "x"],
                "",
                "",
                collections.Counter(),
                collections.Counter({"new": 1}),
                False,
            )[0],
            "flow correct (reading order)",
        )
        self.assertEqual(
            flow_audit.classify(
                "oracle-only",
                2,
                2,
                "same",
                "same",
                ["token"],
                [],
                "",
                "",
                collections.Counter(),
                collections.Counter({"new": 1}),
                False,
            )[0],
            "both acceptable (tokenisation or minimal script)",
        )
        self.assertEqual(
            flow_audit.classify(
                "flow-only",
                2,
                2,
                "",
                "",
                ["x"],
                [],
                "",
                "",
                collections.Counter(),
                collections.Counter(),
                False,
            )[0],
            "undecidable",
        )


if __name__ == "__main__":
    unittest.main()
