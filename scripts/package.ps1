param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $taskRoot
try {
    if (-not $SkipBuild) {
        npm run bundle
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    }
    $version = (Get-Content -LiteralPath 'package.json' -Raw | ConvertFrom-Json).version
    $out = Join-Path $taskRoot 'output\release'
    $stage = Join-Path $taskRoot ('output\release-staging-' + [guid]::NewGuid().ToString('N'))
    $portable = Join-Path $stage 'Workbench'
    New-Item -ItemType Directory -Force -Path $out, $portable | Out-Null
    foreach ($file in @('Workbench.exe', 'workbench-cli.exe')) {
        Copy-Item -LiteralPath (Join-Path $taskRoot ('target\release\' + $file)) -Destination $portable
    }
    foreach ($file in @('LICENSE', 'THIRD_PARTY_NOTICES.txt', 'README.md')) {
        Copy-Item -LiteralPath (Join-Path $taskRoot $file) -Destination $portable
    }
    Copy-Item -LiteralPath 'dist' -Destination (Join-Path $portable 'ui') -Recurse
    Copy-Item -LiteralPath 'docs' -Destination $portable -Recurse
    $portableZip = Join-Path $out "Workbench-$version-windows-x64-portable.zip"
    Compress-Archive -Path $portable -DestinationPath $portableZip -Force
    $installer = Get-ChildItem -LiteralPath 'target\release\bundle\nsis' -Filter '*-setup.exe' | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $installer) { throw 'NSIS installer not found' }
    Copy-Item -LiteralPath $installer.FullName -Destination $out -Force
    $source = Join-Path $stage 'Workbench-source'
    New-Item -ItemType Directory -Path $source | Out-Null
    # A positive source-file inventory keeps runtime databases, credentials,
    # private QA capabilities, dependency caches and generated binaries out.
    $sourceFiles = git ls-files --cached --others --exclude-standard
    if ($LASTEXITCODE -ne 0) { throw 'Source inventory failed' }
    foreach ($relative in $sourceFiles) {
        if ($relative.StartsWith('.frontend-craft/')) { continue }
        $destination = Join-Path $source $relative
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination) | Out-Null
        Copy-Item -LiteralPath (Join-Path $taskRoot $relative) -Destination $destination
    }
    Compress-Archive -Path $source -DestinationPath (Join-Path $out "Workbench-$version-source.zip") -Force
    $hashes = Get-ChildItem -LiteralPath $out -File | Where-Object Extension -in '.exe', '.zip' | Get-FileHash -Algorithm SHA256
    $hashes | ForEach-Object { $_.Hash.ToLowerInvariant() + '  ' + (Split-Path -Leaf $_.Path) } | Set-Content -LiteralPath (Join-Path $out 'SHA256SUMS.txt') -Encoding utf8
    $hashes | Select-Object @{n='File';e={Split-Path -Leaf $_.Path}}, Hash
    # Remove only the verified temporary staging directory created above.
    $absoluteStage = [System.IO.Path]::GetFullPath($stage)
    $expectedParent = [System.IO.Path]::GetFullPath((Join-Path $taskRoot 'output')) + [System.IO.Path]::DirectorySeparatorChar
    if (-not $absoluteStage.StartsWith($expectedParent, [System.StringComparison]::OrdinalIgnoreCase)) { throw 'Unexpected staging location' }
    Remove-Item -LiteralPath $absoluteStage -Recurse -Force
} finally { Pop-Location }
