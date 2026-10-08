"""Test failures that would silently invalidate source quotation gold."""
import hashlib
import pathlib
import tempfile
import unittest
from unittest.mock import patch

from verify_source_gold import verify


class SourceGoldAuditTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        self.root.joinpath("source.pdf").write_bytes(b"frozen PDF fixture")
        source = {"path": "source.pdf", "bytes": 18,
                  "sha256": hashlib.sha256(b"frozen PDF fixture").hexdigest()}
        reference = {"page": 2, "quote": "Original wording."}
        self.gold = {"sources": {"pair": {"old": source, "new": source}},
                     "targets": [{"id": "body", "pair": "pair",
                                  "old": reference, "new": reference}]}

    def test_declared_page_and_punctuation_are_checked(self):
        with patch("verify_source_gold.subprocess.run") as extract:
            extract.return_value.stdout = "cover\fOriginal\nwording.\f"
            self.assertEqual(verify(self.root, self.gold)["source_references_checked"], 2)
            extract.return_value.stdout = "Original wording.\fDifferent body\f"
            with self.assertRaisesRegex(ValueError, "quotation not unique"):
                verify(self.root, self.gold)
            extract.return_value.stdout = "cover\fOriginal wording!\f"
            with self.assertRaisesRegex(ValueError, "quotation not unique"):
                verify(self.root, self.gold)

    def test_duplicate_quote_is_rejected(self):
        with patch("verify_source_gold.subprocess.run") as extract:
            extract.return_value.stdout = "cover\fOriginal wording. Original wording.\f"
            with self.assertRaisesRegex(ValueError, "quotation not unique"):
                verify(self.root, self.gold)

    def test_geometric_label_mutation_is_rejected(self):
        reference = {"page": 2, "quote": "b Energy",
                     "field_label_bbox_pdf_points": [0, 0, 10, 10],
                     "field_body_bbox_pdf_points": [10, 0, 20, 10]}
        self.gold["targets"][0].update(old=reference, new=reference)
        xml = ('<html xmlns="http://www.w3.org/1999/xhtml"><page/><page>'
               '<word xMin="0" yMin="0" xMax="3" yMax="5">b</word>'
               '<word xMin="11" yMin="0" xMax="18" yMax="5">Energy</word>'
               '</page></html>')
        with patch("verify_source_gold.subprocess.run") as extract:
            extract.side_effect = lambda command, **kwargs: type(
                "Extraction", (), {"stdout": xml if "-bbox-layout" in command
                                   else "cover\fEnergy\f"})()
            self.assertEqual(verify(self.root, self.gold)["geometric_field_quotes_checked"], 2)
            reference["quote"] = "27c Energy"
            with self.assertRaisesRegex(ValueError, "geometric field quote mismatch"):
                verify(self.root, self.gold)

    def test_source_mutation_is_rejected_before_extraction(self):
        self.root.joinpath("source.pdf").write_bytes(b"altered PDF bytes!")
        with patch("verify_source_gold.subprocess.run") as extract:
            with self.assertRaisesRegex(ValueError, "source identity mismatch"):
                verify(self.root, self.gold)
            extract.assert_not_called()


if __name__ == "__main__":
    unittest.main()
