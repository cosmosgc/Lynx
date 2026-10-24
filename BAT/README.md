# Windows batch scripts

Run these scripts from the repository or another working directory. Each
script locates the repository root itself and returns the command's exit code.

| Script | Purpose | Example |
|---|---|---|
| `all-checks.bat` | Formatting check, Clippy, workspace tests, and build | `BAT\all-checks.bat` |
| `build.bat` | Build the Rust workspace | `BAT\build.bat --release` |
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
