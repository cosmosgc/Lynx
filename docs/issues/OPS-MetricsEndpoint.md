# OPS-001 — Metrics endpoint and dashboards missing

- **Status:** Open
- **Reported:** 2026-10-28
- **Resolved:** —
- **Related tasks:** [OPS-001](../TODO/60-operations-release.md#ops-001--metrics-endpoint),
  [OPS-002](../TODO/60-operations-release.md#ops-002--dashboards-and-alerts)
- **Affected workflow:** `crates/world_server`, CI/CD pipelines, monitoring

## Current status

The service now exposes a partial Prometheus endpoint on both gameplay and trusted admin listeners. See [operations](../operations.md#metrics-and-logs) for the implemented metrics, security constraints, scrape example, and unsupported families. Dashboards and substantial instrumentation remain open.

A production-ready observability layer must be added:
- Metrics collection for tick duration, entity counts, session counts, action processing rates
- Health and readiness endpoints beyond the current basic `/healthz`
- Audit log volume tracking and error rate monitoring
- Configuration for metrics export (Prometheus-compatible pushgateway or similar)

## Defect

The current `lynx-world` binary exposes:
- `GET /healthz` — basic liveness check
- `GET /readyz` - process readiness check (not continuous dependency health)
- `GET /metrics` — process-local partial metrics
- WebSocket `/world` endpoint

Tick execution overruns, scheduler deadline misses, and bounded rate-limit
rejections are counted, but several requested metric families and abuse
detection dashboards remain unavailable.

## Impact

- Existing counters give only partial visibility into health, performance, and abuse.
- Scheduler delays and several failure modes remain unmeasured; dashboards and tested alert rules are missing.
- This remains a P1 operational risk for production use.

## Acceptance criteria

- [ ] Metrics endpoint returns structured data (Prometheus format) for: tick duration, entity counts, session counts, action processing rates, error rates, and connection statistics
- [ ] Health/readyz endpoints include metric summaries and known issues
- [ ] Audit log volume tracked and reported; anomalies trigger alerts
- [ ] Configuration supports metrics collection mode with configurable retention and export format
- [ ] Integration tests verify metric emission on normal and failure paths
- [ ] Dashboard documentation added to `docs/operations.md`

## Notes

This issue is linked to `OPS-001` in the TODO backlog. Metrics should be collected at multiple layers: world gateway (protocol), world server (simulation), and persistence adapters (database). Consider using a metrics library like Prometheus client-side SDK or OpenTelemetry for structured collection.
