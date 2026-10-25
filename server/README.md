# Native backend assets

This directory holds the non-secret configuration for running the Lynx backend
without Docker:

| Path | Purpose |
|---|---|
| `nakama/local.yml` | Development configuration for Nakama 3.41.0, bound to loopback. |
| `nakama/modules/` | Nakama runtime modules (Lua, JavaScript, or Go plugins). |
| `migrations/` | Versioned SQL migrations for the Lynx schema, applied by `scripts/db-migrate.*`. |

Secrets never live here. The run and migration scripts read the `LYNX_*`
environment variables described in [`.env.example`](../.env.example) and pass
them to Nakama or `psql` at process start.

`local.yml` deliberately omits `database.address`: `scripts/run-nakama.*` builds
the address from the environment. Running `nakama` with only `--config` falls
back to Nakama's default database and fails with a connection error, which is
safer than shipping a checked-in password.

The Nakama console binds to `127.0.0.1:7351`. Its development credentials come
from `LYNX_NAKAMA_CONSOLE_USER` and `LYNX_NAKAMA_CONSOLE_PASSWORD`; when they are
unset, Nakama's insecure defaults (`admin` / `password`) are used and the server
logs a warning. Set them for anything other than a throwaway local database.

See [`docs/getting-started.md`](../docs/getting-started.md) for the full native
setup and [`docs/adr/0003-native-nakama-and-postgresql.md`](../docs/adr/0003-native-nakama-and-postgresql.md)
for the decision behind it.
