#!/usr/bin/env bash
# Apply versioned SQL migrations to the Lynx PostgreSQL schema.
#
# Migrations are read from server/migrations in filename order. Each one runs in
# a single transaction together with its lynx.schema_migrations row and a
# transaction-scoped advisory lock. Applied migrations are skipped; a changed
# checksum aborts the run.
#
# The script reads LYNX_PG_* from the environment or .env and connects as the
# application role. Run scripts/setup-postgres.sh first.
#
# Usage:
#   ./scripts/db-migrate.sh
#   ./scripts/db-migrate.sh --status

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/common.sh
. "$script_dir/common.sh"

status_only=false
case "${1-}" in
    --status | -Status) status_only=true ;;
    '') ;;
    *)
        lynx_die "unknown argument: $1 (expected --status)"
        ;;
esac

root="$(cd "$script_dir/.." && pwd)"
lynx_import_env "$root"
lynx_require_pg
lynx_require_psql

migration_dir="$root/server/migrations"
[[ -d "$migration_dir" ]] || lynx_die "migration directory not found: $migration_dir"

shopt -s nullglob
files=("$migration_dir"/*.sql)
shopt -u nullglob
((${#files[@]} > 0)) || lynx_die "no migrations found in $migration_dir"

# The application role owns the schema; authenticate with its password.
export PGPASSWORD="$LYNX_PG_PASSWORD"
lynx_psql_args

has_schema="$(lynx_psql -c "SELECT to_regnamespace('lynx') IS NOT NULL;")"
[[ "$has_schema" == "t" ]] ||
    lynx_die "the 'lynx' schema does not exist in database '$LYNX_PG_DATABASE'. Run scripts/setup-postgres.sh first."

has_history="$(lynx_psql -c "SELECT to_regclass('lynx.schema_migrations') IS NOT NULL;")"

if [[ "$status_only" == false ]]; then
    lynx_psql -f - <<'SQL'
BEGIN;
SELECT pg_advisory_xact_lock(1280522872);
CREATE TABLE IF NOT EXISTS lynx.schema_migrations (
    version text PRIMARY KEY,
    checksum text NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT schema_migrations_version_format CHECK (
        version ~ '^[0-9]{4}_[a-z0-9_]+$'
    ),
    CONSTRAINT schema_migrations_checksum_sha256 CHECK (
        checksum ~ '^[0-9a-f]{64}$'
    )
);
COMMIT;
SQL
    has_history="t"
fi

highest_applied=""
if [[ "$has_history" == "t" ]]; then
    highest_applied="$(lynx_psql -c "SELECT COALESCE(MAX(version), '') FROM lynx.schema_migrations;")"
fi

applied_count=0
skipped_count=0
pending_count=0
checksum_failures=""

printf 'Lynx migrations in %s\n' "$migration_dir"
printf '  database: %s on %s:%s as %s\n\n' \
    "$LYNX_PG_DATABASE" "$LYNX_PG_HOST" "$LYNX_PG_PORT" "$LYNX_PG_USER"

for file in "${files[@]}"; do
    name="$(basename "$file")"
    version="${name%.sql}"
    [[ "$version" =~ ^[0-9]{4}_[a-z0-9_]+$ ]] ||
        lynx_die "migration '$name' must match NNNN_lower_snake_case.sql"

    checksum="$(lynx_sha256_file "$file")"
    existing=""
    if [[ "$has_history" == "t" ]]; then
        existing="$(lynx_psql -c "SELECT checksum FROM lynx.schema_migrations WHERE version = '$version';")"
    fi

    if [[ -n "$existing" ]]; then
        if [[ "$existing" != "$checksum" ]]; then
            checksum_failures="${checksum_failures:+$checksum_failures, }$version"
            printf '%-28s %s\n' "$name" 'CHANGED'
        elif [[ "$status_only" == true ]]; then
            printf '%-28s %s\n' "$name" 'applied'
        else
            printf '%-28s %s\n' "$name" 'up to date'
        fi
        skipped_count=$((skipped_count + 1))
        continue
    fi

    if [[ "$status_only" == true ]]; then
        printf '%-28s %s\n' "$name" 'pending'
        pending_count=$((pending_count + 1))
        continue
    fi

    if [[ -n "$highest_applied" && "$version" < "$highest_applied" ]]; then
        printf 'warning: migration %s is older than applied migration %s; applying it out of order\n' \
            "$version" "$highest_applied" >&2
    fi

    printf '%-28s ' "$name"
    {
        printf 'BEGIN;\n'
        printf 'SELECT pg_advisory_xact_lock(1280522872);\n'
        cat "$file"
        printf "\nINSERT INTO lynx.schema_migrations (version, checksum) VALUES ('%s', '%s');\n" \
            "$version" "$checksum"
        printf 'COMMIT;\n'
    } | lynx_psql -f - >/dev/null
    printf 'applied\n'
    applied_count=$((applied_count + 1))
done

printf '\n'
if [[ -n "$checksum_failures" ]]; then
    lynx_die "applied migrations changed since they ran: $checksum_failures. Restore the files or write a new migration."
fi

if [[ "$status_only" == true ]]; then
    printf 'Summary: %s applied, %s pending.\n' "$skipped_count" "$pending_count"
else
    printf 'Summary: %s applied, %s already up to date.\n' "$applied_count" "$skipped_count"
fi
