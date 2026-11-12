# SIM-002 — Client prediction and reconciliation contract

**Status: Done.** The backlog records this outcome: reconnect and destination-snapshot resets are covered; graphical review remains tracked in `CLT-019`. All acceptance criteria below are checked complete.

## Original task scope

- Define input sequence numbers, server acknowledgements, and correction
  payloads (authoritative position, tick, and last processed input). Velocity is
  not part of this protocol version.
- Keep the bounded one-entry in-flight prediction that the server's
  single-pending-input rule permits; a correction replaces that prediction and
  never retries an ambiguously delivered endpoint.
- Smooth presentation error toward the latest private authoritative result
  without changing simulation, collision, or request state.
- Define behavior for teleports, zone changes, deaths, and reconnect snapshots,
  which reset the prediction history.
- Document thresholds for "accept small error" versus "snap".

## Acceptance evidence

- [x] Delayed queue/result coverage at 100 ms and 200 ms over a real WebSocket
       shows bounded divergence and recovery after a forced correction.
- [x] Corrections never grant the client authority over position, speed, or
       collision.
- [x] Reconnect and zone-transfer paths reset prediction state explicitly.
- [x] The contract is documented in `docs/networking.md`.

**Foundation:** Initial and resumed snapshots carry the session-private current
position and optional last queue-consumed sequence (including cancelled input).
Processed inputs carry a tick-stamped applied/rejected result and position for
that exact session only. Disconnect and delivery failure recover via a baseline,
not replay of per-input results. Exhausting u64 requires a new session. The
client implements bounded prediction, presentation smoothing, and an explicit
reset seam; velocity remains deliberately absent.

The client reconciliation module is wired through `WorldState` and the bounded
background transport. It predicts the server-permitted single pending endpoint,
keeps it in a one-entry replay buffer, and consumes only correlated private
results. Its authoritative state is never mutable by presentation smoothing.
Small correction error is interpolated by the renderer; correction beyond the
local prediction distance snaps. Disconnect and fresh snapshots reset through
`disconnect`/`baseline`; the explicit `reset` API is ready for teleport, death,
and zone-transfer callers. A destination snapshot is tested as a new prediction
domain: pending source-zone input is discarded, the destination position becomes
authoritative, and sequence numbering restarts. The latency/loss test proves
that dropped replies cannot accumulate unbounded divergence and a forced server
correction recovers to authority. The local distance bound is not server movement
validation. The caller discards messages from old connections and distinguishes
resumed from new sessions.

`delayed_private_corrections_remain_bounded_over_websocket` delays both
`movement.queued` and the private rejected result by 100 ms and 200 ms against a
stub gateway. While the result is pending, additional requests fail, the
presentation endpoint stays bounded, and authority is unchanged. Presentation
then interpolates to authority without mutating it. This is deterministic
transport delay, not a jitter/loss simulation and not a 100–200 ms round-trip
polish measurement; CLT-016 remains the UI-response measurement and CLT-019 the
graphical acceptance.

Graphical validation of the online correction journey is deferred to
[`CLT-019`](../40-bevy-client.md#clt-019--online-reconciliation-visual-validation),
which requires a controlled correction and a graphical demo environment.

## Source

See [`10-simulation-gameplay.md`](../10-simulation-gameplay.md) for the full task history and implementation notes.
