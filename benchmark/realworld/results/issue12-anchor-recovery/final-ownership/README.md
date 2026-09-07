# Diagnose failed replacements from final source ownership

The 29-pair replay changes only the failure reason of two unmatched NIST CSF replacements. Each old quote is completely owned by a final deletion and each new quote by a final insertion, with no final unresolved overlap. Their original alignment span's reading-order uncertainty no longer describes the final output. The benchmark now reports `alignment_or_candidate` for this case. Both replacements remain failures and reviewed recall remains 1/3.

`comparison.json` records absolute before/after coverage, token metrics, event metrics, reviewed recall, and final expected-change failures for every pair. Status, coverage, all measured quality metrics, and all other failure lists are unchanged from `../gap-budget/`. Bulky intermediate recovery-watch traces are omitted from this projection; `source.json` records the raw capture hashes and executable hash. Captures use the annotations at the recorded commit, before the separate six-pair scope review.

The shared diagnostic checks final unresolved overlap first, then unique, unclaimed one-sided ownership. A surviving reading-order unresolved region still reports `reading_order_unresolved`. Ambiguous ownership and exhausted diagnostic budgets do not justify the new classification. `code.patch` records the change against the commit in `source.json` and applies with `git apply --unidiff-zero`; `../csf-final-ownership/` contains the source-range evidence.

Formatting, Clippy with warnings denied, 1,988 workspace tests, and all 48 generated verification cases passed. The latter includes line-wrap invariance, page-break invariance, single replacement, paragraph insertion, and paragraph deletion. The bounded code review found no reproducible defect.

This change corrects diagnostics; it does not add a recovered replacement, raise recall, or establish accuracy outside the reviewed scopes.
