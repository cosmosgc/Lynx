#!/usr/bin/env bash
# Test-only PostgreSQL backup/restore drill. Operates only on an explicitly
# named loopback test database and a uniquely prefixed scratch database.
set -euo pipefail

case "${OSTYPE:-}" in
    msys* | cygwin*)
        printf 'error: run this smoke on Linux, macOS, or WSL; Git Bash cannot reliably supervise the native world-service process\n' >&2
        exit 2
        ;;
esac

if (($# != 1)); then
    printf 'usage: %s PATH/TO/lynx-world\n' "$0" >&2
    exit 2
fi

world_bin="$1"
[[ -x "$world_bin" ]] || {
    printf 'error: world binary is not executable: %s\n' "$world_bin" >&2
    exit 2
}

# shellcheck source=scripts/common.sh
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/common.sh"
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
lynx_import_env "$repo_root"
lynx_require_pg

[[ -n "${LYNX_BACKUP_TEST_DATABASE:-}" ]] ||
    lynx_die 'set LYNX_BACKUP_TEST_DATABASE to an isolated test database before running this destructive-fixture smoke test'
[[ "$LYNX_PG_DATABASE" == "$LYNX_BACKUP_TEST_DATABASE" ]] ||
    lynx_die 'LYNX_PG_DATABASE must equal the explicit LYNX_BACKUP_TEST_DATABASE'
case "$LYNX_PG_DATABASE" in
    lynx_ci | lynx_backup_test_*) ;;
    *) lynx_die 'test database name must be lynx_ci or use the lynx_backup_test_ prefix' ;;
esac
case "$LYNX_PG_HOST" in
    127.* | ::1 | '[::1]') ;;
    *) lynx_die 'backup restore smoke is restricted to loopback PostgreSQL hosts' ;;
esac

for tool in pg_dump pg_restore createdb dropdb curl; do
    command -v "$tool" >/dev/null 2>&1 || lynx_die "$tool was not found on PATH"
done
lynx_require_psql

superuser="${LYNX_PG_SUPERUSER:-postgres}"
maintenance_db="${LYNX_PG_MAINTENANCE_DB:-postgres}"
[[ "$superuser" =~ ^[a-z_][a-z0-9_]{0,62}$ ]] || lynx_die 'invalid LYNX_PG_SUPERUSER'
[[ "$maintenance_db" =~ ^[a-z_][a-z0-9_]{0,62}$ ]] || lynx_die 'invalid LYNX_PG_MAINTENANCE_DB'

scratch="lynx_restore_check_$(date -u +%Y%m%d%H%M%S)_$$"
work="$(mktemp -d)"
chmod 700 "$work"
archive="$work/lynx.dump"
source_password="${PGPASSWORD-}"
had_source_password=false
[[ ${PGPASSWORD+x} ]] && had_source_password=true
scratch_created=false
world_pid=''
world_log="$work/world.log"
world_port="${LYNX_BACKUP_SMOKE_WORLD_PORT:-18090}"
admin_port="${LYNX_BACKUP_SMOKE_ADMIN_PORT:-18091}"

run_world() {
    local database="$1"
    # Keep the test independent of any real demo token hashes in a developer's
    # .env. The hash is deliberately unusable outside this loopback smoke.
    LYNX_WORLD_TOKEN_HASHES=0000000000000000000000000000000000000000000000000000000000000000 \
        LYNX_PG_DATABASE="$database" "$world_bin" serve \
        --state-postgres \
        --bind "127.0.0.1:$world_port" \
        --admin-bind "127.0.0.1:$admin_port" \
        --lease-ttl-ms 5000 \
        --snapshot-ms 1000 >"$world_log" 2>&1 &
    world_pid=$!

    for ((attempt = 0; attempt < 120; attempt++)); do
        if ! kill -0 "$world_pid" 2>/dev/null; then
            cat "$world_log" >&2
            lynx_die 'world service exited before becoming ready'
        fi
        if curl --silent --fail --max-time 1 "http://127.0.0.1:$admin_port/readyz" >/dev/null; then
            return 0
        fi
        sleep 0.25
    done
    cat "$world_log" >&2
    lynx_die 'world service did not become ready within 30 seconds'
}

stop_world() {
    if [[ -n "$world_pid" ]]; then
        kill -INT "$world_pid" 2>/dev/null || true
        wait "$world_pid" || {
            cat "$world_log" >&2
            lynx_die 'world service failed during graceful shutdown'
        }
        world_pid=''
    fi
}

cleanup() {
    local status=$?
    trap - EXIT
    if [[ -n "$world_pid" ]]; then
        kill -INT "$world_pid" 2>/dev/null || true
        wait "$world_pid" 2>/dev/null || status=1
    fi
    if [[ "$scratch_created" == true ]]; then
        if [[ "$had_source_password" == true ]]; then
            export PGPASSWORD="$source_password"
        else
            unset PGPASSWORD
        fi
        if ! dropdb --no-password --host "$LYNX_PG_HOST" --port "$LYNX_PG_PORT" \
            --username "$superuser" --maintenance-db "$maintenance_db" --if-exists "$scratch"; then
            printf "warning: failed to remove scratch database '%s'; verify and remove it manually\n" "$scratch" >&2
            status=1
        fi
    fi
    rm -rf -- "$work"
    exit "$status"
}
trap cleanup EXIT

export PGPASSWORD="$LYNX_PG_PASSWORD"

# Seed durable, non-secret continuity markers in this explicitly test-only DB.
psql -X -q -v ON_ERROR_STOP=1 -h "$LYNX_PG_HOST" -p "$LYNX_PG_PORT" \
    -U "$LYNX_PG_USER" -d "$LYNX_PG_DATABASE" <<'SQL'
INSERT INTO lynx.accounts (account_id, nakama_user_id)
VALUES ('backup-restore-smoke', '00000000-0000-4000-8000-000000000009')
ON CONFLICT (account_id) DO NOTHING;
INSERT INTO lynx.characters (character_id, account_id, name)
VALUES ('chr_00000000000000000000000001', 'backup-restore-smoke', 'Backup Restore Smoke')
ON CONFLICT (character_id) DO NOTHING;
INSERT INTO lynx.audit_events (
    occurred_at, actor_kind, action, outcome, correlation_id, details
)
SELECT clock_timestamp(), 'system', 'backup.restore.smoke', 'succeeded',
       'backup-restore-smoke', '{}'::jsonb
WHERE NOT EXISTS (
    SELECT 1 FROM lynx.audit_events WHERE correlation_id = 'backup-restore-smoke'
);
SQL

# Produce a real whole-world checkpoint before taking the logical backup.
run_world "$LYNX_PG_DATABASE"
stop_world

# Retain the original superuser libpq credential for database create/drop.
if [[ "$had_source_password" == true ]]; then
    export PGPASSWORD="$source_password"
else
    unset PGPASSWORD
fi
createdb --no-password --host "$LYNX_PG_HOST" --port "$LYNX_PG_PORT" \
    --username "$superuser" --maintenance-db "$maintenance_db" \
    --owner "$LYNX_PG_USER" --template template0 "$scratch"
scratch_created=true

export PGPASSWORD="$LYNX_PG_PASSWORD"
pg_dump --no-password --host "$LYNX_PG_HOST" --port "$LYNX_PG_PORT" \
    --username "$LYNX_PG_USER" --dbname "$LYNX_PG_DATABASE" \
    --format=custom --file="$archive"
checksum="$(lynx_sha256_file "$archive")"
[[ "$checksum" =~ ^[0-9a-f]{64}$ ]] || lynx_die 'could not calculate backup SHA-256'
[[ "$(lynx_sha256_file "$archive")" == "$checksum" ]] ||
    lynx_die 'backup archive checksum verification failed'

pg_restore --no-password --host "$LYNX_PG_HOST" --port "$LYNX_PG_PORT" \
    --username "$LYNX_PG_USER" --dbname "$scratch" \
    --no-owner --no-privileges --exit-on-error "$archive"

report_sql=$(cat <<'SQL'
SELECT 'schema_migrations=' || string_agg(version || ':' || checksum, ',' ORDER BY version) FROM lynx.schema_migrations;
SELECT 'accounts=' || count(*) FROM lynx.accounts;
SELECT 'characters=' || count(*) FROM lynx.characters;
SELECT 'smoke_character=' || count(*) FROM lynx.characters WHERE character_id = 'chr_00000000000000000000000001' AND account_id = 'backup-restore-smoke';
SELECT 'audit_events=' || count(*) FROM lynx.audit_events;
SELECT 'smoke_audit=' || count(*) FROM lynx.audit_events WHERE correlation_id = 'backup-restore-smoke';
SELECT 'world_snapshot=' || coalesce((SELECT payload_hash FROM lynx.world_snapshots WHERE world_id = 'demo'), 'missing');
SELECT 'zone_events=' || count(*) FROM lynx.zone_events;
SELECT 'append_only_triggers=' || coalesce(string_agg(c.relname || '.' || t.tgname, ',' ORDER BY c.relname, t.tgname), '')
FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = 'lynx' AND NOT t.tgisinternal AND t.tgname LIKE '%append_only';
SQL
)

source_report="$(psql -X -q -tA -v ON_ERROR_STOP=1 -h "$LYNX_PG_HOST" -p "$LYNX_PG_PORT" \
    -U "$LYNX_PG_USER" -d "$LYNX_PG_DATABASE" -c "$report_sql")"
restored_report="$(psql -X -q -tA -v ON_ERROR_STOP=1 -h "$LYNX_PG_HOST" -p "$LYNX_PG_PORT" \
    -U "$LYNX_PG_USER" -d "$scratch" -c "$report_sql")"
if [[ "$source_report" != "$restored_report" ]]; then
    diff -u <(printf '%s\n' "$source_report") <(printf '%s\n' "$restored_report") >&2 || true
    lynx_die 'restored database differs from the source continuity report'
fi
grep -Fqx 'smoke_character=1' <<<"$restored_report" || lynx_die 'restored character marker is missing'
grep -Fqx 'smoke_audit=1' <<<"$restored_report" || lynx_die 'restored audit marker is missing'
grep -Eq '^world_snapshot=[0-9a-f]{64}$' <<<"$restored_report" || lynx_die 'restored world snapshot is missing'
grep -Eq '^append_only_triggers=.*audit_events_append_only' <<<"$restored_report" ||
    lynx_die 'restored audit append-only trigger is missing'

# Exercise the restored append-only guard, not only its catalog entry.
psql -X -q -v ON_ERROR_STOP=1 -h "$LYNX_PG_HOST" -p "$LYNX_PG_PORT" \
    -U "$LYNX_PG_USER" -d "$scratch" <<'SQL'
DO $body$
BEGIN
    BEGIN
        UPDATE lynx.audit_events SET action = action
        WHERE correlation_id = 'backup-restore-smoke';
        RAISE EXCEPTION 'restored audit append-only trigger allowed an update';
    EXCEPTION WHEN restrict_violation THEN
        NULL;
    END;
END
$body$;
SQL

zone_id="$(psql -X -q -tA -v ON_ERROR_STOP=1 -h "$LYNX_PG_HOST" -p "$LYNX_PG_PORT" \
    -U "$LYNX_PG_USER" -d "$scratch" \
    -c "SELECT checkpoint_zone_id FROM lynx.world_snapshots WHERE world_id = 'demo'")"
[[ -n "$zone_id" ]] || lynx_die 'restored world snapshot has no checkpoint zone'
verify_output="$(LYNX_PG_DATABASE="$scratch" "$world_bin" verify-snapshot \
    --world-id demo --zone-id "$zone_id" 2>&1)" || {
    printf '%s\n' "$verify_output" >&2
    lynx_die 'restored snapshot and event log failed read-only verification'
}
grep -q '^VERIFIED ' <<<"$verify_output" || lynx_die 'restored world snapshot was not verified'
grep -q 'quarantined=0' <<<"$verify_output" || lynx_die 'restored snapshot has quarantined records'

# Finally prove the service itself can acquire authority and become ready on
# the restored database. The scratch copy is disposable and never the source.
run_world "$scratch"
stop_world

printf 'Backup/restore smoke passed; source=%s restored=%s checksum=%s\n' \
    "$LYNX_PG_DATABASE" "$scratch" "$checksum"
