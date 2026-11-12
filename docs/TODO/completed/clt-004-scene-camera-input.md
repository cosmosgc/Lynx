# CLT-004 — Scene, camera, and input abstraction

**Status: Done.** The offline client loads an authored project/content directory
into a presentation-only `ZoneScene`, with a built-in demo zone when no content
path is supplied. Manifest compatibility, safe asset paths, and SHA-256 hashes
are checked before parsing. Missing or unsupported presentation assets produce
diagnostics and placeholders; malformed or incompatible bundles return a clear
scene error.

`InputBindings` maps platform-neutral controls to actions and supports JSON
rebinding. `CameraController` selects `follow3d` or `billboard2d`, and the
offline client keeps input, actions, local movement, camera, and presentation
behind replaceable ordered seams. These remain presentation concerns; movement
authority and interaction outcomes belong to the server.

PNG portraits and GLB/GLTF models are the supported asset policy. Asset decoding
remains future rendering work. The task's acceptance criteria are checked in
[`40-bevy-client.md`](../40-bevy-client.md#clt-004--scene-camera-and-input-abstraction).
