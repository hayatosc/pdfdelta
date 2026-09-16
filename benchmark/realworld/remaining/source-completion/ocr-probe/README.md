# Rust multilingual OCR compatibility probe

This isolated benchmark tests whether local PaddleOCR ONNX models can execute
through RTen 0.26.0 without a Python or C++ inference runtime. It does not join the
production workspace or establish acquisition completeness. Its inputs are
controlled model files and small generated crops; it is not an untrusted-input
provider. Runtime integration still requires a bounded worker, model provenance,
region detection, retention of alternatives, and evidence-store validation.

## Observed results

On x86_64 Linux with one inference thread, the Japanese and English models read
`日本語の比較 10` and `Document comparison 10` respectively. Both matched their
known generated input. Each took approximately 0.05 seconds including model load;
maximum resident memory was 28,860 KiB and 22,280 KiB respectively in this single
measurement. These are crop measurements, not natural-panel speed estimates.
The detector emitted a finite probability map on the Japanese crop; its region
localization and recall have not been adjudicated.

The decoded sequence selects one CTC path and retains its minimum selected
character score for observation. Neither this score nor an exact crop result
proves recognition correctness for other inputs. Detection omissions, competing
characters, languages, rotation, occlusion and renderer warnings remain separate
obligations. This probe adds zero strict completed natural pairs.

## Reproduction

Model files and measurement logs are in the ignored directory
`benchmark/realworld/cache/source-completion/ocr-probe/`. Their SHA-256 values
are retained in `results.json`. The repository contains the generated PNG crops
and Rust source/lockfile, not the model binaries. The original crops used
NotoSansCJK-Regular at 32 pixels on a white RGB background. Python/Pillow was used
only to generate fixtures; the OCR runs entirely in Rust.

From the repository root:

```sh
cargo build --release --locked --manifest-path benchmark/realworld/remaining/source-completion/ocr-probe/Cargo.toml
RTEN_NUM_THREADS=1 benchmark/realworld/remaining/source-completion/ocr-probe/target/release/pdfdelta-ocr-probe benchmark/realworld/cache/source-completion/ocr-probe/ja.onnx benchmark/realworld/cache/source-completion/ocr-probe/ja.dict benchmark/realworld/remaining/source-completion/ocr-probe/ja.png
```

Use `en` in place of `ja` for the English crop. To smoke-test the detector, pass
`det.onnx --detect ja.png` using the corresponding paths. A full-page provider
must not use these isolated crop successes as inventory evidence.

## Candidate and source record

- [RTen](https://github.com/robertknight/rten) provides Rust ONNX inference.
- [ocrs](https://github.com/robertknight/ocrs#language-support) documents Latin-only
  standard recognition; it was not selected for Japanese support.
- [PaddleOCR model distribution](https://huggingface.co/deepghs/paddleocr) supplies
  the converted models and matching dictionaries used here. Downloaded paths:
  `rec/japan_PP-OCRv3_rec/{model.onnx,dict.txt}`,
  `rec/en_PP-OCRv4_rec/{model.onnx,dict.txt}`, and
  `det/ch_PP-OCRv4_det/model.onnx`, each under `resolve/main/`.
  The downloaded bytes are pinned by their recorded hashes. This third-party
  conversion repository is not an official PaddlePaddle release. Production
  distribution and model licensing still require a selected, documented source.
- [PaddleOCR language documentation](https://www.paddleocr.ai/main/en/version3.x/pipeline_usage/OCR.html)
  documents the upstream language families.

Cross-compilation is checked separately from executing the model on another OS.
A successful target check is not a Windows or macOS runtime test.

## CPU page evaluation

See [CPU.md](CPU.md) for the repeated full-page timing prototype, resource use, known-text errors and implementation consequences. The crop observations above are an earlier isolated experiment; their preserved executable is `crop-probe-v1` in the model cache.
