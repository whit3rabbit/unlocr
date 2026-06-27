# unlocr installer for Windows. Builds with cargo, installs unlocr.exe to a per-user
# dir, adds it to PATH, then checks/installs runtime deps via winget or scoop.
#
#   powershell -ExecutionPolicy Bypass -File packaging\windows\install.ps1
$ErrorActionPreference = 'Stop'

$Name    = 'unlocr'
$Root    = (Resolve-Path "$PSScriptRoot\..\..").Path
$Src     = Join-Path $Root 'unlocr'
$InstDir = Join-Path $env:LOCALAPPDATA "Programs\$Name"

function Have($cmd) { return [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }

if (-not (Have 'cargo')) { throw "cargo not found. Install Rust: https://rustup.rs" }

Write-Host "Building $Name (release)..."
cargo build --release --manifest-path (Join-Path $Src 'Cargo.toml')
$Bin = Join-Path $Src "target\release\$Name.exe"
if (-not (Test-Path $Bin)) { throw "build produced no binary at $Bin" }

New-Item -ItemType Directory -Force -Path $InstDir | Out-Null
Copy-Item -Force $Bin (Join-Path $InstDir "$Name.exe")
Write-Host "Installed $InstDir\$Name.exe"

# Add to per-user PATH if missing.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$InstDir*") {
  [Environment]::SetEnvironmentVariable('Path', "$userPath;$InstDir", 'User')
  Write-Host "Added $InstDir to user PATH (restart shell to pick it up)."
}

# Runtime deps. poppler -> pdftoppm.exe; llama.cpp -> llama-server.exe.
# Prefer scoop (has both), fall back to manual hints.
function Ensure($cmd, $scoopPkg, $hint) {
  if (Have $cmd) { Write-Host "  ok: $cmd"; return }
  if (Have 'scoop') {
    Write-Host "  installing $scoopPkg via scoop..."
    scoop install $scoopPkg
  } else {
    Write-Host "  MISSING: $cmd -- $hint"
  }
}
Write-Host "Runtime dependencies:"
Ensure 'pdftoppm'     'poppler'   'scoop install poppler  (or https://github.com/oschwartz10612/poppler-windows/releases)'
Ensure 'llama-server' 'llama-cpp' 'scoop install llama-cpp  (need build >= b8530: https://github.com/ggml-org/llama.cpp/releases)'

Write-Host "Done. Open a new terminal and run: $Name --help"
