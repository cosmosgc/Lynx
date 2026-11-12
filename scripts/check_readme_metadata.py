#!/usr/bin/env python3
"""Check README summary counts against workspace and documentation metadata."""

import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
ADR_STATUS = re.compile(
    r"(?im)^- \*\*Status:\*\*\s*(.+)$|^## Status\s*\n\s*([^\n]+)"
)


def check(root=ROOT):
    root = Path(root)
    errors = []
    readme = (root / "README.md").read_text(encoding="utf-8")

    workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    crate_count = len(workspace["workspace"]["members"])
    crate_metric = re.search(
        r"(?m)^\| Workspace crates \| (\d+) members\b", readme
    )
    if not crate_metric:
        errors.append("README.md: missing Workspace crates metric")
    elif int(crate_metric.group(1)) != crate_count:
        errors.append(
            f"README.md: Workspace crates says {crate_metric.group(1)}, "
            f"but Cargo.toml lists {crate_count} members"
        )

    accepted_adrs = []
    accepted_numbers = []
    proposed_numbers = []
    for path in sorted((root / "docs" / "adr").glob("*.md")):
        number = re.match(r"(\d{4})-", path.name)
        status = ADR_STATUS.search(path.read_text(encoding="utf-8"))
        if not status:
            errors.append(f"{path.relative_to(root)}: missing ADR status")
            continue
        text = status.group(1) or status.group(2)
        if re.search(r"\baccepted\b", text, re.IGNORECASE):
            accepted_adrs.append(int(number.group(1)) if number else path.name)
            if number:
                accepted_numbers.append(int(number.group(1)))
        elif number and re.search(r"\bproposed\b", text, re.IGNORECASE):
            proposed_numbers.append(int(number.group(1)))

    accepted_count = len(accepted_adrs)
    displayed_adr_counts = [
        int(value)
        for value in re.findall(
            r"\b(\d+) accepted (?:architecture decision records|ADRs)\b", readme
        )
    ]
    if not displayed_adr_counts:
        errors.append("README.md: missing accepted ADR count")
    elif any(value != accepted_count for value in displayed_adr_counts):
        errors.append(
            f"README.md: accepted ADR counts {displayed_adr_counts} do not match "
            f"the {accepted_count} accepted ADRs"
        )

    if accepted_numbers:
        adr_range = f"({min(accepted_numbers):04d}–{max(accepted_numbers):04d})"
        if adr_range not in readme:
            errors.append(f"README.md: ADR index is missing current range {adr_range}")
    for number in proposed_numbers:
        if f"proposed ADR {number:04d}" not in readme:
            errors.append(f"README.md: ADR index does not identify proposed ADR {number:04d}")

    docs_section = re.search(
        r"(?ms)^## Documentation\s*\n(.*?)(?=^## |\Z)", readme
    )
    docs_metric = re.search(r"(?m)^\| Docs \| (\d+) design docs\b", readme)
    if not docs_metric:
        errors.append("README.md: missing Docs metric")
    elif not docs_section:
        errors.append("README.md: missing Documentation index")
    else:
        indexed_docs = set()
        for target in re.findall(r"\]\((docs/[^)#]+)(?:#[^)]*)?\)", docs_section.group(1)):
            if target.startswith(("docs/TODO/", "docs/adr/")):
                continue
            indexed_docs.add(target)
        if int(docs_metric.group(1)) != len(indexed_docs):
            errors.append(
                f"README.md: Docs says {docs_metric.group(1)}, but the "
                f"Documentation index lists {len(indexed_docs)} unique design docs"
            )

    return errors


def main():
    errors = check()
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print("README workspace, ADR, documentation counts, and ADR range are current.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
