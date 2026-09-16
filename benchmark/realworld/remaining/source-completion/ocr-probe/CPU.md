# CPU practicality assessment

## Finding

CPU execution is a viable candidate for bounded local acquisition and small documents. This prototype does not justify running OCR over every page of every document, or using its output to certify strict text completion. Recognition correctness and exhaustive acquisition remain unresolved independently of inference speed.

## Method

RTen 0.26.0 executed local PaddleOCR detection and recognition models on an AMD Ryzen 5 5600X (6 physical cores, 12 logical CPUs), in a shared virtualized Linux desktop. No GPU was used. Each case ran in a fresh process three times with one thread and three times with four threads; runs were sequential. Timing includes model loading, PNG decoding, detection, region construction and recognition, but excludes PDF rendering, native extraction and matching. The six cases are two generated 30-line pages and the first pages of IRS Schedule SE and Mask R-CNN, each at 72 and 150 dpi.

The page decoder is an intentionally limited timing prototype: thresholded connected components and axis-aligned padding, not a production DB polygon postprocessor. It retains every detected component up to an explicit limit, including empty recognitions. Rotated text, layout relations, missed components and exhaustive visibility were not established. These timings are not production performance results.

## Measurements

Wall times are medians of three runs. RSS is the largest process peak in the three runs, including retained models and page data.

| Case | Threads | Median seconds | Observed range | Peak MiB | Regions |
| --- | ---: | ---: | ---: | ---: | ---: |
| ja-150 | 1 | 5.87 | 4.64–6.60 | 126.6 | 30 |
| ja-150 | 4 | 3.53 | 2.73–4.77 | 129.7 | 30 |
| en-150 | 1 | 6.71 | 4.30–7.26 | 118.8 | 30 |
| en-150 | 4 | 2.87 | 2.55–4.67 | 122.1 | 30 |
| se-72 | 1 | 16.01 | 10.36–16.65 | 91.6 | 249 |
| se-72 | 4 | 8.33 | 4.31–13.08 | 95.3 | 249 |
| se-150 | 1 | 16.33 | 13.43–17.72 | 127.1 | 275 |
| se-150 | 4 | 5.46 | 3.88–13.54 | 130.0 | 275 |
| mask-72 | 1 | 10.59 | 8.68–10.64 | 91.4 | 108 |
| mask-72 | 4 | 3.13 | 2.71–9.36 | 95.3 | 108 |
| mask-150 | 1 | 12.51 | 10.83–12.81 | 127.0 | 110 |
| mask-150 | 4 | 5.38 | 4.57–10.93 | 130.2 | 110 |

All 36 runs exited successfully; repeated transcriptions were identical within each thread configuration. Timing varied substantially. The host was not reserved, so medians and ranges are reported instead of a guaranteed speedup. Target-platform compilation is distinct from measuring runtime on that platform.

## Accuracy limits

- English generated page: all 30 lines matched exactly; raw character edit distance was zero.
- Japanese generated page: all 30 rows produced regions, but none of the full line strings matched exactly. Raw edit distance was 114 over 869 reference characters, including omitted spaces. Ignoring whitespace only for diagnosis leaves 24 edits; eight lines then match. Neither score is a coverage certificate.
- Natural pages have no independently adjudicated complete visible-text transcript here. Region counts, native extraction and recognizable snippets cannot establish recall or correctness. Schedule SE at 72 dpi produced 249 regions, including 73 empty recognitions; count alone is not usable-text yield.
- Detection and decoding successes leave `inventory_complete` false. No natural comparison became complete through this probe.

## Implementation consequences

1. Prefer a bounded regional provider that requests OCR where native acquisition lacks interpretation, while retaining conservative overlap and missed-region obligations. Do not silently eliminate native/OCR conflicts.
2. A four-thread CPU configuration is worth testing in the production worker; do not assume linear scaling or multiply page-level parallelism without a document-wide CPU and memory budget.
3. The fixed panel contains 5,684 old/new pages. Full-page OCR everywhere could dominate execution. No whole-panel OCR capture was performed, and the two sampled natural page types do not define its throughput distribution.
4. Recognition accuracy, model distribution, cross-platform process limits and worker integration remain required work. Japanese results demonstrate that successful inference cannot be promoted to exact content proof.
5. Preserve the existing comparison completion meaning. This is an acquisition feasibility measurement, with zero newly completed strict pairs.

## Strict completion integration check

The production evidence and comparison APIs were exercised independently of the
timing prototype. The regression
`recognition_inventory_and_confidence_cannot_discharge_strict_sources` runs eight
text-only cases: equal or changed recognized text, complete or incomplete declared
inventory, and confidence zero or 100. Every case produces an actual inferred
comparison. Even with complete inventory and confidence 100, each side retains
its one uncompared recognized source and text coverage remains incomplete.

This isolates an integration prerequisite, not an OCR accuracy measurement. A
production worker using the existing recognition route could supply additional
inferred findings, but adding that worker alone cannot satisfy strict completion.
Neither model agreement nor a confidence threshold supplies the missing source
proof. Preserve this distinction when prioritizing worker integration against
native source-domain and acquisition work; the CPU measurements establish only
the cost of a possible acquisition component.

## Evidence and reproduction

`cpu-results.json` contains run records and the binary/model hashes. `cpu-analysis.json` adds ranges and known-text comparisons; `cpu-environment.json` records the host and scope. Raw outputs, timing logs, input images, and the preserved CPU probe binary are under the ignored `benchmark/realworld/cache/source-completion/ocr-probe/cpu-v1/` and its parent.

After building the isolated crate, run `cpu_benchmark.py` and then `analyze_cpu.py` using Python without extra packages. Python schedules processes and calculates metrics; all detection and recognition inference runs in the Rust executable. Fixture generation used Pillow and MuPDF outside the timed run.
