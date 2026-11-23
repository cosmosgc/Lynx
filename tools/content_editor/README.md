# Lynx Content Editor

This is a standalone Bevy application. It depends on `engine_content` for the
same project loading and validation used by the `lynx` CLI; it does not depend
on the player client or the authoritative simulation.

Open a generated project with:

```powershell
cargo run -p lynx_editor -- D:\path\to\project
```

The editor opens the project's `content/` directory, renders the selected world
map in the scene view, and uses egui for its inspector and authoring controls.
It watches content files and reloads external changes when there are no unsaved
map edits. If files change while edits are pending, the editor keeps those edits
and asks before overwriting the changed files.

The inspector provides clickable map selection, zone selection, placement,
deletion, undo/redo, validation, save, preview, and local play controls. It
adapts to narrow windows; keyboard shortcuts remain available. The current
authoring slice edits world-map zone placements:

| Input | Action |
| --- | --- |
| Click a zone marker | Select it |
| Drag a zone marker | Move it by map cells |
| `M` | Select the next world map |
| `Tab` | Select the next placed zone |
| Arrow keys | Move the selected zone one cell |
| `N` | Place the next unplaced zone definition |
| `Delete` | Remove the selected placement and connected routes |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo the latest map edit |
| `Ctrl+S` | Save changed map files |
| `V` | Re-run shared content validation |
| `Space` | Toggle the read-only map preview |
| `P` | Save edits, then launch or stop the offline Lynx client for the selected zone |

Unsaved map edits are periodically written to
`.lynx/editor-recovery.ron`. Recovery is loaded when the same project is opened
again; saving removes that recovery snapshot. Play preview launches the existing
offline client as a separate process, keeping the editor independent of player
and simulation crates.

Run `cargo test -p lynx_editor --lib` for session-level verification of exact
undo/redo, save/reopen, external reload, and autosave recovery after an interrupted
file replacement. These tests use temporary map fixtures.

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
