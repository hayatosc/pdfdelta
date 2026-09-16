#!/usr/bin/env python3
"""Run a bounded, hash-checked experiment; never upgrade completion in a report."""
import concurrent.futures
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time
import traceback

BASE = '7bf6b0ad646da55fbef6bbaa04c1664ffb773a4c'
ROOT = Path.cwd()
OUT = ROOT / '.experiment/runs/paint-locality-v1'
CACHE = ROOT / '.experiment/cache'
OUT.mkdir(parents=True, exist_ok=True)
CACHE.mkdir(parents=True, exist_ok=True)
STATE = {'base': BASE, 'iteration': 'paint-locality-v1', 'strict_contract_changed': False,
         'quality': {}, 'regression_before': {}, 'pairs': [], 'stage': 'started'}

def digest(path):
    with Path(path).open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()

def save():
    (OUT / 'results.json').write_text(json.dumps(STATE, indent=2) + '\n')

def command(name, argv, timeout=900):
    log = OUT / (name + '.log')
    begin = time.monotonic()
    with log.open('wb') as f:
        try:
            result = subprocess.run(argv, stdout=f, stderr=subprocess.STDOUT, timeout=timeout)
            code = result.returncode
        except subprocess.TimeoutExpired:
            code = 124
    record = {'command': argv, 'exit_code': code, 'seconds': round(time.monotonic()-begin, 3),
              'log': str(log.relative_to(ROOT)), 'sha256': digest(log)}
    print(name, code, flush=True)
    return record

def publish(message, source=False):
    subprocess.run(['git','config','user.name','pdfdelta experiment'], check=True)
    subprocess.run(['git','config','user.email','41898282+github-actions[bot]@users.noreply.github.com'], check=True)
    subprocess.run(['git','add','.experiment/runs'], check=True)
    if source:
        subprocess.run(['git','add',*STATE['tested_sources']], check=True)
    if subprocess.run(['git','diff','--cached','--quiet']).returncode:
        subprocess.run(['git','commit','-m',message+' [skip ci]'], check=True)
        subprocess.run(['git','push','origin','HEAD:work/strict-completion-20260916'], check=True)

def acquire(entry):
    pair, side = entry
    expected = pair[side]
    path = CACHE / 'inputs' / (pair['id']+'-'+side+'.pdf')
    path.parent.mkdir(parents=True, exist_ok=True)
    result = {'pair': pair['id'], 'side': side, 'expected_sha256': expected['sha256'], 'url': expected['url']}
    if path.exists() and digest(path) == expected['sha256']:
        result['status'] = 'verified_cache'
        return result
    temporary = path.with_suffix('.part')
    p = subprocess.run(['curl','--fail','--silent','--show-error','--location','--proto','=https','--proto-redir','=https','--max-redirs','5','--max-time','60','--max-filesize','104857600','--output',str(temporary),expected['url']], capture_output=True)
    result['exit_code'] = p.returncode
    if p.returncode == 0:
        result['actual_sha256'] = digest(temporary)
        result['bytes'] = temporary.stat().st_size
        valid = result['actual_sha256'] == expected['sha256'] and result['bytes'] == expected['bytes'] and temporary.open('rb').read(5) == b'%PDF-'
        result['status'] = 'verified_download' if valid else 'hash_or_size_mismatch'
        if valid:
            temporary.replace(path)
    else:
        result['status'] = 'download_failed'
        result['stderr'] = p.stderr.decode(errors='replace')[:1500]
    temporary.unlink(missing_ok=True)
    return result

def summarize(report):
    comparison = report['comparison']
    scopes = [s['result'] for s in comparison['scopes']]
    return {'comparison_complete': report['comparison_complete'], 'coverage': report['coverage'],
            'typed_changes': report['typed_changes'], 'inferred_changes': report['inferred_changes'],
            'scope_content_changes': report['scope_content_changes'],
            'native_intervals': sum(len(s.get('native_text_intervals',[])) for s in scopes),
            'native_domains': sum(len(s.get('native_text_domains',[])) for s in scopes),
            'inventory_issues': {side: report[side]['issues'] for side in ['old','new']},
            'comparison_sha256': hashlib.sha256(json.dumps(comparison,sort_keys=True).encode()).hexdigest()}

def export_file(path, stem):
    b = gzip.compress(path.read_bytes(), mtime=0)
    chunks = []
    for n, start in enumerate(range(0,len(b),900000)):
        target = OUT / (stem+'.gz.%03d'%n)
        target.write_bytes(b[start:start+900000])
        chunks.append({'path': str(target.relative_to(ROOT)), 'sha256': digest(target)})
    return {'uncompressed_sha256': digest(path), 'chunks': chunks}

def compare(pair, binaries, valid, review=False):
    row = {'pair': pair['id'], 'inputs': {s: pair[s]['sha256'] for s in ['old','new']}, 'runs': {}}
    if not all((pair['id'],s) in valid for s in ['old','new']):
        row['status'] = 'input_unavailable'
        return row
    paths = [CACHE/'inputs'/(pair['id']+'-'+s+'.pdf') for s in ['old','new']]
    for label, binary in binaries.items():
        dest = CACHE / 'comparisons' / label / pair['id']
        dest.mkdir(parents=True, exist_ok=True)
        report = dest / 'report.json'
        report.unlink(missing_ok=True)
        argv = [str(binary),*map(str,paths),'--channels','text','--limit-scale','1','--quiet','--json',str(report)]
        bundle = dest / 'review'
        if review:
            if bundle.exists(): shutil.rmtree(bundle)
            argv += ['--review',str(bundle)]
        start = time.monotonic()
        with (dest/'stdout').open('wb') as out, (dest/'stderr').open('wb') as err:
            p = subprocess.run(['timeout','--kill-after=5','180',*argv],stdout=out,stderr=err)
        record = {'command': argv, 'exit_code': p.returncode, 'seconds': round(time.monotonic()-start,3)}
        if p.returncode in [0,1,3] and report.exists():
            record.update(summarize(json.loads(report.read_text())))
            record['report_sha256'] = digest(report)
            if review:
                record['report_export'] = export_file(report, label+'-se-report')
                sources = bundle/'sources.json'
                if sources.exists(): record['sources_export'] = export_file(sources,label+'-se-sources')
        else:
            record['status'] = 'execution_failed'
            record['stderr'] = (dest/'stderr').read_text(errors='replace')[:2000]
        row['runs'][label] = record
    row['status'] = 'compared' if all('comparison_complete' in r for r in row['runs'].values()) else 'execution_failed'
    print('PAIR',pair['id'],row['status'],flush=True)
    return row

def main():
    panel = json.loads((ROOT/'benchmark/realworld/followup/panel.json').read_text())
    STATE['panel_sha256'] = digest(ROOT/'benchmark/realworld/followup/panel.json')
    STATE['registered_pair_count'] = len(panel['pairs'])
    STATE['toolchain'] = subprocess.check_output(['rustc','--version'],text=True).strip()
    save()
    core = [Path('crates/pdfdelta-core/src/source/content_stream.rs'),Path('crates/pdfdelta-core/src/source/content_stream/paint_bounds.rs')]
    tests = Path('crates/pdfdelta-core/tests/content_stream_extraction.rs')
    extra = Path('.experiment/paint_locality_tests.rs').read_text()
    if 'fn disconnected_pdf_rules_do_not_acquire_joins_between_subpaths' not in tests.read_text():
        tests.write_text(tests.read_text()+extra)
    patch = Path('.experiment/patches/paint-locality.patch')
    patch.write_text(patch.read_text().replace('\n diff --git','\ndiff --git'))
    if 'intersect_paint_bounds' not in core[0].read_text():
        subprocess.run(['git','apply','--check',str(patch)],check=True)
        subprocess.run(['git','apply',str(patch)],check=True)
    cache = Path('crates/pdfdelta-cli/src/extraction_cache.rs')
    cache.write_text(cache.read_text().replace('CACHE_FORMAT_VERSION: u32 = 17;', 'CACHE_FORMAT_VERSION: u32 = 18;'))
    changed = core+[tests,cache]
    saved = {p:p.read_bytes() for p in core}
    try:
        for p in core:
            p.write_bytes(subprocess.check_output(['git','show',BASE+':'+str(p)]))
        for name in ['disconnected_pdf_rules_do_not_acquire_joins_between_subpaths','small_curve_in_large_form_retains_local_bounds_and_native_text','failed_nested_pdf_form_keeps_outer_bounds_and_the_extraction_gap']:
            STATE['regression_before'][name] = command('before-'+name,['cargo','test','--locked','-p','pdfdelta-core','--test','content_stream_extraction',name,'--','--exact'],timeout=900)
    finally:
        for p,b in saved.items(): p.write_bytes(b)
    STATE['stage'] = 'quality_checks'; save()
    STATE['quality']['format_apply'] = command('format-apply',['cargo','fmt','--all'])
    for name,argv in [('fmt',['cargo','fmt','--all','--','--check']),('clippy',['cargo','clippy','--workspace','--all-targets','--locked','--','-D','warnings']),('tests',['cargo','test','--workspace','--locked']),('verify',['cargo','run','-p','pdfdelta-bench','--locked','--','verify']),('release',['cargo','build','--release','--locked','-p','pdfdelta-cli'])]:
        STATE['quality'][name] = command(name,argv)
        save()
        if STATE['quality'][name]['exit_code'] != 0:
            STATE['stage'] = 'quality_failed'; save(); return
    STATE['tested_sources'] = {str(p):digest(p) for p in changed}
    test_log = (OUT/'tests.log').read_text()
    summaries = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', test_log)
    STATE['test_totals'] = {k:sum(int(x[n]) for x in summaries) for n,k in enumerate(['passed','failed','ignored'])}
    STATE['stage'] = 'quality_passed'; save()
    publish('fix(core): preserve conservative paint locality and invalidate stale extraction cache',source=True)
    STATE['tested_commit'] = subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
    baseline = CACHE/'pdfdelta-baseline'
    baseline.write_bytes(gzip.decompress(Path('.experiment/transport/pdfdelta-baseline.gz').read_bytes())); baseline.chmod(0o755)
    binaries = {'baseline': baseline.resolve(),'candidate': (ROOT/'target/release/pdfdelta').resolve()}
    STATE['binaries'] = {k:digest(v) for k,v in binaries.items()}
    STATE['stage'] = 'acquiring_fixed_inputs'; save()
    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as executor:
        acquisition = list(executor.map(acquire,[(p,s) for p in panel['pairs'] for s in ['old','new']]))
    STATE['acquisition'] = acquisition
    valid = {(r['pair'],r['side']) for r in acquisition if r['status'].startswith('verified_')}
    pilot_ids = ['irs-schedule-se-2024-to-2025','irs-schedule-c-2024-to-2025','irs-w9-2018-to-2024','edpb-controller-processor-v1-to-v2-1','nist-ssdf-draft-to-final','arxiv-llama2-v1-to-v2']
    ordered = sorted(panel['pairs'],key=lambda p: (pilot_ids.index(p['id']) if p['id'] in pilot_ids else 100))
    STATE['stage'] = 'natural_comparisons'; save()
    for number,pair in enumerate(ordered,1):
        row = compare(pair,binaries,valid,review=pair['id']==pilot_ids[0])
        STATE['pairs'].append(row)
        STATE['strict_complete'] = {label:sum(r['runs'].get(label,{}).get('comparison_complete') is True for r in STATE['pairs']) for label in binaries}
        save()
        if number == len(pilot_ids): publish('test(experiment): publish measured six-pair paint-locality pilot')
    STATE['stage'] = 'finished'; save()
    publish('test(experiment): publish fixed-panel paint-locality comparison results')

if __name__ == '__main__':
    try:
        main()
    except Exception:
        STATE['stage'] = 'driver_failed'
        STATE['exception'] = traceback.format_exc()
        save()
        raise
