#!/usr/bin/env python3
"""Evaluate the flow diff against the reviewed real-world corpus.

The script reruns ``pdfdelta flow`` for every cached pair, compares its token
counts and change volume with the PyMuPDF oracle bag (``bag.tsv``), and writes
one TSV row per pair plus corpus totals. It also measures

* ``cancellable``: the size of the multiset intersection between the token
  texts reported as deleted/replaced on the old side and inserted/replaced on
  the new side. Tokens that are merely relocated (``Move``) are not included.
* ``in_place_moves``: moves whose old and new ranges fall in the same gap
  between spine anchors. Those should have been promoted into the spine and
  the target count is zero; ``neighbour_equal_moves`` separately counts moves
  whose nearest non-digit neighbours match, which also happens for genuine
  table-of-contents relocations.
* ``--repeat N``: rerun a pair N times and fail if the report or either token
  sidecar differs by SHA-256 between runs.
* ``min_deleted``/``min_inserted``: changed-line counts of GNU ``diff
  --minimal`` on the exact old/new token sidecars, i.e. a minimal edit script
  for the token streams flow used. ``flow_non_move_changed`` counts
  delete+insert+replace tokens (moves and layout-only excluded), and
  ``align_ratio = flow_non_move_changed / (min_deleted + min_inserted)``. A
  ratio above 1.0 means flow changed more tokens than a minimal script needs;
  extracted moves can legitimately make it lower than 1.0.
* ``oracle_order_agreement_{old,new}``: order-preserving overlap between the
  flow tokens and a PyMuPDF reading-order extraction, computed with GNU
  ``diff --minimal`` on token-per-line files (timeout 300 s). Tokens are
  normalised identically on both sides (NFKC, quote/hyphen/soft-hyphen and
  combining-mark folding, control characters dropped) and flow furniture is
  removed from the oracle by masked token sequence, so the metric measures
  reading order rather than tokenisation or furniture retention. This is an
  external evaluation only; the engine never reads it.
* ``noise``: engine changed tokens divided by the oracle bag lower bound.
* reviewed-quote coverage from ``expected/*.json``: a quote is located when
  any exact occurrence of its engine-tokenised text appears in the token
  stream, and it is a hit when any occurrence overlaps a content change or
  when the quote is reported in the furniture section. The fraction of the
  chosen occurrence's tokens that fall inside non-equal, non-layout-only
  changes is still reported as coverage.

The main report is small and is read with ``json.load`` after a compressed-size
check; token texts live in ``*.{old,new}-tokens.jsonl[.gz]`` sidecars and are
read line by line, so no report ever has to be held in memory whole.

Usage:
    uv run --no-project --with pymupdf python -I flow_eval.py \
        --cache benchmark/realworld/cache \
        --results /tmp/flow-eval \
        --bag /tmp/oracle/bag.tsv \
        --expected benchmark/realworld/expected \
        --binary target/release/pdfdelta
"""

from __future__ import annotations

import argparse
import collections
import gzip
import hashlib
import heapq
import json
import math
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unicodedata

REPORT_SIZE_LIMIT = 32 * 1024 * 1024
MINIMAL_DIFF_TIMEOUT = 300
HUNK_PATTERN = re.compile(r"^(\d+)(?:,(\d+))?([acd])(\d+)(?:,(\d+))?$")

CJK_RANGES = (
    (0x1100, 0x11FF),
    (0x2E80, 0x2EFF),
    (0x2F00, 0x2FDF),
    (0x3000, 0x303F),
    (0x3040, 0x309F),
    (0x30A0, 0x30FF),
    (0x3130, 0x318F),
    (0x31F0, 0x31FF),
    (0x3200, 0x33FF),
    (0x3400, 0x4DBF),
    (0x4E00, 0x9FFF),
    (0xA960, 0xA97F),
    (0xAC00, 0xD7AF),
    (0xD7B0, 0xD7FF),
    (0xF900, 0xFAFF),
    (0xFE10, 0xFE1F),
    (0xFE30, 0xFE4F),
    (0xFF00, 0xFFEF),
    (0x1B000, 0x1B16F),
    (0x20000, 0x2FA1F),
    (0x30000, 0x323AF),
)


def is_cjk(scalar: str) -> bool:
    code = ord(scalar)
    return any(start <= code <= end for start, end in CJK_RANGES)


def tokenize(text: str) -> list[str]:
    """Tokenise like the flow engine: word runs, single CJK scalars, punctuation.

    This is the Phase N normalisation shared by the oracle tokeniser. It must
    not split digit/letter runs: the independent PyMuPDF order oracle and the
    flow stream are compared token-for-token under ``metric_token``. Quote
    matching happens separately in ``scan_side``, which normalises both sides
    with ``split_token_edges`` so an extracted ``10th`` can match ``10``/``th``
    without changing the oracle tokens.
    """
    text = unicodedata.normalize("NFC", text)
    tokens: list[str] = []
    word: list[str] = []

    def flush() -> None:
        if word:
            tokens.append("".join(word))
            word.clear()

    for scalar in text:
        if scalar.isspace():
            flush()
        elif scalar.isalnum() and not is_cjk(scalar):
            word.append(scalar)
        else:
            flush()
            tokens.append(scalar)
    flush()
    return tokens


def split_token_edges(token: str) -> list[str]:
    """Split an alphanumeric token at digit/letter transitions.

    The engine's token boundaries follow source runs, so the same visible span
    can appear as ``10th`` or as ``10``/``th`` (and as ``w3`` or ``w``/``3``).
    Normalising both the quote and the stream makes the locator independent of
    that layout decision, in either direction.
    """
    parts: list[str] = []
    for scalar in token:
        if (
            parts
            and parts[-1][-1].isalnum()
            and scalar.isalnum()
            and (parts[-1][-1].isascii() and parts[-1][-1].isdigit())
            != (scalar.isascii() and scalar.isdigit())
        ):
            parts.append(scalar)
        elif parts:
            parts[-1] += scalar
        else:
            parts.append(scalar)
    return parts or [token]


def mask_digits(text: str) -> str:
    """Collapse every ASCII digit run to one zero, as furniture text does."""
    return re.sub(r"[0-9]+", "0", text)


def furniture_sequences(furniture: dict, side: str) -> list[list[str]]:
    """Masked furniture tokens reported for one side of the comparison.

    Only furniture *changes* count: a quote already present in the unchanged
    furniture list was not reported as a change in this comparison.
    """
    kind = "removed" if side == "old" else "added"
    return [
        [
            mask_digits(part)
            for token in tokenize(change.get("text", ""))
            for part in split_token_edges(token)
        ]
        for change in furniture.get("changes", [])
        if change.get("kind") == kind
    ]


def contains_sequence(haystack: list[str], needle: list[str]) -> bool:
    """Whether ``needle`` occurs contiguously in ``haystack``."""
    if not needle:
        return False
    return any(
        haystack[start : start + len(needle)] == needle
        for start in range(len(haystack) - len(needle) + 1)
    )


def token_sidecar(report: pathlib.Path, side: str) -> pathlib.Path:
    compressed = report.suffix.lower() == ".gz"
    base = report.with_suffix("") if compressed else report
    suffix = ".gz" if compressed else ""
    return base.with_name(f"{base.name}.{side}-tokens.jsonl{suffix}")


def read_report(path: pathlib.Path, limit: bool = True) -> dict:
    size = path.stat().st_size
    if limit and size > REPORT_SIZE_LIMIT:
        raise ValueError(f"report_too_large ({size} bytes)")
    with gzip.open(path, "rt", encoding="utf-8") as stream:
        return json.load(stream)


def read_tokens(report: pathlib.Path, side: str) -> list[str]:
    path = token_sidecar(report, side)
    opener = gzip.open if path.suffix.lower() == ".gz" else open
    with opener(path, "rt", encoding="utf-8") as stream:
        return [json.loads(line) for line in stream]


def load_geometry(report: pathlib.Path, side: str) -> list[dict] | None:
    """Read geometry only for diagnostic n-gram runs, ignoring report size."""
    data = read_report(report, limit=False)
    return data.get(f"{side}_geometry")


def load_bag(path: pathlib.Path) -> dict[str, dict]:
    bag = {}
    if not path or not path.exists():
        return bag
    with path.open(encoding="utf-8") as stream:
        header = stream.readline().rstrip("\n").split("\t")
        for line in stream:
            row = dict(zip(header, line.rstrip("\n").split("\t")))
            bag[row["pair"]] = {
                "oracle_old": int(row["oracle_old"]),
                "oracle_new": int(row["oracle_new"]),
                "bag_deleted": int(row["bag_deleted"]),
                "bag_inserted": int(row["bag_inserted"]),
            }
    return bag


def load_expected(path: pathlib.Path) -> dict[str, list[dict]]:
    expected = {}
    if not path:
        return expected
    for file in sorted(path.glob("*.json")):
        data = json.loads(file.read_text(encoding="utf-8"))
        quotes = []
        for change in data.get("changes", []):
            for side in ("old", "new"):
                quote = change.get(f"{side}_quote")
                if quote:
                    quotes.append(
                        {
                            "id": f"{change.get('id', '?')}:{side}",
                            "side": side,
                            "tokens": tokenize(quote),
                        }
                    )
        expected[data["pair"]] = quotes
    return expected


def run_pair(
    binary: pathlib.Path,
    cache: pathlib.Path,
    pair: str,
    out: pathlib.Path,
    timeout: int,
) -> dict:
    old = cache / f"{pair}-old.pdf"
    new = cache / f"{pair}-new.pdf"
    report = out / f"{pair}.json.gz"
    for stale in out.glob(f"{pair}*"):
        if stale.is_file() and (
            stale.name == report.name
            or ".old-tokens." in stale.name
            or ".new-tokens." in stale.name
            or stale.suffix == ".time"
        ):
            stale.unlink()
    time_file = out / f"{pair}.time"
    start = time.monotonic()
    status = "ok"
    try:
        completed = subprocess.run(
            [
                "/usr/bin/time",
                "-v",
                "-o",
                str(time_file),
                str(binary),
                "flow",
                str(old),
                str(new),
                "--json",
                str(report),
                "--tokens",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
            text=True,
        )
        exit_code = completed.returncode
    except subprocess.TimeoutExpired:
        exit_code = -1
        status = "timeout"
    seconds = time.monotonic() - start
    rss = 0
    if time_file.exists():
        for line in time_file.read_text(encoding="utf-8", errors="replace").splitlines():
            if "Maximum resident set size" in line:
                rss = int(line.split(":", 1)[1].strip())
    return {
        "pair": pair,
        "exit_code": exit_code,
        "status": status,
        "seconds": seconds,
        "max_rss_kb": rss,
        "report": report,
    }


def _is_numeric(token: str) -> bool:
    return bool(token) and token.isascii() and token.isdigit()


def _previous_non_numeric(tokens: list[str], index: int) -> str:
    for candidate in range(index - 1, -1, -1):
        if not _is_numeric(tokens[candidate]):
            return tokens[candidate]
    return ""


def _next_non_numeric(tokens: list[str], index: int) -> str:
    for candidate in range(index, len(tokens)):
        if not _is_numeric(tokens[candidate]):
            return tokens[candidate]
    return ""


def change_ranges(changes: list[dict], side: str) -> list[tuple[int, int, str]]:
    key = "old" if side == "old" else "new"
    return [
        (change[key][0], change[key][1], change["kind"])
        for change in changes
        if change["kind"] != "layout_only" and change[key][0] != change[key][1]
    ]


def covered_fraction(indices: list[int], changes: list[dict], side: str) -> float:
    if not indices:
        return 0.0
    ranges = change_ranges(changes, side)
    covered = sum(
        1
        for index in indices
        if any(start <= index < end for start, end, _kind in ranges)
    )
    return covered / len(indices)


def scan_side(
    report: pathlib.Path,
    side: str,
    changes: list[dict],
    quotes: list[dict],
    furniture: list[list[str]] | None = None,
    with_geometry: bool = False,
) -> dict:
    tokens = read_tokens(report, side)
    geometry = load_geometry(report, side) if with_geometry else None
    kind_for_index = [None] * len(tokens)
    for start, end, kind in change_ranges(changes, side):
        if side == "old" and kind not in {"delete", "replace"}:
            continue
        if side == "new" and kind not in {"insert", "replace"}:
            continue
        for index in range(start, min(end, len(tokens))):
            kind_for_index[index] = kind
    changed = collections.Counter(
        text for text, kind in zip(tokens, kind_for_index) if kind is not None
    )
    changed_records = []
    if with_geometry and geometry is not None:
        for index, text in enumerate(tokens):
            if kind_for_index[index] is None:
                continue
            item = geometry[index]
            bounds = item.get("bounds")
            relative_y = (bounds[1] + bounds[3]) / 2 if bounds else 0.0
            changed_records.append((text, item["page"], relative_y))
    move_key = "old" if side == "old" else "new"
    prev: dict[int, str | None] = {}
    next_: dict[int, str | None] = {}
    moves = [change for change in changes if change["kind"] == "move"]
    for index, move in enumerate(moves):
        start, end = move[move_key]
        prev[index] = _previous_non_numeric(tokens, start)
        next_[index] = _next_non_numeric(tokens, end)
    found = {}
    changed_indices = {
        index
        for start, end, _kind in change_ranges(changes, side)
        for index in range(start, end)
    }
    for quote in quotes:
        if quote["side"] != side:
            continue
        wanted = quote["tokens"]
        if not wanted:
            continue
        # The annotation quotes are exact spans, so every contiguous
        # occurrence is a candidate: repeated text must not hide a changed
        # occurrence behind an unchanged one. Both sides are normalised at
        # digit/letter transitions, so the engine's source-run split does not
        # decide whether a quote is found. The occurrence covering the most
        # changed tokens wins; the greedy locator remains the fallback for
        # spans whose source tokenisation is not contiguous (for example a
        # rotated watermark).
        wanted_flat = [part for token in wanted for part in split_token_edges(token)]
        flat_tokens: list[str] = []
        flat_index: list[int] = []
        for index, text in enumerate(tokens):
            for part in split_token_edges(text):
                flat_tokens.append(part)
                flat_index.append(index)
        occurrences = []
        for start in range(len(flat_tokens) - len(wanted_flat) + 1):
            if flat_tokens[start : start + len(wanted_flat)] != wanted_flat:
                continue
            originals: list[int] = []
            for index in flat_index[start : start + len(wanted_flat)]:
                if not originals or originals[-1] != index:
                    originals.append(index)
            occurrences.append(originals)
        if occurrences:
            positions = max(
                occurrences,
                key=lambda occurrence: sum(
                    1 for index in occurrence if index in changed_indices
                ),
            )
            located_span = True
        else:
            positions = []
            for index, text in enumerate(tokens):
                if len(positions) < len(wanted) and text == wanted[len(positions)]:
                    positions.append(index)
                    if len(positions) == len(wanted):
                        break
            located_span = len(positions) == len(wanted)
        if not located_span:
            continue
        wanted_masked = [
            part for token in wanted for part in split_token_edges(token)
        ]
        wanted_masked = [mask_digits(part) for part in wanted_masked]
        furniture_hit = any(
            contains_sequence(line, wanted_masked) for line in furniture or []
        )
        found[quote["id"]] = {
            "positions": positions,
            "hit": any(index in changed_indices for index in positions)
            or furniture_hit,
            "furniture_hit": furniture_hit,
            "occurrences": len(occurrences),
        }
    return {
        "changed": changed,
        "changed_records": changed_records,
        "prev": prev,
        "next": next_,
        "quotes": found,
    }


def _changed_flags(changes: list[dict], side: str, length: int) -> bytearray:
    """One flag per token that flow reports as changed by a non-move change."""
    flags = bytearray(length)
    allowed = ("delete", "replace") if side == "old" else ("insert", "replace")
    for start, end, kind in change_ranges(changes, side):
        if kind not in allowed:
            continue
        for index in range(max(0, start), min(end, length)):
            flags[index] = 1
    return flags


def _diff_ranges(header: str) -> tuple[int, int, int, int] | None:
    """Half-open old/new token ranges from one normal-diff hunk header."""
    match = HUNK_PATTERN.match(header.strip())
    if match is None:
        return None
    first = int(match.group(1))
    first_end = int(match.group(2)) if match.group(2) else first
    operator = match.group(3)
    second = int(match.group(4))
    second_end = int(match.group(5)) if match.group(5) else second
    if operator == "a":
        return (first, first, second - 1, second_end)
    if operator == "d":
        return (first - 1, first_end, second, second)
    return (first - 1, first_end, second - 1, second_end)


def _excess_runs(
    tokens: list[str],
    flow_flags: bytearray,
    minimal_flags: bytearray,
    side: str,
    detail: int,
) -> list[tuple[int, str, int, str]]:
    """Largest runs where flow changed a token that the minimal script keeps."""
    runs: list[tuple[int, str, int, str]] = []
    index = 0
    while index < len(tokens):
        if not (flow_flags[index] and not minimal_flags[index]):
            index += 1
            continue
        end = index
        while end < len(tokens) and flow_flags[end] and not minimal_flags[end]:
            end += 1
        context_before = " ".join(tokens[max(0, index - 5) : index])
        context_after = " ".join(tokens[end : end + 5])
        text = f"{context_before} [{' '.join(tokens[index:end])}] {context_after}"
        record = (end - index, side, index, text[:500])
        if len(runs) < detail:
            heapq.heappush(runs, record)
        elif record[0] > runs[0][0]:
            heapq.heapreplace(runs, record)
        index = end
    return runs


def minimal_diff(
    report: pathlib.Path,
    changes: list[dict],
    timeout: int = MINIMAL_DIFF_TIMEOUT,
    detail: int = 0,
) -> dict:
    """Counts and largest excess regions of GNU ``diff --minimal`` on the tokens.

    The token sidecars are written one token per line to a temporary directory,
    and diff output is streamed, so neither token texts nor diff hunks have to
    be held in memory. ``detail`` keeps that many maximal runs where flow
    changed tokens that the minimal script keeps, with surrounding text.
    """
    old_tokens = read_tokens(report, "old")
    new_tokens = read_tokens(report, "new")
    flow_old = _changed_flags(changes, "old", len(old_tokens))
    flow_new = _changed_flags(changes, "new", len(new_tokens))
    minimal_old = bytearray(len(old_tokens))
    minimal_new = bytearray(len(new_tokens))
    deleted = 0
    inserted = 0
    status = "ok"
    with tempfile.TemporaryDirectory(prefix="pdfdelta-diff-minimal-") as directory:
        old_path = pathlib.Path(directory) / "old.tokens"
        new_path = pathlib.Path(directory) / "new.tokens"
        with old_path.open("w", encoding="utf-8") as stream:
            stream.writelines(f"{token}\n" for token in old_tokens)
        with new_path.open("w", encoding="utf-8") as stream:
            stream.writelines(f"{token}\n" for token in new_tokens)
        command = ["diff", "--minimal", "--", str(old_path), str(new_path)]
        if shutil.which("timeout"):
            command = ["timeout", str(timeout), *command]
        process = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        assert process.stdout is not None
        for line in process.stdout:
            ranges = _diff_ranges(line)
            if ranges is not None:
                old_start, old_end, new_start, new_end = ranges
                minimal_old[old_start:old_end] = b"\x01" * (old_end - old_start)
                minimal_new[new_start:new_end] = b"\x01" * (new_end - new_start)
            elif line.startswith("< "):
                deleted += 1
            elif line.startswith("> "):
                inserted += 1
        return_code = process.wait()
        if return_code not in (0, 1):
            status = f"diff_exit_{return_code}"
    excess_runs: list[tuple[int, str, int, str]] = []
    if detail:
        excess_runs = _excess_runs(old_tokens, flow_old, minimal_old, "old", detail)
        excess_runs.extend(
            _excess_runs(new_tokens, flow_new, minimal_new, "new", detail)
        )
        excess_runs.sort(reverse=True)
        excess_runs = excess_runs[:detail]
        for length, side, start, text in excess_runs:
            print(
                f"  excess={length} side={side} start={start} text={text}",
                file=sys.stderr,
            )
    return {
        "min_deleted": deleted,
        "min_inserted": inserted,
        "minimal_status": status,
        "minimal_excess": [
            {"excess": length, "side": side, "start": start, "text": text}
            for length, side, start, text in excess_runs
        ],
    }


def token_diff_counts(
    old_tokens: list[str],
    new_tokens: list[str],
    timeout: int = MINIMAL_DIFF_TIMEOUT,
) -> tuple[int, int, str]:
    """Changed-line counts of ``diff --minimal`` for two token sequences."""
    deleted = 0
    inserted = 0
    status = "ok"
    with tempfile.TemporaryDirectory(prefix="pdfdelta-token-diff-") as directory:
        old_path = pathlib.Path(directory) / "old.tokens"
        new_path = pathlib.Path(directory) / "new.tokens"
        with old_path.open("w", encoding="utf-8") as stream:
            stream.writelines(f"{token}\n" for token in old_tokens)
        with new_path.open("w", encoding="utf-8") as stream:
            stream.writelines(f"{token}\n" for token in new_tokens)
        command = ["diff", "--minimal", "--", str(old_path), str(new_path)]
        if shutil.which("timeout"):
            command = ["timeout", str(timeout), *command]
        process = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        assert process.stdout is not None
        for line in process.stdout:
            if line.startswith("< "):
                deleted += 1
            elif line.startswith("> "):
                inserted += 1
        return_code = process.wait()
        if return_code not in (0, 1):
            status = f"diff_exit_{return_code}"
    return deleted, inserted, status


def _mask_digits(text: str) -> str:
    return re.sub(r"\d+", "0", text)


def _normalise_line(text: str) -> str:
    return "".join(text.split())


def furniture_tokens(text: str) -> list[str]:
    return [_mask_digits(token) for token in text.split()]


def oracle_page_tokens(
    pdf: pathlib.Path,
    furniture: list[dict],
) -> tuple[dict[int, list[str]], int]:
    """PyMuPDF reading-order tokens per page, with flow furniture removed."""
    import pymupdf

    by_page: dict[int, list[list[str]]] = {}
    numeric_only: dict[int, list[list[str]]] = {}
    for line in furniture:
        text = line.get("text", "")
        sequence = furniture_tokens(text)
        numeric = any(character.isdigit() for character in text) and not any(
            character.isalpha() for character in text
        )
        for page in line.get("pages", []):
            by_page.setdefault(page, []).append(sequence)
            if numeric:
                numeric_only.setdefault(page, []).append(sequence)
    document = pymupdf.open(pdf)
    result: dict[int, list[str]] = {}
    removed = 0
    for page_number, page in enumerate(document, 1):
        sequences = by_page.get(page_number, [])
        numeric = numeric_only.get(page_number, [])
        lines = []
        for block in page.get_text("dict")["blocks"]:
            if block.get("type") != 0:
                continue
            for line in block.get("lines", []):
                text = "".join(span.get("text", "") for span in line.get("spans", []))
                lines.append(
                    [
                        token
                        for token in tokenize(text)
                        if not (
                            len(token) == 1
                            and unicodedata.category(token) in {"Cc", "Cf"}
                        )
                    ]
                )
        page_tokens = []
        for index, line_tokens in enumerate(lines):
            edge = index == 0 or index + 1 == len(lines)
            masked = [_mask_digits(token) for token in line_tokens]
            for sequence in sequences:
                if sequence in numeric and not edge:
                    continue
                if masked[: len(sequence)] == sequence:
                    removed += len(sequence)
                    line_tokens = line_tokens[len(sequence) :]
                    masked = masked[len(sequence) :]
                elif masked[-len(sequence) :] == sequence:
                    removed += len(sequence)
                    line_tokens = line_tokens[: -len(sequence)]
                    masked = masked[: -len(sequence)]
            page_tokens.extend(line_tokens)
        result[page_number] = page_tokens
    return result, removed


def oracle_tokens(
    pdf: pathlib.Path,
    furniture: list[dict],
) -> tuple[list[str], int]:
    """Flatten page-wise oracle tokens in page order."""
    pages, removed = oracle_page_tokens(pdf, furniture)
    tokens = [token for page in sorted(pages) for token in pages[page]]
    return tokens, removed


def metric_token(text: str) -> str:
    """Normalise tokenisation conventions that do not change reading order."""
    text = unicodedata.normalize("NFKC", text)
    text = text.replace("\u00ad", "")
    text = text.translate(
        str.maketrans(
            {
                "\u2018": "'",
                "\u2019": "'",
                "\u201a": "'",
                "\u201b": "'",
                "\u201c": '"',
                "\u201d": '"',
                "\u201e": '"',
                "\u201f": '"',
                "\u2010": "-",
                "\u2011": "-",
                "\u2012": "-",
                "\u2013": "-",
                "\u2014": "-",
                "\u2015": "-",
                "\u2212": "-",
            }
        )
    )
    return "".join(
        scalar
        for scalar in text
        if not unicodedata.combining(scalar)
        and unicodedata.category(scalar) not in {"Cc", "Cf"}
    )


def metric_tokens(tokens: list[str]) -> list[str]:
    return [normalised for token in tokens if (normalised := metric_token(token))]


def oracle_order_agreement(
    flow_tokens: list[str],
    oracle: list[str],
) -> tuple[float, int]:
    """Order-preserving overlap between flow and an independent reader."""
    if not flow_tokens and not oracle:
        return 1.0, 0
    deleted, _inserted, status = token_diff_counts(flow_tokens, oracle)
    if status != "ok":
        return math.nan, len(oracle)
    lcs = len(flow_tokens) - deleted
    longest = max(len(flow_tokens), len(oracle))
    return (lcs / longest if longest else 1.0), len(oracle)


def evaluate(
    pair: str,
    report: pathlib.Path,
    bag: dict,
    expected: list[dict],
    cache: pathlib.Path,
) -> dict:
    data = read_report(report)
    if "report_too_large" in data.get("status", ""):
        raise ValueError(data["status"])
    changes = data["changes"]
    furniture = data.get("furniture", {})
    moves = [change for change in changes if change["kind"] == "move"]
    totals = collections.Counter()
    for change in changes:
        totals[f"{change['kind']}_old"] += change["old"][1] - change["old"][0]
        totals[f"{change['kind']}_new"] += change["new"][1] - change["new"][0]
    old_scan = scan_side(
        report, "old", changes, expected, furniture_sequences(furniture, "old")
    )
    new_scan = scan_side(
        report, "new", changes, expected, furniture_sequences(furniture, "new")
    )
    cancellable = sum((old_scan["changed"] & new_scan["changed"]).values())
    neighbour_equal = sum(
        1
        for index in range(len(moves))
        if old_scan["prev"].get(index) == new_scan["prev"].get(index)
        and old_scan["next"].get(index) == new_scan["next"].get(index)
    )
    gap_moves = [move for move in moves if "old_gap" in move and "new_gap" in move]
    if moves and len(gap_moves) == len(moves):
        in_place = sum(1 for move in moves if move["old_gap"] == move["new_gap"])
    else:
        in_place = neighbour_equal
    scores = []
    located = 0
    hits = 0
    missing_ids = []
    unhit_ids = []
    miss_reasons = []
    for quote in expected:
        record = old_scan["quotes"].get(quote["id"])
        if record is None:
            record = new_scan["quotes"].get(quote["id"])
        if record is None:
            missing_ids.append(quote["id"])
            miss_reasons.append(f"{quote['id']}:unlocated")
            continue
        located += 1
        if record["hit"]:
            hits += 1
        else:
            unhit_ids.append(quote["id"])
            reason = (
                "unchanged_occurrence"
                if record.get("occurrences")
                else "non_contiguous_occurrence"
            )
            miss_reasons.append(f"{quote['id']}:{reason}")
        scores.append(covered_fraction(record["positions"], changes, quote["side"]))
    oracle = bag.get(pair, {})
    changed = (
        totals["delete_old"]
        + totals["insert_new"]
        + totals["replace_old"]
        + totals["replace_new"]
    )
    lower = oracle.get("bag_deleted", 0) + oracle.get("bag_inserted", 0)
    old_conservation = data["old"].get("conservation", {})
    new_conservation = data["new"].get("conservation", {})
    minimal = minimal_diff(report, changes)
    minimal_changed = minimal["min_deleted"] + minimal["min_inserted"]
    if minimal_changed:
        align_ratio = changed / minimal_changed
    else:
        align_ratio = 1.0 if changed == 0 else math.inf
    old_flow_tokens = read_tokens(report, "old")
    new_flow_tokens = read_tokens(report, "new")
    furniture = data.get("furniture", {})
    old_oracle_tokens, old_furniture_removed = oracle_tokens(
        cache / f"{pair}-old.pdf", furniture.get("old", [])
    )
    new_oracle_tokens, new_furniture_removed = oracle_tokens(
        cache / f"{pair}-new.pdf", furniture.get("new", [])
    )
    old_agreement, old_oracle_count = oracle_order_agreement(
        metric_tokens(old_flow_tokens), metric_tokens(old_oracle_tokens)
    )
    new_agreement, new_oracle_count = oracle_order_agreement(
        metric_tokens(new_flow_tokens), metric_tokens(new_oracle_tokens)
    )
    order = data.get("order", {})
    def order_count(side: str, key: str) -> int:
        pages = order.get(side, []) if isinstance(order, dict) else []
        if key == "stream":
            return sum(1 for page in pages if page.get("order") == "stream")
        if key == "baseline":
            return sum(1 for page in pages if page.get("order") == "baseline")
        if key == "uncertain":
            return sum(1 for page in pages if page.get("order_uncertain") is True)
        return sum(1 for page in pages if page.get("order_coherence", 1.0) < 0.5)

    return {
        "old_conservation": f"{old_conservation.get('used', '')}/{old_conservation.get('visible', '')}",
        "new_conservation": f"{new_conservation.get('used', '')}/{new_conservation.get('visible', '')}",
        "conservation_complete": old_conservation.get("complete") is True
        and new_conservation.get("complete") is True,
        "old_tokens": data["old"]["tokens"],
        "new_tokens": data["new"]["tokens"],
        "oracle_old": oracle.get("oracle_old"),
        "oracle_new": oracle.get("oracle_new"),
        "oracle_old_tokens": old_oracle_count,
        "oracle_new_tokens": new_oracle_count,
        "oracle_order_agreement_old": old_agreement,
        "oracle_order_agreement_new": new_agreement,
        "oracle_furniture_removed_old": old_furniture_removed,
        "oracle_furniture_removed_new": new_furniture_removed,
        "stream_pages_old": order_count("old", "stream"),
        "stream_pages_new": order_count("new", "stream"),
        "baseline_pages_old": order_count("old", "baseline"),
        "baseline_pages_new": order_count("new", "baseline"),
        "order_uncertain_old": order_count("old", "uncertain"),
        "order_uncertain_new": order_count("new", "uncertain"),
        "low_coherence_old": order_count("old", "low_coherence"),
        "low_coherence_new": order_count("new", "low_coherence"),
        "bag_deleted": oracle.get("bag_deleted"),
        "bag_inserted": oracle.get("bag_inserted"),
        "extraction_old": data["old"]["extraction_complete"],
        "extraction_new": data["new"]["extraction_complete"],
        "furniture_old": len(data.get("furniture", {}).get("old", [])),
        "furniture_new": len(data.get("furniture", {}).get("new", [])),
        "delete_old": totals["delete_old"],
        "insert_new": totals["insert_new"],
        "replace_old": totals["replace_old"],
        "replace_new": totals["replace_new"],
        "move_old": totals["move_old"],
        "layout_only_old": totals["layout_only_old"],
        "n_changes": len(changes),
        "cancellable": cancellable,
        "min_deleted": minimal["min_deleted"],
        "min_inserted": minimal["min_inserted"],
        "flow_non_move_changed": changed,
        "align_ratio": align_ratio,
        "minimal_status": minimal["minimal_status"],
        "in_place_moves": in_place,
        "neighbour_equal_moves": neighbour_equal,
        "noise": changed / lower if lower else math.inf,
        "coverage": sum(scores) / len(scores) if scores else math.nan,
        "low_coverage_quotes": sum(score < 0.5 for score in scores),
        "missing_quotes": len(missing_ids),
        "missing_quote_ids": ",".join(missing_ids),
        "unhit_quotes": len(unhit_ids),
        "unhit_quote_ids": ",".join(unhit_ids),
        "quote_misses": ",".join(miss_reasons),
        "quote_hits": hits,
        "quote_located": located,
        "quote_total": len(expected),
        "quotes": len(expected),
    }


COLUMNS = [
    "pair",
    "exit_code",
    "status",
    "wall_seconds",
    "max_rss_kb",
    "old_tokens",
    "new_tokens",
    "extraction_old",
    "extraction_new",
    "old_conservation",
    "new_conservation",
    "conservation_complete",
    "furniture_old",
    "furniture_new",
    "delete_old",
    "insert_new",
    "replace_old",
    "replace_new",
    "move_old",
    "layout_only_old",
    "n_changes",
    "cancellable",
    "min_deleted",
    "min_inserted",
    "flow_non_move_changed",
    "align_ratio",
    "minimal_status",
    "in_place_moves",
    "neighbour_equal_moves",
    "oracle_old",
    "oracle_new",
    "oracle_old_tokens",
    "oracle_new_tokens",
    "oracle_order_agreement_old",
    "oracle_order_agreement_new",
    "oracle_furniture_removed_old",
    "oracle_furniture_removed_new",
    "stream_pages_old",
    "stream_pages_new",
    "baseline_pages_old",
    "baseline_pages_new",
    "order_uncertain_old",
    "order_uncertain_new",
    "low_coherence_old",
    "low_coherence_new",
    "bag_deleted",
    "bag_inserted",
    "noise",
    "coverage",
    "low_coverage_quotes",
    "missing_quotes",
    "missing_quote_ids",
    "unhit_quotes",
    "unhit_quote_ids",
    "quote_misses",
    "quote_hits",
    "quote_located",
    "quote_total",
    "quotes",
]


def dump_ngrams(pair: str, report: pathlib.Path, top: int) -> None:
    data = read_report(report, limit=False)
    changes = data["changes"]
    old_scan = scan_side(report, "old", changes, [], with_geometry=True)
    new_scan = scan_side(report, "new", changes, [])
    cancellable = old_scan["changed"] & new_scan["changed"]
    records = old_scan["changed_records"]
    print(
        f"{pair}: {len(records)} changed old tokens, "
        f"{sum(cancellable.values())} cancellable"
    )
    for size in range(3, 7):
        counter: collections.Counter = collections.Counter()
        examples: dict[tuple, list[tuple[int, float]]] = {}
        for index in range(len(records) - size + 1):
            window = records[index : index + size]
            texts = tuple(record[0] for record in window)
            if not all(cancellable[text] for text in texts):
                continue
            counter[texts] += 1
            examples.setdefault(texts, []).append(
                (window[0][1], round(window[0][2], 1))
            )
        print(f"-- n={size}")
        for texts, count in counter.most_common(top):
            print(count, examples[texts][:3], " ".join(texts)[:160])


def repeat_pairs(args: argparse.Namespace, pairs: list[str]) -> int:
    """Runs each pair repeatedly and checks byte-identical outputs."""
    failures = 0
    for pair in pairs:
        digests: dict[tuple[str, ...], int] = {}
        for _ in range(args.repeat):
            run = run_pair(args.binary, args.cache, pair, args.results, args.timeout)
            report = run["report"]
            compressed = report.suffix.lower() == ".gz"
            base = report.with_suffix("") if compressed else report
            suffix = ".gz" if compressed else ""
            paths = [
                report,
                report.with_name(
                    f"{base.name}.old-tokens.jsonl{suffix}"
                ),
                report.with_name(
                    f"{base.name}.new-tokens.jsonl{suffix}"
                ),
            ]
            digest = tuple(
                hashlib.sha256(path.read_bytes()).hexdigest()
                for path in paths
            )
            digests[digest] = digests.get(digest, 0) + 1
        distinct = len(digests)
        print(
            f"{pair}: distinct output hashes {distinct}/{args.repeat}",
            file=sys.stderr,
        )
        if distinct != 1:
            failures += 1
    print(f"repeated pairs with differing output: {failures}")
    return 1 if failures else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=pathlib.Path, required=True)
    parser.add_argument("--results", type=pathlib.Path, required=True)
    parser.add_argument(
        "--binary", type=pathlib.Path, default=pathlib.Path("target/release/pdfdelta")
    )
    parser.add_argument("--bag", type=pathlib.Path)
    parser.add_argument("--expected", type=pathlib.Path)
    parser.add_argument("--only", nargs="*")
    parser.add_argument("--skip-run", action="store_true")
    parser.add_argument(
        "--repeat",
        type=int,
        default=0,
        help="rerun each selected pair N times and fail on differing output hashes",
    )
    parser.add_argument("--timeout", type=int, default=600)
    parser.add_argument("--ngrams", help="dump cancellable n-grams for one existing pair")
    parser.add_argument(
        "--minimal-diagnose",
        help="dump the largest flow-vs-minimal excess regions for one existing pair",
    )
    parser.add_argument("--top", type=int, default=20)
    args = parser.parse_args()

    args.results.mkdir(parents=True, exist_ok=True)
    bag = load_bag(args.bag)
    expected = load_expected(args.expected)

    if args.ngrams:
        dump_ngrams(args.ngrams, args.results / f"{args.ngrams}.json.gz", args.top)
        return 0

    if args.minimal_diagnose:
        report = args.results / f"{args.minimal_diagnose}.json.gz"
        data = read_report(report)
        minimal = minimal_diff(report, data["changes"], detail=args.top)
        print(
            f"{args.minimal_diagnose}: min_deleted={minimal['min_deleted']} "
            f"min_inserted={minimal['min_inserted']} status={minimal['minimal_status']}",
            file=sys.stderr,
        )
        return 0

    pairs = sorted(
        path.name[: -len("-old.pdf")] for path in args.cache.glob("*-old.pdf")
    )
    pairs = [pair for pair in pairs if (args.cache / f"{pair}-new.pdf").exists()]
    if args.only:
        pairs = [pair for pair in pairs if pair in set(args.only)]

    rows = []
    if args.repeat:
        return repeat_pairs(args, pairs)
    for pair in pairs:
        if args.skip_run:
            run = {
                "pair": pair,
                "exit_code": None,
                "status": "cached",
                "seconds": math.nan,
                "max_rss_kb": None,
                "report": args.results / f"{pair}.json.gz",
            }
        else:
            run = run_pair(args.binary, args.cache, pair, args.results, args.timeout)
            print(
                f"{pair}: exit {run['exit_code']} in {run['seconds']:.1f}s",
                file=sys.stderr,
            )
        row = {**run}
        row["wall_seconds"] = run["seconds"]
        try:
            row.update(evaluate(pair, run["report"], bag, expected.get(pair, []), args.cache))
        except (OSError, ValueError, json.JSONDecodeError) as error:
            row["status"] = str(error)
            print(f"{pair}: evaluation failed: {error}", file=sys.stderr)
            continue
        rows.append(row)

    with (args.results / "summary.tsv").open("w", encoding="utf-8") as stream:
        stream.write("\t".join(COLUMNS) + "\n")
        for row in rows:
            stream.write(
                "\t".join(
                    "" if row.get(column) is None else str(row.get(column, ""))
                    for column in COLUMNS
                )
                + "\n"
            )
    totals = collections.Counter()
    for row in rows:
        totals["delete_old"] += row["delete_old"]
        totals["insert_new"] += row["insert_new"]
        totals["replace_old"] += row["replace_old"]
        totals["replace_new"] += row["replace_new"]
        totals["cancellable"] += row["cancellable"]
        totals["in_place_moves"] += row["in_place_moves"]
        oracle = bag.get(row["pair"], {})
        totals["bag_deleted"] += oracle.get("bag_deleted", 0)
        totals["bag_inserted"] += oracle.get("bag_inserted", 0)
    lower = totals["bag_deleted"] + totals["bag_inserted"]
    changed = (
        totals["delete_old"]
        + totals["insert_new"]
        + totals["replace_old"]
        + totals["replace_new"]
    )
    noise = f"{changed / lower:.3f}" if lower else "n/a"
    ratios = [row["align_ratio"] for row in rows if math.isfinite(row["align_ratio"])]
    ratio_label = f"{sum(ratios) / len(ratios):.3f}" if ratios else "n/a"
    over = [
        f"{row['pair']}:{row['align_ratio']:.3f}"
        for row in rows
        if row["align_ratio"] > 1.02
    ]
    print(
        f"pairs={len(rows)} "
        f"exit-codes={dict(collections.Counter(row['exit_code'] for row in rows))} "
        f"changed={changed} cancellable={totals['cancellable']} "
        f"in_place_moves={totals['in_place_moves']} noise={noise} "
        f"mean_align_ratio={ratio_label} "
        f"align_ratio>1.02={len(over)}"
    )
    for label in over:
        print(f"  over: {label}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
