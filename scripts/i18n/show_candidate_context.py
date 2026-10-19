#!/usr/bin/env python3
"""Print source context for candidates listed as PATH:LINENO: TEXT."""
import re
import sys

LINE_RE = re.compile(r'^(.*?):(\d+): (.*)$')

for line in open(sys.argv[1], encoding='utf-8'):
    match = LINE_RE.match(line.rstrip('\n'))
    if not match:
        continue
    path, lineno, text = match.group(1), int(match.group(2)), match.group(3)
    try:
        source = open(path, encoding='utf-8', errors='ignore').read().splitlines()
    except OSError:
        continue
    print(f'### {path}:{lineno}: {text}')
    for index in range(max(0, lineno - 4), min(len(source), lineno + 2)):
        print(f'{index + 1:5d}| {source[index]}')
    print()
