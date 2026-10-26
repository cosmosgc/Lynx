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
`core.item`, and `core.zone` with the schemas used by its generated examples.

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

## Zones

A zone definition contains:

- Stable zone ID and content version.
- Bounds, cell size, spawn points, and safe areas.
- Nav grid: width, height, blocked cells, optional cost multipliers.
- Entity placements with definition IDs and overrides.
- Typed links between placed entities (for example `lever.1 → door.1`,
  `Signal::Activated`).
- Persistence policy per placed entity.
- Population and simulation budgets.

Links are validated at load: endpoints exist, link type is compatible with both
components, no unsupported cycles, and cross-zone links use an explicit route.
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
    .with_ui(UiProvider::new(definition("ui.lockpick")?, "Vault Lock"))
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
