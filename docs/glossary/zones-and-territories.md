# Zones and territories

A **zone** is an authored area of the game world and a boundary for content and
simulation. It has a stable identity, geometry, and connections to other zones.
A zone does not have to be its own server process.

A **territory** is political ownership state associated with one or more zones.
Capturing a territory changes who controls it; it does not change the zone's
map, identity, or authored content. A zone can also be explicitly unclaimed.

Keeping these ideas separate lets a world change ownership without rewriting
its map. See [world simulation: world map and zones](../world-simulation.md#world-map-and-zones).
