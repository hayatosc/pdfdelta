#!/usr/bin/env python3
"""Compact a native benchmark capture, preserving provenance before pruning."""

import argparse
import gzip
import hashlib
import json
import os
import re
from pathlib import Path
import tempfile

from compact_native import compact_report


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def publish_json(path, value):
    with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(value, stream, indent=2)
        stream.write('\n')
    try:
        os.link(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def compact_capture(summary_path, destination, resume=False):
    summary_path = summary_path.resolve(strict=True)
    source_root = summary_path.parent
    summary = json.loads(summary_path.read_text())
    rows = summary['rows']
    if summary.get('route') != 'native' or len({row['pair'] for row in rows}) != len(rows):
        raise ValueError('requires a native capture with unique pairs')
    # Resolve every path before processing. A capture cannot claim unrelated files.
    reports = []
    for row in rows:
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', row['pair']):
            raise ValueError('invalid pair ID')
        if 'report' not in row:
            continue
        original_path = Path(row['report']['path'])
        source = original_path.resolve(strict=True)
        if not source.is_relative_to(source_root) or original_path.is_symlink():
            raise ValueError(f'report outside capture: {source}')
        reports.append((row, source))
    destination.mkdir(parents=True, exist_ok=resume)
    if (destination / "manifest.json").exists():
        raise ValueError("capture already compacted")
    manifest = {
        'format': 'pdfdelta-native-capture-compaction',
        'version': 1,
        'source_summary': {'path': str(summary_path), 'sha256': digest(summary_path)},
        'policy': 'Keep every field except per-glyph sources; retain source summaries and rerun provenance.',
        'tool_sha256': {name: digest(Path(__file__).with_name(name))
                        for name in ('compact_capture.py', 'compact_native.py')},
        'reports': [],
    }
    for row, source in reports:
        expected = row['report']
        if digest(source) != expected['file_sha256']:
            raise ValueError(f'original file checksum mismatch: {source}')
        output = destination / f"{row['pair']}.compact.json.gz"
        reused = resume and output.is_file() and not output.is_symlink()
        if reused:
            with gzip.open(output, 'rt') as stream:
                existing = json.load(stream)
            if existing.get('artifact_format') != 'pdfdelta-native-compact':
                raise ValueError('existing output is not a compact native artifact')
            metadata = existing['source'].copy()
            metadata.update(output_bytes=output.stat().st_size, output_sha256=digest(output))
        else:
            metadata = compact_report(source, output)
        if metadata['source_logical_sha256'] != expected['sha256']:
            raise ValueError(f'original logical checksum mismatch: {source}')
        if metadata['source_logical_bytes'] != expected['logical_bytes']:
            raise ValueError(f'original logical size mismatch: {source}')
        if metadata['source_file_sha256'] != expected['file_sha256']:
            raise ValueError(f'original file changed during conversion: {source}')
        # The compact artifact is read independently before the full report can be pruned.
        with gzip.open(output, 'rt') as stream:
            artifact = json.load(stream)
        report = artifact['report']
        for key, value in report['summary'].items():
            if key in row and row[key] != value:
                raise ValueError(f'summary field changed: {source}: {key}')
        if report.get('schema_version') != 11:
            raise ValueError('compact projection has wrong native schema')
        for side in ('old', 'new'):
            key = f'{side}_extraction_complete'
            if key in row and row[key] != report['extraction'][f'{side}_complete']:
                raise ValueError(f'extraction status changed: {source}')
        assessment = report['assessment']
        if 'assessment_null' in row and row['assessment_null'] != (assessment is None):
            raise ValueError(f'assessment presence changed: {source}')
        if assessment is not None and row.get('candidates_truncated') != assessment['candidates_truncated']:
            raise ValueError(f'assessment truncation changed: {source}')
        if report['comparison_scope'] != {'supported_text': True, 'images_compared': False}:
            raise ValueError(f'comparison scope changed: {source}')
        if report['difference_status'] != row['difference_status']:
            raise ValueError(f'difference status changed: {source}')
        manifest['reports'].append({
            'pair': row['pair'], 'original': expected,
            'compact': {'path': str(output.resolve()), 'sha256': digest(output), 'bytes': output.stat().st_size},
            'conversion': metadata, 'reused_existing_output': reused,
        })
        print(f"{row['pair']}: {source.stat().st_size} -> {output.stat().st_size} bytes", flush=True)
    manifest['original_bytes'] = sum(item['original']['bytes'] for item in manifest['reports'])
    manifest['compact_bytes'] = sum(item['compact']['bytes'] for item in manifest['reports'])
    publish_json(destination / 'manifest.json', manifest)
    return manifest


def prune(manifest_path):
    manifest = json.loads(manifest_path.read_text())
    if manifest.get('format') != 'pdfdelta-native-capture-compaction' or manifest.get('version') != 1:
        raise ValueError('unsupported compaction manifest')
    summary = manifest['source_summary']
    summary_path = Path(summary['path']).resolve(strict=True)
    if digest(summary_path) != summary['sha256']:
        raise ValueError('source summary changed')
    original_rows = {row['pair']: row.get('report') for row in json.loads(summary_path.read_text())['rows']}
    intent = manifest_path.with_name('prune-intent.json')
    manifest_hash = digest(manifest_path)
    resuming = intent.exists()
    if resuming and json.loads(intent.read_text())['manifest_sha256'] != manifest_hash:
        raise ValueError('prune intent belongs to another manifest')
    originals = []
    seen = set()
    # Complete all checks before removing any full report; interrupted pruning is resumable.
    for entry in manifest['reports']:
        if entry['pair'] in seen or original_rows.get(entry['pair']) != entry['original']:
            raise ValueError('manifest does not match capture summary')
        seen.add(entry['pair'])
        output = Path(entry['compact']['path']).resolve(strict=True)
        if not output.is_relative_to(manifest_path.resolve().parent):
            raise ValueError('compact report outside manifest directory')
        if digest(output) != entry['compact']['sha256']:
            raise ValueError(f'compact checksum mismatch: {output}')
        source = Path(entry['original']['path'])
        if source.is_symlink() or not source.resolve().is_relative_to(summary_path.parent):
            raise ValueError('original report outside capture')
        if not source.exists() and not resuming:
            raise ValueError(f'original missing before pruning: {source}')
        if source.exists() and digest(source) != entry['original']['file_sha256']:
            raise ValueError(f'original checksum mismatch: {source}')
        originals.append(source)
    if seen != {pair for pair, report in original_rows.items() if report is not None}:
        raise ValueError('compaction manifest does not cover every original report')
    if not resuming:
        publish_json(intent, {'manifest_sha256': manifest_hash})
    for source in originals:
        source.unlink(missing_ok=True)
    receipt = manifest_path.with_name('pruned.json')
    if not receipt.exists():
        publish_json(receipt, {'manifest_sha256': digest(manifest_path), 'removed_reports': len(originals)})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    convert = sub.add_parser('convert')
    convert.add_argument('summary', type=Path)
    convert.add_argument('destination', type=Path)
    convert.add_argument('--resume', action='store_true', help='verify and reuse completed compact artifacts after an interrupted conversion')
    remove = sub.add_parser('prune')
    remove.add_argument('manifest', type=Path)
    args = parser.parse_args()
    if args.command == 'convert':
        compact_capture(args.summary, args.destination, args.resume)
    else:
        prune(args.manifest)


if __name__ == '__main__':
    main()
