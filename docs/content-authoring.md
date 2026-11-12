# Content authoring

## Formats

| Kind | Format | Location |
|---|---|---|
| Items, entities, zones, behaviors | RON | `content/<kind>/` |
| World atlas and zone routes | RON | `content/maps/` |
| Lore, quest, and event templates | RON | `content/lore/`, `content/quests/`, `content/events/` |
| UI panels | RON | `content/ui/` or `content/panels/` |
| Minigames | RON | `content/minigames/` |
| Localization catalogs | RON | `content/localization/<locale>.ron` |
| Project manifest and runtime config | TOML | project root, `config/` |
| Schemas | JSON Schema | `content/schemas/` |
| Content bundles | directory + `manifest.json` | build output |

RON is the canonical content format. Validation runs at load, at publish, and
in CI. A content file that fails validation is a build failure, not a runtime
surprise.

## Definitions vs. instances

- A **definition** is authored data with a `DefinitionId` such as
  `item.torch` or `door.wooden`. It is immutable for a content version.
- An **instance** is a live object with a `StableId`. It references a
  definition and stores only mutable differences.
- Editing a definition never rewrites live instances silently. Instances record
  the content version they were created from; a migration handles incompatible
  changes.

## Definition shape

Every definition carries:

```ron
(
  id: "entity.tavern_keeper",
  version: (major: 1, minor: 0),
  display: (name: "entity.tavern_keeper.name", description: "entity.tavern_keeper.description"),
  components: {
    "core.interactable": {
      "verbs": ["inspect", "talk"],
      "range_m": 2.5,
      "requires_line_of_sight": true,
    },
  },
  tags: ["npc", "tavern"],
)
```

Component values are RON maps because the content format carries them as typed
key/value data. The CLI's starter project registers `core.interactable`,
`core.door`, `core.trigger`, `core.health`, `core.item`, `core.movement`,
`core.behavior`, and `core.zone` with the schemas used by its generated
examples.

## Required fields and validation

- Unique, stable `id` with a lowercase dot-separated namespace.
- Explicit `version`; adding an optional field is a minor bump, removing or
  retyping a field is a major bump.
- Display strings are message keys that resolve in the `pt-BR` source catalog
  and in every locale the bundle declares.
- Component keys must exist in the component registry, and field values must
  satisfy their schema constraints.
- Referenced assets must exist in the bundle (or be declared as external).
- References between definitions must resolve; missing references produce an
  error with file and field.
- Presentation mode must be one of `Rigged3d`, `Billboard2d`, `Hybrid`,
  `IconAbstract`.
- Gameplay behavior must not depend on the presentation mode.

Lore and quest definitions use stable IDs and localized message keys, and may
reference zones, factions, items, or other definitions. Event templates contain
the authored objectives and versioned rules; a scheduled calendar event is a
durable runtime instance that records the template version it uses. Validate
all references when building and publishing a bundle.

## AI behavior schedules

Behavior definitions may include an optional recurring UTC window:

```ron
behavior: Some((
  kind: guard_chase,
  range_m: 24.0,
  schedule: Some((start_minute_utc: 480, end_minute_utc: 1200)),
  aggro_group: Some("group.harbor_watch"),
  graph: Some((
    entry: "node.detect",
    nodes: [
      (id: "node.detect", instruction: perceive_hostiles,
        on_success: Some("node.attack"), on_failure: Some("node.wait")),
      (id: "node.attack", instruction: attack_target,
        on_success: Some("node.wait"), on_failure: Some("node.chase")),
      (id: "node.chase", instruction: chase_target,
        on_success: Some("node.wait"), on_failure: Some("node.flee")),
      (id: "node.flee", instruction: flee_target,
        on_success: Some("node.wait"), on_failure: Some("node.wait")),
      (id: "node.wait", instruction: wait),
    ],
  )),
))
```

Minutes are measured from UTC midnight. The start is inclusive and the end is
exclusive; windows may cross midnight. Equal endpoints and values outside
`0..1440` fail content validation. Omitting `schedule` leaves the behavior
active all day. Runtime evaluates the window against the server's simulation
timestamp, so the same tick and content produce the same schedule decision.
Behaviors sharing an `aggro_group` may reuse a living hostile target after an
ally's attack succeeds. The simulation policy caps the number of same-tick
shares; each recipient rechecks that the target remains hostile, and resulting
attacks still pass through the normal action pipeline.

An optional `graph` overrides the legacy `kind` primitive. Node transitions
follow operation success or failure; `wait` ends the current decision. Content
validation limits each graph to 64 nodes and checks unique IDs, the entry, and
every transition. The server executes at most 16 instructions per AI decision,
so intentional cycles terminate with a budget diagnostic. Perception and
navigation use the normal zone budgets, and attacks use the shared player/AI
action quota. `flee_target` moves away from the selected hostile through the
same server-side speed, navigation geometry, collision, and dynamic-blocker
checks as other authoritative AI movement. `interact_target` issues the bounded
`use` action for the selected target; component policy, range, and the normal
shared action quota still apply, and rejected uses branch to `on_failure`.

Patrol behaviors author a looping route and use a `patrol` node:

```ron
behavior: Some((
  kind: guard,
  range_m: 12.0,
  patrol_route: [
    (x: 4.0, y: 0.0, z: 4.0),
    (x: 8.0, y: 0.0, z: 4.0),
    (x: 8.0, y: 0.0, z: 8.0),
  ],
  graph: Some((
    entry: "node.patrol",
    nodes: [
      (id: "node.patrol", instruction: patrol,
        on_success: Some("node.patrol"), on_failure: Some("node.patrol")),
    ],
  )),
))
```

Routes contain 2 to 64 finite points. The runtime advances no more than one
patrol movement per actor each tick. The route cursor resets when the zone is
restored, and blocked movement retries the current point on a later tick.

## Zones

A zone definition contains a server-only `core.zone` component for its runtime
geometry and capacity. The first runtime adapter supports one default entry
point and a uniform-cost static navigation grid:

```ron
components: {
  "core.zone": {
    "max_players": 64,
    "cell_size_m": 1.0,
    "width_cells": 128,
    "height_cells": 128,
    "origin_x_m": 0.0,
    "origin_z_m": 0.0,
    "floor_y_m": 0.0,
    "ceiling_y_m": 4.0,
    "entry_x_m": 64.5,
    "entry_y_m": 0.0,
    "entry_z_m": 64.5,
    "blocked_cells": [],
    "max_entities": 1000,
  },
}
```

`width_cells` and `height_cells` are each limited to 512, and the grid is
limited to 262,144 cells. `max_entities` must be at least `max_players`.
`blocked_cells` contains row-major indexes
(`row * width_cells + column`). Omitting the optional geometry fields selects
a 128 by 128 grid at the origin with a four-meter vertical range and a centered
entry point. Explicit entry coordinates must lie inside the zone and on an
unblocked cell. The authored adapter derives a stable zone-instance ID from
the world-map and zone-definition IDs so leases and checkpoints address the
same zone after restart.

Zones can declare bounded, zone-local travel triggers that activate one
explicit map route:

```ron
travel_triggers: [
  (
    id: "to_market",
    destination: "zone.market",
    position: (x: 120.5, y: 0.0, z: 64.5),
    radius_m: 2.0,
  ),
],
```

Zones may declare up to 64 travel triggers. IDs are unique lowercase tags within
the zone. Positions must be finite, inside the zone, and on an unblocked
navigation cell; radii are in `(0, 32]` meters. The destination must be placed
on the same map and have an explicit directed route from the source zone.
Runtime resolution assigns the destination's default safe entry point. After
the controlled actor's first accepted movement, the client requests travel when
its authoritative replicated position enters a trigger radius. The server
rechecks the trigger and position, then atomically commits both zone snapshots,
the handoff receipt, and any durable resume-token zone update. The socket closes
after success; the client reconnects with its existing token, which now routes
to the destination zone.

Zones can also place static entity definitions:

```ron
entity_placements: [
  (
    id: "north_stall",
    definition: "entity.market_stall",
    position: (x: 32.5, y: 0.0, z: 18.5),
    yaw_degrees: 90.0,
  ),
],
```

Placement IDs are unique lowercase tags within the zone. The entity definition
must resolve to the `entity` namespace. Positions must be finite, inside the
zone, and on an unblocked navigation cell; only one authored placement may use
each cell. The server derives a stable entity ID from the map, zone, and
placement IDs, then copies the definition's validated extension components and
tags. These `static_definition` instances are rebuilt from the content bundle
after snapshot recovery and are not stored in zone snapshots. Component data is
preserved for replication and inspection. The starter `core.interactable`
component also binds to authoritative verbs, range, and line-of-sight checks;
the starter `core.door` component binds its initial open state to the simulation
door state. Declare `open` and `close` in the interactable's verbs to expose
those actions. Locked doors remain unsupported by this authoring adapter because
item-key and minigame unlock policies are not yet bound. `core.health` accepts a
maximum from 1 to 1,000,000 and creates a full-health simulation component.
Because placed entities use `static_definition` persistence, damage to authored
health resets to full on restart until per-placement persistence is implemented.
`core.movement` sets a collision radius from 0 to 10 meters and whether the
entity occupies space (default `true`). `core.trigger` defines a server-only
sensor radius from 0.1 to 32 meters and emits a `triggered` link event when an
actor enters it. Occupying placed entities participate
in authoritative body collision and are avoided by player spawn selection. Two
occupying authored bodies cannot overlap at startup. `core.behavior` references
a validated `behavior.*` definition and gives the placed entity an AI mind; the
authored-map service installs the bounded behavior policy. Patrol graphs run,
and hostile target selection uses directional faction relations loaded from the
database-backed world aggregate at startup. Changing those relations while the
map is running requires a future world-level faction refresh coordinator.

Link definitions declare the compatible endpoint components, supported events,
and server-side action. The runtime supports door actions for lever and trigger
events:

```ron
(
  id: "link.lever_to_door",
  version: (major: 1, minor: 0),
  display: (
    name: "link.lever_to_door.name",
    description: "link.lever_to_door.description",
  ),
  components: {},
  link: Some((
    source_component: "core.interactable",
    target_component: "core.door",
    supported_events: [activated],
    action: toggle_door,
  )),
)
```

For `activated`, the source component is `core.interactable` and the source must
advertise the `activate` verb. For `opened` and `closed`, the source component
is `core.door`; the source must also have `core.interactable` with the matching
`open` or `close` verb. For `triggered`, the source component is `core.trigger`;
its required `radius_m` field defines a server-side activation radius. All
actions require a `core.door` target. Link definitions are resolved and checked
against both endpoint entity definitions at bundle validation. Player and AI
movement fires `triggered` when accepted movement enters the radius.
`toggle_door`, `open_door`, and `close_door` operate only on unlocked target
doors; a linked door change does not recursively emit a second door event.

A pressure plate can use the same action with a trigger-specific link type:

```ron
components: {
  "core.trigger": { "radius_m": 1.5 },
},
link: Some((
  source_component: "core.trigger",
  target_component: "core.door",
  supported_events: [triggered],
  action: open_door,
)),
```

Zones may connect placements with directed activation or trigger links:

```ron
entity_links: [
  (
    link_type: "link.lever_to_door",
    source: "market_lever",
    target: "north_gate",
    events: [activated],
    enabled: true,
    cooldown_ticks: 10,
  ),
],
```

Link types must resolve to `link.*` definitions. Endpoints must name placements
in the same zone and match the declared source/target components. Self-links,
duplicate type/source/target triples, unsupported or empty event sets, and
activation cycles are rejected. Links start enabled unless `enabled` is false,
are reconstructed from content after snapshot recovery, and may carry a tick
cooldown. A supported activation toggles an unlocked door and emits the link
type. Other actions/events and cross-zone links are not implemented.

The zone contract also describes future runtime data that is not yet consumed
by this adapter:

- Persistence policy per placed entity.
- Population and simulation budgets.

Dynamic territory ownership is runtime state, not part of a zone's authored
identity or geometry. The editor must visualize routes and territory without
baking live ownership into a content bundle.

### World atlas and routes

A world atlas is authored in `content/maps/` and places stable zone IDs on whole
map cells. Zones carry their own geometry, spawn points, and navigation; the
atlas carries only topology.

```ron
(
  id: "map.overworld",
  version: (major: 1, minor: 0),
  display: (
    name: "map.overworld.name",
    description: "map.overworld.description",
  ),
  zones: [
    (zone: "zone.starter", position: (x: 0, y: 0)),
    (zone: "zone.market", position: (x: 1, y: 0)),
  ],
  routes: [
    (
      from: "zone.starter",
      to: "zone.market",
      bidirectional: true,
      kind: passage,
    ),
  ],
)
```

- Map IDs use the `map` or `world` namespace. Zone references use the `zone`
  namespace and must resolve to a loaded zone definition.
- A zone may be placed only once per map and only on one map. No two placements
  may share an atlas cell.
- Routes name both endpoints explicitly. A route endpoint must be placed on the
  same map, a route cannot connect a zone to itself, and duplicate routes are
  errors. `kind` is `passage`, `gateway`, or `portal`; `bidirectional` defaults
  to `false`.
- Zone-local `travel_triggers` bind an activation point to one of those routes.
  They do not add coordinates to the atlas; arrivals currently use the
  destination zone's default safe entry point.
- Zone definitions that are not placed on any map produce a warning once at
  least one map exists, so an accidentally omitted zone is visible.
- Publishing a map change follows the normal content diff. Authored topology and
  runtime territory ownership are separate: publishing a map must never transfer
  territory or cancel a live event.

## UI panels

UI is declarative. A panel definition names a schema, binds fields to entity
state, and lists allowed actions. The server sends state and allowed actions,
never executable UI code. Unknown panel schemas fall back to a generic
inspection view.

The engine models these contracts explicitly via a panel definition with a
stable schema, localized labels, allowed actions, and binding metadata. The
same pattern applies to a server-authoritative minigame definition, which must
name its panel schema and declare whether authority resides with the server,
a hybrid client/server flow, or the client for cosmetic-only play.

```ron
(
  id: "ui.terminal",
  version: (major: 1, minor: 0),
  display: (
    name: "ui.terminal.title",
    description: "ui.terminal.description",
  ),
  schema: "ui.terminal",
  actions: [
    (name: "close", label: "ui.action.close"),
    (name: "submit", label: "ui.action.submit", gated_by: Some("actor.has_access")),
  ],
  fields: [
    (key: "status", source: "entity.state.status", formatter: Some("status")),
  ],
  fallback_schema: Some("ui.inspect"),
)
```

A panel with no actions is a read-only view and must declare a
`fallback_schema`. Panel IDs use the `ui` or `panel` namespace; schemas, message
keys, action names, and field keys are validated at load. Duplicate action
names and field keys are errors.

At runtime, an entity's `UiProvider` can send resolved read-only fields and
server-advertised actions through `ui.state`. Each `UiAction` may define fixed
server-authored `params` and a set of `allowed_params` whose values are supplied
by the player (for example, a lockpick's `guess`). The gateway authorizes only
actions on the session's currently open panel, requires fixed parameters to match
exactly, and rejects every parameter outside that policy. The online client
renders generic fields/buttons with `bevy_immediate`; clicks only submit intents.
Directly resolving `UiPanelDefinition` bindings and localized message keys into
runtime providers remains client/content integration work.

```ron
(
  id: "minigame.lockpick",
  version: (major: 1, minor: 0),
  display: (
    name: "minigame.lockpick.name",
    description: "minigame.lockpick.description",
  ),
  panel_schema: "ui.lockpick",
  authority: "server",
  rewards: ["item.dust"],
)
```

`authority` is one of `server`, `hybrid`, or `client`. Client-authoritative
minigames are restricted to cosmetic play and must not declare rewards.
Reward references must resolve to a loaded definition. Competitive,
reward-bearing, or world-changing games must use `server` or `hybrid` so the
server validates the result before granting anything.

### Wiring a minigame at runtime

The definition describes the contract; the runtime attaches an authoritative
instance to an entity. The reference runtime (`lynx-world`) attaches a
`Minigame` to a locked vault together with a `UiProvider` whose schema matches
the definition's `panel_schema`:

```rust
EntityRecord::new(vault_id, definition("prop.vault")?, position)
    .with_interactable(Interactable::new(3.0, [Verb::Use, Verb::Play, Verb::Open]))
    .with_door(Door::locked_with(key_id))
    .with_ui(UiProvider::new(definition("ui.lockpick")?, "Vault Lock").with_actions([
        UiAction {
            name: "play".into(),
            label: "ui.action.try".into(),
            params: Default::default(),
            allowed_params: ["guess".to_string()].into(),
        },
    ]))
    .with_minigame(Minigame::new(
        definition("minigame.lockpick")?,
        3,      // digits
        8,      // attempts before lockout
        100,    // lockout length in server ticks
        seed,   // server entropy, never shipped in the bundle
    )?)
```

- The combination is derived from `seed` plus the authenticated principal, so
  the seed must be server entropy. A seed embedded in client-visible content
  lets a modified client derive every player's code.
- The client opens the panel with `use` or `activate`; guesses then travel as
  `ui.action` with `action: "play"`. Content action names must match the verbs
  the simulation supports.
- Server-held attempts, lockouts, and progress are not serialized into
  replication. Only the door's lock state is replicated, so other players see
  the unlock without learning the combination.
- Definitions with `authority: "client"` are cosmetic-only and must not be
  wired to doors, containers, rewards, or other world state. The current runtime
  rejects a client-supplied `play` verb outside the panel channel.

## Localization

Brazilian Portuguese (`pt-BR`) is the default and source language. All
player-facing text is referenced by a stable message key; definitions do not
embed literal English strings.

```ron
(
  locale: "pt-BR",
  messages: {
    "entity.door.wooden.name": "Porta de madeira",
    "entity.door.wooden.description": "Ela emperra no batente.",
    "ui.inventory.capacity": "Capacidade: {used} de {total}",
  },
)
```

- Catalogs live in `content/localization/<locale>.ron` and are declared in the
  bundle manifest with a SHA-256 hash.
- Keys are namespaced (`entity.*`, `ui.*`, `dialogue.*`, `error.*`) and never
  reused for a different meaning.
- Placeholders are named and must match the `pt-BR` source in every locale.
- Plural forms use CLDR categories (`one`, `other`, and `many` where needed).
- Missing keys in the default catalog are errors. Non-default locales may be
  incomplete during development but must be complete for a release bundle.
- Player and character names are inserted verbatim and never translated.

Formatting (dates, numbers, currency) happens on the client. Full rules are in
[`internationalization.md`](internationalization.md).

## Factions

Faction definitions and relations are server database records. Content may
mirror them for tools and previews. Alignment is `-100..=100` with default
thresholds of `<= -34` enemy, `-33..=33` neutral, and `>= 34` friend.
Character-specific reputation overrides are additive and expire when their
modifier expires.

Guilds and NPC polities use the same faction model and authoritative
operations; governance and controller policy distinguish them. Membership,
treasury, power inputs, and territory ownership are durable runtime state.
Wealth-based power is calculated from documented, versioned rules rather than
authored as a client-controlled value.

## World calendar and faction events

Scheduled world events are durable, versioned records with a lifecycle,
world-clock timestamps, affected zones, participant eligibility, and
resolution policy. Conflict definitions may reference faction and territory
IDs, but event enrollment, participation, results, and territory changes are
authoritative runtime state. Content validation must resolve every referenced
zone and faction. Cancellation and retries must not award a result or apply
ownership changes twice.

The full domain contract, map-editor requirements, and upgrade behavior are in
[`world-simulation.md`](world-simulation.md).

## Versioning and diffs

Content versions are semantic. The manifest declares one `content_version` for
the bundle, one `version` per definition, and the engine requirement. Publishing
requires the candidate to be a valid successor to the deployed bundle, so the
declared versions must describe the real change set.

The rules are enforced by `engine_content::diff` and are also available from
the CLI:

```bash
lynx content diff --base deployed/manifest.json --next build/manifest.json --json
```

The world server accepts a verified bundle directory with
`lynx-world serve --content-bundle build`. The directory contains
`manifest.json`; startup checks engine compatibility, loaded definition
metadata, locale set, and every declared SHA-256. World snapshots record the
loaded content version and SHA-256 of the exact manifest bytes, so changing a
bundle while keeping the same semantic version fails closed with a
migration/reset instruction. Authored
behavior definitions whose IDs match installed AI behavior bindings replace
those runtime defaults.

| Change | Required bump |
|---|---|
| No definition or locale bytes changed | none |
| Definition or locale added | minor |
| Definition bytes or locale catalog changed | minor |
| Definition removed, definition kind changed, locale removed | major |

Additional rules:

- A changed definition must also raise its own `version`; otherwise the diff
  reports `content.diff.definition_version_not_bumped`.
- The declared `content_version` must be at least the required bump. A smaller
  bump is an error, not a warning.
- A candidate older than the base is rejected with
  `content.diff.version_regressed`.
- A definition version that moves backwards is rejected with
  `content.diff.definition_version_regressed`.
- Removing a definition or locale, or changing a definition kind, requires a
  major content bump. These changes are reported as warnings so that a
  correctly bumped manifest is compatible; the version rule is what makes them
  a publish failure when the bump is too small.
- Moving a definition or catalog path, bumping a definition without changing
  its bytes, and changing the engine requirement produce warnings without
  forcing a bump.

| Diagnostic code | Severity | Meaning |
|---|---|---|
| `content.diff.version_regressed` | error | Candidate content version is older than the base |
| `content.diff.version_bump_too_small` | error | Declared bump is smaller than the change set requires |
| `content.diff.definition_version_not_bumped` | error | Bytes changed without raising the definition version |
| `content.diff.definition_version_regressed` | error | Definition version moved backwards |
| `content.diff.definition_removed` | warning | Definition removed; requires a major bump |
| `content.diff.kind_changed` | warning | Definition kind changed; requires a major bump |
| `content.diff.locale_removed` | warning | Locale catalog removed; requires a major bump |
| `content.diff.definition_version_bumped_without_change` | warning | Definition version raised without a byte change |
| `content.diff.path_changed` | warning | Definition file moved |
| `content.diff.locale_path_changed` | warning | Locale catalog moved |
| `content.diff.engine_requirement_changed` | warning | Manifest requires a different engine range |

`lynx content diff` always describes the full change set and exits successfully
even when the result is incompatible; inspect the JSON `ok` field (or the
human-readable `compatible`/`incompatible` line) in CI and publish gates. The
same convention applies to `lynx validate`.

See [ADR 0017](adr/0017-content-manifest-versioning.md).

## Content migrations

Reviewed migration plans live in `migrations/content/` as RON files. A plan
names its migration ID, exact source manifest SHA-256, source and target content
versions, affected definition schema transitions, and locale catalog edits.
Operations use JSON Pointers and expected prior values so a stale plan fails
before project content changes. Definition IDs, schema versions, locale
identity, and file paths are controlled by the migration tool.

Create and validate a source manifest, then preview a plan before applying it:

```bash
lynx content build --project . --content-version 1.0.0 --json
lynx content hash content/manifest.json --json
lynx migrate apply --project . --plan migrations/content/upgrade.ron --json
lynx migrate apply --project . --plan migrations/content/upgrade.ron --apply --json
lynx migrate rollback --project . --id upgrade-v1-v2 --json
```

The preview stages a complete candidate outside the project and validates all
definitions, maps, panels, minigames, and locale references. Applying rechecks
the manifest hash, validates the candidate, and retains the exact original
`content/` tree for rollback. A rollback is refused if the applied manifest has
changed. This migration surface does not modify persisted instances; those
transitions are transactional persistence work tracked by PST-013. Re-serializing
a migrated RON document may normalize its formatting and remove comments, so
review the candidate diff before using `--apply`.

## Publishing pipeline

1. Parse and validate schemas.
2. Validate references and assets.
3. Check budgets (entity counts, texture sizes, link depth).
4. Validate localization keys, placeholders, and declared-locale completeness.
5. Build a bundle and compute SHA-256 hashes for content and each catalog.
6. Diff the candidate manifest against the deployed one with
   `lynx content diff` and require a compatible result.
7. Require authorization for risky changes.
8. Publish to staging or one zone.
9. Monitor errors and tick health.
10. Promote or roll back to the previous bundle.

## Common validation errors

| Code | Meaning |
|---|---|
| `content.duplicate_id` | Two definitions use the same ID |
| `content.unknown_component` | Component key not in the registry |
| `content.unknown_reference` | Link or asset reference does not resolve |
| `content.invalid_range` | Numeric value outside schema constraint |
| `content.incompatible_version` | Major version mismatch with the engine |
| `content.link_cycle` | Link graph contains an unsupported cycle |
| `content.missing_asset` | Referenced asset absent from the bundle |
| `world.invalid_id` | A map ID does not use the `map` or `world` namespace |
| `world.empty_map` | A map places no zones |
| `world.invalid_zone_reference` | A placement does not reference a `zone.*` definition |
| `world.unknown_zone` | A map placement does not resolve to a zone definition |
| `world.unknown_route_endpoint` | A route names a zone that is not placed on its map |
| `world.duplicate_zone` | A zone is placed more than once on one map |
| `world.duplicate_position` | Two zones occupy the same atlas cell |
| `world.duplicate_route` | The same directed route is declared twice |
| `world.self_route` | A route connects a zone to itself |
| `world.zone_in_multiple_maps` | A zone is placed on more than one map |
| `world.unmapped_zone` | Warning: a zone definition appears on no map |
| `i18n.missing_key` | A referenced message key is absent from a catalog |
| `i18n.placeholder_mismatch` | A translation does not preserve source placeholders |
| `i18n.unknown_locale` | Locale tag is malformed or not declared |
| `i18n.missing_translation` | A release locale is incomplete |
