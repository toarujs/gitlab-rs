#!/usr/bin/env python3
"""Find long ASCII string literals that are not wrapped in a gettext helper.

Usage: scan_hardcoded.py ROOT [ROOT ...]

Run it inside a GitLab container against app/lib paths. The output is a
candidate list; SQL fragments, CSS classes and identifiers still need review.
"""
import os
import re
import sys

DEFAULT_ROOTS = [
    '/opt/gitlab/embedded/service/gitlab-rails/app/presenters/ci',
    '/opt/gitlab/embedded/service/gitlab-rails/app/serializers/ci',
    '/opt/gitlab/embedded/service/gitlab-rails/app/helpers/ci',
    '/opt/gitlab/embedded/service/gitlab-rails/app/views/ci',
    '/opt/gitlab/embedded/service/gitlab-rails/app/views/projects/jobs',
    '/opt/gitlab/embedded/service/gitlab-rails/lib/gitlab/ci/status',
]

ROOTS = sys.argv[1:] or DEFAULT_ROOTS

STRING_RE = re.compile(r"(['\"])((?:\\.|(?!\1).)*)\1")
GETTEXT_RE = re.compile(r'(?<![A-Za-z0-9_])(_|s_|N_|n_|ns_)\s*\(?\s*$')
SKIP_MARKERS = (
    'http://', 'https://', 'SELECT ', 'FROM ', 'class=', '<a ', '<div', '<span',
    '.rb', '.js', '.svg', '.png', 'data-', '%{', '#{', 'application/', 'vendor/',
)

candidates = {}
for root in ROOTS:
    for dirpath, _, files in os.walk(root):
        for filename in files:
            if not filename.endswith(('.rb', '.haml', '.erb')):
                continue
            path = os.path.join(dirpath, filename)
            with open(path, encoding='utf-8', errors='ignore') as handle:
                for lineno, line in enumerate(handle, 1):
                    stripped = line.strip()
                    if stripped.startswith('#') or stripped.startswith('-#'):
                        continue
                    for match in STRING_RE.finditer(line):
                        text = match.group(2)
                        if len(text) < 20 or not text.isascii():
                            continue
                        words = re.findall(r'[A-Za-z]{2,}', text)
                        if len(words) < 4:
                            continue
                        if GETTEXT_RE.search(line[:match.start()]):
                            continue
                        if any(marker in text for marker in SKIP_MARKERS):
                            continue
                        candidates.setdefault(text, []).append(f'{path}:{lineno}')

for text, locations in sorted(candidates.items()):
    print(f'{locations[0]}: {text}')
    for location in locations[1:4]:
        print(f'    also {location}')
