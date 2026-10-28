# NET — Client-readiness protocol and recovery risks

- **Status:** Open
- **Affected workflow:** `crates/world_gateway`, `crates/world_server`, `crates/lynx_world`
- **Related tasks:** [NET-001, NET-002, NET-006, NET-012](../TODO/30-networking-security.md), [QUA-014](../TODO/70-testing-quality.md)

## Findings requiring verification and follow-up

- A full snapshot or delta can exceed the 64 KiB envelope limit; the current
  transport has no chunking protocol. Initial snapshots are now checked before
  admission, and oversized snapshots reject and clean up the session. An
  unencodable delta now detaches the affected connection rather than silently
  losing replication state; other connections can still receive their deltas.
  Resume requires a fresh snapshot, which can itself be oversized and revoke
  the session. Chunking, transport-level close/recovery, and WebSocket boundary
  tests remain open.
- Resume tokens rotate before the replacement reaches the client. A dropped
  connection before `world.token` delivery can strand the old token. See the
  recovery analysis below; transport-level delivery-failure tests remain open.
- Session acknowledgements track a tick; reconnect sends a fresh snapshot, not
  a durable replay of acknowledged events. Clients must not assume event replay.
- Due ticks now take priority over queued events, with regression tests for a
  ready admission backlog and shutdown. Synchronous snapshot saves can still
  delay ticks; measure tick latency under flooding and slow persistence.
- Session deadlines use wall-clock time; clock corrections can change resume
  windows. Evaluate a monotonic time source for process-local deadlines.

Production identity and character ownership, authoritative movement, and rate
limiting remain open backlog work. A static review does not establish readiness
for production clients; the above risks need targeted integration tests.

## Lost rotated token: security and recovery

ADR 0013 deliberately invalidates the old bearer token at successful gateway
resume, before any transport delivery. The reference service queues
`world.ready` and `world.snapshot` before `world.token`; queue failure, a socket
failure, or a disconnect after a successful write but before the client stores
the token can leave the client without the replacement. A successful queue or
socket write is not proof of receipt. If snapshot construction or wire encoding
fails during admission, the session is instead revoked. On ordinary connection
termination, the new token remains resumable until the new disconnect deadline,
but the client may not know it. The gateway regression test simulates this
state; it does not exercise the actual transport or guarantee timely `Leave`.

**Threat model:** an attacker may possess a previously valid bearer token and
race the legitimate client or replay it after a connection drops; transport
delivery and acknowledgements can be lost, duplicated, or delayed. A token
alone cannot distinguish its rightful holder from the attacker. Restoring the
old token on a failed send, accepting it during a grace period, or resending the
replacement to anyone presenting it would reopen replay/hijacking. An ack alone
does not solve the ambiguity when the ack or its response is lost. TLS and
secret-safe logging remain necessary, but do not provide a recovery proof.

**Current safe fallback:** once the old token fails, authenticate again through
the independent trusted login path. A connected session returns `Conflict`;
retry only after the old connection is detected as detached, without assuming
an immediate deadline. A fresh authenticated login replaces a detached retained
session and invalidates its rotated token. It obtains a new session and fresh
snapshot, not a continuation of the old session: action request deduplication
is session-scoped, so do not blindly retry an action whose outcome is unknown.
The static credential is development-only; production identity and ownership
validation are not yet available. Initial login token loss also requires fresh
authentication after detach. Restarts invalidate all process-local sessions.

**Potential protocol work, not implemented:** retain the strict fallback, or
design a versioned recovery protocol with an independently authenticated,
device-bound proof of possession and bounded, atomic rotation state. Such a
protocol must define concurrent attempts, single active attachment, client
durable storage and acknowledgement ordering, replay and revocation, expiry,
restart behavior, secret handling, and the case where the attacker steals both
credentials. Another option is independently authenticated session recovery,
with explicit ownership checks and a takeover policy; it requires a separate
security decision about preserving session-scoped state and displacing live
connections. Neither option is specified by the current wire contract.

Follow up with integration fault injection before token enqueue, during socket
write, after write but before client persistence, and around lost acknowledgements
and disconnect ordering. Verify replay races, conflict/expiry, eventual cleanup,
fresh-login behavior, and that no secret reaches logs. Do not weaken immediate
token invalidation based solely on transport send success or failure.
