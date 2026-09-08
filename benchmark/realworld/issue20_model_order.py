"""Record CPU layout/order hypotheses without recognizing or rewriting PDF text.

Coordinates are in the rotated CropBox's bottom-left coordinate system, matching
the native glyph extractor. Scores describe detector predictions, not proof of
reading order. The output must not be used as established source evidence.
"""

import argparse
import hashlib
import importlib.metadata
import json
import math
from pathlib import Path
import resource
import time

import pypdfium2 as pdfium
import torch
from transformers import AutoImageProcessor, AutoModelForObjectDetection


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pdf", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--model", type=Path, required=True,
                        help="Local snapshot metadata JSON with repo/revision/path")
    parser.add_argument("--threads", type=int, default=2)
    parser.add_argument("--max-pages", type=int, default=2000)
    args = parser.parse_args()
    if not 1 <= args.threads <= 12 or not 1 <= args.max_pages <= 2000:
        parser.error("threads must be 1..12 and max-pages 1..2000")
    if args.pdf.stat().st_size > 256 * 1024 * 1024:
        parser.error("PDF exceeds 256 MiB input limit")
    if args.output.exists():
        parser.error("output exists; keep prior experimental captures immutable")
    metadata = json.loads(args.model.read_text())
    torch.set_num_threads(args.threads)
    torch.set_num_interop_threads(1)
    start = time.monotonic()
    processor = AutoImageProcessor.from_pretrained(metadata["path"], local_files_only=True)
    model = AutoModelForObjectDetection.from_pretrained(
        metadata["path"], local_files_only=True, use_safetensors=True,
    ).to("cpu").eval()
    loading_seconds = time.monotonic() - start
    pages = []
    source_hash = hashlib.sha256(args.pdf.read_bytes()).hexdigest()
    with pdfium.PdfDocument(args.pdf) as document:
        if len(document) > args.max_pages:
            parser.error("PDF exceeds page limit; no pages silently omitted")
        for index in range(len(document)):
            page = document[index]
            try:
                width, height = page.get_size()
                if not all(math.isfinite(x) and x > 0 for x in (width, height)):
                    raise ValueError(f"Invalid dimensions on page {index}")
                # Match the model's 800-pixel long edge while bounding raster memory.
                scale = 800 / max(width, height)
                page_start = time.monotonic()
                bitmap = page.render(scale=scale)
                try:
                    image = bitmap.to_pil().convert("RGB")
                    inputs = processor(images=image, return_tensors="pt")
                    with torch.inference_mode():
                        output = model(**inputs)
                    results = processor.post_process_object_detection(
                        output, target_sizes=torch.tensor([[height, width]]),
                    )[0]
                    regions = []
                    for rank, (box, score, label) in enumerate(zip(
                        results["boxes"], results["scores"], results["labels"], strict=True,
                    )):
                        left, top, right, bottom = box.tolist()
                        regions.append({
                            "rank": rank,
                            "bbox": [left, height - bottom, right, height - top],
                            "label": model.config.id2label[int(label)],
                            "score": float(score),
                        })
                    pages.append({
                        "page": index, "width": width, "height": height,
                        "rotation": page.get_rotation(),
                        "crop_box": list(page.get_cropbox()),
                        "seconds": time.monotonic() - page_start,
                        "regions": regions,
                    })
                    image.close()
                finally:
                    bitmap.close()
                print(f"page={index} regions={len(regions)} seconds={pages[-1]['seconds']:.3f}",
                      flush=True)
            finally:
                page.close()
    result = {
        "schema_version": 1, "hypothesis_only": True,
        "source_sha256": source_hash,
        "model": {key: metadata[key] for key in ("repo", "revision")},
        "device": "cpu", "threads": args.threads,
        "versions": {name: importlib.metadata.version(name) for name in (
            "torch", "torchvision", "transformers", "pypdfium2",
        )},
        "loading_seconds": loading_seconds,
        "total_seconds": time.monotonic() - start,
        "peak_rss_bytes": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * 1024,
        "pages": pages,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("x") as destination:
        json.dump(result, destination, indent=2, allow_nan=False)
        destination.write("\n")


if __name__ == "__main__":
    main()
