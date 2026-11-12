# Lynx content domain

Terms for authored bundles, their versioned definitions, and controlled upgrades.

## Content and migration

**Content bundle**:
A versioned set of authored definitions and locale catalogs published together.

**Content definition**:
An authored, versioned description of a kind of item, entity, zone, map, behavior, UI, or minigame.

**Typed entity link**:
An authored directed connection between entity placements that declares compatible endpoint components, supported events, and an authoritative action.

**Definition schema version**:
The version of a definition's data contract; a breaking change may require saved instances to be transformed.

**Content migration**:
A reviewed, deterministic transformation from one content bundle version to another.

**Persisted instance**:
A durable entity created from an authored prototype, with mutable state stored independently from the prototype definition.

**Quarantined entity**:
A persisted instance kept outside the active world because the current content bundle cannot safely validate or migrate it.

**Migration plan**:
The ordered, versioned set of changes that describes one content migration.

**Instance migration**:
A transformation of persisted mutable state so a live instance remains valid under a newer definition schema.

## Economy and audit

**Inventory ledger entry**:
An immutable signed quantity change for one item definition owned by one character.

**Opening balance**:
The recorded starting quantity for an owner and item when ledger reconciliation begins.

**Value movement**:
A sourced change to an inventory quantity or currency balance, recorded with its actor and correlation ID.
