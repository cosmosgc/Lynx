# NET — Client-readiness protocol and recovery risks

- **Status:** Open
- **Affected workflow:** `crates/world_gateway`, `crates/world_server`, `crates/lynx_world`
- **Related tasks:** [NET-001, NET-002, NET-006, NET-012](../TODO/30-networking-security.md), [QUA-014](../TODO/70-testing-quality.md)

## Findings requiring verification and follow-up

- A full snapshot or delta can exceed the 64 KiB envelope limit; the current
  transport has no chunking protocol. Initial snapshots are now checked before
  admission, and oversized snapshots reject and clean up the session. Oversized
  deltas and WebSocket boundary tests remain open.
- Resume tokens rotate before the replacement reaches the client. A dropped
  connection before `world.token` delivery can strand the old token. Test
  delivery failures and define a safe recovery protocol.
- Session acknowledgements track a tick; reconnect sends a fresh snapshot, not
  a durable replay of acknowledged events. Clients must not assume event replay.
- The biased authority loop and synchronous snapshot saves may delay ticks under
  load. Measure tick latency under flooding and slow persistence.
- Session deadlines use wall-clock time; clock corrections can change resume
  windows. Evaluate a monotonic time source for process-local deadlines.

Production identity and character ownership, authoritative movement, and rate
limiting remain open backlog work. A static review does not establish readiness
for production clients; the above risks need targeted integration tests.
