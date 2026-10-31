# Fencing tokens

A **fencing token** is an increasing number assigned when a process takes
ownership of a shared resource. Every write carries the token. The storage
system rejects writes with an old token, so a process that paused or lost its
connection cannot continue changing state after another process takes over.

For example, if process A owns a zone with token 12 and its lease expires,
process B may acquire the zone with token 13. Any later write from A using 12
is rejected—even if A wakes up and mistakenly believes it still owns the zone.

Lynx uses zone leases and fencing for PostgreSQL-backed world state. See
[PST-004](../TODO/20-persistence-backend.md#pst-004--zone-leases-fencing-and-event-log).
