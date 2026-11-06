# Online client demo launchers

Open two PowerShell windows at the repository root.

In the first window, start the server:

```powershell
.\tests\client-online-demo\start-demo-server.bat
```

This launcher creates a fresh high-entropy token in the ignored local file
`tests/client-online-demo/demo-token.txt`. It gives only the token's SHA-256
hash to the loopback server, which listens on `127.0.0.1:8090`.

In the second window, start the client:

```powershell
.\tests\client-online-demo\start-demo-client.bat
```

The client launcher reads the token file and starts Cargo from the same
PowerShell process that sets the token. It uses
`ws://127.0.0.1:8090/world`. Stop either process with Ctrl+C. Delete the token
file after stopping both processes if you no longer need this demo setup.

This is a development-only demo. Use WASD to move, T to compose chat, Enter to
send and Escape to cancel chat. Movement pauses while composing or unfocused.
Transient disconnects retry automatically; R retries a terminal failure.
If connection fails, copy the detailed reason
from the window title when reporting the problem.

## Live keypad minigame

These launchers now use the selected **`bevy_immediate` keypad dialog** in the
real online game. No extra Cargo feature or separate prototype command is needed.

1. Use Tab / Shift+Tab to select the vault. Approach it with WASD.
2. Press the number shown next to `use` in the interaction HUD.
3. Click the keypad digits, or type them. Click **Tentar** / press Enter to
   submit; **Apagar** / Backspace deletes one digit.
4. Follow the server's per-digit hints: `correto`, `maior`, and `menor`.
   Attempts, lockout, unlocking, and closure are authoritative.
5. Drag the title bar to move the window; it stays clamped inside the game
   viewport. The **X** in its top-right corner (or the X key) closes it. Walking
   out of range also closes it; return and use the vault again to reopen it at
   its last position.

Chat composition, focus loss, and an outstanding interaction disable keypad
input. Movement, targeting, replicated health, chat/presence, diagnostics, and
resume still run in this same client. Restart both launchers after updating so
the server and client run the current implementation.

`interaction_ui_prototype` remains a standalone historical layout fixture.
Its fixed demonstration code does **not** apply to this server-backed vault.
