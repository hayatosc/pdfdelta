import collections
import csv
import json
from pathlib import Path

root = Path('benchmark/realworld/results/issue20-order-experiment')
trial = root / 'local-trial'
baseline = root / 'baseline-fixed'
with open('benchmark/realworld/manifest.tsv') as source:
    rows = list(csv.DictReader((line for line in source if not line.startswith('#')), delimiter='\t'))
base_annotations = {record['pair']: record for record in json.loads((baseline / 'annotated-summary.json').read_text())['records']}

def read(path):
    return json.loads(path.read_text())

def fp(record):
    tokens = record['scoped_tokens']
    return None if tokens is None else tokens['reported_changed_tokens'] - tokens['true_positive_tokens']

records = []
expectations = []
for row in rows:
    pair = row['pair_id']
    process = read(trial / (pair + '.process.json'))
    before = read(baseline / (pair + '.evaluation.json'))['records'][0]
    eval_path = trial / (pair + '.evaluation.json')
    after = read(eval_path)['records'][0] if eval_path.exists() else None
    result = {'pair': pair, 'split': row['set'], 'annotated': row['expected_file'] != '-',
              'baseline_status': before['trial_status'],
              'trial_status': after['trial_status'] if after else process.get('status', 'process_failure'),
              'trial_process': process}
    for label, record in [('baseline', before), ('trial', after)]:
        result[label] = None if record is None else {
            'matched': record['quality']['matched_changes'],
            'expected': record['quality']['expected_changes'],
            'scoped_fp_tokens': fp(record),
            'accepted_changes': record['accepted_changes'],
            'candidate_changes': record['candidate_changes'],
            'coverage_comparison': record['coverage_comparison'],
            'comparison_complete': record['comparison_complete'],
            'quality_skipped_reason': record['quality_skipped_reason'],
            'unmatched_tiny_changes': record['quality']['unmatched_tiny_changes'],
        }
    if row['expected_file'] != '-':
        expected = read(Path('benchmark/realworld') / row['expected_file'])['changes']
        measurable = after is not None and after['quality']['matched_changes'] is not None
        matched = set()
        failures = {}
        if measurable:
            summary = read(trial / (pair + '.summary.json'))['records'][0]
            diagnostic = summary['expected_change_diagnostics']
            if diagnostic:
                failures = {item['expected_id']: item for item in diagnostic['failures']}
            if after['quality']['matched_changes']:
                if pair == 'w3c-ws-policy-attach-20060927-to-20061102':
                    matched = {item['expected_id'] for item in read(trial / 'w3c-matched-id.json')['matched']}
                elif diagnostic and diagnostic['complete']:
                    matched = {item['id'] for item in expected} - set(failures)
            assert len(matched) == after['quality']['matched_changes'], pair
        before_by_id = {item['expected_id']: item['matched'] for item in base_annotations[pair]['expectations']}
        for item in expected:
            name = item['id']
            previous = before_by_id[name]
            current = name in matched if measurable else None
            transition = ('unmeasurable' if current is None else 'newly_measurable' if previous is None
                          else 'gained' if current and not previous else 'lost' if previous and not current
                          else 'retained_match' if current else 'still_unmatched')
            expectations.append({'pair': pair, 'expected_id': name, 'baseline_matched': previous,
                                 'trial_matched': current, 'transition': transition,
                                 'failure': failures.get(name)})
    records.append(result)

report = {
    'pairs': len(records), 'source_expectations': len(expectations),
    'baseline_status_counts': dict(collections.Counter(item['baseline_status'] for item in records)),
    'trial_status_counts': dict(collections.Counter(item['trial_status'] for item in records)),
    'expectation_transitions': dict(collections.Counter(item['transition'] for item in expectations)),
    'status_changes': [{'pair': item['pair'], 'baseline': item['baseline_status'], 'trial': item['trial_status']}
                       for item in records if item['baseline_status'] != item['trial_status']],
    'scoped_fp_tokens': {label: sum(item[label]['scoped_fp_tokens'] or 0 for item in records if item[label])
                         for label in ['baseline', 'trial']},
    'records': records, 'expectations': expectations,
}
(trial / 'comparison.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps({key: value for key, value in report.items() if key not in ['records', 'expectations']}, indent=2))
