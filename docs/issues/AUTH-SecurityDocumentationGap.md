# AUTH-001 - Security reporting policy gap

- **Status:** Open
- **Reported:** 2026-10-28
- **Related tasks:** [QUA-009](../TODO/70-testing-quality.md#qua-009--documentation-and-code-consistency-sweep)
- **Affected workflow:** Vulnerability reporting and security review

## Defect

An earlier version of this record incorrectly closed the issue, claiming that
`docs/SECURITY.md` existed. Neither that file nor a repository-root `SECURITY.md`
is present. `docs/security.md` advises private reporting but does not provide a
private contact or a response policy.

## Acceptance criteria

- [ ] Maintainers establish a private reporting channel and response policy.
- [ ] A checked-in security policy identifies the channel and expectations.
- [ ] Security documentation links to the actual policy.
