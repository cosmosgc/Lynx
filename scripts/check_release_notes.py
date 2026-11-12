#!/usr/bin/env python3
"""Require versioned changelog and migration notes before publishing a tag."""

import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
RELEASE_TAG = re.compile(r"^v(?P<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$")


def check(root=ROOT, tag=""):
    root = Path(root)
    match = RELEASE_TAG.fullmatch(tag)
    if not match:
        return [f"Release tag {tag!r} must use v<major>.<minor>.<patch> format"]

    errors = []
    version = match.group("version")
    workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    package_version = workspace["workspace"]["package"]["version"]
    if package_version != version:
        errors.append(
            f"Release tag {tag} does not match workspace version {package_version}"
        )

    changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    section_header = re.compile(rf"(?m)^## \[{re.escape(version)}\](?:\s+-.*)?\s*$")
    section_match = section_header.search(changelog)
    if not section_match:
        return errors + [
            f"CHANGELOG.md: add a `## [{version}]` release section before tagging"
        ]

    following = re.search(r"(?m)^## \[", changelog[section_match.end() :])
    section_end = (
        section_match.end() + following.start() if following else len(changelog)
    )
    section = changelog[section_match.end() : section_end]
    category_matches = list(re.finditer(r"(?m)^### (.+?)\s*$", section))
    categories = {}
    for index, category in enumerate(category_matches):
        end = (
            category_matches[index + 1].start()
            if index + 1 < len(category_matches)
            else len(section)
        )
        body = section[category.end() : end]
        body = re.sub(r"<!--.*?-->", "", body, flags=re.DOTALL)
        entries = [
            line.strip()
            for line in body.splitlines()
            if line.strip()
        ]
        categories[category.group(1).strip().casefold()] = entries

    migration_entries = categories.get("migration", [])
    if not migration_entries:
        errors.append(
            f"CHANGELOG.md: [{version}] needs a non-empty `### Migration` section "
            "(write `- None.` when no migration is required)"
        )

    if not any(entries for name, entries in categories.items() if name != "migration"):
        errors.append(
            f"CHANGELOG.md: [{version}] needs at least one changelog entry "
            "outside the Migration section"
        )
    return errors


def main(argv=None):
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 1:
        print("usage: check_release_notes.py v<major>.<minor>.<patch>", file=sys.stderr)
        return 2
    errors = check(tag=args[0])
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"Release notes for {args[0]} include changelog and migration entries.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
