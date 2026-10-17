#!/usr/bin/env python3
"""List hard-coded English error strings that have no zh_CN translation.

Usage: I18N_PO=/path/to/gitlab.po scan_missing.py ROOT [ROOT ...]
"""
import ast
import os
import re
import sys

PO_PATH = os.environ.get(
    'I18N_PO',
    '/opt/gitlab/embedded/service/gitlab-rails/locale/zh_CN/gitlab.po')
ROOTS = sys.argv[1:] or ['/opt/gitlab/embedded/service/gitlab-rails/app']

CALL_RE = re.compile(r'errors\.add\s*\(|add_error\s*\(|message:\s*|message =>')
STRING_RE = re.compile(r"(['\"])((?:\\.|(?!\1).)*)\1")
TRANSLATED_RE = re.compile(r'(?<![A-Za-z0-9_])(_|s_|N_)\s*\(?\s*$')


def po_blocks(text):
    return [block for block in re.split(r'\n\s*\n', text) if block.strip()]


def block_msgid(block):
    lines = block.splitlines()
    for index, line in enumerate(lines):
        if not line.startswith('msgid '):
            continue
        parts = []
        first = line[len('msgid '):].strip()
        if first:
            parts.append(first)
        for follow in lines[index + 1:]:
            if follow.startswith('"'):
                parts.append(follow.strip())
            else:
                break
        return ''.join(ast.literal_eval(part) for part in parts)
    return None


known = set()
with open(PO_PATH, encoding='utf-8') as handle:
    for block in po_blocks(handle.read()):
        msgid = block_msgid(block)
        if msgid:
            known.add(msgid)

results = {}
for root in ROOTS:
    for dirpath, _, files in os.walk(root):
        for filename in files:
            if not filename.endswith(('.rb', '.haml', '.erb')):
                continue
            path = os.path.join(dirpath, filename)
            with open(path, encoding='utf-8', errors='ignore') as handle:
                lines = handle.readlines()
            for lineno, line in enumerate(lines, 1):
                if line.strip().startswith(('#', '-#')):
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
                    if len(text) < 8 or not text.isascii() or text in known:
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
