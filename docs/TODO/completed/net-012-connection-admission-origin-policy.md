# NET-012 — Connection admission and origin policy

**Status: Done.** All acceptance criteria are checked in the source task.

## Original task scope

- Bound total accepted HTTP connections, upgraded connections, and per-IP
  connections before publishing a client session.
- Enforce an optional exact-match browser Origin policy.
- Bound HTTP request headers, incomplete raw header reads, WebSocket handshakes,
  admitted connection idle time, authority queue waits, and socket writes.
- Stop admitting connections while draining and close active connections
  cleanly.

## Acceptance evidence

- [x] A connection flood cannot exhaust memory or file descriptors: accepted
      TCP/HTTP connections, upgraded connections, headers, and per-connection
      queues are independently bounded.
- [x] Browser clients from unlisted origins are rejected when origin policy is
      enabled.
- [x] Draining stops new admissions; existing connections close on client
      closure, idle timeout, transport failure, or process shutdown.
- [x] Capacity and draining admission rejections have stable reasons and
      bounded-cardinality metrics; rate-limit rejections retain stable codes.
- [x] An incomplete raw HTTP header is closed after an absolute read deadline;
      WebSocket upgrades use the separate WebSocket idle timeout.

## Verification

- `cargo test -p lynx-world --lib`: 122 passed, 1 ignored.
- `cargo test -p lynx-world --test websocket`: 8 passed, including admitted idle closure.
- `cargo fmt --all -- --check`: passed.

The shared socket write-timeout helper has a stalled-write regression. The
write deadline remains enforced at the transport boundary, while the admission,
idle, and origin paths are exercised through socket-level tests.

## Source

See [`30-networking-security.md`](../30-networking-security.md) for the task
history and implementation notes.
