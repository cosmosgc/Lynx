# Getting started (native, no Docker)

Lynx requires no container runtime for engine development or for running the
backend natively.

## 1. Engine workspace

Prerequisites:

- Windows 10/11, Linux, or macOS on x86-64.
- Rust installed through [rustup](https://rustup.rs). The pinned toolchain in
  `rust-toolchain.toml` is installed automatically on first build.

```bash
rustup show
cargo test --workspace
cargo run -p project_cli -- new my-world --template minimal --output ../my-world
cargo run -p project_cli -- doctor --project ../my-world
```

Windows users can run the matching scripts in `BAT\` from any working
directory. For example:

```bat
BAT\test.bat
BAT\build.bat
BAT\validate.bat ..\my-world
BAT\all-checks.bat
```

[`BAT/README.md`](../BAT/README.md) lists the available scripts and examples.
`BAT\all-checks.bat` runs formatting verification, Clippy, workspace tests, and
a workspace build. Separate scripts are also available for formatting,
validation, diagnostics, scaffolding, content hashing and diffing, and building
or testing a generated project.

`cargo test --workspace` must pass without network access after dependencies
are fetched. No database or Nakama instance is needed for engine tests.

## 2. PostgreSQL 17 (native)

Install PostgreSQL 17 using your platform installer or package manager, then
run the setup script. It creates a role, a database, and the required
extensions, and is safe to re-run.

Windows (elevated PowerShell, `psql` on `PATH`):

```powershell
./scripts/setup-postgres.ps1
```

Linux/macOS:

```bash
sudo -u postgres ./scripts/setup-postgres.sh
```

Apply migrations:

```powershell
./scripts/db-migrate.ps1
```

```bash
./scripts/db-migrate.sh
```

Environment variables (see `.env.example`):

| Variable | Default | Purpose |
|---|---|---|
| `LYNX_PG_HOST` | `127.0.0.1` | PostgreSQL host |
| `LYNX_PG_PORT` | `5432` | PostgreSQL port |
| `LYNX_PG_DATABASE` | `lynx` | Database name |
| `LYNX_PG_USER` | `lynx` | Least-privilege application role |
| `LYNX_PG_PASSWORD` | — | Required; never commit |

## 3. Nakama 3.41 (native)

Download the official release for your platform from
<https://github.com/heroiclabs/nakama/releases/tag/v3.41.0> and place the
binary on `PATH` (or set `NAKAMA_BIN`).

Windows note: the official Windows build is distributed as a `.tar.gz`; extract
it with `tar -xzf nakama-3.41.0-windows-amd64.tar.gz`.

Start the server:

```powershell
./scripts/run-nakama.ps1
```

```bash
./scripts/run-nakama.sh
```

Verify:

```bash
curl http://127.0.0.1:7350/healthcheck
```

The expected response is `{}` with HTTP 200. Nakama's console listens on
`127.0.0.1:7351`; it is bound to localhost in the development config.

Check engine-to-backend connectivity:

```bash
cargo run -p project_cli -- doctor --backend
```

## 4. First project

```bash
cargo run -p project_cli -- new tavern --template minimal --output ../tavern
cd ../tavern
cargo run -p project_cli -- doctor --project .
cargo run -p project_cli -- validate --project .
```

The generated project is a normal Cargo workspace that depends on Lynx crates
by path or version. It contains a zone, a sample entity, an item, an
interaction, a UI panel definition, a Brazilian Portuguese localization
catalog (`content/localization/pt-BR.ron`), a backend configuration template
with placeholder values, and migration hooks. The project defaults to `pt-BR`;
additional locales are declared in the project manifest.

## 5. Language and localization

`pt-BR` is the default and source language for generated projects. Player-facing
text lives in `content/localization/pt-BR.ron` and is referenced by message key.

```toml
[localization]
default = "pt-BR"
locales = ["pt-BR"]
```

Add another locale by creating `content/localization/<locale>.ron`, declaring
it in `locales`, and translating the keys. `lynx validate` fails on missing
default keys, mismatched placeholders, and incomplete release locales. See
[`internationalization.md`](internationalization.md).

## 6. Troubleshooting

- **`rustup` cannot find the pinned toolchain** — update rustup, then retry.
  Do not edit `rust-toolchain.toml` to work around a local mismatch.
- **`psql` is not recognized** — add `C:\Program Files\PostgreSQL\17\bin` to
  `PATH` or pass its full path in the environment.
- **Nakama exits with a database error** — verify `LYNX_PG_*` values and that
  the database exists; Nakama does not create the database for you.
- **Port already in use** — stop the conflicting service or change the ports in
  `server/nakama/local.yml` and your client configuration together.
- **Doctor reports a content error** — the message includes the file, JSON/RON
  pointer, and schema version. Fix the content; do not disable validation.
