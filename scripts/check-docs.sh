#!/usr/bin/env bash
# Checks the tracked Markdown files: no emojis (CODING_STANDARDS.md, Docs),
# and every relative link points to a file that exists, with its #anchor
# matching a heading there. Prints file:line for each hit.
#
# Usage: scripts/check-docs.sh [repo]
# CI runs this in the changes job, so docs-only pull requests are checked too.
set -euo pipefail
cd "${1:-$(dirname "$0")/..}"

git ls-files -z -- '*.md' | python3 -I -c '
import os, re, sys

EMOJI = re.compile("[\U0001F000-\U0001FAFF☀-➿⬀-⯿️]")
LINK = re.compile(r"\]\(([^)\s]+)\)")
FENCE = re.compile(r"^\s*(```|~~~)")

def anchors(path, cache={}):
    if path not in cache:
        found = set()
        fenced = False
        with open(path, encoding="utf-8") as f:
            for line in f:
                if FENCE.match(line):
                    fenced = not fenced
                elif not fenced and line.startswith("#"):
                    text = line.lstrip("#").strip().lower()
                    found.add(re.sub(r"[^\w\- ]", "", text).replace(" ", "-"))
        cache[path] = found
    return cache[path]

hits = []
for path in sys.stdin.read().split("\0"):
    if not path:
        continue
    fenced = False
    with open(path, encoding="utf-8") as f:
        lines = f.read().splitlines()
    for number, line in enumerate(lines, 1):
        if EMOJI.search(line):
            hits.append(f"{path}:{number}: emoji")
        if FENCE.match(line):
            fenced = not fenced
            continue
        if fenced:
            continue
        for target in LINK.findall(line):
            if re.match(r"^[a-z]+:", target):
                continue
            file, _, anchor = target.partition("#")
            resolved = os.path.normpath(os.path.join(os.path.dirname(path), file)) if file else path
            if not os.path.exists(resolved):
                hits.append(f"{path}:{number}: link to missing {target}")
            elif anchor and resolved.endswith(".md") and anchor not in anchors(resolved):
                hits.append(f"{path}:{number}: no heading for #{anchor} in {resolved}")
if hits:
    print("Markdown problems:", *hits, sep="\n", file=sys.stderr)
    sys.exit(1)
'
