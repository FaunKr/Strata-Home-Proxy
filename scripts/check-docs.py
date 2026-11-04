"""Checks tracked Markdown files; run by scripts/check-docs.sh.

Fails on an emoji, and on a relative link whose file is not tracked or
whose #anchor matches no heading there. Prints file:line for each hit.
"""

import functools
import os
import re
import subprocess
import sys
import urllib.parse

# Characters that render as emoji by default (Unicode Emoji_Presentation),
# and the variation selector that turns a plain symbol into one. Plain
# symbols such as a check mark or an arrow stay allowed.
EMOJI = re.compile(
    "[\U0001F000-\U0001FAFF"
    "⌚⌛⏩-⏳⏸-⏺◽◾☔☕"
    "♈-♓♿⚓⚡⚪⚫⚽⚾⛄⛅"
    "⛎⛔⛪⛲⛳⛵⛺⛽✅✊✋"
    "✨❌❎❓-❕❗➕-➗➰➿"
    "⬛⬜⭐⭕️]"
)
INLINE_LINK = re.compile(r"\]\(\s*(<[^>]*>|(?:[^()\s]|\([^()\s]*\))+)")
REFERENCE = re.compile(r"^ {0,3}\[[^\]]+\]:\s*(<[^>]*>|\S+)")
FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})")
CODE_SPAN = re.compile(r"(`+).*?\1")
ATX = re.compile(r"^ {0,3}#{1,6}\s+(.*?)(?:\s+#+)?\s*$")
SETEXT = re.compile(r"^ {0,3}(=+|-+)\s*$")
HTML_ID = re.compile(r"<a\s[^>]*(?:id|name)=\"([^\"]+)\"")


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read().splitlines()


def outside_fences(lines):
    """Yields (number, line) for each line outside fenced code blocks."""
    fence = None
    for number, line in enumerate(lines, 1):
        match = FENCE.match(line)
        if fence is None and match:
            fence = match.group(1)
        elif fence is not None and match and match.group(1)[0] == fence[0] and len(match.group(1)) >= len(fence):
            fence = None
        elif fence is None:
            yield number, line


def slug(text):
    text = re.sub(r"[^\w\- ]", "", text.strip().lower())
    return text.replace(" ", "-")


@functools.cache
def anchors(path):
    found, counts = set(), {}
    previous = ""
    for _, line in outside_fences(read(path)):
        heading = None
        atx = ATX.match(line)
        if atx:
            heading = atx.group(1)
        elif SETEXT.match(line) and previous.strip() and not previous.lstrip().startswith(("-", "*", "#")):
            heading = previous
        if heading is not None:
            base = slug(heading)
            count = counts.get(base, 0)
            counts[base] = count + 1
            found.add(base if count == 0 else f"{base}-{count}")
        found.update(HTML_ID.findall(line))
        previous = line
    return found


def main():
    tracked = subprocess.run(["git", "ls-files", "-z"], capture_output=True, check=True, text=True).stdout.split("\0")
    tracked = {path for path in tracked if path}
    dirs = set()
    for path in tracked:
        while path := os.path.dirname(path):
            dirs.add(path)
    exists = lambda path: path in tracked or path in dirs or path == "."

    hits = []
    for path in sorted(p for p in tracked if p.endswith(".md")):
        try:
            lines = read(path)
        except UnicodeDecodeError as e:
            hits.append(f"{path}: not UTF-8 ({e.reason} at byte {e.start})")
            continue
        for number, line in enumerate(lines, 1):
            if EMOJI.search(line):
                hits.append(f"{path}:{number}: emoji")
        for number, line in outside_fences(lines):
            text = CODE_SPAN.sub("", line)
            targets = INLINE_LINK.findall(text) + REFERENCE.findall(text)
            for target in targets:
                target = target.strip("<>")
                if not target or re.match(r"^[a-z][a-z0-9+.-]*:", target):
                    continue
                file, _, anchor = target.partition("#")
                file = urllib.parse.unquote(file)
                if file.startswith("/"):
                    resolved = os.path.normpath(file.lstrip("/"))
                elif file:
                    resolved = os.path.normpath(os.path.join(os.path.dirname(path), file))
                else:
                    resolved = path
                if not exists(resolved):
                    hits.append(f"{path}:{number}: link to missing {target}")
                elif anchor and resolved.endswith(".md") and urllib.parse.unquote(anchor) not in anchors(resolved):
                    hits.append(f"{path}:{number}: no heading for #{anchor} in {resolved}")
    if hits:
        print("Markdown problems:", *hits, sep="\n", file=sys.stderr)
        sys.exit(1)


main()
