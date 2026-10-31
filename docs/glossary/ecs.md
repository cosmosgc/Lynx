# Entity-component system (ECS)

An **entity-component system**, or **ECS**, organizes a game world as entities
with components. An entity is an identity; components are pieces of data such
as position, health, inventory, or movement status. Systems operate on entities
that have the components they need.

For example, a door entity might have position, interaction, and door-state
components. The interaction system can find usable entities without every kind
of entity needing its own custom class. Lynx's authoritative simulation has its
own entity model; Bevy uses ECS for client-side presentation. See
[architecture](../architecture.md#key-domain-concepts).
