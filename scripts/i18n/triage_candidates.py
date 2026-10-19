#!/usr/bin/env python3
"""Classify scan_missing.py candidates by the call site that produces them.

Usage: triage_candidates.py SCAN_OUTPUT [ROOT]

SCAN_OUTPUT is the stdout of scan_missing.py. Each candidate is grouped as
validation, service_response, api_response, flash, log, exception or other so
that only user-visible strings need translation.
"""
import collections
import re
import sys

LINE_RE = re.compile(r'^(.*?):(\d+): (.*)$')

RULES = (
    ('validation', re.compile(r'errors\.add\s*\(|add_error\s*\(')),
    ('service_response', re.compile(r'ServiceResponse')),
    ('api_response', re.compile(r'render\s+json|json:\s*\{|head\s+:\w+')),
    ('flash', re.compile(r'\bflash\b')),
    ('log', re.compile(r'logger|AppLogger|Rails\.log|Gitlab::Logging|log_error')),
    ('exception', re.compile(r'\braise\b|\bfail\b')),
)


def candidates(path):
    for line in open(path, encoding='utf-8'):
        line = line.rstrip('\n')
        if not line or line.startswith('    also '):
            continue
        match = LINE_RE.match(line)
        if match:
            yield match.group(1), int(match.group(2)), match.group(3)


def classify(source, lineno):
    lines = source.splitlines()
    start = max(0, lineno - 4)
    window = '\n'.join(lines[start:lineno + 3])
    for name, pattern in RULES:
        if pattern.search(window):
            return name
    return 'other'


def main(scan_path):
    groups = collections.defaultdict(list)
    cache = {}
    for path, lineno, text in candidates(scan_path):
        source = cache.get(path)
        if source is None:
            try:
                source = open(path, encoding='utf-8', errors='ignore').read()
            except OSError:
                source = ''
            cache[path] = source
        groups[classify(source, lineno)].append((path, lineno, text))

    for name in sorted(groups, key=lambda key: -len(groups[key])):
        print(f'## {name} ({len(groups[name])})')
        for path, lineno, text in groups[name]:
            print(f'{path}:{lineno}: {text}')
        print()


if __name__ == '__main__':
    main(sys.argv[1])
