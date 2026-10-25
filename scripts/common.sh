#!/usr/bin/env bash
# Shared helpers for the native PostgreSQL and Nakama scripts.
#
# Source this file from scripts in this directory. It must not print anything,
# and it must not depend on the caller's working directory.

lynx_die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

# Load KEY=VALUE pairs from <root>/.env without overwriting variables that are
# already set in the environment. The process environment is authoritative.
lynx_import_env() {
    local root="$1"
    local file="$root/.env"
    local line key value
    [[ -f "$file" ]] || return 0

    while IFS= read -r line || [[ -n "$line" ]]; do
        line="${line%$'\r'}"
        line="${line#"${line%%[![:space:]]*}"}"
        line="${line%"${line##*[![:space:]]}"}"
        [[ -z "$line" ]] && continue
        [[ "$line" == \#* ]] && continue

        if [[ ! "$line" =~ ^(export[[:space:]]+)?([A-Za-z_][A-Za-z0-9_]*)[[:space:]]*=[[:space:]]*(.*)$ ]]; then
            lynx_die "Invalid .env line: $line"
        fi

        key="${BASH_REMATCH[2]}"
        value="${BASH_REMATCH[3]}"
        value="${value#"${value%%[![:space:]]*}"}"
        value="${value%"${value##*[![:space:]]}"}"
        if [[ ${#value} -ge 2 ]]; then
            if [[ "$value" == \"*\" || "$value" == \'*\' ]]; then
                value="${value:1:${#value}-2}"
            fi
        fi

        if [[ -z "${!key+x}" ]]; then
            export "$key=$value"
        fi
    done <"$file"
}

lynx_require_pg() {
    LYNX_PG_HOST="${LYNX_PG_HOST:-127.0.0.1}"
    LYNX_PG_PORT="${LYNX_PG_PORT:-5432}"
    LYNX_PG_DATABASE="${LYNX_PG_DATABASE:-lynx}"
    LYNX_PG_USER="${LYNX_PG_USER:-lynx}"

    [[ "$LYNX_PG_HOST" =~ ^[]A-Za-z0-9.:[_-]+$ ]] ||
        lynx_die "LYNX_PG_HOST contains unsupported characters: '$LYNX_PG_HOST'"
    [[ "$LYNX_PG_PORT" =~ ^[0-9]{1,5}$ ]] ||
        lynx_die 'LYNX_PG_PORT must be an integer between 1 and 65535'
    LYNX_PG_PORT=$((10#$LYNX_PG_PORT))
    ((LYNX_PG_PORT >= 1 && LYNX_PG_PORT <= 65535)) ||
        lynx_die 'LYNX_PG_PORT must be an integer between 1 and 65535'
    [[ "$LYNX_PG_DATABASE" =~ ^[a-z_][a-z0-9_]{0,62}$ ]] ||
        lynx_die 'LYNX_PG_DATABASE must be a lowercase PostgreSQL identifier'
    [[ "$LYNX_PG_USER" =~ ^[a-z_][a-z0-9_]{0,62}$ ]] ||
        lynx_die 'LYNX_PG_USER must be a lowercase PostgreSQL identifier'
    [[ -n "${LYNX_PG_PASSWORD-}" ]] ||
        lynx_die 'LYNX_PG_PASSWORD is not set. Copy .env.example to .env and fill it in, or export the variable.'
    ((${#LYNX_PG_PASSWORD} <= 1024)) ||
        lynx_die 'LYNX_PG_PASSWORD must be at most 1024 characters'
    [[ "$LYNX_PG_PASSWORD" != *$'\n'* && "$LYNX_PG_PASSWORD" != *$'\r'* ]] ||
        lynx_die 'LYNX_PG_PASSWORD must be on a single line'

    export LYNX_PG_HOST LYNX_PG_PORT LYNX_PG_DATABASE LYNX_PG_USER LYNX_PG_PASSWORD
}

lynx_require_psql() {
    command -v psql >/dev/null 2>&1 ||
        lynx_die 'psql was not found on PATH. Install PostgreSQL 17 or add its bin directory to PATH.'
}

# Rebuild LYNX_PSQL_ARGS. Override LYNX_PSQL_USER / LYNX_PSQL_DATABASE in the
# caller's environment to connect as the superuser during setup.
lynx_psql_args() {
    LYNX_PSQL_ARGS=(
        -X -q -tA -v ON_ERROR_STOP=1
        -h "$LYNX_PG_HOST"
        -p "$LYNX_PG_PORT"
        -U "${LYNX_PSQL_USER:-$LYNX_PG_USER}"
        -d "${LYNX_PSQL_DATABASE:-$LYNX_PG_DATABASE}"
    )
}

lynx_psql() {
    psql "${LYNX_PSQL_ARGS[@]}" "$@"
}

# Print the SHA-256 hex digest of a file with whichever tool is available.
lynx_sha256_file() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print $1}'
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$file" | awk '{print $NF}'
    else
        lynx_die 'no SHA-256 tool found (need sha256sum, shasum, or openssl)'
    fi
}
