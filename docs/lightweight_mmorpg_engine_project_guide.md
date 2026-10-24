# Lightweight MMORPG Engine --- Project Guide

> **Purpose:** Build a reusable, data-driven multiplayer game engine and
> a reference MMORPG using Rust, Bevy, and Heroic Labs Nakama. The
> engine should make it practical to create new worlds and games without
> rewriting networking, entity simulation, tools, UI, or camera systems.
>
> **Working name:** `Lynx` 
>
> **Status:** Architecture and implementation guide. Treat versions,
> APIs, and platform support as decisions to verify against the current
> Bevy and Nakama documentation before implementation.

------------------------------------------------------------------------

## 1. Vision

Create a lightweight 3D MMORPG platform with a deliberately simple
visual style and deep systemic interaction. Characters may be small 3D
tokens with simple rigs and sparse animation, or 2D portraits/sprites
displayed above or on tokens. The game should prioritize readable
worlds, simulation, social interaction, player-created content, and fast
iteration over high-fidelity graphics.

The project has two deliverables:

1.  **The engine/toolkit** --- reusable runtime, entity/component model,
    networking integration, content pipeline, editor tools, UI
    framework, camera modules, and server operations.
2.  **A reference MMORPG** --- a playable game built on the engine that
    demonstrates the systems and provides a real target for performance,
    usability, and architecture decisions.

A new game project should be creatable from a template or CLI command,
then configured with its own rules, content, UI, maps, and server
settings.

### Design principles

-   **Server-authoritative multiplayer:** clients request actions; the
    server validates outcomes and owns important state.
-   **Data-driven and modular:** behavior and content should be defined
    through assets, schemas, scripts, and plugins where appropriate.
-   **ECS-first:** gameplay is composed from components and systems, not
    deep inheritance trees.
-   **Every entity may have a mind:** AI, a player, a scripted
    controller, or no active mind can control an entity.
-   **Interaction over spectacle:** objects should be inspectable,
    usable, combinable, repairable, hackable, and capable of exposing
    interfaces.
-   **Small graphics budget:** use low-poly models, simple materials,
    sparse animation, 2D overlays, and efficient rendering.
-   **Tools are part of the product:** world editing, item creation,
    entity placement, behavior authoring, live operations, and spectator
    tools are first-class features.
-   **Safe live changes:** distinguish safe data edits from changes that
    require validation, migration, or a server restart.
-   **Portable core:** keep gameplay and content logic independent of
    platform-specific rendering and input details.
-   **Observable and recoverable:** logs, metrics, audit trails,
    backups, migrations, and rollback paths are designed in from the
    beginning.

------------------------------------------------------------------------

## 2. Target experience

Players should be able to:

-   Explore persistent worlds and move between zones.
-   Possess, control, or remotely operate different kinds of entities
    when game rules allow.
-   Talk, trade, group up, fight, craft, build, investigate, and
    collaborate.
-   Create and share approved user-authored content, and place eligible
    objects in the world under server-enforced ownership and build rules.
-   Own or lease player property, such as a home or plot, and manage its
    access and placed furnishings.
-   Encounter player and AI factions whose relationships can change
    dynamically from hostile through neutral to friendly.
-   Interact with doors, terminals, computers, vehicles, machinery,
    containers, stations, and other players.
-   Connect world entities into interactive systems, such as levers
    controlling doors, traps responding to triggers, and creatures
    calling nearby allies for help.
-   Open contextual interfaces that belong to the entity being used.
-   Play minigames hosted by in-world devices or activities.
-   Experience AI-controlled creatures, workers, citizens, enemies, and
    world systems.
-   Continue to see a consistent world state after reconnecting.
-   Use different camera and control modes without requiring different
    gameplay code.

Administrators and creators should be able to edit content, inspect the
live world, observe players, diagnose simulation problems, and apply
approved changes without taking down the entire service.

------------------------------------------------------------------------

## 3. Technology baseline

### Client and engine

-   **Rust** for engine and gameplay infrastructure.
-   **Bevy** for rendering, ECS scheduling, asset handling, input,
    windowing, and client-side systems.
-   A modular renderer/content pipeline that supports 3D token models,
    simple rigs, billboarded 2D images, UI overlays, and optional
    effects.
-   Serialization formats such as **RON**, JSON, or TOML for
    configuration; choose one canonical format per content category.
-   A versioned content schema and migration system from the start.

### Backend

-   **Heroic Labs Nakama** as the multiplayer backend for
    authentication, sessions, friends/social features where appropriate,
    groups, chat, matchmaking where useful, and authoritative
    multiplayer services.
-   Use Nakama's server-side runtime and storage APIs for the features
    they fit. Keep authoritative game simulation behind a clear server
    boundary.
-   Do not assume Nakama's general-purpose storage is a substitute for a
    carefully designed world-state database. Define ownership,
    consistency, persistence, and query requirements per data type.
-   Nakama can be run as a native service without Docker where supported
    by the chosen release and database setup. The project must document
    and test a **native, non-Docker development and deployment path**.
    PostgreSQL is a separate dependency and must also have a documented
    native installation/service path. Docker may be offered as an
    optional convenience, never as the only path.
-   Keep the Nakama integration behind an adapter so backend details do
    not leak into gameplay components.

### Language and scripting boundary

Keep core engine code in Rust. For user-authored content, choose a
sandboxed scripting approach only after evaluating security, hot reload,
determinism, debugging, and platform support. Potential categories
include embedded scripting, restricted data-driven behavior graphs, or
WebAssembly modules. Do not allow arbitrary editor-authored code to run
with unrestricted server filesystem or process access.

### AI tooling and Model Context Protocol

Provide an optional Model Context Protocol (MCP) integration so
AI-assisted development clients can discover and use approved engine
tools. Support an MCP client connection to configured external tool
servers and, where useful, an engine MCP server that exposes narrow,
typed operations for project scaffolding, content authoring, schema
validation, builds, tests, and diagnostics. Tool availability is
explicitly configured per user, workspace, and environment; "any tool"
means any tool the user has connected and authorized, not unrestricted
machine or production access.

An AI should be able to create a project from scratch by selecting a
template or defining project settings, invoking the project CLI or
equivalent scaffold API, authoring starter content, and running
validation/build/test tools. The workflow should return created-file
summaries, command results, and actionable errors so the AI can iterate.
Generated projects must use the same schemas, templates, validation,
and compatibility rules as projects created manually.

Treat tool definitions and results as untrusted input. Require
least-privilege, workspace-scoped file access, explicit command
allowlists or user confirmation for shell execution, and approval before
destructive changes, dependency installation, credential access,
publishing, or production/admin actions. Never expose secrets or
privileged server APIs through a general-purpose tool. Apply timeouts,
resource limits, audit logs, and cancellation; show users the requested
tool, inputs, and side effects before approval. MCP support must be
optional and must not be required to build or run a game.

### Repository and build tooling

Use a workspace with distinct crates/packages, for example:

``` text
/
  Cargo.toml
  rust-toolchain.toml
  README.md
  CONTRIBUTING.md
  LICENSE
  docs/
  crates/
    engine_core/
    engine_render/
    engine_assets/
    engine_ecs/
    engine_net/
    engine_camera/
    engine_ui/
    engine_audio/
    engine_tools/
    engine_scripting/
    game_api/
    game_reference/
    editor_app/
    admin_app/
  server/
    nakama/
    migrations/
    config/
  content/
    schemas/
    items/
    entities/
    maps/
    behaviors/
    ui/
  templates/
    minimal_game/
    mmorpg/
  tools/
    project_cli/
    mcp/
    asset_pipeline/
  tests/
```

Adjust crate boundaries based on real dependency direction; avoid
creating many tiny crates before there is a clear benefit.

------------------------------------------------------------------------

## 4. High-level architecture

``` text
┌────────────────────── Client / Editor ──────────────────────┐
│ Bevy app                                                    │
│ Input → Camera → Local prediction/presentation → UI         │
│       Content & asset loading → ECS view → rendering        │
│       Network client / reconciliation / diagnostics         │
└──────────────────────────┬──────────────────────────────────┘
                           │ authenticated protocol
┌──────────────────────────▼──────────────────────────────────┐
│ Multiplayer services                                         │
│ Nakama: auth, sessions, chat, groups, presence, social       │
│ Authoritative game services: actions, zone simulation, rules │
│ Content/config distribution, admin API, audit events         │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│ Persistence                                                  │
│ PostgreSQL and/or purpose-built stores, with migrations      │
│ Durable character/inventory/world state, snapshots, logs     │
└──────────────────────────────────────────────────────────────┘
```

### Important separation

Do not assume the Bevy client ECS is automatically the authoritative
server simulation. A client world contains presentation, input,
prediction, interpolation, and local-only entities. The authoritative
simulation may reuse pure Rust domain logic and data definitions, but it
must run in a server process that clients cannot control.

Keep these concepts distinct:

-   **Definition/prototype:** authored data describing an item, entity,
    component preset, behavior, or map.
-   **Instance:** a live entity with an ID and mutable state.
-   **Client representation:** the subset of state needed to display or
    interact with an instance.
-   **Authoritative state:** trusted state owned by the server.
-   **Persistent state:** state that survives a process or zone restart.
-   **Ephemeral state:** transient state such as animation cues,
    short-lived effects, or nearby presence.

------------------------------------------------------------------------

## 5. ECS and entity model

### Composition

Use ECS composition instead of an object hierarchy. An entity is an ID
plus a collection of components. Systems process entities matching
required component sets.

A typical entity may have:

-   `Transform`
-   `Renderable` or `SpriteToken`
-   `Collider`
-   `Interactable`
-   `Health`
-   `Inventory`
-   `Mind`
-   `Ownership`
-   `NetworkIdentity`
-   `PersistencePolicy`
-   `Permissions`
-   `Tags`
-   `Container`
-   `Equipment`
-   `StatusEffects`
-   `Faction`
-   `FactionMembership` or `FactionReputation`
-   `PlayerProperty` or `PropertyClaim`
-   `Placement` and `EntityLinks`
-   `Conversation`
-   `MachineState`
-   `UiProvider`
-   `AudioEmitter`
-   `NavigationAgent`

Components should contain data, not large webs of behavior. Systems and
behavior modules interpret that data. Prefer stable IDs and explicit
references over raw pointers or assumptions about entity lifetime.

### Entity identity and lifecycle

Define a durable entity ID that remains stable across saves and
sessions. Do not use a transient client-side Bevy `Entity` as the
database identity. Maintain an explicit mapping between network/world
IDs and local Bevy entity IDs.

Support lifecycle operations:

-   Spawn from a prototype.
-   Initialize components and defaults.
-   Attach to a zone or container.
-   Replicate to authorized clients.
-   Save, load, despawn, archive, or destroy.
-   Transfer between zones.
-   Validate references during load.
-   Recover gracefully from missing or obsolete component data.

### Component schema rules

Every persistent component definition should include:

-   Stable type key and schema version.
-   Field types, defaults, and validation constraints.
-   Whether it is client-visible, server-only, or admin-only.
-   Persistence policy.
-   Replication policy and update frequency.
-   Permission requirements for mutation.
-   Migration behavior for older versions.
-   Optional UI contribution or interaction verbs.

Do not serialize arbitrary Rust memory layouts. Use explicit, versioned
serialization.

### "Every entity can have a mind"

Model control as a replaceable component or relation rather than
embedding player logic in the entity:

-   `Mind`: controller slot, state, capabilities, and control policy.
-   `MindController`: AI, player session, scripted process, remote
    device, or none.
-   `PossessionLink`: controlled entity, controlling principal, start
    time, and permission scope.
-   `MindTransferPolicy`: whether transfer is allowed, how it works, and
    what happens to the previous controller.
-   `ControlLease`: optional time-bound claim preventing conflicting
    controllers.

A player character is one kind of mind/controller relationship, not a
special entity class. A player may control a body, a drone, a vehicle, a
machine, or an abstract role if game rules permit. The server must
validate possession, range, access, exclusivity, death/disconnect
behavior, and transfer permissions.

AI should use the same action/command boundary as players wherever
practical. This makes NPCs, player-controlled entities, replay tools,
and automated tests share rules instead of bypassing them.

### Factions and alignment

Keep faction definitions in an authoritative database catalog rather
than hard-coding the available factions in engine logic. For example,
`FactionDefinition` records should have a stable faction ID, display
metadata, version, and default behavior rules; `FactionRelation` records
should identify a source and target faction and store their alignment
points. Alignment is signed and bounded from **-100 to 100**: negative
values are hostile, zero is neutral, and positive values are friendly.
Use documented, configurable thresholds to map points to enemy,
neutral, and friend behavior; a suggested default is -34 or lower for
enemy, -33 through 33 for neutral, and 34 or higher for friend.

Faction membership belongs to a character or AI entity. Support
character-specific reputation overrides so quests, combat, and social
actions can change how an individual is treated without rewriting the
global faction relation. Resolve effective alignment on the server,
including applicable temporary modifiers, and define whether relations
are directional. Persist changes with an audit/event record and
replicate only the resulting relationship state needed by each client.
Faction changes must immediately affect relevant perception, targeting,
and interaction decisions.

------------------------------------------------------------------------

## 6. Interaction model

Create a common interaction pipeline:

1.  Client identifies a target and requests an action.
2.  Server resolves the stable target ID and validates distance,
    visibility, permissions, state, cooldowns, and required items.
3.  Server dispatches an action through the target's supported
    interaction interface.
4.  The action applies a validated state change or returns a structured
    failure.
5.  Server emits state updates and events to relevant clients.
6.  UI displays results based on the response and replicated state.

Use data-driven verbs such as `Inspect`, `Use`, `Open`, `Take`,
`Insert`, `Equip`, `Talk`, `Repair`, `Activate`, `Hack`, `Attack`, and
game-specific actions. Do not expose every verb on every entity;
available actions should be derived from components, permissions, and
state.

Interactions should support:

-   Preconditions and costs.
-   Progress/cancel behavior for long actions.
-   Interruptions and cooldowns.
-   Target and inventory locking where needed.
-   Structured errors for the UI.
-   Audit logs for privileged or high-impact operations.
-   Server-side anti-cheat validation.
-   Optional animation and audio cues that are presentation, not
    authority.

### Linked entities and world mechanisms

Support explicit, typed links between stable entity IDs so authored
objects can coordinate without raw pointers or arbitrary scripts. A
link definition should include its type, source and target, supported
events/actions, enabled state, and permission/persistence rules.
Examples include a lever sending an activation event to a door, a
pressure plate triggering a trap, and an AI creature requesting help
from nearby allied mobs.

The server validates link ownership, endpoint compatibility, zone
boundaries, permissions, and cycles where they could cause recursive
activation. Links are resolved safely after load; missing or
incompatible endpoints produce diagnostics and do not silently execute.
Activation uses the normal server action pipeline, with cooldowns,
deduplication, and audit events where appropriate. Cross-zone links
must use an explicit supported message or event route rather than a
direct entity reference.

For mob assistance, the requesting AI emits a bounded help event. The
server considers only eligible nearby entities whose effective
alignment classifies the requester as a friend, then applies range,
line-of-sight, state, cooldown, and responder-count limits. Responders
receive a validated AI goal; they do not gain permission to bypass
combat or movement rules.

------------------------------------------------------------------------

## 7. UI as an entity capability

Build a modular UI system where entities can expose interfaces without
hardcoding a bespoke screen for every object.

Examples:

-   A computer opens a terminal with apps.
-   A door exposes lock/access controls.
-   A crafting station exposes recipes and progress.
-   A vehicle exposes driving controls and status.
-   A medical station exposes treatment actions.
-   A minigame device opens an embedded game panel.
-   A storage container exposes inventory transfer.
-   An NPC exposes dialogue and trade.

Define a UI provider protocol:

-   UI schema or panel type.
-   Data model and current state.
-   Available actions and their parameters.
-   Permissions and visibility rules.
-   Open/close lifecycle.
-   Server request/response messages.
-   Optional client-side presentation module.
-   Versioning and safe fallback if the client does not support a panel.

The server sends declarative state and allowed actions, not arbitrary
executable UI code. Client UI modules render known schemas and send
validated action requests. Keep a consistent design system with reusable
panels, lists, inventory grids, tooltips, modals, chat, map, status
bars, and accessibility options.

### Minigames

Treat minigames as modules with explicit ownership and authority:

-   Client-only cosmetic minigames may be local.
-   Competitive, reward-bearing, or world-changing minigames must be
    server-authoritative or validated by the server.
-   Provide start, update, pause, cancel, complete, timeout, and
    disconnect handling.
-   Prevent clients from claiming results without server validation.

------------------------------------------------------------------------

## 8. Rendering and visual style

Aim for a clear, low-cost visual language:

-   Low-poly or primitive 3D geometry.
-   Flat or simple stylized materials.
-   Limited real-time lights and shadows.
-   Small texture budgets and texture atlases where useful.
-   Simple effects with strict budgets.
-   Nameplates, health/status indicators, interaction hints, and
    selection outlines.
-   Optional 2D portrait, icon, or sprite displayed above a token.
-   Optional 3D model with a simple rig and a small animation set.
-   LOD and culling for large crowds and distant objects.
-   Instancing/batching where practical.
-   A graphics scalability menu and reduced-effects mode.

### Token presentation modes

Support at least:

1.  **3D rigged token:** low-poly model, minimal skeleton,
    idle/move/attack/use/death poses.
2.  **2D billboard token:** image facing the camera, optionally
    animated.
3.  **Hybrid token:** simple 3D body plus 2D portrait, status, or
    equipment markers.
4.  **Icon/abstract entity:** for machines, map markers, or entities
    that do not need a full model.

Gameplay identity must not depend on which presentation mode is used.
Content definitions select a presentation profile, and the client
resolves it to assets.

### Animation

Animation is a presentation layer. Server state can say `Moving`,
`Using`, `Attacking`, or `Dead`; clients map those states to available
animation clips or procedural poses. Never require every entity to have
a full animation rig.

------------------------------------------------------------------------

## 9. Camera and controls

Implement camera modes as replaceable modules, not one global camera
tightly coupled to gameplay.

Initial modes:

-   First person.
-   Third person orbit/follow.
-   Top-down.
-   Isometric.
-   Classic MMORPG follow camera.
-   Free spectator camera.
-   Editor camera with orbit, pan, zoom, and snapping.

Each camera module should define:

-   Input mapping and sensitivity.
-   Follow target and offset.
-   Collision/occlusion handling.
-   Zoom and field-of-view constraints.
-   Pointer lock and cursor behavior.
-   Selection/interaction ray behavior.
-   Optional smoothing and camera shake.
-   Accessibility options, inversion, and remapping.
-   Whether it supports mouse, keyboard, controller, and touch.

Gameplay actions should be expressed independently of camera mode. Use a
common input/action map and let each camera mode translate controls into
those actions. Validate target selection and interactions on the server;
client raycasts are requests, not proof of permission.

------------------------------------------------------------------------

## 10. Networking and replication

Define an explicit network protocol and version it. Do not replicate
every component field every frame.

### Replication categories

-   **Reliable events:** inventory changes, trades, chat, quest updates,
    ownership changes, interactions, and important state transitions.
-   **Frequent state snapshots:** position, rotation, velocity, and
    other rapidly changing values.
-   **Interest-managed state:** only send an entity to clients who
    should know about it.
-   **Owner-only state:** private inventory details, secrets, and
    control data.
-   **Permission-scoped construction/property state:** replicate
    player-placed entities and housing details only to viewers allowed
    by the property's access policy.
-   **Admin state:** additional diagnostics only for authorized staff.

### Required behavior

-   Server-authoritative validation of movement, combat, inventory,
    economy, and progression.
-   Client interpolation for remote entities.
-   Optional local prediction for responsive movement, with
    reconciliation.
-   Interest management by zone, distance, visibility, party, or
    gameplay rules.
-   Rate limits, message size limits, and malformed-message handling.
-   Reconnect and session recovery.
-   Protocol and content compatibility checks.
-   Graceful handling of packet loss and disconnects.
-   Load tests for crowded areas and chat/event spikes.

Use Nakama's documented transport and multiplayer features where they
fit. Keep a clean adapter around its client/server APIs. Choose
authoritative match/zone design based on actual persistence, scale, and
operational needs; do not assume one match per entire persistent world
is automatically appropriate.

------------------------------------------------------------------------

## 11. Nakama and persistence design

### Responsibility boundaries

Use Nakama for capabilities it provides well, such as authentication,
sessions, social features, chat, groups, and selected multiplayer flows.
Put world rules in trusted server-side code and decide deliberately
whether each zone runs as a Nakama authoritative match, a dedicated Rust
simulation service, or another supported topology.

Do not let the client directly write authoritative inventory, currency,
health, world placement, or combat outcomes to storage.

### Suggested data categories

-   Account/profile metadata.
-   Character records and progression.
-   Inventory, equipment, currency, and trade records.
-   World/zone snapshots and changed entities.
-   Item/entity definitions and content versions.
-   Guilds, groups, parties, and social relationships.
-   Faction catalog, faction-to-faction alignment, faction membership,
    and character-specific reputation.
-   Property/plot ownership or leases, access permissions, placement
    records, and player-authored content metadata.
-   Quest and achievement state.
-   Moderation records and sanctions.
-   Audit log and admin actions.
-   Scheduled tasks and durable queues where needed.

Keep authoritative faction definitions in database records with stable
IDs and versioned migrations; do not treat a client content bundle as
the source of truth for faction membership or alignment. Define
property ownership and placement as server-validated records, and
persist linked-entity references by stable ID with integrity checks.
Player-created content should be stored as versioned, schema-validated
data with its owner, visibility, moderation status, and dependency
metadata.

For each category document the source of truth, read/write path,
consistency expectations, retention, privacy, and backup strategy. Use
database transactions or equivalent atomic operations for multi-step
economy actions where supported. Prevent duplicated rewards and replayed
trades with idempotency keys and server-side validation.

### Native non-Docker operation

The project must provide tested instructions for running Nakama without
Docker:

1.  Install the required supported database (for example, PostgreSQL)
    natively or through a system package/service.
2.  Install a compatible Nakama release using the official distribution
    method for the target OS.
3.  Create a dedicated database and least-privilege database role.
4.  Configure Nakama with local configuration files or documented
    environment variables.
5.  Run database migrations and Nakama's required setup/bootstrap
    commands for the selected version.
6.  Start Nakama as a foreground development process or an OS service.
7.  Verify health, logs, database connectivity, and a test client login.
8.  Document firewall, TLS, secrets, backup, upgrade, and restore steps
    for production.

Provide scripts for common operating systems where feasible, but keep
them transparent and inspectable. Never bake credentials into source
control. Docker/Compose may be an optional alternative for contributors,
not a requirement. Exact commands and configuration fields must be
checked against the official Nakama documentation for the pinned
version.

### Backend interface

Create a small backend adapter with engine-level concepts such as:

-   `AuthService`
-   `SessionService`
-   `SocialService`
-   `ChatService`
-   `WorldGateway`
-   `CharacterService`
-   `InventoryService`
-   `AdminService`

These are logical interfaces, not a requirement to make every item a
separate microservice. Avoid over-distributing the architecture early.

------------------------------------------------------------------------

## 12. World, zone, and persistence model

Organize the world into zones, scenes, rooms, or shards with clear
boundaries.

Each zone should define:

-   Stable ID and content version.
-   Spawn points and safe areas.
-   Coordinate system and bounds.
-   Navigation/collision data.
-   Entity ownership and persistence rules.
-   Simulation tick policy.
-   Visibility/interest rules.
-   Entry/exit and transfer behavior.
-   Population limits and overload policy.
-   Restart/recovery behavior.

Use explicit persistence policies, for example:

-   **Transient:** removed when no longer needed.
-   **Session:** lasts for a player session or activity.
-   **Persistent entity:** saved periodically and on important changes.
-   **Persistent event log:** append-only records for critical
    transactions.
-   **Static definition:** authored content loaded by version.

Avoid saving every transform every frame. Use periodic snapshots plus
change/event records when appropriate, with bounded recovery time and
tested compaction. Define conflict resolution for edits made while a
zone is running.

------------------------------------------------------------------------

## 13. World editor and executable tools

The editor is a core product, not a later convenience. Build it as a
Bevy application or a dedicated tool sharing engine crates.

### World editing

-   Create, rename, duplicate, and delete maps/zones.
-   Place, move, rotate, scale, duplicate, group, and delete entities.
-   Grid snapping, rotation snapping, layers, selection filters, and
    undo/redo.
-   Terrain or floor/room tools appropriate to the chosen world model.
-   Collision/navigation visualization.
-   Spawn-point and path editing.
-   Lighting, fog, and visual settings.
-   Prefab/prototype placement.
-   Entity inspector showing components and validated fields.
-   Link editor for connecting compatible entity endpoints and
    previewing event flows.
-   Property/plot authoring with boundaries, ownership policy,
    placement limits, and access defaults.
-   Search by entity ID, tag, prototype, and component.
-   Diff view, save checkpoints, and content versioning.
-   Import/export of maps and selected entities.
-   Preview in a local simulation before publishing.

### Item and entity authoring

-   Create item/entity definitions from templates.
-   Edit component values with type-aware forms.
-   Define inventory slots, stack sizes, weight, durability, rarity, and
    tags.
-   Set interaction verbs, requirements, costs, cooldowns, and results.
-   Preview all presentation modes.
-   Validate missing assets, invalid references, duplicate IDs, and
    incompatible schemas.
-   Bulk edit and batch validation.
-   Separate immutable definitions from live instance state.

### AI and behavior authoring

Provide an approachable behavior tool with a path toward advanced
scripting:

-   State machines and/or behavior trees.
-   Conditions, selectors, sequences, timers, and action nodes.
-   Perception inputs and memory.
-   Navigation goals and movement constraints.
-   Server-compatible pathfinding over zone navigation data, including
    path costs, dynamic obstacles, movement capabilities, and
    replanning/stuck recovery.
-   Faction-aware perception, target selection, and optional bounded
    ally-help requests based on effective alignment.
-   Dialogue and interaction hooks.
-   Debug view for current state, target, and recent decisions.
-   Deterministic test scenarios where possible.
-   Safe sandboxing, CPU/time limits, and error isolation.
-   Versioned behavior definitions and rollback.

AI must issue the same validated actions as other controllers. Path
requests and movement remain server-authoritative; stagger expensive
replans, cap per-zone pathfinding work, and expose current path/status
for debugging. Never allow behavior scripts to bypass the authoritative
interaction/economy rules.

### Player content, placement, and property

Treat user-created content as untrusted, versioned game data rather
than executable code. Provide schema-bound templates and approved
asset/content types, ownership and attribution metadata, validation,
moderation/reporting, quotas, and revision history. Publishing and
sharing should support visibility controls and revocation; scripts or
modules, if supported later, must follow the sandbox and resource
limits defined by the engine.

Players may place only eligible prototypes in authorized plots or
zones. The server validates placement bounds, collision, cost,
ownership, permissions, object limits, and any zone-specific rules
before creating a persistent instance. A property record should define
its owner or lease, zone/plot bounds, access list or roles, placement
budget, and lifecycle policy. Support transfer, expiry, abandonment,
and recovery rules explicitly. Ownership of a property does not grant
authority over unrelated world entities; placed objects inherit
appropriate access and interaction rules from both the object and
property.

### Validation and publishing

Publishing should be an explicit pipeline:

1.  Validate schema and references.
2.  Check assets and resource budgets.
3.  Run relevant automated tests.
4.  Generate a content bundle and manifest.
5.  Show a diff against the currently deployed version.
6.  Require authorization and, for risky changes, approval.
7.  Publish to a staging environment or selected zone.
8.  Monitor errors and simulation health.
9.  Promote or roll back.

------------------------------------------------------------------------

## 14. Live server changes and operations

Support live changes, but classify them carefully.

### Safe live changes

Examples include text, icons, item descriptions, some balancing values,
UI layouts, spawn configuration, and selected behavior parameters. These
can be hot-reloaded if schema-compatible and validated.

### Risky changes

Component schema changes, persistent data migrations, core simulation
code, networking protocol changes, and changes to ownership or economy
rules may require a staged rollout, migration, or restart. Do not
promise universal hot reload.

### Live change protocol

-   Author changes in a draft revision.
-   Validate and preview in a sandbox.
-   Display the exact diff and expected impact.
-   Publish an immutable versioned bundle.
-   Apply to a selected zone, shard, or all eligible instances.
-   Track acknowledgement and version adoption.
-   Record actor, timestamp, reason, and affected entities.
-   Monitor errors and key metrics.
-   Roll back to the previous compatible version if needed.

Use optimistic concurrency or revision IDs so two editors do not
silently overwrite each other. Maintain backups and a tested restore
procedure.

------------------------------------------------------------------------

## 15. Admin live spectator and control room

Create a separate admin application or clearly isolated admin mode with
strict access control.

### Spectator capabilities

-   Observe live zones and player/entity positions.
-   Free-fly camera, follow player, follow entity, and jump to
    coordinates.
-   Filter by zone, player, entity type, faction, component, or status.
-   Inspect authorized component state and recent events.
-   View current AI state, path, target, and errors.
-   Observe server tick time, entity count, network load, and zone
    health.
-   Replay a bounded event/debug timeline where available.
-   Mark an incident and attach notes.
-   Switch between map view and a live entity inspector.

Spectator mode must not accidentally issue gameplay actions or reveal
hidden/private information to unauthorized staff. Make read-only mode
the default.

### Admin actions

Depending on permission, staff may freeze an entity, move a player,
teleport to a safe location, mute, disconnect, quarantine, or restart a
zone. Destructive and economy-affecting actions require stronger
permissions, confirmation, reason capture, and audit logs.

### Security requirements

-   Separate admin authentication and authorization from normal player
    permissions.
-   Role-based permissions with least privilege.
-   Optional MFA or identity-provider integration.
-   Short-lived sessions and revocation.
-   Server-side authorization for every admin operation.
-   Audit logs that cannot be modified by ordinary admins.
-   Redaction of secrets and private player data.
-   Rate limits and confirmation for high-impact actions.
-   No direct public exposure of privileged APIs.
-   Break-glass access documented and monitored.

------------------------------------------------------------------------

## 16. Content pipeline and project templates

A new game should not require copying the reference MMORPG and manually
removing features.

### Project creation

Provide a CLI such as:

``` bash
mosaic new my-world --template mmorpg
cd my-world
mosaic doctor
mosaic dev
```

Also support a minimal template:

``` bash
mosaic new my-prototype --template minimal
```

These commands are illustrative; implement and document the actual CLI
later.

### AI-assisted project creation

Expose project creation and common development tasks through stable,
typed CLI commands and optionally an MCP server. An authorized AI
assistant can scaffold a complete project from a template or start
without one by composing the supported generators for manifests,
schemas, maps, entities, UI, and behaviors. It should then run the
normal doctor, validation, build, and test commands and report the
result. Do not let AI-generated content bypass normal project
validation, licensing checks, permission checks, or review.

Keep MCP tool schemas versioned and narrow. Separate read-only discovery
and diagnostics from filesystem writes, shell commands, publishing, and
administrative operations. Require user approval for consequential
actions, display diffs before applying generated changes, and preserve
undo/rollback where possible. External MCP servers are optional
dependencies: document their requested capabilities and trust
requirements, and never forward credentials or private player data
unless the user explicitly configures an approved integration.

### A generated project should include

-   A workspace manifest and pinned compatible toolchain.
-   Game metadata and unique project ID.
-   Default map/zone and spawn point.
-   A sample entity, item, interaction, and UI panel.
-   Backend configuration template with no real secrets.
-   Database migration directory.
-   Local run, test, build, and package scripts.
-   Asset/content folders and schemas.
-   Example behavior tree or state machine.
-   A health/diagnostics endpoint or command.
-   Optional MCP configuration and tools for project creation,
    validation, and diagnostics, disabled unless explicitly configured.
-   CI workflow and formatting/lint/test defaults.
-   Upgrade and engine-version compatibility metadata.

### Templates

-   `minimal`: rendering, input, one local scene, basic UI.
-   `multiplayer`: login, session, replicated entities, one test zone.
-   `mmorpg`: characters, inventory, chat, interactions, persistence,
    admin basics.
-   Optional future templates: social sandbox, survival, tactics,
    roleplay, simulation.

Keep game-specific rules out of engine core. A project should be able to
replace its combat, progression, inventory rules, UI theme, world
format, and camera defaults without forking the engine.

### Content packaging

Bundle assets and data with a manifest containing version, hashes,
schema versions, dependencies, and compatibility constraints. Support
local development assets and optimized release bundles. Do not trust
client-side content to authorize gameplay outcomes.

------------------------------------------------------------------------

## 17. Cross-platform strategy

Plan for multiple platforms, but define a supported-platform matrix
early and test it continuously.

Potential targets include desktop Windows, Linux, and macOS first; later
evaluate WebAssembly/browser, handheld devices, and mobile based on
Bevy, graphics backend, input, memory, and networking constraints.

Keep separate concerns separate:

-   Core game rules and schemas should be platform-neutral.
-   Rendering should use Bevy-supported abstractions.
-   Input should be action-based and remappable.
-   UI should handle different resolutions, aspect ratios, DPI, safe
    areas, and input styles.
-   Asset pipelines should support compression and platform variants.
-   Network protocols should not depend on client platform.
-   Server builds should be separate from graphical client builds.
-   Admin tools should work on supported desktop platforms.

Do not promise every platform at launch. Make platform support an
explicit release gate with automated build and smoke tests. Browser
targets may need different storage, threading, asset-loading, and
networking strategies.

------------------------------------------------------------------------

## 18. Performance budgets and scale

Establish measurable budgets before content grows. Initial targets are
hypotheses to profile, not guarantees:

-   Stable frame pacing on the minimum supported hardware.
-   Bounded entity/component counts per active zone.
-   Explicit simulation tick budget.
-   Limits for AI updates, pathfinding, physics, draw calls, and UI
    work.
-   Interest management that avoids sending the entire world to every
    client.
-   Load tests with realistic player counts, chat volume, entity counts,
    and reconnect spikes.
-   Graceful degradation when a zone is overloaded.
-   Server metrics for tick duration, queue depth, memory, database
    latency, and replication bandwidth.

Use profiling before optimizing. Add spatial indexing, LOD, batching, AI
update staggering, and sleeping/inactive simulation only when
measurements justify them. Define which entities need high-frequency
simulation and which can update less often.

------------------------------------------------------------------------

## 19. Security, moderation, and abuse prevention

-   Treat every client request as untrusted.
-   Validate movement, line of sight, range, permissions, cooldowns, and
    resource costs on the server.
-   Use rate limits and payload size limits.
-   Avoid trusting client clocks, random outcomes, inventory claims, or
    minigame results.
-   Make trades and rewards idempotent and transactional where possible.
-   Prevent item duplication during disconnects, retries, and concurrent
    operations.
-   Keep secrets out of source control and client bundles.
-   Treat AI/MCP tool servers, tool metadata, and tool outputs as
    untrusted; grant only scoped capabilities and require confirmation
    for destructive, privileged, or externally visible actions.
-   Log AI-initiated changes and tool execution metadata without
    recording credentials or unnecessary private data.
-   Use TLS for production network connections.
-   Apply least-privilege database credentials.
-   Keep dependencies updated and audit build artifacts.
-   Support reporting, blocking, muting, moderation logs, and retention
    policies.
-   Define privacy policy and data deletion/export behavior appropriate
    to the target jurisdictions.
-   Run backups and restore drills, not just backup jobs.

------------------------------------------------------------------------

## 20. Testing and quality gates

### Test layers

-   Unit tests for domain rules, schemas, inventory, permissions, and
    state transitions.
-   Property-based tests for invariants such as item conservation and
    valid ownership.
-   Integration tests for Nakama, database migrations, auth, and session
    lifecycle.
-   Simulation tests that run zones without rendering.
-   Network tests for disconnects, latency, retries, malformed messages,
    and version mismatch.
-   Content validation tests for maps, prototypes, assets, and behavior
    graphs.
-   MCP integration tests for tool discovery, permission enforcement,
    approval gates, cancellation, and safe error reporting.
-   End-to-end tests proving an AI-assisted project scaffold validates,
    builds, and runs through the standard project workflow.
-   End-to-end tests for login, spawn, interact, trade, reconnect, and
    save/load.
-   Load and soak tests for persistent zones.
-   Visual smoke tests for supported client platforms.

### Required invariants

-   A client cannot grant itself items, currency, admin access, or
    arbitrary control of an entity.
-   Every persistent entity has a stable ID and a valid persistence
    policy.
-   Faction definitions and relations are database-backed, alignment is
    clamped to -100..100, and friend/neutral/enemy behavior uses tested
    thresholds and server-resolved reputation.
-   Player placement and property access are server-authoritative,
    permission-checked, bounded, and recoverable after restart.
-   Entity links use validated stable references, handle missing
    endpoints safely, and cannot bypass normal action rules.
-   AI pathfinding and ally-help behavior respect server budgets,
    navigation constraints, and faction alignment.
-   User-authored content is schema-validated, moderated as required,
    permission-scoped, and versioned.
-   AI-assisted project creation uses supported templates/generators,
    produces reviewable changes, and passes the same validation,
    build, and test gates as manual creation.
-   MCP tools are opt-in, scoped, auditable, and cannot access
    production or secrets without explicit authorization.
-   Every component has explicit schema/version behavior.
-   A trade cannot duplicate or silently destroy assets.
-   A zone restart can recover within a defined recovery-time objective.
-   Live content changes can be audited and, where compatible, rolled
    back.
-   A new project generated from a template builds and runs in CI.
-   Native Nakama setup works without Docker on each documented
    development OS.

------------------------------------------------------------------------

## 21. Observability and operations

Provide structured logs and metrics from the start:

-   Correlation IDs across client requests and server actions.
-   Zone ID, entity ID, and request/action type where safe.
-   Server tick duration and missed deadlines.
-   Active sessions and zone population.
-   Database errors and latency.
-   Replication bandwidth and queue sizes.
-   AI/script errors and execution budgets.
-   Content version per zone.
-   Admin changes and failed authorization attempts.

Avoid logging credentials, access tokens, or unnecessary private
chat/player data. Include health checks, readiness checks, crash
recovery, log rotation, backup verification, and documented
upgrade/rollback procedures.

------------------------------------------------------------------------

## 22. Accessibility and usability

-   Remappable actions and adjustable sensitivity.
-   Keyboard, mouse, controller, and touch support according to target
    platform.
-   UI scaling, readable fonts, colorblind-conscious indicators, and
    non-color status cues.
-   Reduced motion and reduced visual effects.
-   Subtitles and configurable chat text.
-   Clear interaction prompts and understandable failure messages.
-   Configurable camera inversion, zoom, and field of view.
-   Tutorials and inspectable affordances for complex machinery and
    entity possession.

------------------------------------------------------------------------

## 23. Suggested implementation roadmap

### Phase 0 --- Architecture proof

-   Pin Rust, Bevy, Nakama, and PostgreSQL versions.
-   Validate native, non-Docker Nakama setup on target development OSes.
-   Decide the authoritative simulation topology.
-   Build a minimal client login and server health check.
-   Write architecture decision records for key choices.

**Exit criteria:** a clean machine can build the client, start the
backend without Docker, connect, authenticate, and run automated smoke
tests.

### Phase 1 --- Engine skeleton

-   Workspace and crate boundaries.
-   Project CLI and minimal template.
-   Stable IDs, schema definitions, asset loading, and config.
-   Basic Bevy scene and input abstraction.
-   Network adapter and structured logging.

**Exit criteria:** a generated project opens a scene and runs the test
pipeline.

### Phase 2 --- Authoritative entity simulation

-   Server-owned entity IDs and component state.
-   Spawn/despawn and persistence.
-   Replication and interest management.
-   One zone and reconnect recovery.
-   Server-side action validation.

**Exit criteria:** two clients see consistent entity state, and client
tampering cannot authoritatively alter it.

### Phase 3 --- Interaction and mind system

-   Interactable components and verbs.
-   Player controller and AI controller interfaces.
-   Possession/control transfer rules.
-   Inventory and containers.
-   Basic chat and presence.
-   Typed entity links for a lever-controlled door and a triggered trap.
-   Database-backed faction definitions, alignment thresholds, and
    individual reputation overrides.

**Exit criteria:** players can interact with objects and control a
permitted non-player entity without bypassing server rules.

### Phase 4 --- UI and minigame framework

-   Entity-provided UI schemas.
-   Inventory, terminal, dialogue, and crafting panels.
-   One server-validated minigame.
-   UI capability and permission checks.

**Exit criteria:** different entity types can open different interfaces
through a common protocol.

### Phase 5 --- Editor and content pipeline

-   Map editor, placement, inspector, snapping, undo/redo.
-   Item/entity definition editor.
-   Validated link graph editor and player placement/property rules.
-   User-content schemas, ownership metadata, moderation states, and
    publishing controls.
-   Optional MCP tools for project scaffolding, content validation, and
    diagnostics, with workspace-scoped permissions and approval gates.
-   Validation, bundles, versioning, and diffs.
-   Behavior graph editor and runtime debugger.
-   Navigation authoring/visualization and bounded server pathfinding.

**Exit criteria:** a creator can make a small zone and its entities
without editing engine source code.

### Phase 6 --- Live operations

-   Admin spectator mode.
-   Read-only inspection and live metrics.
-   Role-based admin actions and audit log.
-   Staging publish, rollout, and rollback.

**Exit criteria:** an authorized operator can inspect a live zone and
apply a safe content change with a traceable rollback path.

### Phase 7 --- MMORPG vertical slice

-   One polished zone, login/character selection, movement, chat,
    inventory, a few interactions, simple AI, and persistence.
-   Performance profiling and abuse testing.
-   Packaging and first supported-platform matrix.

**Exit criteria:** a small group can play together for a sustained
session and recover from disconnects and server restarts.

### Phase 8 --- Expand and stabilize

-   Economy, progression, quests, parties/guilds, trading, additional
    zones, moderation, localization, and content tools.
-   Load testing, backups, operational drills, and release automation.
-   Additional platform support only after automated validation.

------------------------------------------------------------------------

## 24. Initial vertical slice: keep it small

The first playable build should contain only:

-   One small zone.
-   One player avatar presentation mode and one 2D billboard
    alternative.
-   Movement with one camera mode, plus an architecture path for
    additional modes.
-   Login/session handling.
-   Server-authoritative spawn and movement validation.
-   Chat.
-   One interactable door.
-   One container and a minimal inventory.
-   One computer terminal with a simple UI.
-   One AI entity using the same action interface as players.
-   One server-validated linked mechanism, such as a lever opening a
    door.
-   Persistent character data.
-   A minimal editor that can place and configure entities.
-   A basic read-only spectator view.
-   Automated native Nakama startup and smoke tests.

Avoid building a huge content library, complex combat, or all camera
modes before proving the end-to-end pipeline.

Player housing, unrestricted public user-content publishing, large
faction ecosystems, and complex multi-zone link networks should follow
the vertical slice. Prove the underlying ownership, permissions,
persistence, moderation, and authority rules with small bounded
examples first.

------------------------------------------------------------------------

## 25. Architecture decisions to record

Create short Architecture Decision Records (ADRs) for:

1.  Bevy and Rust version policy.
2.  Nakama version, server runtime, and supported deployment topology.
3.  Native PostgreSQL and Nakama installation procedure.
4.  Whether authoritative zone simulation runs inside Nakama matches or
    a separate Rust service.
5.  Persistent world data model and transaction boundaries.
6.  Network protocol and schema versioning.
7.  Scripting/behavior runtime and sandbox.
8.  Content serialization and migration format.
9.  Stable entity ID format.
10. Asset formats and import pipeline.
11. Camera/input abstraction.
12. Admin identity provider and role model.
13. Initial supported platforms and minimum hardware.
14. Backup retention and recovery objectives.
15. Faction alignment thresholds, directionality, and reputation
    stacking rules.
16. Property ownership/lease lifecycle and player placement limits.
17. User-content formats, moderation workflow, and publishing policy.
18. Navigation representation and pathfinding budget/replan policy.

Do not leave these as accidental consequences of the first prototype.

------------------------------------------------------------------------

## 26. Definition of done

A feature is not complete merely because it works in a local happy path.
It is done when:

-   Its authority boundary is clear.
-   Its schema and compatibility behavior are documented.
-   Permissions and invalid input are tested.
-   It behaves correctly on disconnect/reconnect where applicable.
-   It emits useful diagnostics without leaking secrets.
-   It has appropriate unit/integration coverage.
-   It can be edited or configured without an unnecessary engine fork.
-   It has a rollback or recovery strategy if it changes persistent
    state.
-   It fits the supported-platform and performance budgets.
-   The documentation and generated project template are updated.

------------------------------------------------------------------------

## 27. Documentation and project hygiene

Maintain:

-   `README.md` --- quick overview and quick start.
-   `docs/architecture.md` --- boundaries and data flow.
-   `docs/getting-started.md` --- install and run without Docker.
-   `docs/content-authoring.md` --- maps, items, entities, UI, and AI.
-   `docs/ai-tooling.md` --- MCP setup, supported tools, permissions,
    approvals, and AI-assisted project creation.
-   `docs/networking.md` --- replication, authority, and protocol
    versions.
-   `docs/operations.md` --- deploy, monitor, backup, restore, and
    rollback.
-   `docs/security.md` --- threat model and admin permissions.
-   `docs/platforms.md` --- supported targets and limitations.
-   `docs/adr/` --- architecture decisions.
-   `CONTRIBUTING.md` --- build, test, style, and review rules.
-   `CHANGELOG.md` --- user-visible changes and migration notes.

Keep examples executable in CI where possible. Pin tool versions,
document known limitations, and avoid claiming a platform or deployment
mode is supported until it is tested.

------------------------------------------------------------------------

## 28. Official references

Use the official project documentation as the source of truth for
version-specific commands and capabilities:

-   Nakama repository: <https://github.com/heroiclabs/nakama>
-   Nakama documentation: <https://heroiclabs.com/docs/nakama/>
-   Bevy repository: <https://github.com/bevyengine/bevy>
-   Bevy documentation: <https://docs.rs/bevy/latest/bevy/>
-   Rust installation: <https://www.rust-lang.org/tools/install>
-   PostgreSQL documentation: <https://www.postgresql.org/docs/>

Before implementation, pin compatible versions and verify installation,
runtime, deployment, licensing, and platform requirements. This guide
describes the intended architecture; it is not a substitute for checking
the exact APIs and commands of the selected releases.
