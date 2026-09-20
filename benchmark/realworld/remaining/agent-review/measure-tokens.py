#!/usr/bin/env python3
"""Measure what a review actually costs to read, in tokens.

Baselines and the packet path are counted with one named tokenizer over the
same extraction, so the comparison is between two ways of reviewing one
document rather than between two readings of it.
"""
import json, subprocess, sys
from pathlib import Path

import tiktoken

ROOT = Path(__file__).resolve().parents[4]
PDFDELTA = ROOT / "target/release/pdfdelta"
PDFBENCH = ROOT / "target/release/pdfbench"
RUNS = Path(sys.argv[1])
LIST_BUDGET = int(sys.argv[2]) if len(sys.argv) > 2 else 8192
SHOW_BUDGET = int(sys.argv[3]) if len(sys.argv) > 3 else 16384
TRIAGE = 10          # cases an agent opens before deciding how to proceed
FULL_CAP = 400       # above this, a full review is not measured case by case

ENCODING = "cl100k_base"
enc = tiktoken.get_encoding(ENCODING)
tokens = lambda text: len(enc.encode(text, disallowed_special=()))


def run(args):
    done = subprocess.run(args, capture_output=True, timeout=300)
    return done.returncode, done.stdout.decode("utf-8", "replace")


def measure(pair_dir):
    pair = pair_dir.name
    bundle = pair_dir / "bundle"
    report = pair_dir / "report.json"
    if not (bundle / "manifest.json").exists():
        return None
    baseline_dir = pair_dir / "baseline"
    code, audit_text = run([
        str(PDFBENCH), "audit-agent-review", "--bundle", str(bundle),
        *(["--report", str(report)] if report.exists() else []),
        "--baseline-text", str(baseline_dir),
    ])
    audit = json.loads(audit_text)

    full_text = ""
    for side in ("old.txt", "new.txt"):
        path = baseline_dir / side
        if path.exists():
            full_text += path.read_text("utf-8", "replace")
    baseline_text_tokens = tokens(full_text)
    baseline_report_tokens = (
        tokens(report.read_text("utf-8", "replace")) if report.exists() else None
    )

    # L0: page through the whole index, exactly as an agent would.
    list_tokens, list_calls, case_ids, cursor = 0, 0, [], None
    while True:
        args = [str(PDFDELTA), "review", "list", str(bundle),
                "--max-output-bytes", str(LIST_BUDGET)]
        if cursor:
            args += ["--cursor", cursor]
        _, payload = run(args)
        response = json.loads(payload)
        list_calls += 1
        list_tokens += tokens(payload)
        if "error" in response:
            break
        case_ids += [c["case"] for c in response.get("cases", [])]
        cursor = response.get("next_cursor")
        if not cursor or list_calls > 2000:
            break
    first_page_tokens = 0
    if list_calls:
        _, payload = run([str(PDFDELTA), "review", "list", str(bundle),
                          "--max-output-bytes", str(LIST_BUDGET)])
        first_page_tokens = tokens(payload)

    def show(case):
        _, payload = run([str(PDFDELTA), "review", "show", str(bundle), "--case", case,
                          "--detail", "text", "--max-output-bytes", str(SHOW_BUDGET)])
        return tokens(payload)

    triage_tokens = sum(show(case) for case in case_ids[:TRIAGE])
    if len(case_ids) <= FULL_CAP:
        show_tokens = sum(show(case) for case in case_ids[TRIAGE:]) + triage_tokens
        full_measured = True
    else:
        show_tokens, full_measured = None, False

    manifest = json.loads((bundle / "manifest.json").read_text())
    return {
        "pair": pair,
        "comparison_complete": audit["comparison_complete"],
        "cases": audit["cases"],
        "findings": len(audit["findings"]),
        "unlocalized_gaps": audit["unlocalized_gaps"],
        "engine_status": manifest["engine"]["status"],
        "tokenizer": f"tiktoken/{tiktoken.__version__}:{ENCODING}",
        "baseline_full_text_tokens": baseline_text_tokens,
        "baseline_report_tokens": baseline_report_tokens,
        "index_pages": list_calls,
        "index_first_page_tokens": first_page_tokens,
        "index_all_pages_tokens": list_tokens,
        "triage_tokens": first_page_tokens + triage_tokens,
        "triage_cases": min(TRIAGE, len(case_ids)),
        "full_review_tokens": (list_tokens + show_tokens) if full_measured else None,
        "full_review_measured": full_measured,
        "cost_bytes": audit["cost"],
    }


results = []
for pair_dir in sorted(p for p in RUNS.iterdir() if p.is_dir()):
    record = measure(pair_dir)
    if record:
        results.append(record)
        print(json.dumps(record["pair"]) + " done", flush=True)
(RUNS / "tokens.json").write_text(json.dumps(results, indent=1) + "\n")
print("measured", len(results))
