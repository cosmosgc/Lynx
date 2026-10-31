# Replication, snapshots, and deltas

**Replication** is the process of sending the server's game state to connected
clients so they can display the same world. The client receives a view of
authoritative state; it does not become the owner of that state.

A **snapshot** sends a full baseline of relevant state. A **delta** sends only
changes since a baseline or earlier update. Lynx uses snapshots for initial
state and recovery, then filtered deltas for later changes. If a client misses
updates or reconnects, a fresh snapshot can bring it back into sync. See
[Networking: replication delivery](../networking.md#replication-delivery-target-and-overload-policy).
