$ErrorActionPreference='Stop'
node (Join-Path $PSScriptRoot 'build-windows.mjs')
if($LASTEXITCODE -ne 0){throw 'Windows build failed'}
