#!/usr/bin/env python3
"""Require protocol version and changelog updates for wire-contract edits."""

from __future__ import annotations

import re
import subprocess
import sys


WIRE_CONTRACT_FILES = (
    "clients/lynx_client/src/online.rs",
    "clients/lynx_client/src/world_state.rs",
    "crates/engine_net/src/envelope.rs",
    "crates/engine_net/src/protocol.rs",
    "crates/engine_net/src/replication.rs",
    "crates/world_gateway/src/replication.rs",
    "crates/world_server/src/runtime.rs",
)
VERSION_PATTERN = re.compile(r'pub const PROTOCOL_VERSION: &str = "([0-9]+\.[0-9]+\.[0-9]+)";')


def git(*args: str) -> str:
    return subprocess.check_output(["git", *args], text=True).strip()


def protocol_version(revision: str) -> tuple[int, int, int]:
    source = git("show", f"{revision}:crates/engine_net/src/protocol.rs")
    match = VERSION_PATTERN.search(source)
    if match is None:
        raise ValueError(f"could not read PROTOCOL_VERSION at {revision}")
    return tuple(int(part) for part in match.group(1).split("."))


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: check_protocol_changes.py <base-sha> <head-sha>", file=sys.stderr)
        return 2

    base, head = sys.argv[1:]
    try:
        changed = set(git("diff", "--name-only", f"{base}...{head}").splitlines())
        wire_changes = sorted(changed.intersection(WIRE_CONTRACT_FILES))
        if not wire_changes:
            print("No wire-contract source changes; protocol version gate passed.")
            return 0

        old_version = protocol_version(base)
        new_version = protocol_version(head)
        if new_version <= old_version:
            raise ValueError(
                f"wire-contract files changed ({', '.join(wire_changes)}), but "
                f"PROTOCOL_VERSION did not increase ({'.'.join(map(str, old_version))} "
                f"-> {'.'.join(map(str, new_version))})"
            )
        if "CHANGELOG.md" not in changed:
            raise ValueError(
                f"wire-contract files changed ({', '.join(wire_changes)}), "
                "but CHANGELOG.md is unchanged"
            )
    except (subprocess.CalledProcessError, ValueError) as error:
        print(f"Protocol compatibility gate failed: {error}", file=sys.stderr)
        return 1

    print(
        "Protocol compatibility gate passed: wire-contract changes include "
        f"version {'.'.join(map(str, old_version))} -> "
        f"{'.'.join(map(str, new_version))} and a changelog entry."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
