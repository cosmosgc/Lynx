"""Runner preflight tests with fake database helpers; never connects to PostgreSQL.

Run: python -m unittest scripts.test_db_migrate
Set LYNX_TEST_BASH to a Bash executable if it is not on PATH.
"""

import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
BASH_HELPERS = r'''
lynx_die() { printf 'error: %s\n' "$*" >&2; exit 1; }
lynx_import_env() { :; }
lynx_require_pg() { LYNX_PG_HOST=mock; LYNX_PG_PORT=5432; LYNX_PG_DATABASE=mock; LYNX_PG_USER=mock; LYNX_PG_PASSWORD=mock; }
lynx_require_psql() { :; }
lynx_psql_args() { :; }
lynx_sha256_file() { sha256sum "$1" | awk '{print $1}'; }
lynx_psql() {
    if [[ "$1" == '-f' ]]; then
        cat >> "$MOCK_LOG"
    elif [[ "$2" == *to_regnamespace* || "$2" == *to_regclass* ]]; then
        echo t
    elif [[ "$2" == *'MAX(version)'* ]]; then
        echo "$MOCK_VERSION"
    elif [[ "$2" == *"version = '$MOCK_VERSION'"* ]]; then
        echo "$MOCK_CHECKSUM"
    fi
}
'''
PS_HELPERS = r'''
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
function Get-LynxRepoRoot { (Resolve-Path (Join-Path $PSScriptRoot '..')).Path }
function Import-LynxEnv { param($Root) }
function Get-LynxPgConfig { @{ Host='mock'; Port='5432'; Database='mock'; User='mock'; Password='mock' } }
function New-LynxPsqlArguments { param($Config) @('mock', 'connection') }
function Invoke-LynxPsqlScalar { param($Arguments, $Sql, $Password) 't' }
function Invoke-LynxPsql {
    param($Arguments, $Sql, $Password)
    if ($Arguments -contains '-f') { Add-Content -LiteralPath $env:MOCK_LOG -Value $Sql }
    elseif ($Sql -like '*SELECT version*') { "$env:MOCK_VERSION|$env:MOCK_CHECKSUM" }
}
'''


class MigrationPreflightTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.scripts = root / 'scripts'
        self.scripts.mkdir()
        self.migrations = root / 'server' / 'migrations'
        self.migrations.mkdir(parents=True)
        for name in ('db-migrate.sh', 'db-migrate.ps1'):
            shutil.copyfile(SCRIPTS / name, self.scripts / name)
        (self.scripts / 'common.sh').write_text(BASH_HELPERS, encoding='utf-8')
        (self.scripts / 'common.ps1').write_text(PS_HELPERS, encoding='utf-8')
        self.log = root / 'executed.sql'

    def run_runner(self, runner, version, checksum, status=False):
        env = os.environ.copy()
        env.update(MOCK_VERSION=version, MOCK_CHECKSUM=checksum, MOCK_LOG=str(self.log))
        if runner == 'sh':
            executable = os.environ.get('LYNX_TEST_BASH')
            if not executable and os.name == 'nt':
                git = shutil.which('git')
                if git:
                    git_bash = Path(git).resolve().parent.parent / 'bin' / 'bash.exe'
                    if git_bash.is_file():
                        executable = str(git_bash)
            executable = executable or shutil.which('bash')
            args = [executable, str(self.scripts / 'db-migrate.sh')]
            if status:
                args.append('--status')
        else:
            executable = shutil.which('pwsh') or shutil.which('powershell')
            args = [executable, '-NoProfile', '-File', str(self.scripts / 'db-migrate.ps1')]
            if status:
                args.append('-Status')
        if not executable:
            self.skipTest(f'{runner} executable not available')
        return subprocess.run(args, env=env, text=True, capture_output=True)

    def test_preflight_and_status(self):
        for runner in ('sh', 'ps1'):
            for applied_version in ('0001_first', '0002_second'):
                for status in (False, True):
                    with self.subTest(runner=runner, applied=applied_version, status=status):
                        for version in ('0001_first', '0002_second'):
                            (self.migrations / f'{version}.sql').write_text('SELECT 1;\n')
                        self.log.unlink(missing_ok=True)
                        result = self.run_runner(runner, applied_version, '0' * 64, status)
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn(applied_version, result.stdout + result.stderr)
                        self.assertFalse(self.log.exists(), result.stdout + result.stderr)

    def test_clean_history_applies_only_pending(self):
        for runner in ('sh', 'ps1'):
            with self.subTest(runner=runner):
                first = self.migrations / '0001_first.sql'
                first.write_bytes(b'SELECT 1;\n')
                (self.migrations / '0002_second.sql').write_bytes(b'SELECT 2;\n')
                checksum = hashlib.sha256(first.read_bytes()).hexdigest()
                self.log.unlink(missing_ok=True)
                status = self.run_runner(runner, '0001_first', checksum, status=True)
                self.assertEqual(status.returncode, 0, status.stdout + status.stderr)
                self.assertIn('pending', status.stdout)
                self.assertFalse(self.log.exists())
                result = self.run_runner(runner, '0001_first', checksum)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                sql = self.log.read_text()
                self.assertIn('SELECT 2;', sql)
                self.assertNotIn('SELECT 1;', sql)

    def test_invalid_later_filename_blocks_pending(self):
        for runner in ('sh', 'ps1'):
            with self.subTest(runner=runner):
                (self.migrations / '0001_first.sql').write_text('SELECT 1;\n')
                (self.migrations / 'zz_invalid.sql').write_text('SELECT 2;\n')
                self.log.unlink(missing_ok=True)
                result = self.run_runner(runner, '0000_previous', '0' * 64)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn('zz_invalid.sql', result.stdout + result.stderr)
                self.assertFalse(self.log.exists())


if __name__ == '__main__':
    unittest.main()
