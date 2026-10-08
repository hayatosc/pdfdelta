"""Audit scoped source quotations without consulting comparison output."""
import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import xml.etree.ElementTree as ET


def normalized(text):
    """Collapse extraction whitespace while preserving punctuation."""
    return " ".join(text.split())


def box_words(words, bounds, join):
    """Read independently extracted words fully inside a frozen source box."""
    selected = [word for word in words if word.text != "."
                and float(word.attrib["xMin"]) >= bounds[0]
                and float(word.attrib["yMin"]) >= bounds[1]
                and float(word.attrib["xMax"]) <= bounds[2]
                and float(word.attrib["yMax"]) <= bounds[3]]
    selected.sort(key=lambda word: (float(word.attrib["yMin"]), float(word.attrib["xMin"])))
    return join.join(word.text or "" for word in selected)


def verify(root, gold):
    """Check source identity, declared page, and the registered quotation scope."""
    texts = {}
    geometric = {}
    for pair, sides in gold["sources"].items():
        for side, source in sides.items():
            path = root / source["path"]
            with path.open("rb") as stream:
                digest = hashlib.file_digest(stream, "sha256").hexdigest()
            if digest != source["sha256"] or path.stat().st_size != source["bytes"]:
                raise ValueError(f"source identity mismatch: {pair} {side}")
            result = subprocess.run(
                ["pdftotext", "-layout", str(path), "-"],
                check=True, capture_output=True, text=True,
            )
            texts[pair, side] = result.stdout.split("\f")
            if any(target["pair"] == pair and "field_label_bbox_pdf_points" in target[side]
                   for target in gold["targets"]):
                bbox = subprocess.run(["pdftotext", "-bbox-layout", str(path), "-"],
                                      check=True, capture_output=True, text=True)
                geometric[pair, side] = ET.fromstring(bbox.stdout).findall(
                    ".//{http://www.w3.org/1999/xhtml}page")
    checked = 0
    visual = 0
    for target in gold["targets"]:
        for side in ("old", "new"):
            reference = target[side]
            page = normalized(texts[target["pair"], side][reference["page"] - 1])
            if target["pair"].startswith("irs-schedule-c"):
                page = normalized(re.sub(r"(?:\s*\.\s*){2,}", " ", page))
            quote = normalized(reference["quote"])
            if "field_label_bbox_pdf_points" in reference:
                words = geometric[target["pair"], side][reference["page"] - 1].findall(
                    ".//{http://www.w3.org/1999/xhtml}word")
                label = box_words(words, reference["field_label_bbox_pdf_points"], join="")
                body = box_words(words, reference["field_body_bbox_pdf_points"], join=" ")
                if normalized(label + " " + body) != quote:
                    raise ValueError(f"geometric field quote mismatch: {target['id']} {side}")
                visual += 1
            elif page.count(quote) != 1:
                raise ValueError(f"quotation not unique: {target['id']} {side}")
            checked += 1
    return {"source_references_checked": checked, "geometric_field_quotes_checked": visual,
            "scope": "source identity and quotation audit; no engine accuracy score"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gold", type=pathlib.Path)
    parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path(__file__).resolve().parents[3])
    args = parser.parse_args()
    print(json.dumps(verify(args.root, json.loads(args.gold.read_text())), indent=2))
