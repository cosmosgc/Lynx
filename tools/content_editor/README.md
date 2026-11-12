# Lynx Content Editor

This is a standalone Bevy application. It depends on `engine_content` for the
same project loading and validation used by the `lynx` CLI; it does not depend
on the player client or the authoritative simulation.

Open a generated project with:

```powershell
cargo run -p lynx_editor -- D:\path\to\project
```

The editor opens the project's `content/` directory, renders the selected world
map in the scene view, and reports validation findings in the inspector. The
current authoring slice edits world-map zone placements:

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
and simulation crates. End-to-end open/edit/save/reopen and crash-recovery
verification remains.
