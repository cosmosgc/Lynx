# ADR 0017: Content manifest version compatibility

- **Status:** Accepted
- **Date:** 2026-10-24

## Context

Phase 5 adds a publishing pipeline. A bundle already declares a content
version, an engine requirement, one schema version per definition, and a
SHA-256 hash per definition and locale catalog. Nothing compared two manifests,
so a publisher could ship a changed definition without a version bump, remove a
definition that clients still reference, or move the content version backwards.

Clients and servers need a mechanical rule for whether a candidate bundle is a
valid successor to the one already deployed. The rule has to be precise enough
for CI, expressive enough for legitimate breaking releases, and conservative
enough that an accidental edit cannot pass as a compatible update.

## Decision

- **The manifest is the unit of comparison.** `engine_content::diff` compares a
  base `ContentManifest` with a candidate and reports added, removed, and changed
  definitions and locales. It does not read bundle files; hashes in the
  manifest are the source of truth.
- **Version bumps follow the compatibility of the change, not its size.**
  - Adding a definition or locale is additive and requires a minor bump.
  - Changing definition bytes or a locale catalog requires a minor bump.
  - Removing a definition or locale, or changing a definition kind, is breaking
    and requires a major bump.
  - A manifest with no byte changes requires no content bump.
- **A byte change also bumps the definition's schema version.**
  `SchemaVersion` is a `major.minor` value. Reusing the old version hides the
  change from instance migration and from clients that cache definitions.
- **Incompatible metadata is an error; suspicious metadata is a warning.**
  - Errors: version regression, declared bump smaller than required, definition
    bytes changed without a definition bump, definition version regression.
  - Warnings: removal, kind change, locale removal, path move, definition bump
    without a byte change, engine requirement change.
- **Breaking changes still require the major bump to be compatible.** Removal
  and kind change are warnings rather than unconditional errors so that a
  deliberate `2.0.0` release is valid. The `version_bump_too_small` error is
  what rejects the same change published as `1.1.0`.
- **The diff is a publish gate, not a merge tool.** It never rewrites
  manifests, resolves references, or decides whether a change is desirable. It
  reports the full change set and the smallest safe bump so a publisher can
  show the exact rule that failed.
- **The CLI mirrors the library.** `lynx content diff --base --next` emits the
  same structured result as JSON and a compact human summary. Like
  `lynx validate`, it exits successfully for an incompatible diff; CI inspects
  the `ok` field.

## Consequences

- CI can reject an unversioned edit before a bundle reaches a zone.
- A breaking release is expressible without weakening the gate: remove the
  definition, declare a major content bump, and the diff is compatible.
- Path moves and engine requirement changes are visible without blocking a
  release, because neither changes runtime meaning on its own.
- The rule does not detect semantic changes that leave bytes identical (for
  example, editing a definition and reverting it). Such a change is invisible
  to every hash-based pipeline and is out of scope.
- The diff compares manifests, not live instance data. Instance migration
  remains a separate concern keyed by the definition schema version.
