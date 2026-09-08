import concurrent.futures
import csv
import json
from pathlib import Path
import subprocess
import sys
import time

binary, destination = sys.argv[1:]
out = Path(destination)
out.mkdir(parents=True, exist_ok=True)
with open('benchmark/realworld/manifest.tsv') as source:
    rows = list(csv.DictReader((line for line in source if not line.startswith('#')), delimiter='\t'))
rows.sort(key=lambda row: row['expected_file'] == '-')

def run(row):
    pair = row['pair_id']
    timeout = 900 if row['expected_file'] != '-' else 180
    command = [binary, 'revisions', '--cache-dir', 'benchmark/realworld/cache', '--pair', pair,
               '--evaluation-json-output', str(out / (pair + '.evaluation.json')),
               '--summary-json-output', str(out / (pair + '.summary.json'))]
    start = time.monotonic()
    with (out / (pair + '.log')).open('w') as log:
        try:
            result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, timeout=timeout)
            status = {'exit_code': result.returncode}
        except subprocess.TimeoutExpired:
            status = {'status': 'external_timeout'}
    record = {'pair': pair, 'set': row['set'], 'annotation': row['annotation_scope'],
              'tuning_use': row['tuning_use'], 'seconds': round(time.monotonic() - start, 3),
              'timeout_seconds': timeout, 'command': command, **status}
    (out / (pair + '.process.json')).write_text(json.dumps(record, indent=2) + '\n')
    print(pair, status, flush=True)

with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
    list(pool.map(run, rows))
