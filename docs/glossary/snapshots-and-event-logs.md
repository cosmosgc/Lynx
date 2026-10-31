# Snapshots and event logs

A **snapshot** is a saved picture of the current state, such as the entities
and settings in a zone. Loading a snapshot is usually faster than rebuilding
everything from the beginning.

An **event log** is an ordered history of changes or important operations. A
server can load a snapshot and then replay later events to recover more recent
state. Events must be applied in the correct order, and replay must not apply a
change twice. In Lynx, the current PostgreSQL design records whole-world
checkpoint events; it does not yet capture every gameplay change as a delta.

See [PST-005](../TODO/20-persistence-backend.md#pst-005--snapshot-and-event-replay)
for the current recovery contract and remaining work.
