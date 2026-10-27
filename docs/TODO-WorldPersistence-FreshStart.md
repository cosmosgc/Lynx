# ⚠️ TODO: World Persistence - Implement `--fresh` Restart Mode

This document outlines a necessary improvement to world state persistence and loading, currently tested in `crates/lynx_world/tests/restart.rs`.

## The Problem
Currently, when restarting or resuming the world simulation, the system relies on the latest written snapshot file (the "played" state). If the intended authoritative source of truth is the **authored** state (e.g., what was defined in content files and represents a 'pristine' setup), manually restoring this authored state requires deleting the saved snapshot file entirely. This process is fragile:

1.  It relies on manual intervention (deleting the file).
2.  There is a small, non-deterministic window (`~10s` before first interval save) where the initial state has not been written, potentially causing crashes or inconsistent data if recovery logic isn't perfect.

## Desired Behavior: The `--fresh` Switch
A new command-line switch, tentatively named `--fresh`, should be implemented in the primary world loading sequence (`main`).

When activated, the system must:
1.  **Ignore** the existing persisted snapshot file (the "played" state).
2.  Rebuild the authoritative *authored* world from scratch using standard content sources.
3.  Save this newly constructed authored world back to the snapshot store, effectively overwriting the played state with a guaranteed pristine version.

This provides an explicit, recoverable way for developers or operators to reset the running world simulation to its defined base state without data loss due to incomplete saves, and without needing manual file system manipulation.

## Technical Notes & References
*   **Test Coverage:** The logic for this desired behavior is already validated within `crates/lynx_world/tests/restart.rs` in the test function `a_fresh_start_replaces_the_played_world_with_the_authored_one`. This test serves as an excellent reference for the required functionality.
*   **Scope:** The implementation must ensure that saving the authored state does not break any existing persistence contract or introduce new race conditions during world initialization.

## Action Items (TODO)
- Implement `--fresh` switch handling in the main entry point (`lynx-world`).
- Update documentation (e.g., `docs/getting-started.md`) to explain the existence and usage of the `--fresh` flag.