# Prometheus text format

**Prometheus text format** is a simple, line-based way for a service to publish
measurements. A monitoring program called Prometheus periodically requests a
service's metrics endpoint, reads the lines, and stores the measurements over
time. This request is called a **scrape**.

Lynx's world service exposes metrics at `/metrics`. For example, a response can
contain lines like these:

```text
# HELP lynx_world_ticks_total Attempted world ticks.
# TYPE lynx_world_ticks_total counter
lynx_world_ticks_total 42
lynx_world_entities 120
```

The first two lines describe the metric; the final lines give metric names and
their current numeric values. Labels can add a small, bounded category, for
example `lynx_world_resume_attempts_total{outcome="success"} 8`. Metrics are
measurements, not log messages: they are intended to be counted, graphed, and
queried over time.

## In this project

Lynx uses Prometheus text format version 0.0.4 for its metrics endpoint. The
service exports counters, gauges (values that can go up or down), and histograms
(observations grouped into ranges, useful for durations). Prometheus can be
configured to scrape the endpoint; the example configuration and metric
reference are in [Operations: metrics and logs](../operations.md#metrics-and-logs).

The endpoint is unauthenticated and must remain on loopback or a suitably
protected private network. Metrics reset when the process restarts, and labels
must stay low-cardinality—do not add a unique player name, request ID, or other
unbounded value as a label. See [Networking: metrics exposure](../operations.md#metrics-and-logs)
for the security details and supported metrics.
