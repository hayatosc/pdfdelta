from __future__ import annotations

import gzip
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).with_name("compact_native.py")
SPEC = importlib.util.spec_from_file_location("compact_native", MODULE_PATH)
assert SPEC and SPEC.loader
compact_native = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(compact_native)


def _glyph(glyph_id: int, page: int) -> dict[str, object]:
    return {
        "kind": "glyph",
        "glyph_id": glyph_id,
        "page": page,
        "bbox": {
            "min": {"x": 10.0 + glyph_id, "y": 20.0},
            "max": {"x": 15.0 + glyph_id, "y": 25.0},
        },
        "content_stream": {"object_number": 42, "generation": 0},
        "operator_index": glyph_id,
    }


def _synthetic() -> dict[str, object]:
    return {
        "kind": "synthetic_space",
        "preceding_glyph_id": 7,
        "following_glyph_id": 8,
    }


def _report() -> tuple[dict[str, object], list[list[dict[str, object]]]]:
    first = _glyph(7, 2)
    second = _synthetic()
    third = _glyph(8, 4)
    arrays = [
        [first, second, third],
        [],
        [third],
    ]
    report: dict[str, object] = {
        "schema_version": 11,
        "difference_status": "indeterminate",
        "comparison_scope": {"supported_text": True, "images_compared": False},
        "assessment": {
            "work_limit": 32,
            "work_used": 17,
            "old_resolution": [
                {
                    "block": 0,
                    "comparable_range": {"start": 0, "end": 3},
                    "canonical_range": {"start": 0, "end": 3},
                    "state": "changed",
                    "sources": arrays[0],
                },
                {
                    "block": 1,
                    "comparable_range": {"start": 0, "end": 0},
                    "canonical_range": {"start": 0, "end": 0},
                    "state": "equal",
                    "sources": arrays[1],
                },
            ],
            "relations": [
                {
                    "old_span": {"text": "old", "sources": arrays[2]},
                    "new_span": None,
                    "outcome": "deleted",
                }
            ],
        },
        "unresolved_regions": [],
        "unmapped_tokens": [{"scalar_offset": 1, "font_hash": "deadbeef", "glyph_id": 9}],
        "text": "retain every non-source field",
    }
    return report, arrays


def _write_json(directory: Path, report: dict[str, object], name: str = "report.json") -> Path:
    path = directory / name
    path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return path


def _read_artifact(path: Path) -> dict[str, object]:
    with gzip.open(path, "rb") as stream:
        return json.load(stream)


def _raw_source_arrays(path: Path) -> list[bytes]:
    data = path.read_bytes()
    arrays: list[bytes] = []
    for match in re.finditer(rb'(?m)^( +)"sources": \[', data):
        if data[match.end() - 1 : match.end() + 1] == b"[]":
            arrays.append(b"[]")
            continue
        close = b"\n" + match.group(1) + b"]"
        end = data.find(close, match.end() - 1)
        if end < 0:
            raise AssertionError("fixture source array has no closing delimiter")
        arrays.append(data[match.end() - 1 : end + len(close)])
    return arrays


def _without_sources(value: object) -> object:
    if isinstance(value, dict):
        return {key: _without_sources(child) for key, child in value.items() if key != "sources"}
    if isinstance(value, list):
        return [_without_sources(child) for child in value]
    return value


def _without_summaries(value: object) -> object:
    if isinstance(value, dict):
        return {key: _without_summaries(child) for key, child in value.items() if key != "source_summary"}
    if isinstance(value, list):
        return [_without_summaries(child) for child in value]
    return value


def _summaries(value: object) -> list[dict[str, object]]:
    found: list[dict[str, object]] = []
    if isinstance(value, dict):
        summary = value.get("source_summary")
        if isinstance(summary, dict):
            found.append(summary)
        for child in value.values():
            found.extend(_summaries(child))
    elif isinstance(value, list):
        for child in value:
            found.extend(_summaries(child))
    return found


class CompactNativeTests(unittest.TestCase):
    def test_retains_all_non_source_fields_and_summarizes_each_array(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            report, arrays = _report()
            source = _write_json(directory, report)
            output = directory / "compact.json.gz"
            metadata = compact_native.compact_report(source, output)
            artifact = _read_artifact(output)

            self.assertEqual(artifact["artifact_format"], "pdfdelta-native-compact")
            self.assertEqual(artifact["artifact_version"], 1)
            self.assertEqual(artifact["source_schema_version"], 11)
            self.assertEqual(artifact["omitted_fields"]["sources"].split(";", 1)[0], "source_summary")
            compact_report = artifact["report"]
            self.assertEqual(_without_summaries(compact_report), _without_sources(report))

            summaries = _summaries(compact_report)
            self.assertEqual(len(summaries), len(arrays))
            raw_arrays = _raw_source_arrays(source)
            for summary, values, raw in zip(summaries, arrays, raw_arrays):
                self.assertEqual(summary["raw_array_sha256"], hashlib.sha256(raw).hexdigest())
                self.assertEqual(summary["raw_array_bytes"], len(raw))
                self.assertEqual(summary["item_count"], len(values))
                self.assertEqual(summary["kind_counts"], {"glyph": 2, "synthetic_space": 1} if len(values) == 3 else {"glyph": 1} if values else {})
                self.assertEqual(summary["page_ids"], [2, 4] if len(values) == 3 else [4] if values else [])
                if values:
                    self.assertEqual(summary["samples"]["first"], values[0])
                    self.assertEqual(summary["samples"]["last"], values[-1])
                else:
                    self.assertNotIn("samples", summary)

            logical = source.read_bytes()
            self.assertEqual(metadata["source_file_sha256"], hashlib.sha256(logical).hexdigest())
            self.assertEqual(metadata["source_file_bytes"], len(logical))
            self.assertEqual(metadata["source_logical_sha256"], hashlib.sha256(logical).hexdigest())
            self.assertEqual(metadata["source_logical_bytes"], len(logical))
            self.assertEqual(metadata["source_array_count"], 3)
            self.assertEqual(metadata["omitted_sources_bytes"], sum(len(raw) for raw in raw_arrays))
            self.assertEqual(metadata["output_sha256"], hashlib.sha256(output.read_bytes()).hexdigest())

    def test_output_is_deterministic_and_gzip_input_keeps_logical_hash(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            report, _ = _report()
            plain = _write_json(directory, report)
            compressed = directory / "report.json.gz"
            with compressed.open("wb") as raw:
                with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as stream:
                    stream.write(plain.read_bytes())
            first = directory / "first.json.gz"
            second = directory / "second.json.gz"
            third = directory / "third.json.gz"
            plain_metadata = compact_native.compact_report(plain, first)
            gzip_metadata = compact_native.compact_report(compressed, second)
            compact_native.compact_report(plain, third)
            self.assertEqual(first.read_bytes(), third.read_bytes())
            self.assertEqual(gzip_metadata["source_logical_sha256"], plain_metadata["source_logical_sha256"])
            self.assertEqual(gzip_metadata["source_logical_bytes"], plain_metadata["source_logical_bytes"])
            self.assertNotEqual(gzip_metadata["source_file_sha256"], plain_metadata["source_file_sha256"])

    def test_tiny_chunks_cover_marker_and_source_boundaries(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            report, arrays = _report()
            source = _write_json(directory, report)
            output = directory / "tiny.json.gz"
            with mock.patch.object(compact_native, "CHUNK_SIZE", 7):
                compact_native.compact_report(source, output)
            summaries = _summaries(_read_artifact(output)["report"])
            self.assertEqual([summary["item_count"] for summary in summaries], [len(values) for values in arrays])

    def test_truncated_gzip_does_not_publish_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            report, _ = _report()
            source = _write_json(directory, report, "source.json.gz")
            with gzip.open(source, "wb") as stream:
                stream.write(json.dumps(report, ensure_ascii=False, indent=2).encode())
            source.write_bytes(source.read_bytes()[:-8])
            output = directory / "should-not-exist.json.gz"
            with self.assertRaises((OSError, EOFError, compact_native.CompactError)):
                compact_native.compact_report(source, output)
            self.assertFalse(output.exists())

    def test_invalid_schema_and_layout_do_not_publish_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            bad_schema = _write_json(directory, {"schema_version": 10}, "bad-schema.json")
            bad_output = directory / "bad-schema.gz"
            with self.assertRaises(compact_native.CompactError):
                compact_native.compact_report(bad_schema, bad_output)
            self.assertFalse(bad_output.exists())

            report, _ = _report()
            malformed = json.dumps(report, ensure_ascii=False, separators=(",", ":")).encode()
            malformed_source = directory / "compact-layout.json"
            malformed_source.write_bytes(malformed)
            malformed_output = directory / "compact-layout.gz"
            with self.assertRaises(compact_native.CompactError):
                compact_native.compact_report(malformed_source, malformed_output)
            self.assertFalse(malformed_output.exists())

            pretty = _write_json(directory, report, "odd-indent.json")
            pretty.write_bytes(pretty.read_bytes().replace(b'      "sources": [', b'       "sources": [', 1))
            odd_output = directory / "odd-indent.gz"
            with self.assertRaises(compact_native.CompactError):
                compact_native.compact_report(pretty, odd_output)
            self.assertFalse(odd_output.exists())

    def test_refuses_overwrite_and_preserves_existing_file(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            report, _ = _report()
            source = _write_json(directory, report)
            output = directory / "existing.gz"
            output.write_bytes(b"keep this")
            with self.assertRaises(compact_native.CompactError):
                compact_native.compact_report(source, output)
            self.assertEqual(output.read_bytes(), b"keep this")


if __name__ == "__main__":
    unittest.main()
