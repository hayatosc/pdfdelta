# Rejected interleaved-render inference experiment

This experiment is not adopted. Allowing geometrically disjoint render chunks to have overlapping render intervals is geometrically plausible, but the downstream pipeline did not preserve the existing recovery behavior.

The initial two-pair preview kept CSF coverage and its two failures unchanged. EDPB old coverage rose from 0.844522 to 0.849853 while new coverage fell from 0.855425 to 0.855416. It lost the reviewed consultation-watermark count (51 expected, 5 found) and the exception-effect relation. These regressions prevent adoption.

A subsequent variant preserved the former block-joining barriers separately from reading-order uncertainty and kept recovery enabled for more inferred-order alignment rejections. It still failed a fixture in which interleaved content-stream order and a numeric replacement should produce one change: it emitted six one-sided changes instead. `prototype.patch` and `prototype-source.json` describe this later variant; the two preview JSON files precede it and are not results from that source snapshot. `barrier-variant-tests.log` retains the failing test evidence.

The adopted layout and recovery code was restored. The remaining design problem is to preserve usable source-range and contiguity evidence when inferred order changes block boundaries. Relaxing the render-order gate or broadly enabling heuristic recovery alone is insufficient.
