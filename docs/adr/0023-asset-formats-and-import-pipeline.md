# ADR 0023: Asset formats and import pipeline

- **Status:** Accepted
- **Date:** 2026-11-12

## Decision

Use glTF 2.0 (`.gltf`/`.glb`) for 3D model and scene interchange, PNG for source
2D images, and Ogg Vorbis for source audio. Keep editable sources separate
from generated runtime bundles. The import pipeline validates format, size,
dimensions, references, and licensing metadata, then records content hashes in
the existing versioned manifest. Runtime crates consume imported assets and do
not parse creator project folders. Target-specific compression and previews
are generated artifacts and never replace the source files.

## Consequences

- The editor and runtime share a stable interchange format without making
  Bevy's asset handles part of the project file format.
- Import settings and derived artifacts need explicit versioning so a toolchain
  upgrade cannot silently change published bundle hashes.
- The allowlist, size budgets, and licensing validation are enforced by the
  asset-pipeline task before public publishing is enabled.
