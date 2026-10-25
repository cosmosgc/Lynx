# Shared helpers for the native PostgreSQL and Nakama scripts.
#
# Dot-source this file from scripts in this directory. It must not print
# anything, and it must not depend on the caller's working directory.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Native stderr from psql or nakama is diagnostic output, not a PowerShell
# exception. Exit codes are checked explicitly.
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}

function Get-LynxRepoRoot {
    return (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
}

# Load KEY=VALUE pairs from <root>/.env without overwriting variables that are
# already set in the environment. Later definitions do not win; the process
# environment is authoritative.
function Import-LynxEnv {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root
    )

    $envFile = Join-Path $Root '.env'
    if (-not (Test-Path -LiteralPath $envFile -PathType Leaf)) {
        return
    }

    $lineNumber = 0
    foreach ($line in Get-Content -LiteralPath $envFile) {
        $lineNumber++
        $trimmed = $line.Trim()
        if ($trimmed.Length -eq 0 -or $trimmed.StartsWith('#')) {
            continue
        }

        $match = [regex]::Match(
            $trimmed,
            '^(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$'
        )
        if (-not $match.Success) {
            throw "Invalid line $lineNumber in ${envFile}: expected KEY=VALUE."
        }

        $name = $match.Groups[1].Value
        $value = $match.Groups[2].Value.Trim()
        if ($value.Length -ge 2) {
            $first = $value[0]
            $last = $value[$value.Length - 1]
            if (($first -eq '"' -and $last -eq '"') -or ($first -eq "'" -and $last -eq "'")) {
                $value = $value.Substring(1, $value.Length - 2)
            }
        }

        if (-not (Test-Path -LiteralPath "Env:$name")) {
            Set-Item -Path "Env:$name" -Value $value
        }
    }
}

function Get-LynxSetting {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name,
        [string]$Default = ''
    )

    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrEmpty($value)) {
        return $Default
    }
    return $value
}

# Read and validate the shared PostgreSQL settings. The application password is
# required by every script; the setup script uses it for the role it creates.
function Get-LynxPgConfig {
    $config = [pscustomobject]@{
        Host     = Get-LynxSetting 'LYNX_PG_HOST' '127.0.0.1'
        Port     = Get-LynxSetting 'LYNX_PG_PORT' '5432'
        Database = Get-LynxSetting 'LYNX_PG_DATABASE' 'lynx'
        User     = Get-LynxSetting 'LYNX_PG_USER' 'lynx'
        Password = Get-LynxSetting 'LYNX_PG_PASSWORD' ''
    }

    if ($config.Host -notmatch '^[A-Za-z0-9.:\[\]_-]+$') {
        throw "LYNX_PG_HOST contains unsupported characters: '$($config.Host)'."
    }

    $port = 0
    if (-not [int]::TryParse($config.Port, [ref]$port) -or $port -lt 1 -or $port -gt 65535) {
        throw "LYNX_PG_PORT must be an integer between 1 and 65535."
    }
    $config.Port = [string]$port

    if ($config.Database -notmatch '^[a-z_][a-z0-9_]{0,62}$') {
        throw "LYNX_PG_DATABASE must be a lowercase PostgreSQL identifier."
    }
    if ($config.User -notmatch '^[a-z_][a-z0-9_]{0,62}$') {
        throw "LYNX_PG_USER must be a lowercase PostgreSQL identifier."
    }
    if ([string]::IsNullOrEmpty($config.Password)) {
        throw 'LYNX_PG_PASSWORD is not set. Copy .env.example to .env and fill it in, or export the variable.'
    }
    if ($config.Password.Length -gt 1024 -or $config.Password.Contains("`n") -or $config.Password.Contains("`r")) {
        throw 'LYNX_PG_PASSWORD must be at most 1024 characters on a single line.'
    }

    return $config
}

function New-LynxPsqlArguments {
    param(
        [Parameter(Mandatory = $true)]
        $Config,
        [string]$Database,
        [string]$User
    )

    if (-not $Database) {
        $Database = $Config.Database
    }
    if (-not $User) {
        $User = $Config.User
    }

    return @(
        '-X', '-q', '-tA', '-v', 'ON_ERROR_STOP=1',
        '-h', $Config.Host,
        '-p', $Config.Port,
        '-U', $User,
        '-d', $Database
    )
}

# Run psql with optional SQL on stdin. Returns stdout; throws on a non-zero exit.
# Pass -Password to expose PGPASSWORD to the child process and restore the
# caller's value afterwards. Omit it to leave libpq authentication untouched,
# which is what the superuser setup connection needs.
function Invoke-LynxPsql {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments,
        [string]$Sql,
        [string]$Password
    )

    $previous = $env:PGPASSWORD
    if ($PSBoundParameters.ContainsKey('Password')) {
        $env:PGPASSWORD = $Password
    }

    try {
        if ($PSBoundParameters.ContainsKey('Sql')) {
            $output = $Sql | & psql @Arguments
        } else {
            $output = & psql @Arguments
        }
        $code = $LASTEXITCODE
    } finally {
        if ($null -eq $previous) {
            Remove-Item Env:PGPASSWORD -ErrorAction SilentlyContinue
        } else {
            $env:PGPASSWORD = $previous
        }
    }

    if ($code -ne 0) {
        throw "psql exited with code $code."
    }
    return $output
}

# Run a query that returns a single scalar.
function Invoke-LynxPsqlScalar {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments,
        [Parameter(Mandatory = $true)]
        [string]$Sql,
        [string]$Password
    )

    $parameters = @{ Arguments = $Arguments; Sql = $Sql }
    if ($PSBoundParameters.ContainsKey('Password')) {
        $parameters['Password'] = $Password
    }

    $lines = Invoke-LynxPsql @parameters
    return (($lines | ForEach-Object { "$_" }) -join "`n").Trim()
}
