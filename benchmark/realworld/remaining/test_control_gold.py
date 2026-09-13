"""A partial numeric annotation never excuses an unreviewed outside event."""

import unittest

import verify


class ControlGoldTests(unittest.TestCase):
    def test_partial_gold_requires_outside_review_and_exact_inside_claims(self):
        gold = {("old", 1), ("new", 2)}
        target = gold | {("old", 3), ("new", 4)}

        def event(pointer, atoms):
            return {"category": "A", "pointer": pointer, "sources": atoms,
                    "operation": "replacement", "source_projection": {}}

        inside = event("/inside", gold)
        outside = event("/outside", {("old", 10), ("new", 20)})
        review = {"/outside": {"event_sha256": verify.event_digest(outside)}}

        def check(events, reviews, partial):
            return verify.strict_control_evidence(events, gold, target, 1, reviews, partial)[0]

        self.assertTrue(check([inside, outside], review, True))
        self.assertFalse(check([inside, outside], review, False))
        self.assertFalse(check([inside, outside], {}, True))
        self.assertFalse(check([inside, outside], {"/outside": {"event_sha256": "stale"}}, True))
        self.assertFalse(check([inside, inside], {}, True))
        widened = event("/outside", outside["sources"] | {("old", 3)})
        self.assertFalse(check([inside, widened], {
            "/outside": {"event_sha256": verify.event_digest(widened)}}, True))
        self.assertFalse(check([event("/inside", gold | {("old", 3)})], {}, True))
        self.assertTrue(check([inside], {}, False))


if __name__ == "__main__":
    unittest.main()
