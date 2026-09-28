#!/usr/bin/env python3
"""Project serde-pretty native reports into bounded benchmark diagnostics.

Only ``sources`` arrays are summarized. All other JSON values are preserved.
This consumes trusted schema-11 serializer output, not arbitrary JSON layouts.
"""

import argparse
from collections import Counter
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile

CHUNK_SIZE = 1024 * 1024
MAX_RETAINED_BYTES = 256 * 1024 * 1024
SAMPLE_BYTES = 64 * 1024
ARTIFACT_FORMAT = 'pdfdelta-native-compact'
SOURCE = re.compile(rb'(?m)^( +)"sources": \[')


class CompactError(ValueError):
    pass


def encode(value):
    return json.dumps(value, ensure_ascii=False, separators=(',', ':'), allow_nan=False).encode()


def file_hash(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


class Reader:
    def __init__(self, stream):
        self.stream = stream
        self.buffer = b''
        self.eof = False
        self.sha256 = hashlib.sha256()
        self.size = 0

    def fill(self):
        chunk = self.stream.read(CHUNK_SIZE)
        self.sha256.update(chunk)
        self.size += len(chunk)
        self.buffer += chunk
        self.eof = not chunk


class SourceSummary:
    def __init__(self, indent):
        self.sha256 = hashlib.sha256()
        self.size = 0
        self.kinds = Counter()
        self.pages = set()
        self.count = 0
        self.closes = 0
        self.pending = b''
        self.prefix = b''
        self.suffix = b''
        self.item_indent = indent + b'  '
        field_indent = indent + b'    '
        self.kind = re.compile(rb'(?m)^' + field_indent + rb'"kind": "([^"\n]+)",?$')
        self.page = re.compile(rb'(?m)^' + field_indent + rb'"page": ([0-9]+),?$')
        self.open = re.compile(rb'(?m)^' + self.item_indent + rb'\{$')
        self.close = re.compile(rb'(?m)^' + self.item_indent + rb'\},?$')

    def feed(self, data):
        self.sha256.update(data)
        self.size += len(data)
        self.prefix += data[:max(0, SAMPLE_BYTES - len(self.prefix))]
        self.suffix = (self.suffix + data)[-SAMPLE_BYTES:]
        block = self.pending + data
        end = block.rfind(b'\n') + 1
        complete, self.pending = block[:end], block[end:]
        if len(self.pending) > SAMPLE_BYTES:
            raise CompactError('source line exceeds sample bound')
        self.kinds.update(kind.decode() for kind in self.kind.findall(complete))
        self.pages.update(int(page) for page in set(self.page.findall(complete)))
        self.count += len(self.open.findall(complete))
        self.closes += len(self.close.findall(complete))

    def sample(self, data, last=False):
        start_marker = b'\n' + self.item_indent + b'{\n'
        start = data.rfind(start_marker) if last else data.find(start_marker)
        if start < 0:
            raise CompactError('source sample exceeds bounded window')
        start += len(b'\n' + self.item_indent)
        close_marker = b'\n' + self.item_indent + b'}'
        end = data.find(close_marker, start)
        if end < 0:
            raise CompactError('source sample is incomplete')
        return json.loads(data[start:end + len(close_marker)])

    def finish(self):
        if self.count != self.closes or self.count != sum(self.kinds.values()):
            raise CompactError('unexpected source object layout')
        if set(self.kinds) - {'glyph', 'synthetic_space', 'line_break', 'block_separator_space'}:
            raise CompactError('unknown source kind')
        result = {'raw_array_sha256': self.sha256.hexdigest(), 'raw_array_bytes': self.size,
                  'item_count': self.count, 'kind_counts': dict(sorted(self.kinds.items())),
                  'page_ids': sorted(self.pages)}
        if self.count:
            result['samples'] = {'first': self.sample(self.prefix), 'last': self.sample(self.suffix, True)}
        elif self.prefix.strip() != b'[]':
            raise CompactError('unexpected empty source layout')
        return result


def consume_sources(reader, indent):
    summary = SourceSummary(indent)
    summary.feed(b'[')
    if not reader.buffer and not reader.eof:
        reader.fill()
    if reader.buffer.startswith(b']'):
        summary.feed(b']')
        reader.buffer = reader.buffer[1:]
        return summary.finish()
    if not reader.buffer.startswith(b'\n'):
        raise CompactError('sources require serde pretty layout')
    terminator = b'\n' + indent + b']'
    while True:
        end = reader.buffer.find(terminator)
        if end >= 0:
            end += len(terminator)
            summary.feed(reader.buffer[:end])
            reader.buffer = reader.buffer[end:]
            return summary.finish()
        if reader.eof:
            raise CompactError('truncated source array')
        keep = len(terminator)
        if len(reader.buffer) > keep:
            summary.feed(reader.buffer[:-keep])
            reader.buffer = reader.buffer[-keep:]
        reader.fill()


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise CompactError(f'duplicate JSON key: {key}')
        result[key] = value
    return result


def reject_sources(value):
    if isinstance(value, dict):
        if 'sources' in value:
            raise CompactError('sources field has unsupported layout')
        for child in value.values():
            reject_sources(child)
    elif isinstance(value, list):
        for child in value:
            reject_sources(child)


def compact_report(source, output):
    source, output = Path(source), Path(output)
    if source.is_symlink() or not source.is_file():
        raise CompactError('source must be a regular file')
    if output.exists() or output.is_symlink():
        raise CompactError('refusing to overwrite output')
    original_file_hash = file_hash(source)
    retained = bytearray()
    arrays = 0
    omitted_bytes = 0

    def retain(data):
        if len(retained) + len(data) > MAX_RETAINED_BYTES:
            raise CompactError('retained report exceeds 256 MiB bound')
        retained.extend(data)

    opener = gzip.open if source.suffix == '.gz' else open
    with opener(source, 'rb') as stream:
        reader = Reader(stream)
        while True:
            match = SOURCE.search(reader.buffer)
            if match:
                indent = match[1]
                if len(indent) > 256 or len(indent) % 2:
                    raise CompactError('unexpected source indentation')
                retain(reader.buffer[:match.start()])
                reader.buffer = reader.buffer[match.end():]
                summary = consume_sources(reader, indent)
                retain(indent + b'"source_summary": ' + encode(summary))
                arrays += 1
                omitted_bytes += summary['raw_array_bytes']
                continue
            if reader.eof:
                retain(reader.buffer)
                break
            # Keep the unfinished line so a key split over reads is never missed.
            end = reader.buffer.rfind(b'\n') + 1
            retain(reader.buffer[:end])
            reader.buffer = reader.buffer[end:]
            if len(reader.buffer) > MAX_RETAINED_BYTES:
                raise CompactError('retained line exceeds bound')
            reader.fill()
    report = json.loads(retained, object_pairs_hook=unique_object)
    if not isinstance(report, dict) or report.get('schema_version') != 11:
        raise CompactError('requires native schema 11')
    reject_sources(report)
    metadata = {'source_file_sha256': original_file_hash, 'source_file_bytes': source.stat().st_size,
                'source_logical_sha256': reader.sha256.hexdigest(), 'source_logical_bytes': reader.size,
                'source_array_count': arrays, 'omitted_sources_bytes': omitted_bytes}
    artifact = {'artifact_format': ARTIFACT_FORMAT, 'artifact_version': 1,
                'source_schema_version': 11,
                'omitted_fields': {'sources': 'source_summary; full glyph geometry and operator provenance require regeneration'},
                'source': metadata.copy(), 'report': report}
    output.parent.mkdir(parents=True, exist_ok=True)
    handle, temporary_name = tempfile.mkstemp(prefix='.compact-', dir=output.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(handle, 'wb') as raw:
            with gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0, compresslevel=9) as packed:
                for chunk in json.JSONEncoder(ensure_ascii=False, separators=(',', ':'), allow_nan=False).iterencode(artifact):
                    packed.write(chunk.encode())
        metadata.update(output_bytes=temporary.stat().st_size, output_sha256=file_hash(temporary))
        os.link(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)
    return metadata


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    print(json.dumps(compact_report(args.source, args.output), sort_keys=True))


if __name__ == '__main__':
    main()
