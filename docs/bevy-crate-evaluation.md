# Bevy crate evaluation

**Reviewed:** 2026-11-05. Initial fit/compatibility screen; subsequent prototype
adoption is recorded below.

## Project constraints

Lynx pins Bevy **0.20.0** and confines Bevy to client/editor crates; the headless `engine_core`/`engine_sim` gameplay surface must stay Bevy-free ([workspace manifest](../Cargo.toml), [ADR 0001](adr/0001-rust-and-bevy-version-policy.md)). The world server has its own authenticated WebSocket transport and `engine_net` protocol, while Nakama owns account/session/social services ([architecture](architecture.md), [networking](networking.md)). Persistent player/world records use PostgreSQL and explicit versioned/idempotent policies, not client-local files ([ADR 0005](adr/0005-persistence-and-transactions.md)). Navigation is currently deterministic, bounded A* over authored uniform cost grids; navmeshes are reserved as a future content variant ([ADR 0010](adr/0010-navigation-and-pathfinding.md)).

“Use now” means a direct low-risk fit for the current client/editor; “conditional” means only adopt for the stated feature after a small prototype and boundary review; “not recommended” means incompatible or mismatched for current scope.

## Evaluation

| Crate | Verdict | Fit, compatibility, and integration notes |
|---|---|---|
| [`bevy-persistent`](https://crates.io/crates/bevy-persistent) | **Use now — narrowly, for client-local preferences** | Current 0.12.0 (released 2026-10-24) directly depends on Bevy 0.20 ECS/log/reflect and supports file formats plus browser storage ([crate metadata/source](https://docs.rs/crate/bevy-persistent/latest/), [README](https://github.com/umut-sahin/bevy-persistent)). Good candidate for settings/keybinds or other single-client preferences. Its resource wrapper writes local storage; it is not a replacement for Lynx's PostgreSQL transactions, versioned zone snapshots, or server-authoritative persistence. Keep it client-only. |
| [`avian`](https://github.com/avianphysics/avian) | **Conditional — client-side physics prototype** | Avian 0.8 targets Bevy 0.20 and provides ECS-driven 2D/3D rigid bodies, collision queries, and debug rendering ([project README/version table](https://github.com/avianphysics/avian)). Useful if the client needs local collision, interaction queries, or physical gameplay. It would not automatically satisfy server authority, deterministic movement validation, or current server grid navigation; adding it to simulation would violate ADR 0001. Prototype only in a client crate and define what state the server validates. |
| [`bevy_vector_shapes`](https://github.com/james-j-obrien/bevy_vector_shapes) | **Conditional — editor/debug visuals** | 0.14.0 targets Bevy 0.20 and offers instanced 2D/3D vector shapes and a canvas API ([README](https://github.com/james-j-obrien/bevy_vector_shapes), [manifest](https://raw.githubusercontent.com/james-j-obrien/bevy_vector_shapes/main/Cargo.toml)). Could help draw navigation, territory, or interaction overlays in the editor/client. Its README explicitly warns it is early-stage, so keep it an optional presentation dependency and compare against Bevy's built-in gizmos first. |
| [`bevy_immediate`](https://github.com/PPakalns/bevy_immediate/) | **Conditional — UI architecture choice** | Current 0.9.0 targets Bevy 0.20 (manifest and README version table); it builds immediate-mode-style APIs on Bevy ECS/retained UI and supports widgets, floating windows, and extensions ([README](https://github.com/PPakalns/bevy_immediate/), [manifest](https://github.com/PPakalns/bevy_immediate/blob/main/Cargo.toml)). It could speed up client/editor menus, but it is a UI framework choice rather than a small widget add-on, and the README says it is under active development with breaking changes expected. Trial against Lynx's planned panel/session state and accessibility needs before standardizing. |
| [`bevy_rich_text3d`](https://github.com/mintlu8/bevy_rich_text3d) | **Conditional — world-space labels only** | Current 0.8.0 uses Bevy 0.20 and renders styled text as mesh/image-atlas glyphs with shader-oriented per-glyph data ([README/version table](https://github.com/mintlu8/bevy_rich_text3d), [manifest](https://github.com/mintlu8/bevy_rich_text3d/blob/main/Cargo.toml)). Consider for floating names/signage where mesh text is specifically wanted, not ordinary HUD/chat/localized UI text. It brings font-atlas and mesh rendering concerns and adds no authoritative/game-domain capability. |
| [`bevy_voxel_world`](https://github.com/splashdust/bevy_voxel_world) | **Conditional — only for a voxel-world direction** | Current 0.19.1 explicitly supports Bevy 0.20; it provides chunk streaming, multithreaded meshing, editable/persisted voxel overrides, and LOD ([README/version table](https://github.com/splashdust/bevy_voxel_world), [manifest](https://github.com/splashdust/bevy_voxel_world/blob/main/Cargo.toml)). The repository is active enough to document current support, but README notes procedural chunks are cached and may consume substantial memory with large spawn distance. This does not match today's authored zone atlas and grid-navigation model; if used, keep it a client renderer/authoring tool initially, and design server-owned voxel changes and persistence separately. |
| [`dogoap`](https://github.com/victorb/dogoap) | **Conditional — AI-system research/prototype** | The repository describes data-oriented GOAP plus a Bevy integration. Its current `bevy_dogoap` 0.6.0 manifest pins Bevy 0.17 components, not 0.20 ([README](https://github.com/victorb/dogoap), [workspace manifest](https://github.com/victorb/dogoap/blob/master/Cargo.toml), [integration manifest](https://github.com/victorb/dogoap/blob/master/crates/bevy_dogoap/Cargo.toml)). The planner could inform more compositional NPC task planning, but current Lynx AI is headless and server-authoritative, so adopting the Bevy integration directly conflicts with the engine boundary. Revisit only if a Bevy-free core is viable or after a Bevy 0.20 port/prototype and bounded deterministic planning review. |

## Bottom line

**Prototype adoption (2026-11-05):** At the user's request, `lynx_client` pins
`bevy_immediate = 0.9.0` behind the optional `immediate-prototype` feature.
Only `ui` and `bevy_ui_widgets` are enabled; Feathers and scene support are not
needed. `interaction_ui_prototype` compares three native layouts using a local
fixture and real Bevy immediate buttons. It builds against the workspace's
Bevy 0.20.0 and is excluded from normal/headless client builds. User layout
review remains pending; this is not yet the production panel framework.

**Selected layout integration:** The user selected the dialog/keypad variant.
`bevy_immediate` is now enabled by the normal client `window` feature, and
`lock_panel.rs` renders the real server-backed minigame in both demo and account
online windows. The standalone example remains a historical fixture. Headless
no-default-features builds still exclude the dependency. This validates one
minigame panel; generic authored schemas and inventory remain separate work.

The only straightforward current addition is `bevy-persistent` for **client-local preferences**, if Lynx wants Bevy-resource integration instead of its own settings adapter. `bevy_vector_shapes`, `bevy_immediate`, `bevy_rich_text3d`, Avian, `bevy_voxel_world`, and `dogoap` merit targeted prototypes only when their specific feature need lands. The rest are mismatched, outdated, or add an unrelated network/world model. None should be placed in `engine_sim` merely because it integrates with Bevy.
