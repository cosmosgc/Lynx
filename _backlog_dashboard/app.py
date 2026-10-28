from __future__ import annotations

import re
from collections import Counter
from heapq import heappop, heappush
from pathlib import Path
from typing import Any

from flask import Flask, abort, jsonify, render_template, request


TASK_HEADING = re.compile(
    r"^##\s+([A-Z]{2,5}-\d{3})\s+[—–-]\s+(.+?)\s*$", re.MULTILINE
)
METADATA_LINE = re.compile(r"^- \*\*(Priority|Status|Depends on|References):\*\*\s*(.*)$")
TASK_ID = re.compile(r"\b[A-Z]{2,5}-\d{3}\b")
PROJECT_PATH = re.compile(
    r"(?:^|[`/\\\s(])((?:crates|clients|tools|docs|server|scripts|BAT)/[A-Za-z0-9_./\\-]+|(?:Cargo\.toml|Cargo\.lock|README\.md|CHANGELOG\.md))"
)
STATUS_PREFIXES = (
    ("done", ("done", "complete", "completed")),
    ("in_progress", ("in progress", "in-progress")),
    ("blocked", ("blocked",)),
    ("ready", ("ready",)),
    ("proposed", ("proposed",)),
    ("icebox", ("icebox",)),
)

STATUS_LABELS = {
    "done": "Done",
    "in_progress": "In progress",
    "blocked": "Blocked",
    "ready": "Ready",
    "proposed": "Proposed",
    "icebox": "Icebox",
    "unspecified": "Unspecified",
}
PHASE_LINE = re.compile(
    r"^Phase\s+(\d+)\s+(.+?)\s+\[([█░]+)\]\s+(\d+)%\s+(.+?)\s*$",
    re.MULTILINE,
)


def _metadata(section: str) -> dict[str, str]:
    lines = section.splitlines()
    result: dict[str, str] = {}
    current_key: str | None = None

    for line in lines:
        match = METADATA_LINE.match(line)
        if match:
            current_key = match.group(1).lower().replace(" ", "_")
            result[current_key] = match.group(2).strip()
        elif current_key and line.startswith("  ") and line.strip():
            result[current_key] = f"{result[current_key]} {line.strip()}".strip()
        else:
            current_key = None
    return result


def _normalize_status(value: str) -> tuple[str, str]:
    normalized = value.strip().casefold()
    for key, prefixes in STATUS_PREFIXES:
        for prefix in prefixes:
            if normalized == prefix or normalized.startswith(f"{prefix} "):
                detail = value.strip()[len(prefix) :].lstrip(" —–-:").strip()
                return key, detail
    return "unspecified", value.strip()


def _acceptance_criteria(section: str) -> list[dict[str, Any]]:
    lines = section.splitlines()
    start = next(
        (index for index, line in enumerate(lines) if "acceptance criteria" in line.casefold()),
        None,
    )
    if start is None:
        return []

    criteria = []
    for line in lines[start + 1 :]:
        stripped = line.strip()
        if stripped == "---" or stripped.startswith("## "):
            break
        if not stripped:
            if criteria:
                break
            continue
        match = re.match(r"- \[([ xX])\]\s*(.+)", stripped)
        if match:
            criteria.append(
                {"text": match.group(2).strip(), "complete": match.group(1).casefold() == "x"}
            )
        elif criteria and line.startswith("  ") and not stripped.startswith("- "):
            criteria[-1]["text"] += f" {stripped}"
        elif criteria:
            break
    return criteria


def _section_bullets(section: str, heading: str) -> list[str]:
    lines = section.splitlines()
    start = next(
        (
            index
            for index, line in enumerate(lines)
            if line.strip().casefold() == f"**{heading.casefold()}**"
        ),
        None,
    )
    if start is None:
        return []

    bullets: list[str] = []
    for line in lines[start + 1 :]:
        stripped = line.strip()
        if stripped.startswith("**") or stripped.startswith("## ") or stripped == "---":
            break
        match = re.match(r"-\s+(.+)", stripped)
        if match:
            bullets.append(match.group(1).strip())
        elif bullets and line.startswith("  ") and stripped:
            bullets[-1] += f" {stripped}"
    return bullets


def _why_summary(section: str) -> str:
    match = re.search(r"^\*\*Why:\*\*\s*(.+?)(?:\n\s*\n|\n\*\*)", section, re.MULTILINE | re.DOTALL)
    return re.sub(r"\s+", " ", match.group(1)).strip() if match else ""


def _relevant_files(section: str, references: str, source: Path, repo_root: Path) -> list[str]:
    candidates = PROJECT_PATH.findall(section)
    candidates.extend(re.findall(r"\[[^\]]+\]\(([^)#]+)", references))
    result = set()
    for candidate in candidates:
        normalized = candidate.strip().replace("\\", "/").rstrip(".,;:")
        if not normalized or "://" in normalized:
            continue
        candidate_path = Path(normalized)
        resolved = (source.parent / candidate_path).resolve() if normalized.startswith("../") else (repo_root / candidate_path).resolve()
        if resolved.is_file() and (resolved == repo_root or repo_root in resolved.parents):
            result.add(resolved.relative_to(repo_root).as_posix())
    return sorted(result)


def load_backlog(todo_dir: Path) -> list[dict[str, Any]]:
    if not todo_dir.is_dir():
        raise FileNotFoundError(f"TODO directory not found: {todo_dir}")

    items: list[dict[str, Any]] = []
    repo_root = todo_dir.resolve().parents[1]
    for source in sorted(todo_dir.rglob("*.md")):
        if source.name.lower() == "readme.md":
            continue
        text = source.read_text(encoding="utf-8")
        headings = list(TASK_HEADING.finditer(text))
        for index, heading in enumerate(headings):
            section_end = (
                headings[index + 1].start()
                if index + 1 < len(headings)
                else len(text)
            )
            section = text[heading.end() : section_end]
            fields = _metadata(section)
            status, status_detail = _normalize_status(fields.get("status", ""))
            item_id = heading.group(1)
            references = fields.get("references", "").strip()
            depends = fields.get("depends_on", "").strip()
            if depends.casefold() in {"—", "–", "-", "none", "n/a"}:
                depends = ""
            dependency_labels = list(
                dict.fromkeys(
                    re.findall(r"`([^`]+)`", depends) + TASK_ID.findall(depends)
                )
            )
            items.append(
                {
                    "id": item_id,
                    "title": heading.group(2),
                    "category": source.stem.split("-", 1)[-1].replace("-", " ").title(),
                    "priority": fields.get("priority", "").strip() or "Unspecified",
                    "status": status,
                    "status_label": STATUS_LABELS[status],
                    "status_detail": status_detail,
                    "summary": _why_summary(section),
                    "scope": _section_bullets(section, "Scope"),
                    "depends": depends,
                    "dependency_labels": dependency_labels,
                    "dependency_ids": [
                        label for label in dependency_labels if TASK_ID.fullmatch(label)
                    ],
                    "references": references,
                    "relevant_files": _relevant_files(section, references, source, repo_root),
                    "reference_ids": TASK_ID.findall(fields.get("references", "")),
                    "acceptance": _acceptance_criteria(section),
                    "file": source.relative_to(todo_dir.parent.parent).as_posix(),
                    "line": text.count("\n", 0, heading.start()) + 1,
                    "anchor": item_id.lower(),
                }
            )

    seen: set[str] = set()
    for item in items:
        if item["id"] in seen:
            raise ValueError(f"Duplicate backlog task ID: {item['id']}")
        seen.add(item["id"])

    known_ids = seen
    for item in items:
        item["dependency_ids"] = [
            dependency
            for dependency in dict.fromkeys(item["dependency_ids"])
            if dependency in known_ids
        ]
        item["external_dependencies"] = [
            label
            for label in item["dependency_labels"]
            if label not in known_ids and label not in item["dependency_ids"]
        ]
        item["reference_ids"] = [
            reference
            for reference in dict.fromkeys(item["reference_ids"])
            if reference in known_ids
        ]

    dependents: dict[str, list[str]] = {item["id"]: [] for item in items}
    for item in items:
        for dependency in item["dependency_ids"]:
            dependents[dependency].append(item["id"])
    for item in items:
        item["dependent_ids"] = dependents[item["id"]]

    return items


def order_tasks(items: list[dict[str, Any]]) -> list[dict[str, Any]]:
    by_id = {item["id"]: item for item in items}
    source_order = {item["id"]: index for index, item in enumerate(items)}
    indegree = {
        item["id"]: sum(dependency in by_id for dependency in item["dependency_ids"])
        for item in items
    }
    dependents: dict[str, list[str]] = {item["id"]: [] for item in items}
    for item in items:
        for dependency in item["dependency_ids"]:
            if dependency in by_id:
                dependents[dependency].append(item["id"])

    priority_rank = {"P0": 0, "P1": 1, "P2": 2, "P3": 3}
    status_rank = {"ready": 0, "in_progress": 1, "blocked": 2, "proposed": 3, "icebox": 4, "done": 5}
    queue: list[tuple[int, int, int, str]] = []

    def enqueue(item_id: str) -> None:
        item = by_id[item_id]
        heappush(
            queue,
            (
                priority_rank.get(item["priority"], 99),
                status_rank.get(item["status"], 99),
                source_order[item_id],
                item_id,
            ),
        )

    for item_id, count in indegree.items():
        if count == 0:
            enqueue(item_id)

    ordered = []
    while queue:
        _, _, _, item_id = heappop(queue)
        ordered.append(by_id[item_id])
        for dependent in dependents[item_id]:
            indegree[dependent] -= 1
            if indegree[dependent] == 0:
                enqueue(dependent)

    ordered_ids = {item["id"] for item in ordered}
    ordered.extend(
        item
        for item in sorted(items, key=lambda value: source_order[value["id"]])
        if item["id"] not in ordered_ids
    )
    return ordered


def backlog_summary(items: list[dict[str, Any]]) -> dict[str, Any]:
    statuses = Counter(item["status"] for item in items)
    priorities = Counter(item["priority"] for item in items)
    total = len(items)
    done = statuses["done"]
    summary = {
        "total": total,
        "done": done,
        "in_progress": statuses["in_progress"],
        "blocked": statuses["blocked"],
        "ready": statuses["ready"],
        "proposed": statuses["proposed"],
        "icebox": statuses["icebox"],
        "unspecified": statuses["unspecified"],
        "completion_percent": round(done * 100 / total) if total else 0,
        "priorities": dict(sorted(priorities.items())),
    }
    summary["status_segments"] = _status_segments(statuses, total)
    return summary


def _status_segments(statuses: Counter[str], total: int) -> list[dict[str, Any]]:
    return [
        {
            "status": status,
            "label": STATUS_LABELS[status],
            "count": statuses[status],
            "percent": statuses[status] * 100 / total if total else 0,
        }
        for status in STATUS_LABELS
        if statuses[status] > 0
    ]


def backlog_categories(items: list[dict[str, Any]]) -> list[dict[str, Any]]:
    grouped: dict[str, list[dict[str, Any]]] = {}
    for item in items:
        grouped.setdefault(item["file"], []).append(item)

    categories = []
    for file, category_items in sorted(grouped.items()):
        statuses = Counter(item["status"] for item in category_items)
        total = len(category_items)
        done = statuses["done"]
        categories.append(
            {
                "file": file,
                "title": category_items[0]["category"],
                "total": total,
                "done": done,
                "in_progress": statuses["in_progress"],
                "blocked": statuses["blocked"],
                "ready": statuses["ready"],
                "completion_percent": round(done * 100 / total) if total else 0,
                "status_segments": _status_segments(statuses, total),
            }
        )
    return categories


def load_phase_progress(readme_path: Path) -> list[dict[str, Any]]:
    if not readme_path.is_file():
        return []
    text = readme_path.read_text(encoding="utf-8")
    return [
        {
            "number": int(match.group(1)),
            "title": match.group(2).strip(),
            "bar": match.group(3),
            "percent": int(match.group(4)),
            "status": match.group(5).strip(),
        }
        for match in PHASE_LINE.finditer(text)
    ]


def create_app(todo_dir: Path | None = None) -> Flask:
    app = Flask(__name__)
    source_todo_dir = todo_dir or Path(__file__).resolve().parents[1] / "docs" / "TODO"

    def get_items() -> list[dict[str, Any]]:
        return load_backlog(source_todo_dir)

    def get_readme_path() -> Path:
        return source_todo_dir.resolve().parents[1] / "README.md"

    @app.get("/")
    def index():
        try:
            items = get_items()
            phases = load_phase_progress(get_readme_path())
        except (OSError, UnicodeError, ValueError) as error:
            return render_template("error.html", error=str(error)), 500
        return render_template(
            "index.html",
            items=items,
            summary=backlog_summary(items),
            categories=backlog_categories(items),
            phases=phases,
        )

    @app.get("/dependencies")
    def dependencies():
        try:
            items = get_items()
        except (OSError, UnicodeError, ValueError) as error:
            return render_template("error.html", error=str(error)), 500
        return render_template("dependencies.html", items=items)

    @app.get("/task-order")
    def task_order():
        try:
            items = order_tasks(get_items())
        except (OSError, UnicodeError, ValueError) as error:
            return render_template("error.html", error=str(error)), 500
        return render_template("task_order.html", items=items)

    @app.get("/api/items")
    def api_items():
        try:
            items = get_items()
            phases = load_phase_progress(get_readme_path())
        except (OSError, UnicodeError, ValueError) as error:
            return jsonify({"error": str(error)}), 500
        return jsonify(
            {
                "items": items,
                "summary": backlog_summary(items),
                "categories": backlog_categories(items),
                "phases": phases,
            }
        )

    @app.get("/source")
    def source():
        relative = request.args.get("file", "")
        repository_root = source_todo_dir.resolve().parents[1]
        candidate = (repository_root / relative).resolve()
        if (
            not relative
            or candidate != repository_root and repository_root not in candidate.parents
            or candidate.suffix.lower() != ".md"
            or not candidate.is_file()
        ):
            abort(404)
        try:
            lines = candidate.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeError):
            abort(404)
        return render_template(
            "source.html",
            file=candidate.relative_to(repository_root).as_posix(),
            lines=lines,
        )

    return app


app = create_app()


if __name__ == "__main__":
    app.run(host="127.0.0.1", port=5000, debug=False)
