#!/usr/bin/env python3
"""Append msgid/msgstr blocks from a .po fragment into an existing .po file.

Usage: merge_po.py BASE_PO FRAGMENT_PO OUTPUT_PO

Existing msgids are kept untouched, so the upstream translation wins.
"""
import ast
import re
import sys


def po_blocks(text):
    return [block for block in re.split(r"\n\s*\n", text) if block.strip()]


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


def main(base_path, fragment_path, output_path):
    base = open(base_path, encoding='utf-8').read()
    fragment = open(fragment_path, encoding='utf-8').read()

    existing = {block_msgid(block) for block in po_blocks(base)}
    additions = []
    seen = set()
    for block in po_blocks(fragment):
        msgid = block_msgid(block)
        if not msgid or msgid in existing or msgid in seen:
            continue
        seen.add(msgid)
        additions.append(block.strip())

    with open(output_path, 'w', encoding='utf-8') as handle:
        handle.write(base.rstrip() + '\n')
        for block in additions:
            handle.write('\n' + block + '\n')

    print(f'existing={len(existing)} added={len(additions)} skipped={len(seen) - len(additions)}')


if __name__ == '__main__':
    main(*sys.argv[1:4])
