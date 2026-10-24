# ADR 0015: Server-owned UI panel session state

- **Status:** Accepted
- **Date:** 2026-10-24

## Context

Phase 4 introduces entity-provided interfaces. `engine_sim` already models a
`UiProvider` on an entity and the action pipeline emits `ActionEffect::UiOpened`
when a validated interaction opens one. The effect reached the client only
inside `action.result`, so there was no open/close lifecycle and no server-side
record of which panel a session was showing.

That gap made panel actions unsafe to add naively. If `ui.action` carried a
target, a client could act on an entity it had never opened or could keep
acting after walking out of range. If it carried an arbitrary schema, a stale
client could apply an old panel's actions to a replacement panel. If the server
trusted an action name without re-running the action pipeline, it would bypass
range, visibility, permission, capability, state, and cooldown checks that
every other interaction already uses.

The gateway must stay transport-agnostic and must not depend on a particular
content bundle or client presentation module.

## Decision

- **Panels are session state, created only from validated effects.**
  `SessionRegistry` stores an `OpenPanel { target, schema, title }` for each
  session. The gateway writes it only when `Zone::apply_action` returns an
  applied `UiOpened` effect. The client never supplies the target, schema, or
  title when opening a panel.
- **`ui.state` is authoritative.** After a panel opens or changes, the gateway
  sends `ui.state` with the authoritative tick and either the open panel or
  `null`. The action result still carries the `UiOpened` effect, but
  `ui.state` is the canonical lifecycle message.
- **`ui.action` carries intent only.** Its body is
  `{ schema, action, params?, client_tick? }` with `deny_unknown_fields`.
  Attempts to include a target, actor, principal, session, or other field are
  rejected with `InvalidRequest` before the simulation runs. The target comes
  from the session's stored panel.
- **The schema is an optimistic concurrency check.** If the supplied schema
  does not equal the open panel's schema, the gateway returns `Conflict`.
  This prevents a stale client from acting on a panel that has been replaced.
- **Actions use the normal action pipeline.** The action name is parsed into a
  `Verb`; known engine verbs map directly, and other compact names become
  `Verb::Custom`, which the current simulation rejects as unsupported. The
  gateway builds an `ActionRequest` with the session's actor and principal and
  the stored panel target, then calls `Zone::apply_action`. Range, visibility,
  permission, target state, capabilities, cooldowns, and idempotency are all
  re-validated on every action.
- **`ui.close` is idempotent.** It has an empty body, clears the panel, and
  returns `ui.state` even when no panel was open.
- **Panel lifetime is explicit.** A new `UiOpened` replaces the current panel.
  A disconnect clears it, a resume starts with no panel, and an action that
  reports the provider is gone (`NotFound`) clears it. Session removal drops it
  with the rest of the session record.
- **Control errors stay correlated.** Malformed bodies, missing panels, and
  schema mismatches return `protocol.error`. Gameplay rejections return
  `action.result`, followed by `ui.state`, so the client can keep rendering
  the authoritative panel.
- **No content dependency is added.** `world_gateway` maps action names to
  engine verbs rather than loading panel definitions. Content-declared actions
  remain the declarative contract for rendering and gating; the simulation
  remains the authority for what an action can do.

## Consequences

- A client cannot redirect a panel action to another entity, act after moving
  out of range, or replay an action against a replaced panel.
- Panel state is in memory and resets on disconnect, resume, or process
  restart. Durable UI sessions are not part of this increment.
- The gateway does not yet verify that an action name is declared by the
  loaded `UiPanelDefinition`. That cross-check belongs to a future content
  runtime; today the simulation rejects verbs an entity does not support.
- Custom verbs remain declarative placeholders. A content runtime must define
  their effects before they can be executed safely.
- The gateway and server tests cover open, close, action, replacement, schema
  mismatch, spoofed fields, permission and range rejection, provider
  disappearance, and panel reset on disconnect and resume.
