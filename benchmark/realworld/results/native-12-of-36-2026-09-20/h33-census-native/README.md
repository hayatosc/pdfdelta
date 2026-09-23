# H33 post-H32 residual census (diagnostic only)

Temporary env-gated hook (H33_CENSUS/H33_DUMP_PATH) in the assessment tail,
restored byte-exact after the run; patch and hash archived here. One bounded
IRS1099 native capture with the hooked release binary; production unchanged.

Totals match the report exactly: unowned 1044 (old) / 797 (new) comparable
tokens in 117 segments, 0 diagnostic errors, 0 aborted. remaining_work 906,341.

Classification: 56/55 segments (1026/779 tokens) are domain-unresolved (covered
only by relations that are not Established+Complete+reason-free with a Complete
domain), and 3/3 segments (18/18 tokens) are established-equal (fully covered by
strict-equal established domains yet unowned). There are five new-side
candidate-overlapping segments: blocks 62 0..19, 63 0..25, 64 0..12 (candidate 0)
and 201 0..19, 202 0..24 (candidate 1), 99 tokens total. No proven overlap. The
domain-unresolved label only names the other covering records under this
classifier; it is not a first-veto or closure diagnosis, and no cause conclusion
is claimed yet.
