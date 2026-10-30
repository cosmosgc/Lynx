#!/usr/bin/env python3
"""Record a CI job's elapsed time in the GitHub Actions run summary."""

import argparse
import os
from pathlib import Path
import sys
import time


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("phase", choices=("start", "finish"))
    parser.add_argument("job")
    args = parser.parse_args()

    temp = os.environ.get("RUNNER_TEMP")
    if not temp:
        parser.error("RUNNER_TEMP must be set by the GitHub Actions runner")
    marker = Path(temp) / f"lynx-ci-{args.job}-started-at"

    if args.phase == "start":
        marker.write_text(f"{time.time():.6f}\n", encoding="ascii")
        return 0

    try:
        started = float(marker.read_text(encoding="ascii").strip())
    except (OSError, ValueError) as error:
        print(f"Unable to read CI duration start marker {marker}: {error}", file=sys.stderr)
        return 1

    elapsed = max(0, round(time.time() - started))
    minutes, seconds = divmod(elapsed, 60)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if not summary:
        print("GITHUB_STEP_SUMMARY must be set by the GitHub Actions runner", file=sys.stderr)
        return 1

    with Path(summary).open("a", encoding="utf-8") as output:
        output.write("### CI job duration\n\n| Job | Elapsed |\n|---|---:|\n")
        output.write(f"| {args.job} | {minutes}m {seconds:02d}s |\n")
    print(f"{args.job} elapsed: {minutes}m {seconds:02d}s")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
