#!/usr/bin/env python3
"""Recover and replay the frozen public native-text panel without dropping failures.

Install native-panel-requirements.txt for streaming report validation. Run capture
inside a shared cgroup with at most 6 GB of memory and no swap. Downloads and
dated capture records stay in the ignored cache/results directories.
"""

import argparse
from collections import Counter
from datetime import datetime, timezone
import gzip
import hashlib
import json
import math
import os
from pathlib import Path
import signal
import subprocess
import time
from urllib.parse import urlsplit
from urllib.request import Request, urlopen


DEFAULT_PANEL = Path(__file__).with_name("native-panel.json")
MEMORY_LIMIT = 6_000_000_000
REPORT_BUDGET = 1_000_000_000
LOG_LIMIT = 64 * 1024


def sha256(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def write_record(path, payload):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(payload, indent=2, allow_nan=False) + "\n")
    temporary.replace(path)


def load_panel(path, root):
    panel = json.loads(path.read_text())
    pairs = panel["pairs"]
    ids = [pair["id"] for pair in pairs]
    if len(pairs) != 36 or len(set(ids)) != 36:
        raise ValueError("the frozen panel must contain 36 distinct pairs")
    for pair in pairs:
        if not pair["id"] or any(c not in "abcdefghijklmnopqrstuvwxyz0123456789-" for c in pair["id"]):
            raise ValueError("unsafe pair id")
        for side in ("old", "new"):
            source = pair[side]
            url = urlsplit(source["url"])
            if url.scheme != "https" or not url.hostname or url.username or url.password:
                raise ValueError("input must have a credential-free HTTPS URL")
            path = (root / source["path"]).resolve()
            if not path.is_relative_to((root / "benchmark/realworld/cache").resolve()):
                raise ValueError("input path must remain in the benchmark cache")
            digest = source["sha256"]
            if len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
                raise ValueError("invalid input digest")
            if type(source["bytes"]) is not int or not 0 < source["bytes"] <= 100 * 1024 * 1024:
                raise ValueError("invalid input size")
    return pairs


def check_source(root, source):
    path = root / source["path"]
    if not path.is_file():
        return {"status": "missing_input", "expected_sha256": source["sha256"]}
    size, digest = path.stat().st_size, sha256(path)
    return {"status": "verified" if size == source["bytes"] and digest == source["sha256"] else "hash_mismatch",
            "bytes": size, "sha256": digest, "expected_sha256": source["sha256"]}


def fetch_source(root, source, timeout, retry=False):
    result = check_source(root, source)
    if result["status"] != "missing_input":
        return result
    path = root / source["path"]
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".download")
    previous = None
    # Existing mismatched inputs are evidence and are never overwritten.
    if temporary.exists():
        size, digest = temporary.stat().st_size, sha256(temporary)
        if size == source["bytes"] and digest == source["sha256"]:
            temporary.replace(path)
            return {"status": "verified", "bytes": size, "sha256": digest, "recovered_completed_download": True}
        if not retry:
            return {"status": "unfinished_download", "path": str(temporary), "bytes": size, "sha256": digest}
        retained = temporary.with_suffix(".download." + digest)
        if retained.exists():
            return {"status": "retained_download_exists", "path": str(retained), "sha256": digest}
        temporary.rename(retained)
        previous = {"path": str(retained), "bytes": size, "sha256": digest}
    started = time.monotonic()
    try:
        request = Request(source["url"], headers={"User-Agent": "pdfdelta-public-corpus/1"})
        with urlopen(request, timeout=min(timeout, 10)) as response, temporary.open("xb") as sink:
            size = 0
            while True:
                if time.monotonic() - started > timeout:
                    raise TimeoutError("download deadline exceeded")
                chunk = response.read(min(65536, source["bytes"] + 1 - size))
                if not chunk:
                    break
                sink.write(chunk)
                size += len(chunk)
                if size > source["bytes"]:
                    raise ValueError("response exceeds frozen input byte count")
            result = {"http_status": response.status, "resolved_url": response.url, "bytes": size}
        # Hash only after closing the buffered writer, including its final tail.
        result["sha256"] = sha256(temporary)
        if size != source["bytes"] or result["sha256"] != source["sha256"]:
            result["status"] = "hash_mismatch"
        else:
            temporary.replace(path)
            result["status"] = "verified"
    except (OSError, ValueError) as error:
        result = {"status": "download_failed", "error": str(error)}
    # Bounded failed bytes remain separate from checksum-verified PDFs.
    if temporary.exists():
        result.update(partial_path=str(temporary), partial_bytes=temporary.stat().st_size,
                      partial_sha256=sha256(temporary))
    if previous is not None:
        result["previous_download"] = previous
    return result


def current_cgroup():
    relative = next(line[3:] for line in Path("/proc/self/cgroup").read_text().splitlines() if line.startswith("0::"))
    return Path("/sys/fs/cgroup") / relative.lstrip("/")


def require_memory_guard(cgroup=None):
    cgroup = cgroup if cgroup is not None else current_cgroup()
    limit = (cgroup / "memory.max").read_text().strip()
    swap = (cgroup / "memory.swap.max").read_text().strip()
    if limit == "max" or not 0 < int(limit) <= MEMORY_LIMIT or swap != "0":
        raise ValueError("capture needs a shared memory.max <= 6000000000 and memory.swap.max = 0")
    return {"cgroup": str(cgroup), "memory_max": int(limit), "memory_swap_max": int(swap)}


def read_native_summary(path):
    """Consume the complete gzip/JSON stream while retaining only contract fields."""
    import ijson

    def events(stream):
        try:
            yield from ijson.parse(stream)
        except (ijson.common.JSONError, EOFError) as error:
            raise ValueError("invalid or truncated native report: " + str(error)) from error

    values = {}
    issues = Counter()
    fields = {"artifact_format", "artifact_version", "source_schema_version",
              "report.schema_version", "report.difference_status",
              "report.assessment", "report.assessment.candidates_truncated"}
    scopes = {"supported_text", "images_compared"}
    counts = {"established_changes", "content_changes", "proven_changed_regions",
              "formatting_only_changes", "uncertain_changes", "unresolved_regions",
              "tentative_candidates", "unsupported_extraction_issues", "unresolved_extraction_issues"}
    fields |= {"report.comparison_scope." + key for key in scopes}
    fields |= {"report.summary.comparison_scope." + key for key in scopes}
    fields |= {"report.summary." + key for key in counts | {"comparison_complete", "difference_status"}}
    fields |= {"report.extraction." + side + "_complete" for side in ("old", "new")}
    fields |= {f"report.summary.{side}_alignment_coverage.{key}"
               for side in ("old", "new") for key in ("total_tokens", "resolved_tokens", "ratio")}
    with gzip.open(path, "rb") as stream:
        for prefix, event, value in events(stream):
            if prefix in fields and event in ("string", "number", "boolean", "null", "start_map"):
                if prefix in values:
                    raise ValueError("duplicate contract field: " + prefix)
                values[prefix] = "object" if event == "start_map" else value
            if prefix == "report.extraction.issues.item.kind" and event == "string":
                issues[value] += 1
    def required(key):
        if key not in values:
            raise ValueError("missing contract field: " + key)
        return values[key]
    def boolean(key):
        value = required(key)
        if type(value) is not bool:
            raise ValueError("expected boolean: " + key)
        return value
    def count(key):
        value = required(key)
        if type(value) is not int or value < 0:
            raise ValueError("expected nonnegative integer: " + key)
        return value
    if (required("artifact_format"), required("artifact_version"), required("source_schema_version"),
            required("report.schema_version")) != ("pdfdelta-native-compact", 2, 11, 11):
        raise ValueError("unsupported native compact schema")
    for prefix in ("report.comparison_scope.", "report.summary.comparison_scope."):
        if not boolean(prefix + "supported_text") or boolean(prefix + "images_compared"):
            raise ValueError("capture requires the native text-only scope")
    summary = {key: count("report.summary." + key) for key in counts}
    status = required("report.difference_status")
    if status not in ("detected", "indeterminate", "no_content_change") or status != required("report.summary.difference_status"):
        raise ValueError("inconsistent difference status")
    summary["difference_status"] = status
    if summary["established_changes"] != summary["content_changes"]:
        raise ValueError("inconsistent established change count")
    if set(issues) - {"unsupported", "unresolved"}:
        raise ValueError("unknown extraction issue kind")
    for kind in ("unsupported", "unresolved"):
        if issues[kind] != summary[kind + "_extraction_issues"]:
            raise ValueError("inconsistent extraction issue count")
    resolved = True
    empty = False
    extraction_complete = True
    for side in ("old", "new"):
        complete = boolean("report.extraction." + side + "_complete")
        extraction_complete &= complete
        prefix = "report.summary." + side + "_alignment_coverage."
        total, done, ratio = count(prefix + "total_tokens"), count(prefix + "resolved_tokens"), required(prefix + "ratio")
        if done > total or (not complete and ratio is not None):
            raise ValueError("inconsistent extraction coverage")
        if complete and (isinstance(ratio, bool) or ratio is None or not math.isfinite(float(ratio))
                         or abs(float(ratio) - (done / total if total else 1)) > 1e-12):
            raise ValueError("inconsistent coverage ratio")
        resolved &= total == done
        empty |= total == 0
        summary[side + "_extraction_complete"] = complete
        summary[side + "_alignment_coverage"] = {"total_tokens": total, "resolved_tokens": done,
                                                    "ratio": None if ratio is None else float(ratio)}
    assessment = required("report.assessment")
    if assessment is None:
        if "report.assessment.candidates_truncated" in values:
            raise ValueError("null assessment has truncation metadata")
        truncated = False
    elif assessment == "object":
        truncated = boolean("report.assessment.candidates_truncated")
    else:
        raise ValueError("invalid assessment")
    complete = boolean("report.summary.comparison_complete")
    recomputed = (resolved and extraction_complete and not truncated
                  and all(summary[key] == 0 for key in ("tentative_candidates", "proven_changed_regions", "unresolved_regions")))
    if complete != recomputed or (complete and status == "indeterminate") or (not complete and status == "no_content_change"):
        raise ValueError("comparison completeness disagrees with contract fields")
    summary.update(comparison_complete=complete, empty_native_text=empty,
                   meaningful_complete=complete and not empty, candidates_truncated=truncated)
    return summary


def capture_pair(binary, root, pair, destination, timeout):
    sources = {side: check_source(root, pair[side]) for side in ("old", "new")}
    record = {"pair": pair["id"], "inputs": sources}
    if any(source["status"] != "verified" for source in sources.values()):
        return dict(record, status="input_unavailable")
    report = destination / (pair["id"] + ".json.gz")
    command = [str(binary), *(str(root / pair[side]["path"]) for side in ("old", "new")),
               "--native-text-only", "--limit-scale", "1", "--quiet", "--json", str(report)]
    log = destination / (pair["id"] + ".log")
    started = time.monotonic()
    with log.open("xb") as output:
        process = subprocess.Popen(command, stdout=output, stderr=output, start_new_session=True)
        try:
            while process.poll() is None:
                if time.monotonic() - started > timeout or log.stat().st_size > LOG_LIMIT or (report.exists() and report.stat().st_size > REPORT_BUDGET):
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
                    record["status"] = "timeout_or_output_limit"
                    break
                time.sleep(0.1)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
    record.update(command=command, exit_code=process.returncode, wall_seconds=round(time.monotonic() - started, 3))
    with log.open("rb") as source, gzip.open(str(log) + ".gz", "xb") as sink:
        while chunk := source.read(65536):
            sink.write(chunk)
    log.unlink()
    if "status" not in record:
        record["status"] = "captured" if process.returncode in (0, 1, 3) and report.is_file() else "execution_failed"
    if report.is_file():
        record.update(report=str(report), report_bytes=report.stat().st_size, report_sha256=sha256(report))
        if record["status"] == "captured":
            try:
                summary = read_native_summary(report)
                expected_exit = 3 if not summary["comparison_complete"] else (1 if summary["difference_status"] == "detected" else 0)
                if process.returncode != expected_exit:
                    raise ValueError("exit code disagrees with validated report")
                record["summary"] = summary
            except (OSError, ValueError, EOFError) as error:
                record.update(status="invalid_report", error=str(error))
    return record


def totals(rows):
    return {"fixed_denominator": 36, "outcomes": dict(Counter(row["status"] for row in rows)),
            "meaningful_complete": sum(row.get("summary", {}).get("meaningful_complete", False) for row in rows),
            "engine_complete": sum(row.get("summary", {}).get("comparison_complete", False) for row in rows),
            "incomplete_comparisons": sum("summary" in row and not row["summary"]["comparison_complete"] for row in rows),
            "empty_native_text": sum(row.get("summary", {}).get("empty_native_text", False) for row in rows)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("fetch", "check", "capture"))
    parser.add_argument("--panel", type=Path, default=DEFAULT_PANEL)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--pair", action="append")
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--retry-downloads", action="store_true", help="retain incomplete download bytes separately before retrying")
    args = parser.parse_args()
    root = args.root.resolve()
    pairs = load_panel(args.panel, root)
    selected = set(args.pair or [pair["id"] for pair in pairs])
    if not selected <= {pair["id"] for pair in pairs} or (args.pair and len(selected) != len(args.pair)):
        parser.error("pair selections must be unique frozen panel members")
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("timeout must be a positive finite number")
    guard = require_memory_guard() if args.action == "capture" else None
    args.output.mkdir(parents=True, exist_ok=False)
    record = {"started_utc": datetime.now(timezone.utc).isoformat(), "action": args.action,
              "panel_sha256": sha256(args.panel), "tool_sha256": sha256(__file__),
              "timeout_seconds": args.timeout, "selected_pairs": sorted(selected), "rows": []}
    (args.output / "capture-script.py").write_bytes(Path(__file__).read_bytes())
    (args.output / "panel.json").write_bytes(args.panel.read_bytes())
    if args.action == "capture":
        record["memory"] = guard
        if args.binary is None:
            parser.error("capture requires --binary")
        binary = args.binary.resolve()
        record.update(binary_sha256=sha256(binary), limit_scale=1, timeout_seconds=args.timeout,
                      output_budget_bytes=REPORT_BUDGET, capture_script_sha256=sha256(__file__))
        record["head"] = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
        record["tree"] = subprocess.check_output(["git", "rev-parse", "HEAD^{tree}"], cwd=root, text=True).strip()
        patch = subprocess.check_output(["git", "diff", "HEAD", "--", "crates", "Cargo.toml", "Cargo.lock"], cwd=root)
        (args.output / "production.patch").write_bytes(patch)
        record["production_patch_sha256"] = hashlib.sha256(patch).hexdigest()
        with binary.open("rb") as source, gzip.open(args.output / "pdfdelta.gz", "xb") as sink:
            while chunk := source.read(65536):
                sink.write(chunk)
    budget_exhausted = False
    for pair in sorted(pairs, key=lambda item: sum(item[side]["bytes"] for side in ("old", "new"))):
        if pair["id"] not in selected:
            row = {"pair": pair["id"], "status": "not_run"}
        elif args.action == "capture":
            if budget_exhausted:
                row = {"pair": pair["id"], "status": "output_budget_exhausted"}
            else:
                row = capture_pair(binary, root, pair, args.output, args.timeout)
                budget_exhausted = sum(path.stat().st_size for path in args.output.iterdir() if path.is_file()) >= REPORT_BUDGET
        else:
            inputs = {side: fetch_source(root, pair[side], args.timeout, args.retry_downloads)
                      if args.action == "fetch" else check_source(root, pair[side]) for side in ("old", "new")}
            row = {"pair": pair["id"], "inputs": inputs,
                   "status": "verified" if all(value["status"] == "verified" for value in inputs.values()) else "input_unavailable"}
        record["rows"].append(row)
        record["totals"] = totals(record["rows"])
        write_record(args.output / "summary.json", record)
        print(pair["id"], row["status"], row.get("summary", {}).get("meaningful_complete", ""), flush=True)
    if args.action == "capture":
        cgroup = Path(record["memory"]["cgroup"])
        record["memory"].update(events=(cgroup / "memory.events").read_text(),
                                  peak_bytes=int((cgroup / "memory.peak").read_text()))
    record["finished_utc"] = datetime.now(timezone.utc).isoformat()
    write_record(args.output / "summary.json", record)
    print(json.dumps(record["totals"], sort_keys=True))


if __name__ == "__main__":
    main()
