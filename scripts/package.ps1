param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$packageScript = Join-Path $PSScriptRoot 'package.mjs'
if ($SkipBuild) { node $packageScript --skip-build } else { node $packageScript }
if ($LASTEXITCODE -ne 0) { throw 'Packaging failed' }
