param([switch]$SkipBuild)

$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Push-Location -LiteralPath $projectRoot
try {
    if (-not $SkipBuild) {
        & cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    }
    $executable = Join-Path $projectRoot 'target/release/potree-converter.exe'
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'Release executable not found.' }

    $manifestText = Get-Content -LiteralPath (Join-Path $projectRoot 'Cargo.toml') -Raw
    if ($manifestText -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'Cannot read application version.' }
    $version = $Matches[1]
    $packageName = "Potree-Converter-$version-windows-x64"
    $dist = Join-Path $projectRoot 'dist'
    New-Item -ItemType Directory -Force -Path $dist | Out-Null
    $stage = Join-Path $dist $packageName
    # Only remove the exact generated staging directory inside this repository.
    $stage = [System.IO.Path]::GetFullPath($stage)
    $distPrefix = [System.IO.Path]::GetFullPath($dist).TrimEnd('\') + '\'
    if (-not $stage.StartsWith($distPrefix, [System.StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid staging path.' }
    if ((Get-Item -LiteralPath $dist -Force).Attributes -band [System.IO.FileAttributes]::ReparsePoint) { throw 'Distribution folder must not be a directory link.' }
    if (Test-Path -LiteralPath $stage) {
        $existingTargets = @(Get-Item -LiteralPath $stage -Force) + @(Get-ChildItem -LiteralPath $stage -Force -Recurse)
        foreach ($target in $existingTargets) {
            if ($target.Attributes -band [System.IO.FileAttributes]::ReparsePoint) { throw 'Staging folder must not contain filesystem links.' }
            $resolvedTarget = [System.IO.Path]::GetFullPath($target.FullName)
            if (($resolvedTarget -ne $stage) -and (-not $resolvedTarget.StartsWith(($stage.TrimEnd('\') + '\'), [System.StringComparison]::OrdinalIgnoreCase))) { throw 'Staging target is outside the expected folder.' }
        }
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
    New-Item -ItemType Directory -Path $stage | Out-Null
    Copy-Item -LiteralPath $executable -Destination $stage
    foreach ($name in @('README.md', 'AGENTS.MD', 'LICENSE', 'THIRD_PARTY.md', 'VALIDATION.md')) {
        Copy-Item -LiteralPath (Join-Path $projectRoot $name) -Destination $stage
    }

    $noticesDirectory = Join-Path $stage 'ThirdPartyLicenses'
    New-Item -ItemType Directory -Path $noticesDirectory | Out-Null
    $metadataText = & cargo metadata --locked --format-version 1
    if ($LASTEXITCODE -ne 0) { throw 'Cannot collect dependency metadata.' }
    $metadata = $metadataText | ConvertFrom-Json
    $licenseOutput = New-Object System.Text.StringBuilder
    [void]$licenseOutput.AppendLine('Locked Rust dependency notices')
    foreach ($package in ($metadata.packages | Where-Object { $_.name -ne 'potree-converter' } | Sort-Object name, version)) {
        [void]$licenseOutput.AppendLine(('=' * 72))
        [void]$licenseOutput.AppendLine(($package.name + ' ' + $package.version))
        [void]$licenseOutput.AppendLine(('License: ' + $package.license))
        [void]$licenseOutput.AppendLine(('Repository: ' + $package.repository))
        $packageDirectory = Split-Path -Parent $package.manifest_path
        $licenseFiles = @(Get-ChildItem -LiteralPath $packageDirectory -File | Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)([._-]|$)' })
        if ($package.license_file) {
            $declaredLicense = Join-Path $packageDirectory $package.license_file
            if ((Test-Path -LiteralPath $declaredLicense -PathType Leaf) -and ($licenseFiles.FullName -notcontains $declaredLicense)) {
                $licenseFiles += Get-Item -LiteralPath $declaredLicense
            }
        }
        foreach ($file in ($licenseFiles | Sort-Object FullName -Unique)) {
            [void]$licenseOutput.AppendLine(('--- ' + $file.Name + ' ---'))
            [void]$licenseOutput.AppendLine((Get-Content -LiteralPath $file.FullName -Raw))
        }
    }
    [System.IO.File]::WriteAllText((Join-Path $noticesDirectory 'Rust-Dependency-Licenses.txt'), $licenseOutput.ToString(), (New-Object System.Text.UTF8Encoding($false)))

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $viewerArchive = [System.IO.Compression.ZipFile]::OpenRead((Join-Path $projectRoot 'assets/viewer-libs.zip'))
    try {
        $viewerNotices = Join-Path $noticesDirectory 'viewer'
        $viewerPrefix = [System.IO.Path]::GetFullPath($viewerNotices).TrimEnd('\') + '\'
        foreach ($entry in $viewerArchive.Entries) {
            if ($entry.Name -notmatch '(?i)(license|licence|copying|notice)') { continue }
            $destination = [System.IO.Path]::GetFullPath((Join-Path $viewerNotices $entry.FullName))
            if (-not $destination.StartsWith($viewerPrefix, [System.StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe viewer archive path.' }
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination) | Out-Null
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $destination, $true)
        }
    } finally { $viewerArchive.Dispose() }

    $exeHash = (Get-FileHash -LiteralPath (Join-Path $stage 'potree-converter.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
    [System.IO.File]::WriteAllText((Join-Path $stage 'SHA256SUMS.txt'), "$exeHash  potree-converter.exe`n", (New-Object System.Text.UTF8Encoding($false)))
    $archivePath = Join-Path $dist ($packageName + '.zip')
    if (Test-Path -LiteralPath $archivePath) { Remove-Item -LiteralPath $archivePath -Force }
    [System.IO.Compression.ZipFile]::CreateFromDirectory($stage, $archivePath, [System.IO.Compression.CompressionLevel]::Optimal, $true)
    $archiveHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    [System.IO.File]::WriteAllText(($archivePath + '.sha256'), ($archiveHash + '  ' + [System.IO.Path]::GetFileName($archivePath) + "`n"), (New-Object System.Text.UTF8Encoding($false)))
    Get-Item -LiteralPath $archivePath, ($archivePath + '.sha256') | Select-Object Name, Length, FullName
} finally { Pop-Location }
