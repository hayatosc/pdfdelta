"""Evaluator regressions; constructed records never count as real PDF results."""

import copy
import unittest

import verify


class EvidenceTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
