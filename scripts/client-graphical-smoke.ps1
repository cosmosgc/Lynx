param(
    [Parameter(Mandatory = $true)]
    [string] $ClientPath
)

$ErrorActionPreference = 'Stop'
$windowTitle = 'Lynx Account & Characters'
$screenshot = Join-Path $env:TEMP 'lynx-account-screen.png'

if (-not (Test-Path -LiteralPath $ClientPath -PathType Leaf)) {
    throw "Client executable not found: $ClientPath"
}
if (Test-Path -LiteralPath $screenshot) {
    Remove-Item -LiteralPath $screenshot -Force
}

Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public struct LynxWindowRect
{
    public int Left;
    public int Top;
    public int Right;
    public int Bottom;
}

public static class LynxWindowCapture
{
    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr handle, out LynxWindowRect rect);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr handle);
}
'@

$client = Start-Process -FilePath $ClientPath -ArgumentList '--account' -PassThru
try {
    $deadline = (Get-Date).AddSeconds(45)
    $windowHandle = [IntPtr]::Zero
    while ((Get-Date) -lt $deadline) {
        if ($client.HasExited) {
            throw "Client exited before opening its account window (exit code $($client.ExitCode))."
        }

        $client.Refresh()
        if ($client.MainWindowTitle -eq $windowTitle -and $client.MainWindowHandle -ne [IntPtr]::Zero) {
            $windowHandle = $client.MainWindowHandle
            break
        }
        Start-Sleep -Milliseconds 250
    }

    if ($windowHandle -eq [IntPtr]::Zero) {
        throw "Timed out waiting for window '$windowTitle'."
    }

    [void][LynxWindowCapture]::SetForegroundWindow($windowHandle)
    Start-Sleep -Seconds 2

    $rect = New-Object LynxWindowRect
    if (-not [LynxWindowCapture]::GetWindowRect($windowHandle, [ref]$rect)) {
        throw 'Could not read the account window bounds.'
    }

    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    if ($width -lt 640 -or $height -lt 400) {
        throw "Account window is unexpectedly small: ${width}x${height}."
    }

    $bitmap = [System.Drawing.Bitmap]::new($width, $height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
        $bitmap.Save($screenshot, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }

    # Sample the captured client window. A clear-color-only frame is not enough:
    # the login UI should contribute text and panel pixels as well.
    $captured = [System.Drawing.Bitmap]::FromFile($screenshot)
    $colors = [System.Collections.Generic.HashSet[int]]::new()
    try {
        for ($y = 0; $y -lt $captured.Height; $y += 4) {
            for ($x = 0; $x -lt $captured.Width; $x += 4) {
                [void]$colors.Add($captured.GetPixel($x, $y).ToArgb())
            }
        }
    }
    finally {
        $captured.Dispose()
    }

    if ($colors.Count -le 4) {
        throw "The account window opened, but the screenshot looks blank ($($colors.Count) sampled colors)."
    }

    Write-Output "Graphical account-screen smoke passed: ${width}x${height}, $($colors.Count) sampled colors."
    Write-Output "Screenshot: $screenshot"
}
finally {
    if (-not $client.HasExited) {
        Stop-Process -Id $client.Id -Force
        $client.WaitForExit()
    }
}
