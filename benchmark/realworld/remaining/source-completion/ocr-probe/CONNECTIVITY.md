# Detector connectivity prerequisite

The controlled Rust OCR prototype now groups diagonal foreground neighbours in addition to horizontal and vertical neighbours. This prevents one connected foreground region from being split solely by diagonal adjacency. Bitmap size, dimensions and the 1,024-region limit remain checked; no recognition or completeness threshold is relaxed. The output records `eight-connected-axis-box-v2` and still declares incomplete inventory.

This remains an approximate axis-aligned component cropper. It is not the full [PaddleOCR DB postprocessor](https://github.com/PaddlePaddle/PaddleOCR/blob/main/ppocr/postprocess/db_postprocess.py), which also uses contours, region scoring, polygon expansion and oriented boxes. That upstream inspection identifies remaining implementation work, not a claim of compatibility or exhaustive detection.

## Same-input observations

Six previous inputs were run once each with four CPU threads and identical model/dictionary/input hashes. English and Japanese generated readings are byte-for-byte unchanged: 30/30 and 0/30 exact full-line matches respectively. Schedule SE regions change from 249 to 242 at 72 dpi and 275 to 268 at 150 dpi; nonempty readings change from 176 to 175 and 187 to 185. Mask R-CNN at 72 dpi changes from 108 to 107 regions with 97 nonempty readings; its 150 dpi output is unchanged. These counts do not establish better recall or correct recognition of the merged regions. Natural transcriptions remain unadjudicated.

The six runs take 1.16–3.49 seconds including model load and inference but excluding PDF rendering and comparison. One run per input on a shared host is not a new throughput benchmark or evidence of speedup over earlier medians. The original repeated CPU measurements remain historical and unchanged.

Two unit tests verify diagonal connectivity, separated regions, region-budget rejection, invalid dimensions and empty foreground. The isolated OCR crate passes Clippy; the unchanged workspace also passes 2,515 tests with two ignored, formatting and Clippy. `connectivity-checks.json` binds frozen source, executable, raw outputs and logs. The disposable isolated debug build is removed after verification; frozen evidence and the release build remain.

No production OCR provider, image-text coverage or new complete pair is claimed. Full detector postprocessing, recognition alternatives, visibility/source binding and an independently justified acquisition contract remain required before production integration. The existing recognition route correctly retains inferred source obligations even at confidence 100; replacing connectivity cannot discharge them.
