# World Persistence — `--fresh` Restart Mode

**Status: implemented.** This note remains as the design record for the reset
switch and its boundaries.

## The problem

Durable snapshots hold the *played* world: time has advanced, doors are open,
inventories moved. Restarting normally resumes that state by design. Returning
to the *authored* demo (the pristine setup built from content) therefore used
to require deleting the snapshot file by hand, which is fragile and easy to
forget.

## Behavior

`lynx-world serve --state-file world.json --fresh --token-hash <sha256-hex>`

1. **Ignores** the existing snapshot file. It is not read, so an unreadable or
   corrupt file cannot block a reset.
2. Rebuilds the authored demo from content, exactly as a first run does.
3. Writes the rebuilt world over the snapshot **before** the authority loop
   starts, so the reset is durable immediately.
4. Serves normally from that authored world.

`--fresh` requires `--state-file`; without a file there is nothing to reset.
The choice lives in `lynx_world::startup::load`, which is also what normal
startup uses, so the test exercises the real path rather than a copy.

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
- `crates/lynx_world/src/config.rs`:
  `fresh_requires_a_state_file` covers the flag validation.
