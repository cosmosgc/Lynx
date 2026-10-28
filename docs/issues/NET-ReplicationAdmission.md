# NET — Admission consumed pending replication changes

- **Status:** Fixed locally; CI confirmation pending
- **Affected workflow:** `crates/world_gateway`
- **Related tasks:** [QUA-014](../TODO/70-testing-quality.md), [NET-006](../TODO/30-networking-security.md)

## Defect and fix

Connecting or resuming a session synchronized the shared world baseline before
the next tick, discarding changes that existing sessions had not yet received.
The gateway now retains those changes until the next tick, while building the
admitted session's snapshot from current state. A regression test covers both
connect and resume with a pending door change.

## Verification

- [x] Pending state is sent to the existing observer after connect and resume.
- [ ] CI confirms the workspace gates.
