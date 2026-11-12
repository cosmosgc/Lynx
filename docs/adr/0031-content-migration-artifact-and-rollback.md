# ADR 0031: Reviewed content migrations and rollback

- **Status:** Accepted
- **Date:** 2026-11-12

Content migrations are versioned, declarative plans that identify their source
bundle and target content version, then apply ordered data transformations to
definitions and locale catalogs. A dry run must stage and validate the complete
candidate without modifying project files; apply rechecks source hashes, keeps
an exact rollback snapshot, and commits only after whole-project validation.
Rollback restores the captured source bytes rather than attempting to reverse
operations. Plans contain no executable code or arbitrary filesystem paths.

Each definition step names a stable definition ID and its exact source and
target schema versions. Each locale step names a canonical locale. Operations
address data with JSON Pointers, declare the expected prior value, and replace,
remove, or rename a value; the engine owns definition IDs, versions, locale
identity, and file paths. A stale plan therefore fails before any source file is
changed, while the expected value makes the plan's effect reviewable.

This tooling owns authored content files. Persisted instance migration remains
in PST-013: it uses the same definition schema transition but runs inside the
backend's transactional persistence boundary, as required by ADR 0017. A content
file rollback never claims to roll back already migrated live instances.

## Considered options

- **Run user-authored scripts:** Rejected because executable migration code
  weakens reproducibility and makes CI and preview unsafe.
- **Infer migrations from manifest diffs:** Rejected because hashes identify
  changed files but cannot explain how data should be transformed.
- **Reverse operations on rollback:** Rejected because operations can be lossy;
  retaining exact source bytes makes rollback deterministic.
