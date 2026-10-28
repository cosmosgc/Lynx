# OPS-001 — Metrics endpoint and dashboards missing

- **Status:** Open
- **Reported:** 2026-10-28
- **Resolved:** —
- **Related tasks:** [OPS-001](../TODO/60-operations-release.md#ops-001--metrics-endpoint),
  [OPS-002](../TODO/60-operations-release.md#ops-002--dashboards-and-alerts)
- **Affected workflow:** `crates/world_server`, CI/CD pipelines, monitoring

## Resolution

The project guide lists metrics and dashboards as a live-operations requirement, yet no metrics endpoint or monitoring infrastructure exists. Operators cannot observe system health, performance bottlenecks, or abuse indicators without manual instrumentation.

A production-ready observability layer must be added:
- Metrics collection for tick duration, entity counts, session counts, action processing rates
- Health and readiness endpoints beyond the current basic `/healthz`
- Audit log volume tracking and error rate monitoring
- Configuration for metrics export (Prometheus-compatible pushgateway or similar)

## Defect

The current `lynx-world` binary exposes:
- `GET /healthz` — basic liveness check
- `GET /readyz` - process readiness check (not continuous dependency health)
- WebSocket `/world` endpoint

There is no structured metrics endpoint, no performance monitoring for tick budgets, and no abuse detection dashboards. This violates the project's observability requirements in `docs/operations.md`.

## Impact

- Operators have no way to monitor server health, performance, or security incidents
- Tick budget regressions cannot be detected proactively; they only manifest as lag after users report it
- No visibility into authentication failures, connection drops, or abuse patterns
- This is a P1 risk: without metrics, the system cannot be safely scaled or monitored in production

## Acceptance criteria

- [ ] Metrics endpoint returns structured data (Prometheus format) for: tick duration, entity counts, session counts, action processing rates, error rates, and connection statistics
- [ ] Health/readyz endpoints include metric summaries and known issues
- [ ] Audit log volume tracked and reported; anomalies trigger alerts
- [ ] Configuration supports metrics collection mode with configurable retention and export format
- [ ] Integration tests verify metric emission on normal and failure paths
- [ ] Dashboard documentation added to `docs/operations.md`

## Notes

This issue is linked to `OPS-001` in the TODO backlog. Metrics should be collected at multiple layers: world gateway (protocol), world server (simulation), and persistence adapters (database). Consider using a metrics library like Prometheus client-side SDK or OpenTelemetry for structured collection.
