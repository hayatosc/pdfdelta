"""Controls for report auditing; these do not establish PDF recovery."""

import copy
import itertools
import unittest

from audit_reports import audit_report


def report(inventory=True, source=True, search=True):
    coverage = {"channel": "text", "complete": inventory and source}
    for side in ("old", "new"):
        coverage.update({f"{side}_inventory_complete": inventory,
                         f"{side}_discovered_sources": 3,
                         f"{side}_compared_sources": 3 if source else 1,
                         f"{side}_uncompared_sources": 0 if source else 2})
    return {"contract": {"channels": ["text"]}, "coverage": [coverage],
            "comparison_complete": inventory and source and search,
            "comparison": {"relation_unresolved": [], "scopes": [{
                "interpretation": "conditional_on_correspondence", "result": {
                    "unresolved": [] if search else ["competing optima"],
                    "structural_correspondences": [], "matching": {"components": []}}}]}}


class AuditTests(unittest.TestCase):
    def test_all_independent_obligations_survive_non_owning_reviews(self):
        for inventory, source, search, reviews in itertools.product((False, True), repeat=4):
            value = report(inventory, source, search)
            value["comparison"]["scopes"][0]["result"]["text_scope_reviews"] = [{}] if reviews else []
            with self.subTest(inventory=inventory, source=source, search=search, reviews=reviews):
                self.assertEqual(audit_report(value)["obligations"],
                                 {"inventory": inventory, "source": source, "search": search})

    def test_missing_or_false_complete_evidence_is_rejected(self):
        for mutation in ("empty", "duplicate", "wrong_channel", "lost_source", "false_complete"):
            value = report(False, False, False)
            if mutation == "empty":
                value["coverage"] = []
            elif mutation == "duplicate":
                value["coverage"] *= 2
            elif mutation == "wrong_channel":
                value["coverage"][0]["channel"] = "visual"
            elif mutation == "lost_source":
                value["coverage"][0]["old_uncompared_sources"] = 0
            else:
                value["comparison_complete"] = True
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                audit_report(value)

    def test_relations_and_pending_child_scope_independently_block_search(self):
        for relation in (False, True):
            value = copy.deepcopy(report())
            value["comparison_complete"] = False
            if relation:
                value["comparison"]["relation_unresolved"] = ["unknown relation"]
            else:
                value["comparison"]["scopes"][0]["result"]["structural_correspondences"] = [0]
            self.assertFalse(audit_report(value)["obligations"]["search"])


if __name__ == "__main__":
    unittest.main()
