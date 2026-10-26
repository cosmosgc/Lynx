# ADR 0019: World map atlas and explicit zone routes

- **Status:** Accepted
- **Date:** 2026-10-26

## Context

The world-simulation design needs an authored atlas of stable zone IDs and the
routes between them before zones, territory, or scheduled conflicts can be
edited and validated. Zone geometry cannot be the source of world topology, and
live territory ownership must not be baked into a content bundle.

The content pipeline already validates component-backed definitions, UI panels,
and minigames. It had no representation for a world map, so it could not catch
duplicate zone placements, missing route endpoints, or zones that no map
references.

## Decision

- **A world map is authored content.** `engine_content::WorldMapDefinition`
  declares a `map.*` (or `world.*`) ID, a schema version, localized display
  keys, zone placements, and explicit routes. Stored files live in
  `content/maps/`.
- **Zone IDs are stable definition IDs.** A placement references a loaded
  `zone.*` definition. The atlas never restates zone geometry, spawn data, or
  navigation; those remain with the zone definition.
- **Atlas positions are stable map cells.** `MapPosition` uses signed integer
  coordinates for deterministic layout and diffs. Two zones may not occupy the
  same cell.
- **Routes are explicit.** Each `ZoneRoute` names a `from` and `to` zone on the
  same map, a `RouteKind` (`passage`, `gateway`, or `portal`), and whether it is
  bidirectional. Route endpoints must be placed on the map; a route cannot be
  inferred from adjacent coordinates.
- **Validation is layered.** `WorldMapDefinition::validate_shape` checks map
  structure without loading definitions. `ContentSet` resolves each placement
  against loaded zone definitions, reports unknown zones, rejects a zone placed
  on more than one map, and warns about zone definitions that no map references.
- **Live state is separate.** Territory ownership, contest state, faction
  wealth, and ownership history are durable runtime records. A map revision may
  change topology but must not silently transfer ownership. Publishing a map
  change follows the normal manifest diff and version-bump rules.
- **Maps are manifest definitions.** `ContentKind::Map` participates in bundle
  manifests and content diffs like every other definition kind. Map byte changes
  require a definition version bump and a compatible content version bump.

## Consequences

- An editor can create, rename, connect, and reposition zones against a stable
  atlas without owning a live world.
- Duplicate placements, overlapping cells, unknown endpoints, duplicate routes,
  unmapped zones, and self-routes fail or warn at load and publish time.
- The initial map model is deliberately topological. Terrain, environment, and
  lore attachments can be added to placements later without changing identity.
- Cross-map or cross-zone links still use explicit supported routes and
  messages; this ADR does not permit direct entity references between zones.
