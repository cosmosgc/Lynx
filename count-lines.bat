@echo off
setlocal
powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "$files = Get-ChildItem -LiteralPath '%~dp0' -Recurse -File -Include *.rs,*.toml,*.md | Where-Object { $_.FullName -notmatch '\\(\.git|target|node_modules)\\' }; $lines = ($files | Get-Content | Measure-Object -Line).Lines; Write-Output ('Files: {0}  Lines: {1}' -f $files.Count, $lines)"
endlocal
