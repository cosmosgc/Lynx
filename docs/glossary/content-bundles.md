# Content bundles and versions

A **content bundle** is the collection of authored game data that a game
publishes for the server and clients to use. It can include things such as item
and entity definitions, maps, zones, quests, and localization. A bundle is
validated and packaged with a manifest that identifies its contents.

A bundle's **version** lets the engine check whether saved entities and clients
are compatible with the content currently deployed. Changing content does not
automatically rewrite saved game state: incompatible changes need a migration,
or the affected state must be rejected or quarantined safely.

In Lynx, see [content authoring](../content-authoring.md) and the
[content versioning task](../TODO/20-persistence-backend.md#pst-013--content-version-pinning-and-migration).
