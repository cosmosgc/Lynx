#!/usr/bin/env bash
# Start Nakama 3.41.0 against the native PostgreSQL database.
#
# The checked-in server/nakama/local.yml holds non-secret development settings.
# This script renders a gitignored overlay in .lynx/ with the database address,
# console credentials, server key, and log level from the environment, then
# starts the pinned Nakama binary.
#
# The database address is built from LYNX_PG_* and must already exist; run
# scripts/setup-postgres.sh and scripts/db-migrate.sh first.
#
# Usage:
#   ./scripts/run-nakama.sh
#   ./scripts/run-nakama.sh --check
#   ./scripts/run-nakama.sh --migrate

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/common.sh
. "$script_dir/common.sh"

check_only=false
migrate_only=false
case "${1-}" in
    --check | -Check) check_only=true ;;
    --migrate | -Migrate) migrate_only=true ;;
    '') ;;
    *)
        lynx_die "unknown argument: $1 (expected --check or --migrate)"
        ;;
esac

root="$(cd "$script_dir/.." && pwd)"
lynx_import_env "$root"
lynx_require_pg

nakama_bin="${NAKAMA_BIN:-nakama}"
resolved_bin="$(command -v "$nakama_bin" 2>/dev/null || true)"
[[ -n "$resolved_bin" ]] ||
    lynx_die "Nakama binary '$nakama_bin' was not found. Download Nakama 3.41.0 from https://github.com/heroiclabs/nakama/releases/tag/v3.41.0 and put it on PATH, or set NAKAMA_BIN."

config_path="$root/server/nakama/local.yml"
[[ -f "$config_path" ]] || lynx_die "Nakama configuration not found: $config_path"

console_user="${LYNX_NAKAMA_CONSOLE_USER:-}"
console_password="${LYNX_NAKAMA_CONSOLE_PASSWORD:-}"
console_signing_key="${LYNX_NAKAMA_CONSOLE_SIGNING_KEY:-}"
server_key="${LYNX_NAKAMA_SERVER_KEY:-}"
session_encryption_key="${LYNX_NAKAMA_SESSION_ENCRYPTION_KEY:-}"
refresh_encryption_key="${LYNX_NAKAMA_REFRESH_ENCRYPTION_KEY:-}"
runtime_http_key="${LYNX_NAKAMA_RUNTIME_HTTP_KEY:-}"
metrics_port="${LYNX_NAKAMA_METRICS_PORT:-}"
log_level="${LYNX_NAKAMA_LOG_LEVEL:-}"

[[ -z "$console_user" || "$console_user" =~ ^[A-Za-z0-9_.-]{3,32}$ ]] ||
    lynx_die 'LYNX_NAKAMA_CONSOLE_USER must be 3-32 ASCII letters, digits, dots, dashes, or underscores'
[[ -z "$console_password" || ${#console_password} -le 256 ]] ||
    lynx_die 'LYNX_NAKAMA_CONSOLE_PASSWORD must be at most 256 characters'
[[ -z "$console_password" || "$console_password" != *$'\n'* && "$console_password" != *$'\r'* ]] ||
    lynx_die 'LYNX_NAKAMA_CONSOLE_PASSWORD must be on a single line'
[[ -z "$log_level" || "$log_level" =~ ^(debug|info|warn|error)$ ]] ||
    lynx_die 'LYNX_NAKAMA_LOG_LEVEL must be one of: debug, info, warn, error'
validate_secret() {
    local name="$1"
    local value="$2"
    [[ -z "$value" || (${#value} -le 512 && ! "$value" =~ [[:cntrl:]]) ]] ||
        lynx_die "$name must be at most 512 characters without control characters"
}

validate_secret 'LYNX_NAKAMA_CONSOLE_SIGNING_KEY' "$console_signing_key"
validate_secret 'LYNX_NAKAMA_SERVER_KEY' "$server_key"
validate_secret 'LYNX_NAKAMA_SESSION_ENCRYPTION_KEY' "$session_encryption_key"
validate_secret 'LYNX_NAKAMA_REFRESH_ENCRYPTION_KEY' "$refresh_encryption_key"
validate_secret 'LYNX_NAKAMA_RUNTIME_HTTP_KEY' "$runtime_http_key"

if [[ -n "$metrics_port" ]]; then
    [[ "$metrics_port" =~ ^[0-9]{1,5}$ ]] && ((10#$metrics_port <= 65535)) ||
        lynx_die 'LYNX_NAKAMA_METRICS_PORT must be an integer from 0 to 65535'
fi

# Percent-encode credentials so '@', ':', '/', and '%' in a password cannot
# change the meaning of the database address. Nakama parses the address as a
# URL and decodes each component.
urlencode() {
    local LC_ALL=C
    local input="$1"
    local output=""
    local char i
    for ((i = 0; i < ${#input}; i++)); do
        char="${input:i:1}"
        case "$char" in
            [a-zA-Z0-9.~_-]) output+="$char" ;;
            *) output+="$(printf '%%%02X' "'$char")" ;;
        esac
    done
    printf '%s' "$output"
}

yaml_quote() {
    local value="$1"
    value="${value//\'/\'\'}"
    printf "'%s'" "$value"
}

db_host="$LYNX_PG_HOST"
if [[ "$db_host" == *:* && "$db_host" != \[*\] ]]; then
    db_host="[$db_host]"
fi
database_address="$(urlencode "$LYNX_PG_USER"):$(urlencode "$LYNX_PG_PASSWORD")@${db_host}:${LYNX_PG_PORT}/${LYNX_PG_DATABASE}"
redacted_address="${LYNX_PG_USER}:***@${db_host}:${LYNX_PG_PORT}/${LYNX_PG_DATABASE}"

overlay="$root/.lynx/nakama.local.yml"

warn_console_defaults() {
    if [[ -z "$console_user" || -z "$console_password" ]]; then
        printf 'warning: the console falls back to insecure defaults for missing credentials. Set LYNX_NAKAMA_CONSOLE_USER and LYNX_NAKAMA_CONSOLE_PASSWORD for anything but a local throwaway database.\n' >&2
    fi
}

warn_security_defaults() {
    local insecure=()
    [[ -n "$console_signing_key" && "$console_signing_key" != 'defaultsigningkey' ]] || insecure+=('console signing key')
    [[ -n "$server_key" && "$server_key" != 'defaultkey' ]] || insecure+=('socket server key')
    [[ -n "$session_encryption_key" && "$session_encryption_key" != 'defaultencryptionkey' ]] || insecure+=('session encryption key')
    [[ -n "$refresh_encryption_key" && "$refresh_encryption_key" != 'defaultrefreshencryptionkey' ]] || insecure+=('refresh-session encryption key')
    [[ -n "$runtime_http_key" && "$runtime_http_key" != 'defaulthttpkey' ]] || insecure+=('runtime HTTP key')
    if ((${#insecure[@]} > 0)); then
        printf 'warning: Nakama uses insecure defaults for: %s. Set the matching LYNX_NAKAMA_* values for any non-disposable deployment.\n' "$(IFS=', '; printf '%s' "${insecure[*]}")" >&2
    fi
}

warn_metrics_exposure() {
    if [[ -n "$metrics_port" && "$metrics_port" != '0' ]]; then
        printf 'warning: Nakama metrics bind unauthenticated on all interfaces at port %s. Restrict the port to a private scrape network.\n' "$metrics_port" >&2
    fi
}

if [[ "$check_only" == true ]]; then
    printf 'Nakama preflight\n'
    printf '  binary:   %s\n' "$resolved_bin"
    printf '  config:   %s\n' "$config_path"
    printf '  database: %s\n' "$redacted_address"
    printf '  overlay:  %s (not written in --check mode)\n' "$overlay"
    warn_console_defaults
    warn_security_defaults
    warn_metrics_exposure
    printf 'Preflight OK.\n'
    exit 0
fi

mkdir -p "$root/.lynx"
(
    umask 077
    {
        printf 'database:\n  address:\n    - %s\n' "$(yaml_quote "$database_address")"
        if [[ -n "$server_key" ]]; then
            printf 'socket:\n  server_key: %s\n' "$(yaml_quote "$server_key")"
        fi
        if [[ -n "$console_user" || -n "$console_password" || -n "$console_signing_key" ]]; then
            printf 'console:\n'
            if [[ -n "$console_user" ]]; then
                printf '  username: %s\n' "$(yaml_quote "$console_user")"
            fi
            if [[ -n "$console_password" ]]; then
                printf '  password: %s\n' "$(yaml_quote "$console_password")"
            fi
            if [[ -n "$console_signing_key" ]]; then
                printf '  signing_key: %s\n' "$(yaml_quote "$console_signing_key")"
            fi
        fi
        if [[ -n "$session_encryption_key" || -n "$refresh_encryption_key" ]]; then
            printf 'session:\n'
            if [[ -n "$session_encryption_key" ]]; then
                printf '  encryption_key: %s\n' "$(yaml_quote "$session_encryption_key")"
            fi
            if [[ -n "$refresh_encryption_key" ]]; then
                printf '  refresh_encryption_key: %s\n' "$(yaml_quote "$refresh_encryption_key")"
            fi
        fi
        if [[ -n "$runtime_http_key" ]]; then
            printf 'runtime:\n  http_key: %s\n' "$(yaml_quote "$runtime_http_key")"
        fi
        if [[ -n "$metrics_port" ]]; then
            printf 'metrics:\n  prometheus_port: %s\n' "$metrics_port"
        fi
        if [[ -n "$log_level" ]]; then
            printf 'logger:\n  level: %s\n' "$(yaml_quote "$log_level")"
        fi
    } >"$overlay"
)

if [[ "$migrate_only" == true ]]; then
    printf 'Preparing Nakama 3.41.0 migration\n'
else
    printf 'Starting Nakama 3.41.0\n'
fi
printf '  binary:   %s\n' "$resolved_bin"
printf '  config:   %s\n' "$config_path"
printf '  overlay:  %s\n' "$overlay"
printf '  database: %s\n' "$redacted_address"
printf '  console:  http://127.0.0.1:7351\n'
if [[ -n "$metrics_port" && "$metrics_port" != '0' ]]; then
    printf '  metrics:  http://127.0.0.1:%s/metrics (restrict network access)\n' "$metrics_port"
else
    printf '  metrics:  disabled\n'
fi
warn_console_defaults
warn_security_defaults
warn_metrics_exposure
printf '\n'

cd "$root"
if [[ "$migrate_only" == true ]]; then
    printf 'Applying Nakama database migrations\n'
    exec "$resolved_bin" migrate up --config "$config_path" --config "$overlay"
fi

exec "$resolved_bin" --config "$config_path" --config "$overlay"
