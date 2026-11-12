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


MIN_SUCCESSFUL_SAMPLES = 10
REGRESSION_FRACTION = 0.50
REGRESSION_MIN_SECONDS = 300


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
    parser.add_argument("--limit", type=int, default=50)
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

        tracked = (
            "Rust checks (Ubuntu)",
            "Rust checks (Windows)",
            "Rust checks (macOS)",
            "PostgreSQL 17 migrations",
        )
        lines = [
            "### CI duration and job outcome trend",
            "",
            f"Compared with the previous {len(history)} completed `{args.branch}` run(s) of this workflow.",
            "Non-success rate is the share of completed job attempts whose conclusion was not `success` (including cancellations); it is not a test-level flakiness measure.",
            f"Duration gate: a current successful job is flagged when it exceeds the successful-run median by more than max({REGRESSION_FRACTION:.0%} of the median, {REGRESSION_MIN_SECONDS // 60} minutes), after {MIN_SUCCESSFUL_SAMPLES} prior successful samples.",
            "",
            "| Job | Current | Previous successful median (n) | Gate threshold | Change vs median | Previous non-success |",
            "|---|---:|---:|---:|---:|---:|",
        ]
        regressions = []
        for name in tracked:
            current = next((job for job in current_jobs if job.get("name") == name), None)
            prior = [job for _, jobs in history for job in jobs if job.get("name") == name]
            durations = [
                value
                for job in prior
                if job.get("conclusion") == "success"
                and (value := duration_seconds(job)) is not None
            ]
            median = statistics.median(durations) if durations else None
            current_duration = duration_seconds(current) if current else None
            if current_duration is None or median is None:
                change = "n/a"
            else:
                delta = current_duration - median
                change = f"{delta:+.0f}s ({delta / median:+.0%})" if median else f"{delta:+.0f}s"
            threshold = None
            if median is not None and len(durations) >= MIN_SUCCESSFUL_SAMPLES:
                threshold = median + max(median * REGRESSION_FRACTION, REGRESSION_MIN_SECONDS)
            if threshold is None:
                gate = f"warming up ({len(durations)}/{MIN_SUCCESSFUL_SAMPLES})"
            else:
                gate = format_duration(threshold)
                if (
                    current_duration is not None
                    and current
                    and current.get("conclusion") == "success"
                    and current_duration > threshold
                ):
                    regressions.append((name, current_duration, threshold))
            finished = [job for job in prior if job.get("conclusion")]
            non_success = sum(job.get("conclusion") != "success" for job in finished)
            outcome_rate = f"{non_success}/{len(finished)} ({non_success / len(finished):.0%})" if finished else "n/a"
            lines.append(
                f"| {name} | {format_duration(current_duration)} | {format_duration(median)} ({len(durations)}) | {gate} | {change} | {outcome_rate} |"
            )
        lines.extend(("", "Job outcome history is observational and does not retry tests. Duration gating applies only to current successful jobs with a mature successful-run baseline."))
        if regressions:
            lines.extend(("", "Duration regression(s) exceeded the gate:"))
            lines.extend(
                f"- `{name}`: {format_duration(current)} exceeds {format_duration(threshold)}."
                for name, current, threshold in regressions
            )
        with Path(args.summary).open("a", encoding="utf-8") as summary:
            summary.write("\n".join(lines) + "\n")
        print("\n".join(lines))
    except (HTTPError, URLError, TimeoutError, OSError, KeyError, ValueError) as error:
        warning = f"CI history unavailable; duration gate skipped: {error}"
        print(warning, file=sys.stderr)
        with Path(args.summary).open("a", encoding="utf-8") as summary:
            summary.write(f"\n> Warning: {warning}\n")
        return 0
    return 1 if regressions else 0


if __name__ == "__main__":
    raise SystemExit(main())
