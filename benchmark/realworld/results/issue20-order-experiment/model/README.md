# CPU model captures

These are order hypotheses, not established source correspondence. The model
does not recognize text; the downstream probe retains extracted source glyphs.

The captures use `PaddlePaddle/PP-DocLayoutV3_safetensors` at revision
`97d101e6db2642e162a1d05392d1b0231c91033e`, PyTorch `2.10.0+cpu`,
torchvision `0.25.0+cpu`, Transformers `5.4.0`, pypdfium2 `5.13.0`,
and opencv-python-headless `5.0.0.93`. Hardware: AMD Ryzen 5 5600X,
12 logical CPUs, approximately 16 GiB RAM. Each inference process uses two
CPU threads and no GPU. Page rasters have an 800-pixel long edge.

Install the pinned CPU packages in an isolated environment, then download the
model repository at the pinned revision with `huggingface_hub.snapshot_download`
(only `*.json` and `*.safetensors` are needed). Create local metadata with the
keys `repo`, `revision`, and `path`, where `path` points to that snapshot.
For example, on the recorded Linux CPU environment:

```sh
UV_CACHE_DIR=/tmp/pdfdelta-order-uv-cache uv venv --python /usr/bin/python3 /tmp/pdfdelta-order-env
UV_CACHE_DIR=/tmp/pdfdelta-order-uv-cache uv pip install --python /tmp/pdfdelta-order-env/bin/python --index-url https://download.pytorch.org/whl/cpu torch==2.10.0+cpu torchvision==0.25.0+cpu
UV_CACHE_DIR=/tmp/pdfdelta-order-uv-cache uv pip install --python /tmp/pdfdelta-order-env/bin/python transformers==5.4.0 pypdfium2==5.13.0 opencv-python-headless==5.0.0.93 numpy==2.5.2
HF_HOME=/tmp/pdfdelta-order-hf /tmp/pdfdelta-order-env/bin/python - <<'PY'
import json
from pathlib import Path
from huggingface_hub import snapshot_download
repo = "PaddlePaddle/PP-DocLayoutV3_safetensors"
revision = "97d101e6db2642e162a1d05392d1b0231c91033e"
path = snapshot_download(repo, revision=revision, allow_patterns=["*.json", "*.safetensors"])
Path("/tmp/pdfdelta-order-model.json").write_text(json.dumps({
    "repo": repo, "revision": revision, "path": path,
}))
PY
```

Run from the repository root, using a fresh output path:

```sh
/tmp/pdfdelta-order-env/bin/python benchmark/realworld/issue20_model_order.py \
  benchmark/realworld/cache/irs-form-1040-2024-to-2025-old.pdf \
  /tmp/irs1040-old-order.json --model /tmp/pdfdelta-order-model.json
```

To capture every annotated pair with two concurrent processes:

```sh
/tmp/pdfdelta-order-env/bin/python benchmark/realworld/issue20_run_model.py /tmp/pdfdelta-order-captures --model /tmp/pdfdelta-order-model.json
```

Each JSON records the input hash, exact versions, page geometry, predicted
regions in reading order, inference time, and process peak RSS. Regions use
bottom-left coordinates in the rotated CropBox frame. A detector's confidence
does not establish that either its segmentation or order is correct.

The initial smoke attempt failed because `PdfPage` does not implement a Python
context manager. Explicit page closure fixed that harness error; no partial
capture from the failed attempt is used.
