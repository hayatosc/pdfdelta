#!/usr/bin/env python3
"""Capture the fixed natural panel without changing its population or contracts."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import urllib.parse
import urllib.request

MAX_BYTES = 100 * 1024 * 1024
BASE = '7bf6b0ad646da55fbef6bbaa04c1664ffb773a4c'
PANEL = Path('benchmark/realworld/remaining/source-completion/tagged-row-regions-v1-panel.json')
DRIVER = Path('benchmark/realworld/next/development/capture-comparisons.py')


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


class HttpsRedirect(urllib.request.HTTPRedirectHandler):
    max_redirections = 5
    max_repeats = 2
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urllib.parse.urlsplit(newurl).scheme != 'https':
            raise ValueError('non-HTTPS redirect')
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def acquire(record, path):
    wanted = record['sha256']
    if path.is_file() and digest(path) == wanted and path.stat().st_size == record['bytes']:
        return {'status': 'verified_cache', 'sha256': wanted, 'bytes': path.stat().st_size}
    url = record['url']
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme != 'https' or parsed.username or parsed.password:
        return {'status': 'refused_url'}
    errors = []
    opener = urllib.request.build_opener(HttpsRedirect())
    for attempt in range(2):
        temp = None
        try:
            request = urllib.request.Request(url, headers={'User-Agent': 'pdfdelta-reproducible-benchmark/1.0'})
            with opener.open(request, timeout=45) as response, tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as out:
                temp = Path(out.name)
                total = 0
                while True:
                    chunk = response.read(min(1024 * 1024, MAX_BYTES + 1 - total))
                    if not chunk:
                        break
                    total += len(chunk)
                    if total > MAX_BYTES:
                        raise ValueError('PDF exceeds acquisition byte limit')
                    out.write(chunk)
            actual = digest(temp)
            if actual != wanted or total != record['bytes']:
                raise ValueError(f'frozen input mismatch: sha256={actual}, bytes={total}')
            with temp.open('rb') as stream:
                if stream.read(5) != b'%PDF-':
                    raise ValueError('not a PDF header')
            temp.replace(path)
            return {'status': 'downloaded_verified', 'sha256': actual, 'bytes': total}
        except Exception as error:
            errors.append(f'{type(error).__name__}: {error}')
        finally:
            if temp is not None and temp.exists():
                temp.unlink()
    return {'status': 'acquisition_failed', 'errors': errors}


def hash_value(value):
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def summarize(path):
    if path.stat().st_size > 256 * 1024 * 1024:
        return {'summary_status': 'report_exceeds_summary_budget'}
    report = json.loads(path.read_text())
    comparison = report.get('comparison', {})
    scopes = comparison.get('scopes', [])
    data = {'summary_status': 'available', 'schema_version': report.get('schema_version'),
            'comparison_complete': report.get('comparison_complete'),
            'coverage': report.get('coverage'), 'comparison_sha256': hash_value(comparison),
            'comparison_wall_time_ms': report.get('comparison_wall_time_ms'),
            'search_resolved': not comparison.get('relation_unresolved') and all(
                not scope['result'].get('unresolved') and not scope['result'].get('structural_correspondences')
                for scope in scopes), 'scope_count': len(scopes)}
    for key in ('typed_changes', 'inferred_changes', 'scope_content_changes', 'inferred_scope_changes'):
        value = report.get(key)
        data[key] = value if isinstance(value, (int, bool, type(None))) else {'count': len(value), 'sha256': hash_value(value)}
    for side in ('old', 'new'):
        evidence = report.get(side, {})
        data[side] = {key: evidence.get(key) for key in ('revision', 'pages', 'native_glyphs', 'rendered_regions', 'structured_elements', 'non_text_paint_pages', 'inventories', 'issues')}
    for name in ('native_text_intervals', 'native_text_domains', 'text_scope_reviews'):
        values = []
        for scope in scopes:
            for original in scope['result'].get(name, []):
                item = dict(original)
                item.pop('boundaries', None)
                item.pop('proposal', None)
                values.append(hash_value(item))
        data[name] = {'count': len(values), 'fingerprints': sorted(values)}
    return data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--label', required=True)
    parser.add_argument('--shard', type=int, required=True)
    parser.add_argument('--shards', type=int, default=6)
    args = parser.parse_args()
    frozen = json.loads(PANEL.read_text())['rows']
    assert len(frozen) == 36 and len({row['pair'] for row in frozen}) == 36
    cache = Path('.experiment/pdf-cache'); cache.mkdir(parents=True, exist_ok=True)
    raw = Path('.experiment/raw') / args.label / str(args.shard); raw.mkdir(parents=True, exist_ok=True)
    output = Path('.experiment/panel') / args.label / f'shard-{args.shard}.json'
    output.parent.mkdir(parents=True, exist_ok=True)
    result = {'base': BASE, 'implementation_label': args.label, 'binary_sha256': digest(args.binary),
              'panel_definition_sha256': digest(PANEL), 'driver_sha256': digest(DRIVER),
              'population_size': 36, 'shard': args.shard, 'shards': args.shards,
              'route': 'text', 'limit_scale': 1, 'timeout_seconds': 180,
              'annotation_scoring': False, 'rows': []}
    selected = []
    for index, row in enumerate(frozen):
        if index % args.shards != args.shard:
            continue
        command = row['command']
        manifest = Path(command[command.index('--manifest') + 1])
        pairs = {pair['id']: pair for pair in json.loads(manifest.read_text())['pairs']}
        selected.append((index, row['pair'], manifest, pairs[row['pair']]))
    selected.sort(key=lambda item: sum(item[3][side]['bytes'] for side in ('old', 'new')))
    for index, pair_id, manifest, pair in selected:
        record = {'panel_index': index, 'pair': pair_id, 'manifest': str(manifest), 'manifest_sha256': digest(manifest)}
        record['acquisition'] = {side: acquire(pair[side], cache / f'{pair_id}-{side}.pdf') for side in ('old', 'new')}
        directory = raw / pair_id
        command = ['python3', str(DRIVER), str(args.binary), str(cache), str(directory), '--manifest', str(manifest), '--pair', pair_id, '--implementation', args.label, '--route', 'text']
        execution = subprocess.run(command, capture_output=True, text=True, timeout=200, check=False)
        record['driver_exit'] = execution.returncode
        record['driver_stderr'] = execution.stderr[-4096:]
        runs = directory / 'runs.json'
        if runs.exists():
            record['run'] = json.loads(runs.read_text())['runs'][0]
        report = directory / f'{pair_id}-text.json'
        if report.exists():
            try:
                record['result'] = summarize(report)
            except Exception as error:
                record['summary_error'] = f'{type(error).__name__}: {error}'
        result['rows'].append(record)
        output.write_text(json.dumps(result, indent=2, ensure_ascii=False) + '\n')
        print(pair_id, record.get('run', {}).get('status'), record.get('result', {}).get('comparison_complete'), flush=True)


if __name__ == '__main__':
    main()
