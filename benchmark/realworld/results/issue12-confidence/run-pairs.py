"""Capture every manifest pair, including failures, without replacing captures.

Run from the repository root with BINARY and an empty output directory.
Resource limits and benchmark options use the binary's defaults.
"""
import concurrent.futures
import csv
import hashlib
import json
import pathlib
import subprocess
import sys

binary = pathlib.Path(sys.argv[1]).resolve()
output = pathlib.Path(sys.argv[2]).resolve()
output.mkdir(parents=True, exist_ok=True)
if any(output.iterdir()):
    raise SystemExit(f"Refusing nonempty output directory: {output}")
pairs = list(csv.DictReader(
    (line for line in pathlib.Path('benchmark/realworld/manifest.tsv').read_text().splitlines()
     if line and not line.startswith('#')), delimiter='\t'))
(output / 'run.json').write_text(json.dumps({
    'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
    'pair_count': len(pairs),
    'command': ['BINARY', 'revisions', '--cache-dir', 'benchmark/realworld/cache',
                '--pair', 'PAIR', '--summary-json-output', 'OUTPUT/PAIR.json'],
}, indent=2) + '\n')


def run(row):
    pair = row['pair_id']
    destination = output / f'{pair}.json'
    with (output / f'{pair}.log').open('w') as log:
        result = subprocess.run([
            str(binary), 'revisions', '--cache-dir', 'benchmark/realworld/cache',
            '--pair', pair, '--summary-json-output', str(destination)
        ], stdout=log, stderr=subprocess.STDOUT)
    if not destination.exists():
        raise RuntimeError(f'{pair}: exit {result.returncode}, no capture')
    capture = json.loads(destination.read_text())
    print(pair, result.returncode, flush=True)
    return pair, capture


captures = {}
with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
    for future in concurrent.futures.as_completed([pool.submit(run, row) for row in pairs]):
        pair, capture = future.result()
        captures[pair] = capture
        (output / 'all.json').write_text(json.dumps(captures, indent=2, sort_keys=True) + '\n')
assert len(captures) == len(pairs)
