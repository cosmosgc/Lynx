# AUTH-001 - Security reporting policy gap

- **Status:** Open
- **Reported:** 2026-10-28
- **Related tasks:** [QUA-009](../TODO/70-testing-quality.md#qua-009--documentation-and-code-consistency-sweep)
- **Affected workflow:** Vulnerability reporting and security review

## Defect

An earlier version of this record incorrectly closed the issue, claiming that
`docs/SECURITY.md` existed. At that time neither that file nor a repository-root
`SECURITY.md` was present. The new root [security guidance](../../SECURITY.md)
provides interim reporting advice, but no verified private contact or response
policy. `docs/security.md` links to that guidance.

## Remaining decisions

Maintainers need to establish and verify a private reporting channel and decide
the response and disclosure expectations, including supported versions and any
acknowledgement or remediation targets. The interim guidance does not make
those decisions or satisfy the acceptance criteria below.

## Acceptance criteria

- [ ] Maintainers establish a private reporting channel and response policy.
- [ ] A checked-in security policy identifies the channel and expectations.
- [ ] Security documentation links to the actual policy.
