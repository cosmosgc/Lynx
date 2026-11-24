# Lynx Content Editor

This is a standalone Bevy application. It depends on `engine_content` for the
same project loading and validation used by the `lynx` CLI; it does not depend
on the player client or the authoritative simulation.

Open a generated project with:

```powershell
cargo run -p lynx_editor -- D:\path\to\project
```

The editor opens the project's `content/` directory and uses egui for its menu
bar and panels. The layout has a central scene view, a resizable Hierarchy panel
on the left, a resizable Inspector on the right, and a status bar. The hierarchy
lists world-map zones and, in Zone Scene view, the selected zone's scene objects.
It watches content files and reloads external changes when there are no unsaved
edits. If files change while edits are pending, the editor keeps those edits and
asks before overwriting the changed files.

The **File**, **Edit**, and **View** menus provide project open/save/reload,
undo/redo, surface selection, and panel visibility controls. The Open Project
dialog accepts a project folder path containing `content/`. The inspector
provides atlas placement controls, zone-placement transform fields, validation,
preview, and local play. The current authoring slice edits world-map zone
placements and the selected zone's existing entity-placement transforms:

| Input | Action |
| --- | --- |
| Click a zone marker | Select it |
| Drag a zone marker | Move it by map cells |
| `M` | Select the next world map |
| `Tab` | Select the next placed zone |
| Arrow keys | Move the selected zone one cell |
| `N` | Place the next unplaced zone definition |
| `Delete` | Remove the selected placement and connected routes |
| `Ctrl+O` | Open another project |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo the latest content edit |
| `Ctrl+S` | Save changed content files |
| `V` | Re-run shared content validation |
| `Space` | Toggle read-only preview |
| Zone Scene: right-drag / middle-drag / wheel | Orbit / pan / zoom the 3D view |
| Click a scene placeholder | Select it in the scene and hierarchy |
| Inspector → Apply transform | Save the selected placement's position/yaw as one undoable edit |
| View menu | Switch atlas/zone-scene views and show/hide Hierarchy or Inspector |
| `P` | Save edits, then launch or stop the offline Lynx client for the selected zone |

Unsaved map and zone-definition edits are periodically written to
`.lynx/editor-recovery.ron`. Recovery is loaded when the same project is opened
again; saving removes that recovery snapshot. Play preview launches the existing
offline client as a separate process, keeping the editor independent of player
and simulation crates.

On Windows, double-click `BAT\open-content-editor.bat` to create the MMORPG demo
project in `content-editor-demo` (if it does not exist) and launch the editor.
The launcher does not overwrite an existing non-demo folder at that path.

Run `cargo test -p lynx_editor` for session, UI-control, and generated-project
verification, including undo/redo, save/reopen, external reload, transform
recovery, and CLI validation.

Run `cargo test -p lynx_editor --test generated_project -- --nocapture` for the
generated-project workflow. It builds the `lynx` CLI, generates both templates
in temporary directories, edits their atlases through the editor session, checks
exact undo/redo and save/reopen state, and runs `lynx validate` before and after.
It also injects a failure during a two-map save, confirms the partial disk state,
and verifies that reopening recovers both edits consistently before saving and
passing CLI validation.
The diagnostic test introduces an unknown zone reference, checks its map ID and
file/field path, compares the complete error list with `lynx validate --json`,
and verifies that repairing the reference clears errors in both paths.
Rendered control acceptance remains open.
