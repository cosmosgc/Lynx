# Interest management

**Interest management** decides which parts of a large or changing world each
client needs to receive. A player usually does not need every update for every
entity in every zone. Filtering updates reduces bandwidth and avoids revealing
state the player should not see.

Lynx tracks a per-session interest set and sends updates filtered for that
session, based on factors such as distance and visibility. It is a delivery
optimization and privacy boundary, not permission to change the authoritative
world. See [ADR 0012](../adr/0012-interest-management-and-replication-audiences.md).
