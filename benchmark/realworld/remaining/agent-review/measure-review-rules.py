#!/usr/bin/env python3
"""Measure what three ways of reading one review bundle cost, in tokens.

Each rule is the actual sequence of commands an agent would issue, counted with
one named tokenizer over the same extraction that produces the baseline text:

- settled     stop where the engine stopped. Read index pages while they still
              hold cases the engine settled something about, then open exactly
              those. The listing is ordered by that finding, so everything
              after the first unsettled page is unsettled too.
- full        read every index page, then open every case at the text level.
              This is the mechanical "read everything the packet offers" rule.
- directed    read every index page, then open every case at the level its own
              index record names. For material no comparison examined that is
              the quote, so this rule reads the whole document through the
              packet and is the upper bound on what a bundle can cost.
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

enc = tiktoken.get_encoding("cl100k_base")
tok = lambda text: len(enc.encode(text, disallowed_special=()))
run = lambda args: subprocess.run(args, capture_output=True, timeout=600).stdout.decode(
    "utf-8", "replace"
)


def listing(bundle):
    """Every index page, with the tokens each one costs."""
    pages, cursor = [], None
    while True:
        args = [str(PDFDELTA), "review", "list", str(bundle),
                "--max-output-bytes", str(LIST_BUDGET)]
        if cursor:
            args += ["--cursor", cursor]
        payload = run(args)
        response = json.loads(payload)
        if "error" in response:
            break
        pages.append((tok(payload), response))
        cursor = response.get("next_cursor")
        if not cursor or len(pages) > 2000:
            break
    return pages


def show(bundle, case, detail):
    return tok(run([str(PDFDELTA), "review", "show", str(bundle), "--case", case,
                    "--detail", detail, "--max-output-bytes", str(SHOW_BUDGET)]))


out = []
for pair_dir in sorted(p for p in RUNS.iterdir() if p.is_dir()):
    bundle = pair_dir / "bundle"
    if not (bundle / "manifest.json").exists():
        continue
    baseline = pair_dir / "baseline"
    run([str(PDFBENCH), "audit-agent-review", "--bundle", str(bundle),
         "--baseline-text", str(baseline)])
    full_text = sum(tok((baseline / side).read_text("utf-8", "replace"))
                    for side in ("old.txt", "new.txt") if (baseline / side).exists())

    pages = listing(bundle)
    index_all = sum(cost for cost, _ in pages)
    records = [record for _, page in pages for record in page.get("cases", [])]
    settled_cases, index_settled = [], 0
    for cost, page in pages:
        index_settled += cost
        page_cases = page.get("cases", [])
        settled_cases += [c["case"] for c in page_cases
                          if c["finding"] == "difference_established"]
        if not any(c["finding"] == "difference_established" for c in page_cases):
            break

    # One cache per (case, detail): the same answer is never charged twice.
    answers = {}
    def answer(case, detail):
        key = (case, detail)
        if key not in answers:
            answers[key] = show(bundle, case, detail)
        return answers[key]

    settled = index_settled + sum(answer(case, "text") for case in settled_cases)
    full = index_all + sum(answer(r["case"], "text") for r in records)
    directed = index_all + sum(answer(r["case"], r["next"]) for r in records)
    unexamined = sum(1 for r in records if r["finding"] == "not_examined")
    out.append({
        "pair": pair_dir.name,
        "cases": len(records),
        "settled_cases": len(settled_cases),
        "unexamined_cases": unexamined,
        "index_pages": len(pages),
        "index_tokens": index_all,
        "full_text_tokens": full_text,
        "settled_review_tokens": settled,
        "full_review_tokens": full,
        "directed_review_tokens": directed,
    })
    print(json.dumps(out[-1]), flush=True)
(RUNS / "review-rules.json").write_text(json.dumps(out, indent=1) + "\n")
