# H22 full-panel acceptance evidence (accepted intermediate)

Capture `h22-full-iteration-009-native`, binary `3e24da968876782640438e5d119f18b98f8711bc318e1ebe0244be9f5d69173a`: 36
captured / 0 failed / 3 complete. Whole reports logically identical to H17
full008: 19; differing: 17,
every one retention-`pass` with prior-resolved loss 0 and no review/problems.

Three changes are bundled: (1) batched exact-anchor occurrence search grouped
by prefix fingerprint with exact verification and bounded KMP fallback;
(2) bounded compact TokenPostings fallback with checked retained-byte
admission (token capacity + Unmapped font-hash payloads + metadata) and
two-pass counting; (3) actual per-token comparison charging that stops at the
first mismatch while fully equal sequences still cost at least the retired
full length.

Coverage gains (old/new resolved): W9 +1263/+1212, W4 +2870/side,
EDPB restrictions +24745/+24878, EDPB design-default +16159/+16084,
NIST-SHA +11490/+11513, arxiv-bert +2435/+2451,
EDPB controller-processor +68765/+68925, NIST ai-rmf +11724/+11644.

Seven required gates exit 0 on this source (`../h20-batched-anchor-search/h22n-gates-meta.json`).
Score remains 3/36 against the 12/36 goal. No commit yet at capture time.
