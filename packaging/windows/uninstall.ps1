# Uninstall unlocr (Windows): remove the binary, strip it from PATH, and delete
# the model cache. Does NOT remove llama.cpp or poppler (installed separately).
#
#   powershell -ExecutionPolicy Bypass -File packaging\windows\uninstall.ps1
$ErrorActionPreference = 'Stop'

$Name    = 'unlocr'
$InstDir = Join-Path $env:LOCALAPPDATA "Programs\$Name"
# Cache dir matches unlocr/src/model.rs: %LOCALAPPDATA%\unlocr (no XDG on Windows
# in practice; honor it if set, as the binary does).
$Cache   = if ($env:XDG_CACHE_HOME) { Join-Path $env:XDG_CACHE_HOME $Name } else { Join-Path $env:LOCALAPPDATA $Name }

if (Test-Path $InstDir) {
  Remove-Item -Recurse -Force $InstDir
  Write-Host "Removed $InstDir"
} else {
  Write-Host "No install dir at $InstDir (skipping)"
}

# Strip install dir from user PATH.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -and $userPath -like "*$InstDir*") {
  $new = ($userPath -split ';' | Where-Object { $_ -and $_ -ne $InstDir }) -join ';'
  [Environment]::SetEnvironmentVariable('Path', $new, 'User')
  Write-Host "Removed $InstDir from user PATH (restart shell)."
}

if ($Cache -and $Cache -ne "\" -and $Cache -ne "/" -and $Cache -ne $env:USERPROFILE -and $Cache.EndsWith("unlocr") -and (Test-Path $Cache)) {
  $mb = [math]::Round((Get-ChildItem -Recurse -File $Cache | Measure-Object Length -Sum).Sum / 1MB, 1)
  Remove-Item -Recurse -Force $Cache
  Write-Host "Removed model cache $Cache ($mb MB freed)"
} else {
  Write-Host "No cache at $Cache (skipping)"
}

Write-Host "Done. (llama.cpp and poppler were not touched.)"
