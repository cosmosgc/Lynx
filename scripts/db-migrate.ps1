#requires -Version 5.1
<#
.SYNOPSIS
    Apply versioned SQL migrations to the Lynx PostgreSQL schema.

.DESCRIPTION
    Migrations are read from server/migrations in filename order. Each one runs
    in a single transaction together with its lynx.schema_migrations row and a
    transaction-scoped advisory lock. Applied migrations are skipped; a changed
    checksum aborts the run.

    The script reads LYNX_PG_* from the environment or .env and connects as the
    application role. Run scripts/setup-postgres.ps1 first.

.PARAMETER Status
    Print each migration as applied or pending without changing the database.

.EXAMPLE
    ./scripts/db-migrate.ps1

.EXAMPLE
    ./scripts/db-migrate.ps1 -Status
#>
[CmdletBinding()]
param(
    [switch]$Status
)

. (Join-Path $PSScriptRoot 'common.ps1')

$root = Get-LynxRepoRoot
Import-LynxEnv -Root $root
$pg = Get-LynxPgConfig

$migrationDirectory = Join-Path $root 'server/migrations'
if (-not (Test-Path -LiteralPath $migrationDirectory -PathType Container)) {
    throw "Migration directory not found: $migrationDirectory"
}

$files = @(
    Get-ChildItem -LiteralPath $migrationDirectory -Filter '*.sql' -File |
        Sort-Object -Property Name
)
if ($files.Count -eq 0) {
    throw "No migrations found in $migrationDirectory."
}

$psqlArgs = New-LynxPsqlArguments -Config $pg

$hasSchema = Invoke-LynxPsqlScalar `
    -Arguments $psqlArgs `
    -Sql "SELECT to_regnamespace('lynx') IS NOT NULL;" `
    -Password $pg.Password
if ($hasSchema -ne 't') {
    throw "The 'lynx' schema does not exist in database '$($pg.Database)'. Run scripts/setup-postgres.ps1 first."
}

$hasHistory = Invoke-LynxPsqlScalar `
    -Arguments $psqlArgs `
    -Sql "SELECT to_regclass('lynx.schema_migrations') IS NOT NULL;" `
    -Password $pg.Password

if (-not $Status) {
    $bootstrapSql = @'
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
'@
    Invoke-LynxPsql -Arguments $psqlArgs -Sql $bootstrapSql -Password $pg.Password | Out-Null
    $hasHistory = 't'
}

$applied = @{}
if ($hasHistory -eq 't') {
    $rows = Invoke-LynxPsql `
        -Arguments $psqlArgs `
        -Sql "SELECT version || '|' || checksum FROM lynx.schema_migrations ORDER BY version;" `
        -Password $pg.Password
    foreach ($row in @($rows)) {
        $line = "$row"
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        $parts = $line.Split([char]'|', 2)
        if ($parts.Count -ne 2) {
            throw "Malformed row in lynx.schema_migrations: $line"
        }
        $applied[$parts[0]] = $parts[1]
    }
}

$highestApplied = $null
if ($applied.Count -gt 0) {
    $highestApplied = @($applied.Keys | Sort-Object)[-1]
}

$appliedCount = 0
$skippedCount = 0
$pendingCount = 0
$checksumFailures = @()

Write-Host "Lynx migrations in $migrationDirectory"
Write-Host "  database: $($pg.Database) on $($pg.Host):$($pg.Port) as $($pg.User)"
Write-Host ''

foreach ($file in $files) {
    $version = [IO.Path]::GetFileNameWithoutExtension($file.Name)
    if ($version -notmatch '^[0-9]{4}_[a-z0-9_]+$') {
        throw "Migration '$($file.Name)' must match NNNN_lower_snake_case.sql."
    }

    $checksum = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()

    if ($applied.ContainsKey($version)) {
        if ($applied[$version] -ne $checksum) {
            $checksumFailures += $version
            Write-Host ("{0,-28} {1}" -f $file.Name, 'CHANGED') -ForegroundColor Red
        } elseif ($Status) {
            Write-Host ("{0,-28} {1}" -f $file.Name, 'applied')
        } else {
            Write-Host ("{0,-28} {1}" -f $file.Name, 'up to date') -ForegroundColor DarkGray
        }
        $skippedCount++
        continue
    }

    if ($Status) {
        Write-Host ("{0,-28} {1}" -f $file.Name, 'pending')
        $pendingCount++
        continue
    }

    if ($null -ne $highestApplied -and $version -lt $highestApplied) {
        Write-Warning "Migration $version is older than the applied migration $highestApplied; applying it out of order."
    }

    Write-Host ("{0,-28} {1}" -f $file.Name, 'applying...') -NoNewline
    $body = Get-Content -LiteralPath $file.FullName -Raw
    $wrapped = @"
BEGIN;
SELECT pg_advisory_xact_lock(1280522872);
$body
INSERT INTO lynx.schema_migrations (version, checksum) VALUES ('$version', '$checksum');
COMMIT;
"@
    try {
        Invoke-LynxPsql -Arguments ($psqlArgs + @('-f', '-')) -Sql $wrapped -Password $pg.Password | Out-Null
    } catch {
        Write-Host ' failed' -ForegroundColor Red
        throw "Migration $version failed: $($_.Exception.Message)"
    }
    Write-Host ' applied' -ForegroundColor Green
    $appliedCount++
}

Write-Host ''
if ($checksumFailures.Count -gt 0) {
    throw "Applied migrations changed since they ran: $($checksumFailures -join ', '). Restore the files or write a new migration."
}

if ($Status) {
    Write-Host "Summary: $($applied.Count) applied, $pendingCount pending."
} else {
    Write-Host "Summary: $appliedCount applied, $skippedCount already up to date."
}
