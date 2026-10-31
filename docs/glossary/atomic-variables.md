# Atomic variables

An **atomic variable** is a small value that multiple threads can read or change
without interrupting one another halfway through that single operation. In
Rust, atomic types such as `AtomicU64` and `AtomicBool` are provided by
`std::sync::atomic`.

## Why they are useful

If two threads both update an ordinary shared counter at the same time, one
update can overwrite the other. An atomic counter makes operations such as
“add one” indivisible, so both updates are counted. It also allows threads to
share a small value without putting a mutex around every access.

In Lynx, the world service uses atomic counters for operational measurements
such as ticks, actions, and connection counts. For example,
`crates/lynx_world/src/service.rs` increments metric counters while handling
work and reads them when it prepares metrics for reporting.

## Important limits

- Atomic means **one operation on one atomic value** is indivisible. It does
  not make a group of several fields update as one unit.
- It does not replace a database transaction. Updating a character's inventory
  and recording a transfer receipt together needs a transaction or another
  multi-value consistency mechanism.
- Atomicity alone does not automatically make other data safe to share. Related
  data may need a mutex, channel, or a carefully designed synchronization
  protocol.
- Rust's memory `Ordering` controls how atomic operations relate to other memory
  operations. Lynx's metrics commonly use `Ordering::Relaxed`: that is suitable
  for independent counters where an approximate scrape-time snapshot is enough,
  but it is not a signal that safely publishes unrelated data to another
  thread.

**Rule of thumb:** use an atomic for a small independent flag or counter. Use a
transaction or lock when multiple values must agree with each other.

For more detail, see the [Rust atomic types documentation](https://doc.rust-lang.org/std/sync/atomic/).
