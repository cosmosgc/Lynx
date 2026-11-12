# PST-001 — Account and character persistence

**Status: Done.** The backlog records this outcome: Nakama-authenticated PostgreSQL character persistence. All acceptance criteria below are checked complete.

## Original task scope

- Implement `AuthService`, `CharacterService`, and the `WorldService`
  zone-entry/leave calls used by character selection against PostgreSQL, or
  through the chosen Nakama-backed path. Record the choice in ADR 0018 or a
  successor.
- Use migrations `0001` tables; add a migration only for missing fields.
- Preserve ownership checks: a session may only select/create characters it
  owns; character IDs are server-generated stable IDs.
- Hash or delegate credentials; never store plaintext secrets. Keep tokens out
  of logs and error details.
- Define transaction boundaries for create/select/delete and make retries
  idempotent.
- Provide a contract test suite shared by memory and persistent adapters, and
  skip the live PostgreSQL test cleanly when `LYNX_PG_*` is unset.

## Acceptance evidence

- [x] Creating a character, restarting the service, and selecting it again
      returns the same character with the same owner.
- [x] A session cannot select, rename, or delete another account's character.
- [x] Duplicate create/select retries do not duplicate rows.
- [x] The persistent adapter and `backend_memory` pass the same contract tests.
- [x] No credential or session secret appears in logs or error chains.

## Source

See [`20-persistence-backend.md`](../20-persistence-backend.md) for the full task history and implementation notes.
