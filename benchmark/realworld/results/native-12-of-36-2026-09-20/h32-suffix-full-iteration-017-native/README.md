# H32 full-panel capture 017 (intermediate increment)

36/36 captured, comparison_complete 3/36 (bounded capture summary; the accepted
score remains 3/36 until the final goal evaluation). Binary f31e7a76370c044d70bc26a0...

Whole-report logical hashes: 23/36 pairs identical to the full016 baseline.
The other 13 were audited: 12 are event-stream identical apart from
assessment.work_used/work_by_stage (event-compare.log), and IRS1099 has the
intended +623/side resolution gain. native_retention_audit passes for all 13
(retention-*.json.gz); no prior resolved token is lost and review claims are
covered by the new equal suffix plus retained whole anchors.

Formatting equivalence: rustfmt output applied to the archived full017 source
equals the final tree (crates inventory/content, manifests, Cargo.lock,
rustfmt.toml); see formatting-equivalence.txt. 48-fixture pdfbench verify rc=0.
