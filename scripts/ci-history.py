#!/usr/bin/env python3
"""Summarize recent GitHub Actions job durations and outcomes."""

import argparse
from datetime import datetime
import json
import os
from pathlib import Path
import statistics
import sys
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, urlopen


def github_get(api_url: str, token: str, endpoint: str, query: dict | None = None) -> dict:
    url = f"{api_url.rstrip('/')}/{endpoint}"
    if query:
        url += "?" + urlencode(query)
    request = Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    with urlopen(request, timeout=20) as response:
        return json.load(response)


def duration_seconds(job: dict) -> float | None:
    if not job.get("started_at") or not job.get("completed_at"):
        return None
    start = datetime.fromisoformat(job["started_at"].replace("Z", "+00:00"))
    end = datetime.fromisoformat(job["completed_at"].replace("Z", "+00:00"))
    return max(0.0, (end - start).total_seconds())


def jobs_for_run(api_url: str, token: str, repository: str, run_id: int) -> list[dict]:
    payload = github_get(
        api_url, token, f"repos/{repository}/actions/runs/{run_id}/jobs", {"per_page": 100}
    )
    return payload.get("jobs", [])


def format_duration(seconds: float | None) -> str:
    if seconds is None:
        return "n/a"
    minutes, seconds = divmod(round(seconds), 60)
    return f"{minutes}m {seconds:02d}s"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api-url", default=os.environ.get("GITHUB_API_URL", "https://api.github.com"))
    parser.add_argument("--repository", default=os.environ.get("GITHUB_REPOSITORY"))
    parser.add_argument("--workflow", default="ci.yml")
    parser.add_argument("--branch", default=os.environ.get("CI_HISTORY_BRANCH", os.environ.get("GITHUB_REF_NAME")))
    parser.add_argument("--run-id", type=int, default=os.environ.get("GITHUB_RUN_ID"))
    parser.add_argument("--token", default=os.environ.get("GITHUB_TOKEN"))
    parser.add_argument("--summary", default=os.environ.get("GITHUB_STEP_SUMMARY"))
    parser.add_argument("--limit", type=int, default=20)
    args = parser.parse_args()

    missing = [name for name in ("repository", "branch", "run_id", "token", "summary") if not getattr(args, name)]
    if missing:
        parser.error("missing required GitHub context: " + ", ".join(missing))
    if args.limit < 1 or args.limit > 100:
        parser.error("--limit must be between 1 and 100")

    try:
        workflow = f"repos/{args.repository}/actions/workflows/{args.workflow}/runs"
        runs = github_get(
            args.api_url,
            args.token,
            workflow,
            {"branch": args.branch, "status": "completed", "per_page": 100},
        ).get("workflow_runs", [])

        history = []
        for run in runs:
            if run["id"] == args.run_id:
                continue
            history.append((run["id"], jobs_for_run(args.api_url, args.token, args.repository, run["id"])))
            if len(history) == args.limit:
                break
        current_jobs = jobs_for_run(args.api_url, args.token, args.repository, args.run_id)

        tracked = ("Rust checks (Ubuntu)", "Rust checks (Windows)")
        lines = [
            "### CI duration and job outcome trend",
            "",
            f"Compared with the previous {len(history)} completed `{args.branch}` run(s) of this workflow.",
            "Failure rate is the share of completed job attempts whose conclusion was not `success`; it is not a test-level flakiness measure.",
            "",
            "| Job | Current | Previous median | Change vs median | Previous failures |",
            "|---|---:|---:|---:|---:|",
        ]
        for name in tracked:
            current = next((job for job in current_jobs if job.get("name") == name), None)
            prior = [job for _, jobs in history for job in jobs if job.get("name") == name]
            durations = [value for job in prior if (value := duration_seconds(job)) is not None]
            median = statistics.median(durations) if durations else None
            current_duration = duration_seconds(current) if current else None
            if current_duration is None or median is None:
                change = "n/a"
            else:
                delta = current_duration - median
                change = f"{delta:+.0f}s ({delta / median:+.0%})" if median else f"{delta:+.0f}s"
            finished = [job for job in prior if job.get("conclusion")]
            failures = sum(job.get("conclusion") != "success" for job in finished)
            failure_rate = f"{failures}/{len(finished)} ({failures / len(finished):.0%})" if finished else "n/a"
            lines.append(
                f"| {name} | {format_duration(current_duration)} | {format_duration(median)} | {change} | {failure_rate} |"
            )
        lines.extend(("", "This is observational only: it does not retry tests or change CI pass/fail outcomes."))
        with Path(args.summary).open("a", encoding="utf-8") as summary:
            summary.write("\n".join(lines) + "\n")
        print("\n".join(lines))
    except (HTTPError, URLError, TimeoutError, OSError, KeyError, ValueError) as error:
        print(f"Unable to retrieve CI history: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
