#!/usr/bin/env python3
"""Audit the flow diff against an independent PyMuPDF + minimal-diff oracle.

The audit is deliberately independent of the flow alignment: it reads the two
PDFs with PyMuPDF, tokenises each page in PyMuPDF's own reading order with the
pinned oracle tokeniser from ``flow_eval.py``, removes the flow report's
furniture lines from the oracle, and runs GNU ``diff --minimal`` on the
token-per-line streams. The resulting changed-token multiset is compared with
the flow report's non-move changes.

For a deterministic sample of regions where only one tool changed tokens, the
audit extracts the text inside the region's bounding box from both PDFs and
shows the glyphs that ``pdfdelta inspect --glyphs`` reports for the same page,
then records a verdict with its evidence.

Usage:
    uv run --no-project --with pymupdf python -I flow_audit.py \
        --cache benchmark/realworld/cache \
        --results /tmp/flow-eval \
        --binary target/release/pdfdelta \
        --output /tmp/flow-audit \
        --sample 40
"""

from __future__ import annotations

import argparse
import collections
import gzip
import json
import math
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import unicodedata

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import flow_eval

DIFF_TIMEOUT_SECONDS = 300
INSPECT_TIMEOUT_SECONDS = 300
DEFAULT_CONTEXT = 15
DEFAULT_SAMPLE = 40
GLYPH_PATTERN = re.compile(
    r'^glyph id=\d+ page=(\d+) text="((?:[^"\\]|\\.)*)" bbox=\(([^)]*)\)'
)


def normalise(tokens: list[str]) -> list[str]:
    """Pinned comparison tokens: metric folding plus digit/letter edge splits."""
    result: list[str] = []
    for token in tokens:
        folded = flow_eval.metric_token(token)
        for part in flow_eval.split_token_edges(folded):
            if part:
                result.append(part)
    return result


def normalise_text(text: str) -> list[str]:
    return normalise(flow_eval.tokenize(text))


def canonical(token: str) -> str:
    """The pinned comparison spelling of one whole token."""
    return flow_eval.metric_token(token)


def flow_any_changes(
    report: dict, old_tokens: list[str], new_tokens: list[str]
) -> tuple[collections.Counter, collections.Counter]:
    """Every flow change, including moves and layout-only changes."""
    old_counter: collections.Counter = collections.Counter()
    new_counter: collections.Counter = collections.Counter()
    for change in report["changes"]:
        start, end = change["old"]
        for index in range(max(0, start), min(end, len(old_tokens))):
            old_counter.update([canonical(old_tokens[index])])
        start, end = change["new"]
        for index in range(max(0, start), min(end, len(new_tokens))):
            new_counter.update([canonical(new_tokens[index])])
    return old_counter, new_counter


def flow_full(report: dict, old_tokens: list[str], new_tokens: list[str]) -> tuple[collections.Counter, collections.Counter]:
    """Every token that survived flow's furniture removal, on each side."""
    return (
        collections.Counter(canonical(token) for token in old_tokens),
        collections.Counter(canonical(token) for token in new_tokens),
    )


def covers(counter: collections.Counter, parts: list[str]) -> bool:
    """Whether ``counter`` contains every part of ``parts`` (multiset)."""
    needed = collections.Counter(parts)
    return all(counter[part] >= count for part, count in needed.items())


def flow_changes(
    report: dict, old_tokens: list[str], new_tokens: list[str]
) -> tuple[bytearray, bytearray, collections.Counter, collections.Counter]:
    """Flow's non-move changed tokens, flagged in the token streams."""
    old_flags = bytearray(len(old_tokens))
    new_flags = bytearray(len(new_tokens))
    old_counter: collections.Counter = collections.Counter()
    new_counter: collections.Counter = collections.Counter()
    for change in report["changes"]:
        kind = change["kind"]
        if kind in ("delete", "replace"):
            start, end = change["old"]
            for index in range(max(0, start), min(end, len(old_tokens))):
                old_flags[index] = 1
                old_counter.update([canonical(old_tokens[index])])
        if kind in ("insert", "replace"):
            start, end = change["new"]
            for index in range(max(0, start), min(end, len(new_tokens))):
                new_flags[index] = 1
                new_counter.update([canonical(new_tokens[index])])
    return old_flags, new_flags, old_counter, new_counter


def diff_changed(
    old_tokens: list[str], new_tokens: list[str], timeout: int
) -> tuple[bytearray, bytearray, collections.Counter, collections.Counter, str]:
    """GNU ``diff --minimal`` changed tokens, flagged in both streams."""
    old_flags = bytearray(len(old_tokens))
    new_flags = bytearray(len(new_tokens))
    old_counter: collections.Counter = collections.Counter()
    new_counter: collections.Counter = collections.Counter()
    directory = tempfile.mkdtemp(prefix="pdfdelta-flow-audit-")
    try:
        old_path = pathlib.Path(directory) / "old.tokens"
        new_path = pathlib.Path(directory) / "new.tokens"
        old_path.write_text("".join(f"{token}\n" for token in old_tokens), encoding="utf-8")
        new_path.write_text("".join(f"{token}\n" for token in new_tokens), encoding="utf-8")
        command = ["diff", "--minimal", "--", str(old_path), str(new_path)]
        if shutil.which("timeout"):
            command = ["timeout", str(timeout), *command]
        process = subprocess.run(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=timeout + 30,
            check=False,
        )
        status = "ok" if process.returncode in (0, 1) else f"diff_exit_{process.returncode}"
        for line in process.stdout.splitlines():
            ranges = flow_eval._diff_ranges(line)
            if ranges is None:
                continue
            old_start, old_end, new_start, new_end = ranges
            for index in range(max(0, old_start), min(old_end, len(old_tokens))):
                old_flags[index] = 1
                old_counter.update([canonical(old_tokens[index])])
            for index in range(max(0, new_start), min(new_end, len(new_tokens))):
                new_flags[index] = 1
                new_counter.update([canonical(new_tokens[index])])
    finally:
        shutil.rmtree(directory, ignore_errors=True)
    return old_flags, new_flags, old_counter, new_counter, status


def multiset_compare(
    flow_counter: collections.Counter, oracle_counter: collections.Counter
) -> dict:
    common = flow_counter & oracle_counter
    common_count = sum(common.values())
    flow_total = sum(flow_counter.values())
    oracle_total = sum(oracle_counter.values())
    union = flow_total + oracle_total - common_count
    return {
        "flow": flow_total,
        "oracle": oracle_total,
        "common": common_count,
        "flow_only": flow_total - common_count,
        "oracle_only": oracle_total - common_count,
        "agreement": common_count / max(1, max(flow_total, oracle_total)),
        "jaccard": common_count / union if union else 1.0,
    }


def only_flags(
    changed_flags: bytearray,
    tokens: list[str],
    only_counter: collections.Counter,
) -> bytearray:
    """Flags changed tokens that consume the multiset of exclusive tokens."""
    remaining = collections.Counter(only_counter)
    flags = bytearray(len(changed_flags))
    for index, token in enumerate(tokens):
        if not changed_flags[index] or remaining[token] <= 0:
            continue
        remaining[token] -= 1
        flags[index] = 1
    return flags


def maximal_runs(flags: bytearray, context: int) -> list[tuple[int, int]]:
    """Maximal runs of flagged tokens, expanded by context and merged."""
    runs: list[tuple[int, int]] = []
    index = 0
    while index < len(flags):
        if not flags[index]:
            index += 1
            continue
        start = index
        while index < len(flags) and flags[index]:
            index += 1
        start = max(0, start - context)
        end = min(len(flags), index + context)
        if runs and start <= runs[-1][1]:
            runs[-1] = (runs[-1][0], max(runs[-1][1], end))
        else:
            runs.append((start, end))
    return runs


def page_bounds(pages: list[int]) -> dict[int, tuple[int, int]]:
    """First and exclusive-last token index of every page in a flat stream."""
    bounds: dict[int, tuple[int, int]] = {}
    for index, page in enumerate(pages):
        if page not in bounds:
            bounds[page] = (index, index + 1)
        else:
            bounds[page] = (bounds[page][0], index + 1)
    return bounds


def split_run_at_pages(
    start: int, end: int, flags: bytearray, pages: list[int]
) -> list[tuple[int, int]]:
    """Splits a candidate run so every region stays on one page.

    A flag run can cover the end of one page and the start of the next (a
    page-number footer followed by a contents heading, for example). Clipping a
    single page cannot decide such a region: the two halves are on different
    pages, so the text inside one page's box says nothing about the other half.
    Grouping the flagged tokens by page keeps each region page-local, and the
    context stays inside that page's token range.
    """
    bounds = page_bounds(pages)
    groups: list[tuple[int, int, int]] = []
    for index in range(start, end):
        if not flags[index]:
            continue
        page = pages[index]
        if groups and groups[-1][0] == page:
            groups[-1] = (page, groups[-1][1], index + 1)
        else:
            groups.append((page, index, index + 1))
    regions: list[tuple[int, int]] = []
    for page, _, _ in groups:
        page_start, page_end = bounds.get(page, (start, end))
        region = (max(page_start, start), min(page_end, end))
        if region[0] >= region[1]:
            continue
        if regions and region[0] < regions[-1][1]:
            regions[-1] = (regions[-1][0], max(regions[-1][1], region[1]))
        else:
            regions.append(region)
    return regions or [(start, end)]


def page_array_from_changes(report: dict, side: str, length: int) -> list[int]:
    """One-based page number for every flow token, from the change ranges."""
    pages = [0] * length
    key = "old_pages" if side == "old" else "new_pages"
    for change in report["changes"]:
        start, end = change["old" if side == "old" else "new"]
        page_list = change.get(key) or []
        page = page_list[0] if page_list else 0
        for index in range(max(0, start), min(end, length)):
            pages[index] = page
    return pages


def flatten_oracle(
    pages: dict[int, list[str]],
) -> tuple[list[str], list[int]]:
    tokens: list[str] = []
    numbers: list[int] = []
    for page in sorted(pages):
        numbers.extend([page] * len(pages[page]))
        tokens.extend(pages[page])
    return tokens, numbers


def contains_sequence(haystack: list[str], needle: list[str]) -> bool:
    if not needle:
        return False
    width = len(needle)
    if width > len(haystack):
        return False
    first = needle[0]
    for start in range(len(haystack) - width + 1):
        if haystack[start] == first and haystack[start : start + width] == needle:
            return True
    return False


def build_candidates(
    pair: str,
    old_tokens: list[str],
    new_tokens: list[str],
    old_pages: list[int],
    new_pages: list[int],
    old_flow_flags: bytearray,
    new_flow_flags: bytearray,
    oracle_old_tokens: list[str],
    oracle_new_tokens: list[str],
    oracle_old_pages: list[int],
    oracle_new_pages: list[int],
    old_oracle_flags: bytearray,
    new_oracle_flags: bytearray,
    flow_old: collections.Counter,
    flow_new: collections.Counter,
    oracle_old: collections.Counter,
    oracle_new: collections.Counter,
    flow_any_old: collections.Counter,
    flow_any_new: collections.Counter,
    context: int,
) -> list[dict]:
    candidates: list[dict] = []
    for side, flow_tokens, flow_pages, flow_flags, oracle_tokens, oracle_pages, oracle_flags, flow_counter, oracle_counter, flow_any in [
        (
            "old",
            old_tokens,
            old_pages,
            old_flow_flags,
            oracle_old_tokens,
            oracle_old_pages,
            old_oracle_flags,
            flow_old,
            oracle_old,
            flow_any_old,
        ),
        (
            "new",
            new_tokens,
            new_pages,
            new_flow_flags,
            oracle_new_tokens,
            oracle_new_pages,
            new_oracle_flags,
            flow_new,
            oracle_new,
            flow_any_new,
        ),
    ]:
        flow_canon = [canonical(token) for token in flow_tokens]
        oracle_canon = [canonical(token) for token in oracle_tokens]
        for tool, tokens, pages, base_flags, parts, only in [
            (
                "flow-only",
                flow_tokens,
                flow_pages,
                flow_flags,
                flow_canon,
                flow_counter - oracle_counter,
            ),
            (
                "oracle-only",
                oracle_tokens,
                oracle_pages,
                oracle_flags,
                oracle_canon,
                oracle_counter - flow_counter,
            ),
        ]:
            if not only:
                continue
            flags = only_flags(base_flags, parts, only)
            runs = [
                region
                for start, end in maximal_runs(flags, context)
                for region in split_run_at_pages(start, end, flags, pages)
            ]
            for start, end in runs:
                changed = [
                    tokens[index]
                    for index in range(start, end)
                    if flags[index]
                ]
                changed_parts = [
                    part
                    for index in range(start, end)
                    if flags[index]
                    for part in normalise([tokens[index]])
                ]
                first_flagged = next(
                    (index for index in range(start, end) if flags[index]),
                    start,
                )
                candidates.append(
                    {
                        "pair": pair,
                        "side": side,
                        "tool": tool,
                        "start": start,
                        "end": end,
                        "page": pages[first_flagged] if pages[first_flagged] else 0,
                        "changed": changed,
                        "changed_parts": changed_parts,
                        "context": " ".join(tokens[start:end]),
                        "context_parts": [
                            part
                            for token in tokens[start:end]
                            for part in normalise([token])
                        ],
                        "flags": flags,
                        "tokens": tokens,
                        "pages": pages,
                        "flow_any": flow_any,
                    }
                )
    return candidates


def sample_candidates(candidates: list[dict], limit: int) -> list[dict]:
    """Round-robin over pair/side/tool strata, deterministic and sorted."""
    groups: dict[tuple[str, str, str], list[dict]] = collections.defaultdict(list)
    for candidate in candidates:
        groups[(candidate["pair"], candidate["side"], candidate["tool"])].append(candidate)
    for group in groups.values():
        group.sort(key=lambda item: (item["start"], item["end"]))
    keys = sorted(groups)
    sampled: list[dict] = []
    round_index = 0
    while len(sampled) < limit:
        progressed = False
        for key in keys:
            if round_index >= len(groups[key]):
                continue
            sampled.append(groups[key][round_index])
            progressed = True
            if len(sampled) >= limit:
                break
        if not progressed:
            break
        round_index += 1
    return sampled


def page_words(
    document, page_number: int
) -> list[tuple[list[str], tuple[float, float, float, float]]]:
    """Normalised words and bounding boxes for one one-based page."""
    if page_number < 1 or page_number > len(document):
        return []
    words = []
    for word in document[page_number - 1].get_text("words"):
        for part in normalise([word[4]]):
            words.append(([part], (word[0], word[1], word[2], word[3])))
    return words


def find_bbox(
    words: list[tuple[list[str], tuple[float, float, float, float]]],
    changed: list[str],
    context: list[str] | None = None,
) -> tuple[float, float, float, float] | None:
    flat = [part for parts, _rect in words for part in parts]
    sequences = [changed]
    if context:
        sequences.append(context)
    for sequence in sequences:
        for width in range(min(len(sequence), 12), 2, -1):
            needle = sequence[:width]
            for start in range(len(flat) - width + 1):
                if flat[start : start + width] != needle:
                    continue
                rects = [words[index][1] for index in range(start, start + width)]
                return (
                    min(rect[0] for rect in rects),
                    min(rect[1] for rect in rects),
                    max(rect[2] for rect in rects),
                    max(rect[3] for rect in rects),
                )
    for token in changed:
        if len(token) < 2:
            continue
        for parts, rect in words:
            if token in parts:
                return rect
    return None


def glyph_pages(
    binary: pathlib.Path,
    pdf: pathlib.Path,
    pages: set[int],
) -> dict[int, list[tuple[tuple[float, float, float, float], str]]]:
    """Glyphs on the requested pages, with their boxes, from the CLI inspect."""
    if not pages:
        return {}
    command = [
        "timeout",
        str(INSPECT_TIMEOUT_SECONDS),
        str(binary),
        "inspect",
        "--glyphs",
        str(pdf),
    ]
    try:
        process = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
    except OSError as error:
        return {page: [(None, f"inspect failed: {error}")] for page in pages}
    assert process.stdout is not None
    kept: dict[int, list[tuple[tuple[float, float, float, float], str]]] = collections.defaultdict(list)
    for line in process.stdout:
        match = GLYPH_PATTERN.match(line)
        if not match:
            continue
        page = int(match.group(1)) + 1
        if page not in pages:
            continue
        text = json.loads(f'"{match.group(2)}"')
        bbox = tuple(float(value) for value in match.group(3).split(","))
        kept[page].append((bbox, text))
    process.wait()
    return kept


def glyph_text(
    glyphs: dict[int, list[tuple[tuple[float, float, float, float], str]]],
    page: int,
    rect: tuple[float, float, float, float] | None,
) -> str:
    if rect is None:
        return ""
    texts = [
        text
        for bbox, text in glyphs.get(page, [])
        if not isinstance(bbox, str) and rects_overlap(rect, bbox)
    ]
    return " ".join(texts)


def rects_overlap(
    left: tuple[float, float, float, float],
    right: tuple[float, float, float, float],
) -> bool:
    return not (
        left[2] < right[0]
        or right[2] < left[0]
        or left[3] < right[1]
        or right[3] < left[1]
    )


def watermark_at(document, page: int, rect: tuple[float, float, float, float] | None) -> bool:
    """Whether a large or transparent span covers the region box."""
    if rect is None or page < 1 or page > len(document):
        return False
    for block in document[page - 1].get_text("dict")["blocks"]:
        for line in block.get("lines", []):
            for span in line.get("spans", []):
                if span.get("alpha", 1) == 0 or span.get("size", 0) >= 72:
                    bbox = span.get("bbox")
                    if bbox and not (
                        bbox[2] < rect[0]
                        or rect[2] < bbox[0]
                        or bbox[3] < rect[1]
                        or rect[3] < bbox[1]
                    ):
                        return True
    return False


def classify(
    tool: str,
    page_old: int,
    page_new: int,
    old_clip: str,
    new_clip: str,
    changed: list[str],
    other_side_tokens: list[str],
    glyph_old: str,
    glyph_new: str,
    flow_any: collections.Counter,
    flow_full_counter: collections.Counter,
    watermark: bool,
) -> tuple[str, str]:
    other_side_counter = collections.Counter(other_side_tokens)
    old_norm = normalise_text(old_clip)
    new_norm = normalise_text(new_clip)
    evidence = (
        f"pages old={page_old} new={page_new}; "
        f"clip old={old_clip[:100]!r} new={new_clip[:100]!r}; "
        f"glyphs old={glyph_old[:100]!r} new={glyph_new[:100]!r}"
    )
    flow_reported = covers(flow_any, changed)
    if old_norm != new_norm:
        if tool == "flow-only":
            return "flow correct", evidence + "; the clipped content differs"
        if watermark:
            return "both acceptable (watermark text)", evidence + "; the span is a watermark"
        if flow_reported:
            return (
                "both acceptable (flow reports a move/layout change)",
                evidence + "; flow changed the same tokens under a move or layout change",
            )
        if not covers(flow_full_counter, changed):
            return (
                "both acceptable (text absent from flow content)",
                evidence + "; flow's content stream does not contain the tokens",
            )
        if changed and all(is_numeric_or_punct(token) for token in changed):
            return (
                "both acceptable (numeric page-number alignment)",
                evidence + "; the changed tokens are only numbers or punctuation",
            )
        # The single-page clip cannot see a region that the oracle's reading
        # order placed on another page, and flow's change may cover the same
        # tokens on any page. A changed token that still occurs in the other
        # document is a re-pairing of tokens both documents contain, not new or
        # missing content.
        if changed and covers(other_side_counter, changed):
            return (
                "both acceptable (text exists in the other document)",
                evidence
                + "; every changed token also occurs in the other document, so the clip difference is alignment",
            )
        return "oracle correct", evidence + "; the clipped content differs and flow did not report it"
    needle = changed[: min(5, len(changed))]
    if contains_sequence(other_side_tokens, needle):
        if tool == "flow-only":
            return "flow correct (reading order)", evidence + "; the text exists on the other side"
        return "both acceptable (equivalent minimal script)", evidence + "; the text exists on the other side"
    if flow_reported and tool == "oracle-only":
        return (
            "both acceptable (equivalent minimal script)",
            evidence + "; flow changed the same tokens under a move or layout change",
        )
    if not old_clip.strip() and not new_clip.strip():
        if changed and all(is_numeric_or_punct(token) for token in changed):
            return (
                "both acceptable (numeric page-number alignment)",
                evidence + "; the changed tokens are only numbers or punctuation",
            )
        if changed and all(is_unmapped_placeholder(token) for token in changed):
            kinds = sorted(
                {
                    kind
                    for token in changed
                    if (kind := unmapped_identity_kind(token)) is not None
                }
            )
            detail = ", ".join(kinds) if kinds else "unknown"
            return (
                "both acceptable (unmapped glyph placeholder)",
                evidence
                + f"; the changed tokens are undecoded glyph placeholders ({detail} identity)",
            )
        if changed and all(is_image_identity(token) for token in changed):
            return (
                "flow correct (image identity change)",
                evidence + "; flow reports a changed image and the oracle has no text there",
            )
        if changed and covers(other_side_counter, changed):
            return (
                "both acceptable (text exists in the other document)",
                evidence
                + "; every changed token also occurs in the other document, so the clip difference is alignment",
            )
        return "undecidable", evidence + "; no text inside the region box"
    return "both acceptable (tokenisation or minimal script)", evidence


def is_unmapped_placeholder(token: str) -> bool:
    return token.startswith("\ufffc") and ":" in token


def unmapped_identity_kind(token: str) -> str | None:
    """Evidence kind of one flow placeholder: name, outline, or program."""
    if not is_unmapped_placeholder(token):
        return None
    kind, separator, _ = token[1:].partition(":")
    if not separator or kind not in {"name", "outline", "program"}:
        return None
    return kind


def is_image_identity(token: str) -> bool:
    """Flow's encoded image identity token (`⟦image:<sha256>⟧`)."""
    return token.startswith("\u27e6image:") and token.endswith("\u27e7")


def is_numeric_or_punct(token: str) -> bool:
    return bool(token) and all(
        character.isascii() and (character.isdigit() or not character.isalnum())
        for character in token
    )


def classify_only(
    token: str,
    page: int,
    oracle_changed_by_page: dict[int, collections.Counter],
    oracle_unchanged_by_page: dict[int, collections.Counter],
    oracle_changed: collections.Counter,
    oracle_unchanged: collections.Counter,
    other_side: collections.Counter,
    oracle_changed_parts: collections.Counter,
) -> str:
    if is_unmapped_placeholder(token):
        kind = unmapped_identity_kind(token)
        return (
            f"unmapped glyph placeholder ({kind} identity)"
            if kind is not None
            else "unmapped glyph placeholder"
        )
    if is_numeric_or_punct(token):
        return "numeric/punctuation"
    if oracle_changed_by_page.get(page, collections.Counter())[token] > 0:
        return "oracle changed same page"
    if oracle_unchanged_by_page.get(page, collections.Counter())[token] > 0:
        return "oracle keeps equal same page"
    if oracle_changed[token] > 0:
        return "oracle changed another page"
    if oracle_unchanged[token] > 0:
        return "oracle keeps equal another page"
    if other_side[token] > 0:
        return "cross-side reorder"
    parts = normalise([token])
    if len(parts) > 1 and all(oracle_changed_parts[part] > 0 for part in parts):
        return "tokenisation"
    return "unexplained"


def targeted_counts(
    pair: str, side: str, data: dict, results: pathlib.Path
) -> collections.Counter:
    """Classifies every flow-only token of one pair/side by kind and cause."""
    tokens = flow_eval.read_tokens(results / f"{pair}.json.gz", side)
    canon = [canonical(token) for token in tokens]
    flow_flags = data[f"{side}_flow_flags"]
    flow_counter = data["flow_old" if side == "old" else "flow_new"]
    oracle_counter = data["oracle_old_c" if side == "old" else "oracle_new_c"]
    only = flow_counter - oracle_counter
    flags = only_flags(flow_flags, canon, only)
    oracle_tokens = data["old_oracle" if side == "old" else "new_oracle"]
    oracle_canon = [canonical(token) for token in oracle_tokens]
    oracle_pages = data["old_oracle_pages" if side == "old" else "new_oracle_pages"]
    oracle_flags = data[f"{side}_oracle_flags"]
    other_side = collections.Counter(
        data["new_oracle_canon" if side == "old" else "old_oracle_canon"]
    )
    oracle_changed: collections.Counter = collections.Counter()
    oracle_unchanged: collections.Counter = collections.Counter()
    oracle_changed_by_page: dict[int, collections.Counter] = collections.defaultdict(collections.Counter)
    oracle_unchanged_by_page: dict[int, collections.Counter] = collections.defaultdict(collections.Counter)
    oracle_changed_parts: collections.Counter = collections.Counter()
    for token, page, flag in zip(oracle_canon, oracle_pages, oracle_flags, strict=True):
        if flag:
            oracle_changed[token] += 1
            oracle_changed_by_page[page][token] += 1
            for part in normalise([token]):
                oracle_changed_parts[part] += 1
        else:
            oracle_unchanged[token] += 1
            oracle_unchanged_by_page[page][token] += 1
    kinds = ["unknown"] * len(tokens)
    allowed = ("delete", "replace") if side == "old" else ("insert", "replace")
    for change in data["report"]["changes"]:
        if change["kind"] not in allowed:
            continue
        start, end = change["old" if side == "old" else "new"]
        for index in range(max(0, start), min(end, len(kinds))):
            kinds[index] = change["kind"]
    flow_pages = data["old_flow_pages" if side == "old" else "new_flow_pages"]
    counts: collections.Counter = collections.Counter()
    for index, flag in enumerate(flags):
        if not flag:
            continue
        cause = classify_only(
            canon[index],
            flow_pages[index],
            oracle_changed_by_page,
            oracle_unchanged_by_page,
            oracle_changed,
            oracle_unchanged,
            other_side,
            oracle_changed_parts,
        )
        counts[(kinds[index], cause)] += 1
    return counts


def create_document(path: pathlib.Path):
    import pymupdf

    return pymupdf.open(path)


def audit_pair(
    pair: str,
    cache: pathlib.Path,
    results: pathlib.Path,
) -> tuple[dict, list[dict]]:
    report_path = results / f"{pair}.json.gz"
    report = flow_eval.read_report(report_path)
    old_tokens = flow_eval.read_tokens(report_path, "old")
    new_tokens = flow_eval.read_tokens(report_path, "new")
    old_pdf = cache / f"{pair}-old.pdf"
    new_pdf = cache / f"{pair}-new.pdf"
    old_furniture = report.get("furniture", {}).get("old", [])
    new_furniture = report.get("furniture", {}).get("new", [])
    old_pages_map, _ = flow_eval.oracle_page_tokens(old_pdf, old_furniture)
    new_pages_map, _ = flow_eval.oracle_page_tokens(new_pdf, new_furniture)
    oracle_old, oracle_old_pages = flatten_oracle(old_pages_map)
    oracle_new, oracle_new_pages = flatten_oracle(new_pages_map)
    flow_any_old, flow_any_new = flow_any_changes(report, old_tokens, new_tokens)
    flow_full_old, flow_full_new = flow_full(report, old_tokens, new_tokens)

    old_flow_flags, new_flow_flags, flow_old, flow_new = flow_changes(
        report, old_tokens, new_tokens
    )
    old_oracle_flags, new_oracle_flags, oracle_old_c, oracle_new_c, status = diff_changed(
        oracle_old, oracle_new, DIFF_TIMEOUT_SECONDS
    )
    old_compare = multiset_compare(flow_old, oracle_old_c)
    new_compare = multiset_compare(flow_new, oracle_new_c)
    old_flow_pages = page_array_from_changes(report, "old", len(old_tokens))
    new_flow_pages = page_array_from_changes(report, "new", len(new_tokens))
    candidates = build_candidates(
        pair,
        old_tokens,
        new_tokens,
        old_flow_pages,
        new_flow_pages,
        old_flow_flags,
        new_flow_flags,
        oracle_old,
        oracle_new,
        oracle_old_pages,
        oracle_new_pages,
        old_oracle_flags,
        new_oracle_flags,
        flow_old,
        flow_new,
        oracle_old_c,
        oracle_new_c,
        flow_any_old,
        flow_any_new,
        DEFAULT_CONTEXT,
    )
    return {
        "pair": pair,
        "old_tokens": len(old_tokens),
        "new_tokens": len(new_tokens),
        "old": old_compare,
        "new": new_compare,
        "status": status,
        "candidates": candidates,
        "old_pdf": old_pdf,
        "new_pdf": new_pdf,
        "old_oracle": oracle_old,
        "new_oracle": oracle_new,
        "old_oracle_canon": [canonical(token) for token in oracle_old],
        "new_oracle_canon": [canonical(token) for token in oracle_new],
        "old_flow_any": flow_any_old,
        "new_flow_any": flow_any_new,
        "old_flow_flags": old_flow_flags,
        "new_flow_flags": new_flow_flags,
        "old_oracle_flags": old_oracle_flags,
        "new_oracle_flags": new_oracle_flags,
        "flow_old": flow_old,
        "flow_new": flow_new,
        "oracle_old_c": oracle_old_c,
        "oracle_new_c": oracle_new_c,
        "old_flow_pages": old_flow_pages,
        "new_flow_pages": new_flow_pages,
        "old_flow_full": flow_full_old,
        "new_flow_full": flow_full_new,
        "old_oracle_pages": oracle_old_pages,
        "new_oracle_pages": oracle_new_pages,
        "report": report,
    }, candidates


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=pathlib.Path, required=True)
    parser.add_argument("--results", type=pathlib.Path, required=True)
    parser.add_argument("--binary", type=pathlib.Path, default=pathlib.Path("target/release/pdfdelta"))
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--sample", type=int, default=DEFAULT_SAMPLE)
    parser.add_argument("--only", nargs="*")
    parser.add_argument("--skip-verdicts", action="store_true")
    parser.add_argument(
        "--targeted",
        action="store_true",
        help="classify every flow-only token of the three most asymmetric pairs per side",
    )
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)

    pairs = sorted(
        path.name[: -len("-old.pdf")] for path in args.cache.glob("*-old.pdf")
    )
    pairs = [pair for pair in pairs if (args.cache / f"{pair}-new.pdf").exists()]
    if args.only:
        pairs = [pair for pair in pairs if pair in set(args.only)]

    rows = []
    all_candidates: list[dict] = []
    pair_data = {}
    for pair in pairs:
        data, candidates = audit_pair(pair, args.cache, args.results)
        pair_data[pair] = data
        all_candidates.extend(candidates)
        old = data["old"]
        new = data["new"]
        rows.append(
            {
                "pair": pair,
                "flow_old": old["flow"],
                "oracle_old": old["oracle"],
                "common_old": old["common"],
                "flow_only_old": old["flow_only"],
                "oracle_only_old": old["oracle_only"],
                "agree_old": old["agreement"],
                "flow_new": new["flow"],
                "oracle_new": new["oracle"],
                "common_new": new["common"],
                "flow_only_new": new["flow_only"],
                "oracle_only_new": new["oracle_only"],
                "agree_new": new["agreement"],
                "jaccard": (old["jaccard"] + new["jaccard"]) / 2,
                "diff_status": data["status"],
                "regions": len(candidates),
            }
        )
        print(
            f"{pair}: old {old['common']}/{max(1, max(old['flow'], old['oracle']))} "
            f"new {new['common']}/{max(1, max(new['flow'], new['oracle']))} "
            f"regions={len(candidates)}",
            file=sys.stderr,
        )

    with (args.output / "audit.tsv").open("w", encoding="utf-8") as stream:
        columns = list(rows[0].keys()) if rows else ["pair"]
        stream.write("\t".join(columns) + "\n")
        for row in rows:
            stream.write("\t".join(str(row[column]) for column in columns) + "\n")

    if args.targeted and rows:
        targets = {}
        for side in ("old", "new"):
            ordered = sorted(
                rows,
                key=lambda row: (
                    int(row[f"flow_only_{side}"]) - int(row[f"oracle_only_{side}"]),
                    int(row[f"flow_only_{side}"]),
                ),
                reverse=True,
            )
            targets[side] = [row["pair"] for row in ordered[:3]]
        with (args.output / "targeted.tsv").open("w", encoding="utf-8") as stream:
            stream.write("pair\tside\tkind\tcause\tcount\n")
            for side, pairs in targets.items():
                for pair in pairs:
                    for (kind, cause), count in sorted(
                        targeted_counts(pair, side, pair_data[pair], args.results).items()
                    ):
                        stream.write(f"{pair}\t{side}\t{kind}\t{cause}\t{count}\n")
        print(f"targeted={targets}", file=sys.stderr)

    sampled = sample_candidates(all_candidates, args.sample)
    verdicts = []
    if not args.skip_verdicts:
        glyph_cache: dict[tuple[str, str], dict[int, list]] = {}
        grouped_pages: dict[tuple[str, str], set[int]] = collections.defaultdict(set)
        for candidate in sampled:
            if candidate["page"]:
                grouped_pages[(candidate["pair"], candidate["side"])].add(candidate["page"])
        for (pair, side), pages in sorted(grouped_pages.items()):
            data = pair_data[pair]
            pdf = data["old_pdf"] if side == "old" else data["new_pdf"]
            glyph_cache[(pair, side)] = glyph_pages(args.binary, pdf, pages)
        for candidate in sampled:
            pair = candidate["pair"]
            data = pair_data[pair]
            old_pdf = data["old_pdf"]
            new_pdf = data["new_pdf"]
            side = candidate["side"]
            page = candidate["page"]
            other_tokens = (
                data["new_oracle_canon"] if side == "old" else data["old_oracle_canon"]
            )
            other_pages = (
                data["new_oracle_pages"] if side == "old" else data["old_oracle_pages"]
            )
            needle = candidate["changed"][: min(5, len(candidate["changed"]))]
            other_page = page
            if needle:
                for start in range(len(other_tokens) - len(needle) + 1):
                    if other_tokens[start : start + len(needle)] == needle:
                        other_page = other_pages[start]
                        break
            if side == "old":
                page_old, page_new = page, other_page
            else:
                page_old, page_new = other_page, page
            side_flow_full = (
                data["old_flow_full"] if side == "old" else data["new_flow_full"]
            )
            old_document = create_document(old_pdf)
            new_document = create_document(new_pdf)
            try:
                old_words = page_words(old_document, page_old)
                new_words = page_words(new_document, page_new)
                rect_old = find_bbox(
                    old_words, candidate["changed_parts"], candidate["context_parts"]
                )
                rect_new = find_bbox(
                    new_words, candidate["changed_parts"], candidate["context_parts"]
                )
                watermark = watermark_at(old_document, page_old, rect_old) or watermark_at(
                    new_document, page_new, rect_new
                )
                old_clip = (
                    old_document[page_old - 1].get_text("text", clip=rect_old)
                    if rect_old and 1 <= page_old <= len(old_document)
                    else ""
                )
                new_clip = (
                    new_document[page_new - 1].get_text("text", clip=rect_new)
                    if rect_new and 1 <= page_new <= len(new_document)
                    else ""
                )
            finally:
                old_document.close()
                new_document.close()
            glyph_old = glyph_text(glyph_cache.get((pair, "old"), {}), page_old, rect_old)
            glyph_new = glyph_text(glyph_cache.get((pair, "new"), {}), page_new, rect_new)
            verdict, evidence = classify(
                candidate["tool"],
                page_old,
                page_new,
                old_clip,
                new_clip,
                candidate["changed"],
                other_tokens,
                glyph_old,
                glyph_new,
                candidate["flow_any"],
                side_flow_full,
                watermark,
            )
            verdicts.append(
                {
                    **candidate,
                    "old_clip": old_clip,
                    "new_clip": new_clip,
                    "glyph_old": glyph_old,
                    "glyph_new": glyph_new,
                    "page_old": page_old,
                    "page_new": page_new,
                    "verdict": verdict,
                    "evidence": evidence,
                }
            )
            print(f"{pair} {side} {candidate['tool']} p{page}: {verdict}", file=sys.stderr)

    with (args.output / "regions.tsv").open("w", encoding="utf-8") as stream:
        columns = [
            "pair",
            "side",
            "tool",
            "page",
            "page_old",
            "page_new",
            "start",
            "end",
            "changed",
            "context",
            "old_clip",
            "new_clip",
            "verdict",
            "evidence",
        ]
        stream.write("\t".join(columns) + "\n")
        for region in verdicts:
            row = {
                **region,
                "changed": " ".join(region["changed"]),
            }
            stream.write(
                "\t".join(str(row.get(column, "")).replace("\t", " ") for column in columns)
                + "\n"
            )

    counts = collections.Counter(region["verdict"] for region in verdicts)
    with (args.output / "verdicts.md").open("w", encoding="utf-8") as stream:
        stream.write("# Flow differential audit verdicts\n\n")
        stream.write(
            f"Sampled {len(verdicts)} of {len(all_candidates)} candidate regions.\n\n"
        )
        for verdict, count in sorted(counts.items()):
            stream.write(f"- **{verdict}**: {count}\n")
        stream.write("\n")
        for index, region in enumerate(verdicts, 1):
            stream.write(
                f"## {index}. {region['pair']} {region['side']} "
                f"{region['tool']} page {region['page']}\n\n"
            )
            stream.write(f"- verdict: **{region['verdict']}**\n")
            stream.write(f"- changed: `{' '.join(region['changed'])[:500]}`\n")
            stream.write(f"- context: `{region['context'][:500]}`\n")
            stream.write(f"- old clip: `{region['old_clip'][:500]}`\n")
            stream.write(f"- new clip: `{region['new_clip'][:500]}`\n")
            stream.write(f"- glyphs old: `{region['glyph_old'][:500]}`\n")
            stream.write(f"- glyphs new: `{region['glyph_new'][:500]}`\n")
            stream.write(f"- evidence: {region['evidence']}\n\n")
    print(
        f"pairs={len(rows)} candidates={len(all_candidates)} sampled={len(verdicts)} "
        f"verdicts={dict(sorted(counts.items()))}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
