# SIM-005 — Combat depth: status, mitigation, death, respawn

**Status: Done.** Timed stun and slow effects are server-owned, expire on
simulation ticks, persist through snapshots, and use explicit refresh/cleanse
rules. Typed physical, fire, frost, and poison damage uses versioned resistance
data. Source-keyed periodic damage and finite shields are bounded and
snapshot-persistent; shield absorption follows stored application order.

Combat outcomes remain server-authoritative. A bounded redacted audit ring
records resolved source/target, damage school, mitigation, shield absorption,
and health changes without replication to clients. Server-authored respawn
policies choose the safe location and delay. Death releases control and clears
timed effects; reconnect restores a scheduled respawn without restoring the old
controller. Cross-zone transfer recovery remains assigned to SIM-014.

The original acceptance checklist and implementation detail remain in
[`10-simulation-gameplay.md`](../10-simulation-gameplay.md#sim-005--combat-depth-status-mitigation-death-respawn).
