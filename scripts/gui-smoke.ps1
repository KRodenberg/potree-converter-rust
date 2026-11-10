param(
    [string]$Executable = (Join-Path $PSScriptRoot '..\target\release\potree-converter.exe'),
    [string]$Screenshot = (Join-Path $PSScriptRoot '..\test-output\gui.png'),
    [switch]$ScrollDown
)

$ErrorActionPreference = 'Stop'
$appPath = (Resolve-Path -LiteralPath $Executable).Path
$capturePath = [IO.Path]::GetFullPath($Screenshot)
Add-Type -AssemblyName System.Drawing
if (-not ('PotreeGuiSmoke' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class PotreeGuiSmoke {
    public delegate bool EnumWindowProc(IntPtr window, IntPtr parameter);
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowProc callback, IntPtr parameter);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetWindowText(IntPtr window, StringBuilder text, int count);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out Rect rectangle);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr window, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr word, IntPtr value);
    public static IntPtr FindWindow(uint wantedProcess) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((window, parameter) => {
            uint process; GetWindowThreadProcessId(window, out process);
            if (process != wantedProcess) return true;
            var title = new StringBuilder(512); GetWindowText(window, title, title.Capacity);
            Rect bounds;
            if (title.ToString() == "Potree Converter" && GetWindowRect(window, out bounds)
                && bounds.Right - bounds.Left > 100 && bounds.Bottom - bounds.Top > 100) {
                found = window; return false;
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@
}

$appProcess = $null
$bitmap = $null
$graphics = $null
try {
    # Launch only this application, hidden, and capture its window rather than the desktop.
    $appProcess = Start-Process -FilePath $appPath -WindowStyle Hidden -PassThru
    $appWindow = [IntPtr]::Zero
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        $appProcess.Refresh()
        if ($appProcess.HasExited) { throw "GUI exited during startup with code $($appProcess.ExitCode)." }
        $appWindow = [PotreeGuiSmoke]::FindWindow([uint32]$appProcess.Id)
        if ($appWindow -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 150
    }
    if ($appWindow -eq [IntPtr]::Zero) { throw 'The application did not create its expected window.' }
    Start-Sleep -Milliseconds 1500
    $bounds = New-Object PotreeGuiSmoke+Rect
    if (-not [PotreeGuiSmoke]::GetWindowRect($appWindow, [ref]$bounds)) { throw 'Cannot read application window bounds.' }
    $width = $bounds.Right - $bounds.Left
    $height = $bounds.Bottom - $bounds.Top
    if ($ScrollDown) {
        $localX = [int]($width / 2)
        $localY = [int]($height * 0.6)
        $localPoint = ($localY -shl 16) -bor ($localX -band 65535)
        $screenPoint = (($bounds.Top + $localY) -shl 16) -bor (($bounds.Left + $localX) -band 65535)
        [void][PotreeGuiSmoke]::PostMessage($appWindow, 0x0200, [IntPtr]::Zero, [IntPtr]$localPoint)
        Start-Sleep -Milliseconds 150
        [void][PotreeGuiSmoke]::PostMessage($appWindow, 0x020A, [IntPtr](-720 -shl 16), [IntPtr]$screenPoint)
        Start-Sleep -Milliseconds 600
    }
    $bitmap = New-Object Drawing.Bitmap($width, $height)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    $dc = $graphics.GetHdc()
    try { $captured = [PotreeGuiSmoke]::PrintWindow($appWindow, $dc, 2) }
    finally { $graphics.ReleaseHdc($dc) }
    if (-not $captured) { throw 'Windows could not capture the application window.' }
    $colors = New-Object 'System.Collections.Generic.HashSet[int]'
    for ($y = [int]($height * 0.15); $y -lt [int]($height * 0.9); $y += 8) {
        for ($x = [int]($width * 0.1); $x -lt [int]($width * 0.9); $x += 8) {
            [void]$colors.Add($bitmap.GetPixel($x, $y).ToArgb())
        }
    }
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($capturePath)) | Out-Null
    $bitmap.Save($capturePath, [Drawing.Imaging.ImageFormat]::Png)
    if ($colors.Count -lt 4) { throw "Window started, but hidden GPU rendering produced an empty capture ($($colors.Count) colors). Screenshot: $capturePath" }
    [pscustomobject]@{ Window = 'Potree Converter'; Width = $width; Height = $height; SampledColors = $colors.Count; Screenshot = $capturePath } | ConvertTo-Json
}
finally {
    if ($graphics) { $graphics.Dispose() }
    if ($bitmap) { $bitmap.Dispose() }
    if ($appProcess) {
        $appProcess.Refresh()
        if (-not $appProcess.HasExited) { Stop-Process -Id $appProcess.Id -Force }
    }
}
