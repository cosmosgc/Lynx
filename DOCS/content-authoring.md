# Content authoring

## Formats

| Kind | Format | Location |
|---|---|---|
| Items, entities, zones, behaviors, UI panels | RON | `content/<kind>/` |
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
  display: (name: "Wooden Door", description: "It sticks in the frame."),
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
4. Build a bundle and compute SHA-256 hashes.
5. Show a diff against the deployed content version.
6. Require authorization for risky changes.
7. Publish to staging or one zone.
8. Monitor errors and tick health.
9. Promote or roll back to the previous bundle.

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
