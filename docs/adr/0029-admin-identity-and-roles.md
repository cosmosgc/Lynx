# ADR 0029: Admin identity and roles

- **Status:** Accepted
- **Date:** 2026-11-12

## Decision

Admin authentication uses a configured external OpenID Connect issuer, separate
from player/Nakama sessions. Lynx authorizes admin roles and scoped
capabilities after identity validation; the provider authenticates a person
but does not grant Lynx permissions. Require MFA at the chosen provider for
production admin access, short-lived sessions, revocation, and append-only
audit for privileged actions. Provider selection and deployment-specific
claims mapping belong to the environment configuration, not the game client.

## Consequences

- A provider outage fails closed for admin operations and does not affect
  player authentication.
- Role changes and revocation must take effect in Lynx authorization state;
  a valid identity token alone is insufficient.
- Production enablement requires a configured issuer, MFA policy, secure
  callback/origin configuration, and an audited break-glass procedure.
