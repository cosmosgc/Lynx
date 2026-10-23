# ADR 0008: Behavior runtime and scripting boundary

- **Status:** Accepted (Phase 1 scope)
- **Date:** 2026-10-23

## Context

Creators need to author behavior without forking the engine. Arbitrary
editor-authored code on the server is a security and determinism risk.

## Decision

- Phase 1 ships **data-driven behavior graphs** (state machines and behavior
  trees) as RON definitions interpreted by `engine_sim`. Nodes have typed
  conditions, selectors, sequences, timers, and actions.
- Behavior graphs can only emit actions through the same validated action
  pipeline used by players. They cannot mutate components, inventories, or
  world state directly.
- Execution is bounded: per-tick node budget, per-behavior wall-clock budget,
  maximum graph depth, and maximum pathfinding requests. Exceeding a budget
  yields a structured diagnostic and suspends the behavior.
- Graphs are versioned and validated at load. Cycles, missing node references,
  and unsupported action parameters fail validation.
- **WebAssembly** modules are the planned escape hatch for advanced creators.
  They will run in a sandbox with no filesystem, process, network, or clock
  access, a memory cap, and an explicit host-call allowlist. They are not
  enabled until the sandbox, determinism, and operational story are tested.
- Arbitrary interpreted languages (Lua, JavaScript) are not embedded in the
  server. If one is added later, it must meet the same sandbox and budget
  requirements.

## Consequences

- Behavior content is reviewable, diffable, and safe to hot-reload.
- Advanced behavior needs new node types or a future WASM module, not a
  privileged script.
- Deterministic simulation tests can replay behavior without a scripting
  runtime.
