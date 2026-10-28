# Windows batch scripts

Run these scripts from the repository or another working directory. Each
script locates the repository root itself, waits for a keypress after showing
the result, and returns the command's exit code. The aggregate check suppresses
pauses in the scripts it calls and waits once at the end.

| Script | Purpose | Example |
|---|---|---|
| `all-checks.bat` | Formatting check, Clippy, workspace tests, and build | `BAT\all-checks.bat` |
| `build.bat` | Build the Rust workspace | `BAT\build.bat --release` |
| `run-server.bat` | Start the local demo world server and show logs | `BAT\run-server.bat` |
| `run-client.bat` | Launch the local demo client or offline placeholder | `BAT\run-client.bat` |
| `check.bat` | Check all workspace targets without linking | `BAT\check.bat` |
| `clippy.bat` | Run Clippy with warnings denied | `BAT\clippy.bat` |
| `clean.bat` | Remove Cargo build artifacts | `BAT\clean.bat` |
| `format.bat` | Format the workspace | `BAT\format.bat` |
| `format-check.bat` | Verify formatting without changing files | `BAT\format-check.bat` |
| `test.bat` | Run all workspace tests | `BAT\test.bat` |
| `new-project.bat` | Generate a project from a template | `BAT\new-project.bat tavern --template minimal --output ..\tavern` |
| `validate.bat` | Validate content in a project directory | `BAT\validate.bat ..\tavern` |
| `doctor.bat` | Diagnose a project, or pass CLI doctor options | `BAT\doctor.bat --backend` |
| `doctor-backend.bat` | Check connectivity to the configured Nakama backend | `BAT\doctor-backend.bat` |
| `content-hash.bat` | Print a file's SHA-256 hash | `BAT\content-hash.bat content\manifest.json` |
| `build-project.bat` | Build a generated project's manifest | `BAT\build-project.bat ..\tavern\Cargo.toml` |
| `test-project.bat` | Test a generated project's manifest | `BAT\test-project.bat ..\tavern\Cargo.toml` |
| `install-cli.bat` | Install the `lynx` CLI with Cargo | `BAT\install-cli.bat` |

`doctor-backend.bat` requires a running Nakama service. Set
`LYNX_NAKAMA_URL` (or `NAKAMA_URL` / `NAKAMA_BASE_URL`) to override the
default `http://127.0.0.1:7350`. Other engine checks do not require Nakama or
PostgreSQL.

`run-server.bat` prompts for one or more demo tokens (the typed text is visible),
hashes them, and listens only on `127.0.0.1:8090`. Enter a distinct token per
simultaneous client and reuse the same tokens in the same order on restart. The
server logs to the command window; press Ctrl+C to stop it. World snapshots
are stored in the gitignored `.lynx/world.json`; sessions are not persisted.
The launcher displays the health, readiness, and WebSocket addresses. It does
not provide an interactive server console: query endpoints from another terminal
or use an authenticated protocol client for supported world commands.
Extra arguments are passed to `lynx-world serve`. This static-token demo is not
production authentication and must not be exposed publicly. Rust and PowerShell
must be installed; Nakama and PostgreSQL are not needed for this launcher.

For a separate multiplayer test world, run `BAT\run-server.bat --multiplayer`
from Command Prompt and enter at least two different tokens. It saves to
`.lynx/multiplayer-world.json`, leaving `.lynx/world.json` untouched. Each
client must use a different token. This option does not accept extra arguments.
Demo snapshots do not automatically gain characters when tokens are added later:
choose all tokens on the first start of a given world. Existing saved worlds
are not reset automatically.

`run-client.bat` prompts for the same demo token as the server (typed text is
visible) and opens the development-only online view. Start `run-server.bat`
first. The online view is read-only; client movement and interactions are not
yet available. The default address is `ws://127.0.0.1:8090/world`; set
`LYNX_DEMO_URL` before launching to override it. Run
`BAT\run-client.bat --offline` for the standalone placeholder with WASD movement.
Use disposable credentials: environment variables can be visible to other local
processes or diagnostics. Rust and a working graphical environment are required.
