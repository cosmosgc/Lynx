# CNT-014 — Content migration and upgrade tooling

- **Status:** Done
- **Completed:** 2026-11-12
- **Source task:** [Content migration and upgrade tooling](../50-content-editor-tooling.md#cnt-014--content-migration-and-upgrade-tooling)
- **Design:** [ADR 0031](../../adr/0031-content-migration-artifact-and-rollback.md)

## Delivered

- `engine_content` validates reviewed RON migration plans bound to a source
  manifest SHA-256, content versions, definition schema versions, and canonical
  definition/catalog targets.
- `lynx migrate apply` stages the complete candidate, applies ordered
  expected-value JSON Pointer operations, validates the candidate content and
  manifest compatibility, and reports dry-run results before any project
  changes.
- Apply retains the exact source content tree and commits the validated
  candidate as one directory swap. Rollback restores the source bytes and
  refuses to overwrite an edited candidate.
- CLI integration coverage exercises dry-run, failure atomicity, apply, and
  rollback in a generated project.

Authored content files are in scope. Persisted-instance transformations remain
in PST-013 and use the backend transaction boundary.

## Verification

- `cargo test -p engine_content`
- `cargo test -p project_cli`
- `cargo clippy -p project_cli --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- Regenerated `docs/cli-reference.md` from the command tree.
