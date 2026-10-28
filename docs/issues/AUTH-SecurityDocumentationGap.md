# AUTH-001 — Security documentation and policy gap (false positive)

- **Status:** Closed
- **Reported:** 2026-10-28
- **Resolved:** 2026-10-28
- **Related tasks:** [QUA-009](../TODO/70-testing-quality.md#qua-009--documentation-and-code-consistency-sweep)
- **Affected workflow:** Documentation, security review

## Resolution

The repository already contains [`docs/SECURITY.md`](../SECURITY.md). The report was caused by stale documentation text in [`docs/security.md`](../security.md) that still said the file would be added before deployment. The security policy is present, and the stale reference has been corrected to point at the actual document.

This issue is not a missing-policy defect; it was a documentation drift issue and is now closed.

## Defect

The stale note in `security.md` said:
> "Do not open a public issue for a vulnerability. Contact the maintainers privately with reproduction steps, affected version, and impact. See SECURITY.md (to be added before any public deployment)."

That text no longer matches the repository state. The actual policy already exists in `docs/SECURITY.md`.

## Impact

- No security governance gap was present in the repository itself.
- The only issue was stale cross-reference text and a stale issue record.
- This did not prevent contributors from following the vulnerability-reporting policy.

## Acceptance criteria

- [x] `SECURITY.md` exists in the `docs/` directory
- [x] Cross-references point to the current document
- [x] The stale issue text was corrected
- [x] No repository-level security policy gap remains