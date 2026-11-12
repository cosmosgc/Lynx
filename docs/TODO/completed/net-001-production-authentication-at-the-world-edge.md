# NET-001 — Production authentication at the world edge

**Status: Done.** All acceptance criteria below are checked complete in the source task.

## Original task scope

- Validate the presented session with the identity backend on admission,
  including expiry, revocation, and refresh semantics.
- Resolve account → owned characters → selected character; reject sessions that
  try to control a character they do not own.
- Add a server-side concurrent-session policy with `per_character` and
  `per_account` options. Keep the current per-character behavior as the
  default; enforce per-account limits using only authenticated account claims,
  including retained resume sessions, without replacing an existing session.
- Keep static token mode explicitly development-only: require loopback binding
  or an opt-in flag and log a startup warning.
- Map identity failures to stable error codes without leaking backend details.
- Decide and document token lifetimes, clock skew, and revalidation cadence.
- Keep the gateway free of identity-provider types; pass trusted claims across
  the seam.

**Implementation:** `--auth-mode nakama` constructs the selected
`NakamaPostgresBackend` behind `auth::IdentityBackend`. Fresh admissions validate
the Nakama bearer session, parse exactly one `X-Lynx-Character` `chr_` selection,
and resolve active ownership in the account-scoped PostgreSQL store before
creating trusted non-staff gateway claims. Identity failures have stable redacted
HTTP codes and foreign selections emit a structured admission audit record.
Verification runs on Tokio's blocking pool, so provider and PostgreSQL I/O do
not block socket tasks. Resume tokens remain local; their configured TTL bounds
the upstream-revocation observation gap. The deliberate population policy is to
admit only an already-present authoritative actor; persistent rows neither spawn
actors nor fall back to demo characters. New-character loading and multi-zone
routing remain separate backlog work. The client now persists its device ID and
rotating refresh token in the OS credential store, refreshes on login, supports
`--logout`, and can list/create/select characters through the authenticated
world HTTP API. Character creation and selection do not yet provision or load a
live actor, and the client has no character onboarding UI.

## Acceptance evidence

- [x] An expired, revoked, malformed, or unknown session is rejected before a
      gateway session is created.
- [x] Selecting a character owned by another account is rejected and audited.
- [x] The configured session policy rejects a duplicate character or account
      session as appropriate, including during resume windows, without
      disconnecting the existing session.
- [x] Static-token mode cannot be enabled on a non-loopback listener without an
      explicit development flag.
- [x] An integration test with a fake identity backend covers login, expiry, and
      ownership failure.
- [x] No token or credential appears in logs, errors, or debug output.

## Source

See [`30-networking-security.md`](../30-networking-security.md) for the full task history and implementation notes.
