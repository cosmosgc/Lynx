# OPS-004 — Staging publish, rollout, and upgrade workflow missing

- **Status:** Open
- **Reported:** 2026-10-28
- **Resolved:** —
- **Related tasks:** [OPS-003](../TODO/60-operations-release.md#ops-003--backups-and-restore-drills), 
  [OPS-005](..//TODO/60-operations-release.md#ops-005--content-publish-rollout-and-upgrade-workflow)
- **Affected workflow:** CI/CD pipelines, deployment automation, `tools/project_cli`

## Resolution

The project guide explicitly lists staging publish, rollout, and rollback automation as a Phase 8 stability requirement (line 217 of README), yet no such workflow exists. Content publishers need a staged release process that validates before production exposure and supports safe rollbacks.

A staging pipeline must be added:
- Staging database isolation with content-only writes for validation
- Automated pre-publish checks (content validation, migration testing, compatibility verification)
- Rollback mechanism that reverts to the previous known-good state
- Dry-run mode that simulates a publish without actually applying changes

## Defect

The `lynx` CLI has `content diff`, `content publish-check`, and `doctor` commands, but no staged publish workflow. Publishing content directly to production with no rollback path violates operational best practices and the project's deployment requirements. The documentation describes this capability (line 194 of README: "A staging publish, rollout, and rollback automation" under Phase 6), yet it does not exist in any form.

## Impact

- No safe way to release content changes without risk of breaking existing worlds
- Operators lack automated validation before production exposure
- Rollback capabilities are missing for both content and infrastructure changes
- This is a P1 risk: unsafe deployment process could lead to data corruption or service degradation

## Acceptance criteria

- [ ] `lynx staging publish` command validates content against staging database without affecting production
- [ ] Pre-publish checks include: manifest validation, migration testing, compatibility verification, and schema versioning
- [ ] Rollback mechanism restores previous known-good state (content bundles + migrations)
- [ ] Dry-run mode simulates entire publish pipeline without applying changes
- [ ] Audit trail of all publishes, rollbacks, and validations is maintained