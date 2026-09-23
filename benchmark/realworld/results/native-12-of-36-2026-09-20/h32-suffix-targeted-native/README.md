# H32 conservative suffix translation: targeted audit

Producer binary: f31e7a76370c044d70bc26a0... (full017), final executable
80b86bebfeb4d9d5906bd94e... (formatting-only rebuild; final 3-pair replay
logical reports identical to full017).

Bindings: the full017 production archive (production.tar.gz) is the complete
source binding because the untracked suffix.rs is absent from production.patch.
The archive sha256 and the formatting-equivalence proof are recorded in the
full-iteration results directory.

Results: IRS1099 native capture resolves 623 additional tokens per side
(9974/9970) with 117 unresolved unchanged, changes 12, candidates 2, all
retained exactly. The retained review claims drop from 9 to 2: the seven
removed claims are covered by the new 623-token equal suffix plus the retained
whole anchors (47/160 tokens), proven by 1099-review-projection-certificate.log.
ScheduleSE and W2 are content-identical to the full016 baseline; SE differs only
in work accounting. native_retention_audit reports pass for all three pairs.
