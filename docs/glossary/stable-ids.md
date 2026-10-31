# Stable IDs

A **stable ID** is an identifier intended to keep referring to the same object
across saves, restarts, and network messages. It is different from a temporary
array position or an in-memory handle that may be reused after an object is
deleted.

Lynx uses stable IDs for durable entities, zones, factions, and other records so
references remain meaningful after reload. Authored definitions use a separate
kind of key, such as `door.wooden`; an instance of that door has its own stable
ID. See [ADR 0007](../adr/0007-stable-entity-ids.md) and
[content authoring: definitions vs. instances](../content-authoring.md#definitions-vs-instances).
