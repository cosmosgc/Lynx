# Content authoring

## Formats

| Kind | Format | Location |
|---|---|---|
| Items, entities, zones, behaviors, UI panels | RON | `content/<kind>/` |
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
  id: "door.wooden",
  version: (major: 1, minor: 0),
  display: (name: "entity.door.wooden.name", description: "entity.door.wooden.description"),
  components: {
    "core.interactable": (
      verbs: [Inspect, Open, Lock],
      range_m: 2.5,
      requires_line_of_sight: true,
      permission: OwnerOrPublic,
    ),
    "core.door": (starts_open: false, starts_locked: false, key: None),
  },
  presentation: Token(
    mode: Hybrid,
    model: "models/door_wooden.glb",
    portrait: "ui/portraits/door.png",
    animation_set: "anim/door_minimal.ron",
  ),
  tags: ["structure", "wood"],
)
```

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

## Publishing pipeline

1. Parse and validate schemas.
2. Validate references and assets.
3. Check budgets (entity counts, texture sizes, link depth).
4. Validate localization keys, placeholders, and declared-locale completeness.
5. Build a bundle and compute SHA-256 hashes for content and each catalog.
6. Show a diff against the deployed content version.
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
| `i18n.missing_key` | A referenced message key is absent from a catalog |
| `i18n.placeholder_mismatch` | A translation does not preserve source placeholders |
| `i18n.unknown_locale` | Locale tag is malformed or not declared |
| `i18n.missing_translation` | A release locale is incomplete |
