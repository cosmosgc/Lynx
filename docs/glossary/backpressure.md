# Backpressure

**Backpressure** is a way for a busy service to avoid accepting more work than
it can safely handle. Instead of allowing queues and memory use to grow without
limit, the service makes the sender wait, slows input, or rejects/disconnects
work when a configured capacity is reached.

In Lynx, a full outgoing queue for one client must not stall the authoritative
world tick. The server detaches that slow client, which can reconnect and
receive a fresh state snapshot. See [Networking: replication delivery](../networking.md#replication-delivery-target-and-overload-policy).
