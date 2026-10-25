#requires -Version 5.1
<#
.SYNOPSIS
    Create the Lynx PostgreSQL role, database, schema, and extension.

.DESCRIPTION
    Native PostgreSQL 17 setup with no container runtime. Safe to re-run: the
    role, database, and schema are created only when missing, and the
    application password is reset to LYNX_PG_PASSWORD each time.

    The script connects as a superuser (LYNX_PG_SUPERUSER, default "postgres")
    and uses libpq authentication for that connection. Set PGPASSWORD or a
    pgpass file if the superuser needs a password. The application password is
    read from LYNX_PG_PASSWORD and never appears in an argument list.

.EXAMPLE
    ./scripts/setup-postgres.ps1

.EXAMPLE
    ./scripts/setup-postgres.ps1 -Superuser postgres -MaintenanceDatabase postgres
#>
[CmdletBinding()]
param(
    [string]$Superuser,
    [string]$MaintenanceDatabase
)

. (Join-Path $PSScriptRoot 'common.ps1')

$root = Get-LynxRepoRoot
Import-LynxEnv -Root $root
$pg = Get-LynxPgConfig

if (-not $Superuser) {
    $Superuser = Get-LynxSetting 'LYNX_PG_SUPERUSER' 'postgres'
}
if (-not $MaintenanceDatabase) {
    $MaintenanceDatabase = Get-LynxSetting 'LYNX_PG_MAINTENANCE_DB' 'postgres'
}
if (-not (Get-Command psql -ErrorAction SilentlyContinue)) {
    throw 'psql was not found on PATH. Install PostgreSQL 17 or add its bin directory to PATH.'
}

Write-Host 'Lynx PostgreSQL setup'
Write-Host "  server:    $($pg.Host):$($pg.Port)"
Write-Host "  database:  $($pg.Database)"
Write-Host "  app role:  $($pg.User)"
Write-Host "  superuser: $Superuser"
Write-Host ''

$superArgs = New-LynxPsqlArguments -Config $pg -Database $MaintenanceDatabase -User $Superuser

$serverVersion = [int](Invoke-LynxPsqlScalar -Arguments $superArgs -Sql 'SHOW server_version_num;')
if ($serverVersion -lt 170000) {
    throw "PostgreSQL 17 or newer is required; the server reports version $serverVersion."
}
if ($serverVersion -ge 180000) {
    Write-Warning "PostgreSQL $serverVersion is newer than the pinned baseline (17). Verify migrations before relying on it."
}

# \getenv keeps the application password out of the psql argument list.
$roleSql = @'
\getenv app_password LYNX_PG_PASSWORD
SELECT format('CREATE ROLE %I LOGIN', :'app_user')
WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'app_user')
\gexec
ALTER ROLE :"app_user" WITH LOGIN PASSWORD :'app_password';
SELECT format('CREATE DATABASE %I', :'db_name')
WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = :'db_name')
\gexec
'@

Write-Host 'Creating role and database (idempotent)...'
Invoke-LynxPsql `
    -Arguments ($superArgs + @('-v', "app_user=$($pg.User)", '-v', "db_name=$($pg.Database)")) `
    -Sql $roleSql | Out-Null

$databaseArgs = New-LynxPsqlArguments -Config $pg -Database $pg.Database -User $Superuser
$schemaSql = @'
CREATE SCHEMA IF NOT EXISTS lynx AUTHORIZATION :"app_user";
ALTER SCHEMA lynx OWNER TO :"app_user";
CREATE EXTENSION IF NOT EXISTS pg_trgm;
GRANT CONNECT ON DATABASE :"db_name" TO :"app_user";
GRANT USAGE, CREATE ON SCHEMA public TO :"app_user";
'@

Write-Host 'Creating schema, extension, and grants (idempotent)...'
Invoke-LynxPsql `
    -Arguments ($databaseArgs + @('-v', "app_user=$($pg.User)", '-v', "db_name=$($pg.Database)")) `
    -Sql $schemaSql | Out-Null

$version = Invoke-LynxPsqlScalar -Arguments $superArgs -Sql 'SHOW server_version;'
Write-Host ''
Write-Host "PostgreSQL $version is ready."
Write-Host "Schema 'lynx' is owned by role '$($pg.User)'."
Write-Host 'Next: ./scripts/db-migrate.ps1'
Write-Host ''
Write-Host 'Note: Nakama shares this database and creates its own tables in the public schema.' -ForegroundColor DarkGray
