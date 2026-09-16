#!/usr/bin/env python3
"""Validate compound envelopes and live interval exit status on frozen inputs."""
import concurrent.futures
import gzip
import json
from pathlib import Path
import re
import subprocess
import traceback
import iterate as run

run.OUT = run.ROOT / '.experiment/runs/compound-v1'
run.OUT.mkdir(parents=True, exist_ok=True)
run.STATE = {'base': run.BASE, 'iteration': 'compound-v1', 'strict_contract_changed': False,
             'quality': {}, 'regression_before': {}, 'pairs': [], 'stage': 'started'}
S = run.STATE
P = Path('.experiment/compound')

def append_once(target, source, marker):
    path = Path(target)
    if marker not in path.read_text():
        path.write_text(path.read_text() + (P/source).read_text())

def checked_replace(path, before, after):
    path = Path(path)
    text = path.read_text()
    if after in text:
        return
    if text.count(before) != 1:
        raise ValueError('unexpected source while updating '+str(path))
    path.write_text(text.replace(before,after))

def compact_previous():
    source = Path('.experiment/runs/paint-locality-v2/results.json')
    data = json.loads(source.read_text())
    S['previous_full_results'] = run.export_file(source,'previous-a2-results')
    for row in data.get('pairs',[]):
        for result in row.get('runs',{}).values():
            issues = result.pop('inventory_issues',{})
            result['inventory_issue_counts'] = {side:len(items) for side,items in issues.items()}
    (run.OUT/'previous-a2-summary.json').write_text(json.dumps(data,indent=2)+'\n')
    run.save()
    run.publish('test(experiment): retain compact measured locality ablation')

def prepare():
    append_once('crates/pdfdelta-core/tests/content_stream_extraction.rs','extraction_tests.rs','fn compound_path_bounds_preserve_subpath_gaps_for_every_fill_rule')
    append_once('crates/pdfdelta-cli/tests/comparison_cli.rs','cli_tests.rs','fn selected_text_exit_code_counts_live_owned_interval_changes_without_relabeling_reviews')
    for name, package, target in [
        ('compound_path_bounds_preserve_subpath_gaps_for_every_fill_rule','pdfdelta-core','content_stream_extraction'),
        ('selected_text_exit_code_counts_live_owned_interval_changes_without_relabeling_reviews','pdfdelta-cli','comparison_cli'),
        ('selected_text_compound_paint_preserves_gaps_but_never_certifies_inventory','pdfdelta-cli','comparison_cli')]:
        S['regression_before'][name] = run.command('before-'+name,['cargo','test','--locked','-p',package,'--test',target,name,'--','--exact'])
        run.save()
        if S['regression_before'][name]['exit_code'] != 101:
            raise RuntimeError('regression did not reproduce against the locality-only revision: '+name)
    for filename in ['components.patch','exit.patch']:
        patch = str(P/filename)
        subprocess.run(['git','apply','--check',patch],check=True)
        subprocess.run(['git','apply',patch],check=True)
    path = Path('crates/pdfdelta-core/tests/content_stream_extraction.rs')
    text = path.read_text()
    start = text.index('fn disconnected_pdf_rules_do_not_acquire_joins_between_subpaths')
    end = text.index('\n#[test]',start)
    part = text[start:end]
    assert 'assert_eq!(paints.len(), 1);' in part
    part = part.replace('assert_eq!(paints.len(), 1);','assert_eq!(paints.len(), 2);')
    text = text[:start]+part+text[end:]
    start = text.index('fn joined_pdf_rules_and_device_dependent_widths_remain_conservative')
    end = text.index('\n#[test]',start)
    part = text[start:end]
    assert 'assert_eq!(paints.len(), 1);' in part
    part = part.replace('assert_eq!(paints.len(), 1);','assert_eq!(paints.len(), if bounded { 2 } else { 1 });')
    path.write_text(text[:start]+part+text[end:])
    append_once('crates/pdfdelta-core/tests/document_text_scopes_fixture.rs','interval_tests.rs','fn native_interval_change_authority_does_not_survive_report_deserialization')
    checked_replace('crates/pdfdelta-cli/src/extraction_cache.rs','CACHE_FORMAT_VERSION: u32 = 18;','CACHE_FORMAT_VERSION: u32 = 19;')
    checked_replace('crates/pdfdelta-cli/src/native_worker.rs','content-stream-v13-type3-code-gaps-worker-v1','content-stream-v14-compound-paint-worker-v1')
    checked_replace('crates/pdfdelta-core/src/model.rs',
        '/// An opaque or non-text painting operation with a conservative bound in native page\n/// coordinates. An unknown bound remains an obstruction to local text closure;',
        '/// A conservative enclosure of an opaque or non-text painting effect in native\n/// page coordinates. One operation may have several subpath enclosures sharing\n/// its provenance; their union bounds the effect, without asserting disjointness\n/// or independent content. An unknown bound obstructs local text closure;')

def quality():
    run.command('format-apply',['cargo','fmt','--all'])
    for name,argv in [('fmt',['cargo','fmt','--all','--','--check']),
        ('clippy',['cargo','clippy','--workspace','--all-targets','--locked','--','-D','warnings']),
        ('tests',['cargo','test','--workspace','--locked']),
        ('verify',['cargo','run','-p','pdfdelta-bench','--locked','--','verify']),
        ('release',['cargo','build','--release','--locked','-p','pdfdelta-cli'])]:
        S['quality'][name] = run.command(name,argv)
        run.save()
        if S['quality'][name]['exit_code'] != 0:
            raise RuntimeError('quality gate failed: '+name)
    summaries = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', (run.OUT/'tests.log').read_text())
    S['test_totals'] = {key:sum(int(row[n]) for row in summaries) for n,key in enumerate(['passed','failed','ignored'])}
    paths = subprocess.check_output(['git','diff','--name-only',run.BASE,'--','crates'],text=True).splitlines()
    S['tested_sources'] = {p:run.digest(Path(p)) for p in paths}
    binary = run.ROOT/'target/release/pdfdelta'
    S['candidate_binary'] = run.export_file(binary,'pdfdelta-candidate')
    S['stage'] = 'quality_passed'
    run.save()
    run.publish('fix(core): preserve compound paint gaps and honor live interval changes',source=True)
    S['tested_commit'] = subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()

def measure():
    panel_path = Path('benchmark/realworld/followup/panel.json')
    panel = json.loads(panel_path.read_text())['pairs']
    S['panel_sha256'] = run.digest(panel_path)
    S['registered_pair_count'] = len(panel)
    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as executor:
        acquisition = list(executor.map(run.acquire,[(pair,side) for pair in panel for side in ['old','new']]))
    S['acquisition'] = acquisition
    valid = {(r['pair'],r['side']) for r in acquisition if r['status'].startswith('verified_')}
    baseline = run.CACHE/'pdfdelta-baseline'
    baseline.write_bytes(gzip.decompress(Path('.experiment/transport/pdfdelta-baseline.gz').read_bytes()))
    baseline.chmod(0o755)
    binaries = {'baseline':baseline.resolve(),'candidate':(run.ROOT/'target/release/pdfdelta').resolve()}
    S['binaries'] = {key:run.digest(value) for key,value in binaries.items()}
    S['stage'] = 'natural_comparisons'
    run.save()
    priority = ['irs-schedule-se-2024-to-2025','irs-schedule-c-2024-to-2025','irs-w9-2018-to-2024','edpb-controller-processor-v1-to-v2-1','nist-ssdf-draft-to-final','arxiv-llama2-v1-to-v2']
    panel.sort(key=lambda p:priority.index(p['id']) if p['id'] in priority else 100)
    def compare(pair):
        row = run.compare(pair,binaries,valid,review=pair['id']==priority[0])
        for label, record in row['runs'].items():
            issues = record.pop('inventory_issues',{})
            record['inventory_issue_counts'] = {side:len(items) for side,items in issues.items()}
            record['inventory_issue_samples'] = {side:items[:3] for side,items in issues.items()}
        return row
    for start,stop in [(0,6),(6,len(panel))]:
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
            for row in executor.map(compare,panel[start:stop]):
                S['pairs'].append(row)
                S['strict_complete'] = {label:sum(r['runs'].get(label,{}).get('comparison_complete') is True for r in S['pairs']) for label in binaries}
                S['evaluated_pair_count'] = sum(r['status']=='compared' for r in S['pairs'])
                run.save()
        run.publish('test(experiment): retain measured compound-paint panel progress')
    S['stage'] = 'finished'
    run.save()
    run.publish('test(experiment): retain full fixed-panel compound-paint results')

if __name__=='__main__':
    try:
        S['toolchain'] = subprocess.check_output(['rustc','--version'],text=True).strip()
        compact_previous()
        prepare()
        quality()
        measure()
    except Exception:
        S['stage']='failed'
        S['exception']=traceback.format_exc()
        run.save()
        raise
