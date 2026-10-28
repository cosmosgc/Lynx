# World Persistence — `--fresh` Restart Mode

**Status: Completed.** Archived design record for the reset switch and its
boundaries. See [the backlog index](../README.md) for outstanding work.

## The problem

Durable snapshots hold the *played* world: time has advanced, doors are open,
inventories moved. Restarting normally resumes that state by design. Returning
to the *authored* demo (the pristine setup built from content) therefore used
to require deleting the snapshot file by hand, which is fragile and easy to
forget.

## Behavior

`lynx-world serve --state-file world.json --fresh --token-hash <sha256-hex>`

The durable snapshot may instead live in PostgreSQL
(`lynx.world_snapshots`, migration `0006`):

`lynx-world serve --state-postgres --fresh --token-hash <sha256-hex>`

1. **Ignores** the existing snapshot. It is not read, so an unreadable, corrupt,
   or unreachable saved world cannot block a reset.
2. Rebuilds the authored demo from content, exactly as a first run does.
3. Writes the rebuilt world over the snapshot **before** the authority loop
   starts, so the reset is durable immediately. The PostgreSQL store writes one
   row per world with an atomic upsert; a failed write leaves the previous world.
4. Serves normally from that authored world.

`--fresh` requires `--state-file` or `--state-postgres`; without a store there is
nothing to reset. The choice lives in `lynx_world::startup::load`, which is also
what normal startup uses, so the tests exercise the real path rather than a copy.

## Deliberate boundaries

- Sessions, resume tokens, and already-open connections are process-local and
  simply do not survive the restart.
- Fresh demo minigame seeds come from server entropy on every build. They are
  server-held runtime values, not authored or played state.
- `--fresh` resets the durable world only. It does not delete unrelated files,
  migrate schemas, or change configured tokens.

## Tests

- `crates/lynx_world/tests/restart.rs`:
  `a_fresh_start_replaces_the_played_world_with_the_authored_one` saves a
  played world at tick 7, proves a normal start resumes it, then proves
  `--fresh` returns tick zero and the authored entity placement.
- The PostgreSQL integration test drives save → load → verify → `--fresh` reset
  → overwrite → delete against the live database, and skips when `LYNX_PG_*` is
  unset.
- `crates/lynx_world/src/config.rs`:
  `fresh_requires_a_state_file` covers the flag validation, including the
  PostgreSQL store and mutual exclusion.
