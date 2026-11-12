# ADR 0028: Platform support policy

- **Status:** Accepted
- **Date:** 2026-11-12

## Decision

Linux x86-64 is the server deployment target. Windows 10/11 x86-64 is the
primary development target. Desktop clients remain experimental on Windows and
Linux until each passes the graphical build, smoke, packaging, and update gates
in `docs/platforms.md`; macOS remains experimental and browser, mobile, and
console targets are unsupported. The documented client/server hardware and
network figures are initial profiling budgets, not minimum-system guarantees.

## Consequences

- A platform gains a support promise only after its release gates pass in CI.
- Platform-specific rendering and packaging work stays behind the client
  boundary; server and domain crates remain headless.
- Hardware budgets must be measured on representative machines before they
  become marketing or support requirements.
