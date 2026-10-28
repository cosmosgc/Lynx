#!/usr/bin/env python3
"""Check common local Markdown links; see QUA-009 for supported syntax."""

import html
import re
import sys
import unicodedata
from collections import Counter
from pathlib import Path
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parent.parent
SKIP_DIRS = {".git", ".lynx", "target", "__pycache__"}
LINK = re.compile(r"!?\[[^\]\n]*\]\(\s*(<[^>\n]*>|[^\s)]+)(?:\s+[^)]*)?\)")
HEADING = re.compile(r"^ {0,3}#{1,6}\s+(.+?)\s*#*\s*$")
FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})")


def visible_lines(text):
    """Ignore fenced and indented code, preserving source line numbers."""
    fence = None
    for number, line in enumerate(text.splitlines(), 1):
        match = FENCE.match(line)
        if match:
            marker = match.group(1)
            if fence is None:
                fence = marker
            elif marker[0] == fence[0] and len(marker) >= len(fence):
                fence = None
            continue
        if fence is None and not line.startswith(("    ", "\t")):
            yield number, line


def slug(text):
    text = re.sub(r"!?\[([^]]+)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"<[^>]*>", "", text)
    text = html.unescape(text).lower()
    return "".join("-" if c == " " else c for c in text
                   if c == " " or c in "-_" or unicodedata.category(c)[0] in "LMN")


def anchors(text):
    counts = Counter()
    result = set()
    for _, line in visible_lines(text):
        match = HEADING.match(line)
        if match:
            base = slug(match.group(1))
            suffix = counts[base]
            candidate = base if not suffix else f"{base}-{suffix}"
            while candidate in result:
                suffix += 1
                candidate = f"{base}-{suffix}"
            counts[base] = suffix + 1
            result.add(candidate)
        result.update(re.findall(r'<a\s+[^>]*(?:id|name)=["\']([^"\']+)["\']', line, re.I))
    return result


def check(root):
    root = Path(root)
    files = sorted(p for p in root.rglob("*.md")
                   if not any(part in SKIP_DIRS for part in p.relative_to(root).parts))
    cache = {}
    errors = []
    for source in files:
        for number, line in visible_lines(source.read_text(encoding="utf-8")):
            # Inline code is not rendered as links.
            line = re.sub(r"(`+).*?\1", "", line)
            for match in LINK.finditer(line):
                destination = html.unescape(match.group(1).strip("<>"))
                parts = urlsplit(destination)
                if parts.scheme or parts.netloc or destination.startswith("/"):
                    continue
                target = (source.parent / unquote(parts.path)).resolve() if parts.path else source.resolve()
                if not target.exists():
                    errors.append(f"{source.relative_to(root)}:{number}: missing target: {destination}")
                elif parts.fragment:
                    if not target.is_file() or target.suffix.lower() != ".md":
                        continue
                    if target not in cache:
                        cache[target] = anchors(target.read_text(encoding="utf-8"))
                    if unquote(parts.fragment) not in cache[target]:
                        errors.append(f"{source.relative_to(root)}:{number}: missing anchor: {destination}")
    return files, errors


def main():
    files, errors = check(ROOT)
    for error in errors:
        print(error, file=sys.stderr)
    print(f"Checked {len(files)} Markdown files; {len(errors)} broken links.")
    return bool(errors)


if __name__ == "__main__":
    sys.exit(main())
