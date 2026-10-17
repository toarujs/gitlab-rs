#!/usr/bin/env python3
"""Collect hard-coded English strings passed to errors.add / add_error.

Usage: scan_errors.py ROOT [ROOT ...]
"""
import os
import re
import sys

ROOTS = sys.argv[1:] or ['/opt/gitlab/embedded/service/gitlab-rails/app']

CALL_RE = re.compile(r'errors\.add\s*\(|add_error\s*\(')
STRING_RE = re.compile(r"(['\"])((?:\\.|(?!\1).)*)\1")
TRANSLATED_RE = re.compile(r'(?<![A-Za-z0-9_])(_|s_|N_)\s*\(?\s*$')

results = {}
for root in ROOTS:
    for dirpath, _, files in os.walk(root):
        for filename in files:
            if not filename.endswith('.rb'):
                continue
            path = os.path.join(dirpath, filename)
            with open(path, encoding='utf-8', errors='ignore') as handle:
                lines = handle.readlines()
            for lineno, line in enumerate(lines, 1):
                stripped = line.strip()
                if stripped.startswith('#'):
                    continue
                if not CALL_RE.search(line):
                    continue
                window = line
                for extra in lines[lineno:lineno + 3]:
                    window += extra
                    if window.count('(') <= window.count(')'):
                        break
                for match in STRING_RE.finditer(window):
                    text = match.group(2)
                    if len(text) < 8 or not text.isascii():
                        continue
                    if '%{' in text or '#{' in text:
                        continue
                    if TRANSLATED_RE.search(window[max(0, match.start() - 40):match.start()]):
                        continue
                    if not re.search(r'[A-Za-z]{3,}', text):
                        continue
                    results.setdefault(text, []).append(f'{path}:{lineno}')

for text, locations in sorted(results.items()):
    print(f'{locations[0]}: {text}')
    for location in locations[1:3]:
        print(f'    also {location}')
