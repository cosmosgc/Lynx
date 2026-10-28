# AI workflow guide

This directory stores the repo-specific guidance that helps AI assistants work
productively in Lynx without drifting from the project’s actual constraints.

## How to work in this repository

1. Start from the repository truth: `README.md`, the relevant backlog in
   `docs/TODO/`, and the design docs that govern the subsystem.
2. Read the smallest relevant code path before proposing a fix.
3. Prefer existing conventions over new abstractions.
4. Write or update the relevant issue or TODO entry in the same change when the
   work changes the project’s known status.
5. Validate against the expected behavior the repository already documents.

## Issue and TODO conventions

- Use `docs/issues/` for issues and defects.
- Use `docs/TODO/` for planned work and acceptance criteria.
- Keep issues factual, stateful, and checkable.
- Keep TODOs explicit about priority, dependencies, and acceptance.
- When the repository already contains the required behavior, do not create a
  new “missing feature” issue; correct the documentation or the task record.

## Documentation standards

- Prefer direct, concrete documentation over broad narrative writing.
- Link the doc to the code, ADR, or backlog item it explains.
- State the system boundary and trust assumptions clearly.
- Distinguish between implemented behavior, planned behavior, and temporary
  development-only paths.
- Do not document a capability as implemented unless the repo and tests support
  it.

## Recommended code documentation structure

For a Rust crate or module, prefer a compact structure:

- `README.md` or module doc comment: purpose, scope, and system boundary
- local comments: invariants, assumptions, and edge-case handling
- tests: expected behavior and compatibility constraints
- ADR or doc links: design rationale, if the behavior is cross-cutting

A module doc should answer three questions in plain language:

1. What problem does this code solve?
2. What is the trust boundary and contract?
3. What invariants or failure modes must be preserved?

## AI-specific guidance

Write small, precise patches. Keep the scope locked to the task or issue. When a
change affects status or risk, update the relevant issue or TODO in the same
edit. Preserve the repository’s wording and checklist style; avoid inventing a
separate project taxonomy.

The project is intentionally conservative: if a behavior is not implemented or
not validated, it should be described as planned or incomplete rather than as a
finished capability.
