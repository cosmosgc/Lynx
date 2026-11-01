#!/usr/bin/env bash
# Create the Lynx PostgreSQL role, database, schema, and extension.
#
# Native PostgreSQL 17 setup with no container runtime. Safe to re-run: the
# role, database, and schema are created only when missing, and the application
# passwords are reset from the configured runtime/migration variables each time.
#
# Run as a PostgreSQL superuser. On Linux, for example:
#   sudo -u postgres ./scripts/setup-postgres.sh
# Set LYNX_PG_SUPERUSER to use a different role, and PGPASSWORD or a pgpass file
# if that role needs a password. The application password is read from
# LYNX_PG_PASSWORD and never appears in an argument list.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/common.sh
. "$script_dir/common.sh"

root="$(cd "$script_dir/.." && pwd)"
lynx_import_env "$root"
lynx_require_pg
lynx_require_psql

superuser="${LYNX_PG_SUPERUSER:-postgres}"
maintenance_db="${LYNX_PG_MAINTENANCE_DB:-postgres}"

printf 'Lynx PostgreSQL setup\n'
printf '  server:    %s:%s\n' "$LYNX_PG_HOST" "$LYNX_PG_PORT"
printf '  database:  %s\n' "$LYNX_PG_DATABASE"
printf '  app role:  %s\n' "$LYNX_PG_USER"
printf '  superuser: %s\n\n' "$superuser"

LYNX_PSQL_USER="$superuser" LYNX_PSQL_DATABASE="$maintenance_db" lynx_psql_args
server_version="$(lynx_psql -c 'SHOW server_version_num;')"
server_version="${server_version//[[:space:]]/}"
[[ "$server_version" =~ ^[0-9]+$ ]] ||
    lynx_die "could not read the PostgreSQL server version: '$server_version'"
if ((server_version < 170000)); then
    lynx_die "PostgreSQL 17 or newer is required; the server reports version $server_version"
fi
if ((server_version >= 180000)); then
    printf 'warning: PostgreSQL %s is newer than the pinned baseline (17)\n' "$server_version" >&2
fi

printf 'Creating role and database (idempotent)...\n'
# \getenv keeps the application password out of the psql argument list.
LYNX_PSQL_USER="$superuser" LYNX_PSQL_DATABASE="$maintenance_db" lynx_psql_args
lynx_psql \
    -v app_user="$LYNX_PG_USER" \
    -v migration_user="$LYNX_PG_MIGRATION_USER" \
    -v db_name="$LYNX_PG_DATABASE" \
    -f - <<'SQL'
\getenv app_password LYNX_PG_PASSWORD
\getenv migration_password LYNX_PG_MIGRATION_PASSWORD
SELECT format('CREATE ROLE %I LOGIN', :'app_user')
WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'app_user')
\gexec
SELECT format('CREATE ROLE %I LOGIN', :'migration_user')
WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'migration_user')
\gexec
ALTER ROLE :"app_user" WITH LOGIN PASSWORD :'app_password';
ALTER ROLE :"migration_user" WITH LOGIN PASSWORD :'migration_password';
SELECT format('CREATE DATABASE %I', :'db_name')
WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = :'db_name')
\gexec
SQL

printf 'Creating schema, extension, and grants (idempotent)...\n'
LYNX_PSQL_USER="$superuser" LYNX_PSQL_DATABASE="$LYNX_PG_DATABASE" lynx_psql_args
lynx_psql \
    -v app_user="$LYNX_PG_USER" \
    -v migration_user="$LYNX_PG_MIGRATION_USER" \
    -v db_name="$LYNX_PG_DATABASE" \
    -f - <<'SQL'
CREATE SCHEMA IF NOT EXISTS lynx AUTHORIZATION :"migration_user";
ALTER SCHEMA lynx OWNER TO :"migration_user";
CREATE EXTENSION IF NOT EXISTS pg_trgm;
SELECT format(
    'ALTER %s %I.%I OWNER TO %I',
    CASE c.relkind WHEN 'S' THEN 'SEQUENCE' WHEN 'v' THEN 'VIEW'
        WHEN 'm' THEN 'MATERIALIZED VIEW' WHEN 'f' THEN 'FOREIGN TABLE' ELSE 'TABLE' END,
    n.nspname, c.relname, :'migration_user')
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
JOIN pg_roles AS r ON r.oid = c.relowner
WHERE n.nspname = 'lynx' AND r.rolname = :'app_user'
  AND :'app_user' <> :'migration_user'
  AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
\gexec
GRANT CONNECT ON DATABASE :"db_name" TO :"app_user";
GRANT CONNECT ON DATABASE :"db_name" TO :"migration_user";
GRANT USAGE ON SCHEMA lynx TO :"app_user";
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA lynx TO :"app_user";
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA lynx TO :"app_user";
GRANT SELECT ON ALL TABLES IN SCHEMA lynx TO :"migration_user";
ALTER DEFAULT PRIVILEGES FOR ROLE :"migration_user" IN SCHEMA lynx
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO :"app_user";
ALTER DEFAULT PRIVILEGES FOR ROLE :"migration_user" IN SCHEMA lynx
    GRANT USAGE, SELECT ON SEQUENCES TO :"app_user";
GRANT USAGE, CREATE ON SCHEMA public TO :"app_user";
SQL

server_version="$(lynx_psql -c 'SHOW server_version;')"
printf '\nPostgreSQL %s is ready.\n' "$server_version"
printf "Schema 'lynx' is owned by role '%s'.\n" "$LYNX_PG_MIGRATION_USER"
printf 'Next: ./scripts/db-migrate.sh\n\n'
printf 'Note: Nakama shares this database and creates its own tables in the public schema.\n'
