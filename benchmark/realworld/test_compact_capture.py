"""Retention safety checks for compact native captures."""

import gzip
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from compact_native import compact_report

from compact_capture import compact_capture, digest, prune


class CaptureRetentionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.capture = self.root / 'capture'
        self.capture.mkdir()
        self.source = self.capture / 'pair.json.gz'
        report = {
            'schema_version': 11,
            'difference_status': 'detected',
            'comparison_scope': {'supported_text': True, 'images_compared': False},
            'assessment': None,
            'summary': {'comparison_complete': True, 'difference_status': 'detected'},
            'changes': [], 'change_candidates': [], 'proven_changed_regions': [],
            'formatting_only_changes': [], 'unresolved_regions': [],
            'extraction': {'old_complete': True, 'new_complete': True, 'issues': []},
        }
        raw = (json.dumps(report, indent=2) + '\n').encode()
        self.source.write_bytes(gzip.compress(raw, mtime=0))
        self.summary = self.capture / 'summary.json'
        self.summary.write_text(json.dumps({
            'route': 'native',
            'rows': [{
                'pair': 'pair', 'comparison_complete': True, 'difference_status': 'detected',
                'report': {'path': str(self.source), 'sha256': hashlib.sha256(raw).hexdigest(),
                           'file_sha256': digest(self.source), 'bytes': self.source.stat().st_size,
                           'logical_bytes': len(raw)},
            }],
        }))
        self.destination = self.root / 'compact'

    def test_conversion_preserves_original_until_verified_prune(self):
        compact_capture(self.summary, self.destination)
        self.assertTrue(self.source.exists())
        manifest = self.destination / 'manifest.json'
        prune(manifest)
        self.assertFalse(self.source.exists())
        self.assertTrue(self.summary.exists())
        self.assertTrue((self.destination / 'pair.compact.json.gz').exists())
        prune(manifest)

    def test_corrupt_replacement_blocks_pruning(self):
        compact_capture(self.summary, self.destination)
        (self.destination / 'pair.compact.json.gz').write_bytes(b'corrupt')
        with self.assertRaisesRegex(ValueError, 'compact checksum mismatch'):
            prune(self.destination / 'manifest.json')
        self.assertTrue(self.source.exists())

    def test_changed_original_blocks_pruning(self):
        compact_capture(self.summary, self.destination)
        self.source.write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'original checksum mismatch'):
            prune(self.destination / 'manifest.json')
        self.assertTrue(self.source.exists())

    def test_missing_manifest_entry_blocks_pruning(self):
        compact_capture(self.summary, self.destination)
        path = self.destination / 'manifest.json'
        manifest = json.loads(path.read_text())
        manifest['reports'] = []
        path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, 'every original report'):
            prune(path)
        self.assertTrue(self.source.exists())

    def test_unsafe_pair_id_is_rejected(self):
        summary = json.loads(self.summary.read_text())
        summary['rows'][0]['pair'] = '../outside'
        self.summary.write_text(json.dumps(summary))
        with self.assertRaisesRegex(ValueError, 'invalid pair ID'):
            compact_capture(self.summary, self.destination)
        self.assertTrue(self.source.exists())

    def test_missing_original_without_intent_blocks_prune(self):
        compact_capture(self.summary, self.destination)
        self.source.unlink()
        with self.assertRaisesRegex(ValueError, 'original missing before pruning'):
            prune(self.destination / 'manifest.json')

    def test_resume_verifies_and_reuses_completed_report(self):
        self.destination.mkdir()
        output = self.destination / 'pair.compact.json.gz'
        compact_report(self.source, output)
        expected = output.read_bytes()
        with mock.patch('compact_capture.compact_report', side_effect=AssertionError('must reuse')):
            manifest = compact_capture(self.summary, self.destination, resume=True)
        self.assertTrue(manifest['reports'][0]['reused_existing_output'])
        self.assertEqual(output.read_bytes(), expected)
        self.assertTrue(self.source.exists())

    def test_changed_logical_hash_blocks_conversion(self):
        summary = json.loads(self.summary.read_text())
        summary['rows'][0]['report']['sha256'] = '0' * 64
        self.summary.write_text(json.dumps(summary))
        with self.assertRaisesRegex(ValueError, 'logical'):
            compact_capture(self.summary, self.destination)
        self.assertTrue(self.source.exists())
        self.assertFalse((self.destination / 'manifest.json').exists())


if __name__ == '__main__':
    unittest.main()
