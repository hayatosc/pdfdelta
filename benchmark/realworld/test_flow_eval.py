"""Pin the oracle tokeniser and normalisation used by flow_eval.

The PyMuPDF order oracle in ``flow_eval.oracle_page_tokens`` calls ``tokenize``
and the order-agreement columns compare the oracle with the flow tokens under
``metric_token``/``metric_tokens``. Those functions are separate from quote
matching: quotes use ``split_token_edges`` so an extracted ``10th`` can match
``10``/``th`` without changing the oracle. A change to the shared helpers that
alters the oracle previously moved the reported order agreement by up to 0.09,
so this test pins the Phase N normalisation.
"""

import unittest

import flow_eval


class OracleTokenizerTests(unittest.TestCase):
    def test_oracle_tokenizer_keeps_digit_letter_runs(self):
        self.assertEqual(
            flow_eval.tokenize("10th Edition / December 2020"),
            ["10th", "Edition", "/", "December", "2020"],
        )
        self.assertEqual(
            flow_eval.tokenize("w3 and 03762v6 and 2024"),
            ["w3", "and", "03762v6", "and", "2024"],
        )
        self.assertEqual(flow_eval.tokenize("A2B"), ["A2B"])

    def test_oracle_tokenizer_splits_punctuation_and_cjk(self):
        self.assertEqual(flow_eval.tokenize("a,b"), ["a", ",", "b"])
        self.assertEqual(flow_eval.tokenize("設定"), ["設", "定"])
        self.assertEqual(flow_eval.tokenize("word\u00a0word"), ["word", "word"])

    def test_quote_edge_split_is_separate_from_the_oracle(self):
        self.assertEqual(flow_eval.split_token_edges("10th"), ["10", "th"])
        self.assertEqual(flow_eval.split_token_edges("03762v6"), ["03762", "v", "6"])
        self.assertEqual(flow_eval.split_token_edges("w3"), ["w", "3"])
        self.assertEqual(flow_eval.split_token_edges("2024"), ["2024"])

    def test_metric_normalisation(self):
        self.assertEqual(flow_eval.metric_token("\u00ad"), "")
        self.assertEqual(flow_eval.metric_token("\ufb01"), "fi")
        self.assertEqual(flow_eval.metric_token("e\u0301"), "\u00e9")
        self.assertEqual(flow_eval.metric_token("\u201cquoted\u201d"), '"quoted"')
        self.assertEqual(flow_eval.metric_token("\u2014"), "-")
        self.assertEqual(flow_eval.metric_token("a\u200bb"), "ab")
        self.assertEqual(flow_eval.metric_tokens(["a", "", "b"]), ["a", "b"])

    def test_order_agreement_uses_token_sequences(self):
        self.assertEqual(
            flow_eval.oracle_order_agreement(
                ["a", "b", "c"], ["a", "b", "c"]
            ),
            (1.0, 3),
        )
        self.assertEqual(
            flow_eval.oracle_order_agreement(["a", "c"], ["a", "b", "c"]),
            (2 / 3, 3),
        )
        self.assertEqual(flow_eval.oracle_order_agreement([], []), (1.0, 0))


if __name__ == "__main__":
    unittest.main()
