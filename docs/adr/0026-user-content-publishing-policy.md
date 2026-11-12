# ADR 0026: User-content publishing policy

- **Status:** Accepted
- **Date:** 2026-11-12

## Decision

Player-created content starts as private drafts owned by the submitting
principal. Publishing requires an explicit moderation approval and records an
append-only audit event with owner, revision, reviewer, action, and outcome.
Revocation prevents delivery to new sessions while retaining the audit history.
Public uploads and user-authored executable code are out of scope until the
moderation, reporting, quota, and legal-retention paths are implemented.

## Consequences

- Draft visibility is owner-only; a publish request is not itself publication.
- Revisions are immutable once submitted for review, and edits create a new
  revision requiring review.
- Revocation affects future content delivery and does not rewrite durable audit
  records or content ownership history.
