# unlocr installer for Windows. Builds with cargo, installs unlocr.exe to a per-user
# dir, adds it to PATH, then checks/installs runtime deps via winget or scoop.
#
#   powershell -ExecutionPolicy Bypass -File packaging\windows\install.ps1
$ErrorActionPreference = 'Stop'

$Name    = 'unlocr'
$Root    = (Resolve-Path "$PSScriptRoot\..\..").Path
$Src     = $Root
$InstDir = Join-Path $env:LOCALAPPDATA "Programs\$Name"

function Have($cmd) { return [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }

if (-not (Have 'cargo')) { throw "cargo not found. Install Rust: https://rustup.rs" }

Write-Host "Building $Name (release)..."
cargo build --release --locked --manifest-path (Join-Path $Src 'Cargo.toml')
$Bin = Join-Path $Src "target\release\$Name.exe"
if (-not (Test-Path $Bin)) { throw "build produced no binary at $Bin" }

New-Item -ItemType Directory -Force -Path $InstDir | Out-Null
Copy-Item -Force $Bin (Join-Path $InstDir "$Name.exe")
Write-Host "Installed $InstDir\$Name.exe"

# Add to per-user PATH if missing, safely.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$parts = if ($userPath) { $userPath -split ';' } else { @() }
$cleanParts = @()
$normalizedInstDir = [System.IO.Path]::GetFullPath($InstDir).TrimEnd(([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar))
$found = $false

foreach ($part in $parts) {
    $trimmed = $part.Trim()
    if ($trimmed) {
        try {
            $norm = [System.IO.Path]::GetFullPath($trimmed).TrimEnd(([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar))
            if ($norm -eq $normalizedInstDir) {
                $found = $true
            }
        } catch {
            if ($trimmed -eq $InstDir) {
                $found = $true
            }
        }
        if (-not ($cleanParts -contains $trimmed)) {
            $cleanParts += $trimmed
        }
    }
}

if (-not $found) {
    $cleanParts += $InstDir
    $newPath = $cleanParts -join ';'
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host "Added $InstDir to user PATH (restart shell to pick it up)."
}

# Runtime deps. poppler -> pdftoppm.exe; llama.cpp -> llama-server.exe.
# Prefer scoop or winget, fall back to manual/conda hints.
function Ensure($cmd, $scoopPkg, $hint) {
  if (Have $cmd) { Write-Host "  ok: $cmd"; return }
  if (Have 'scoop') {
    Write-Host "  installing $scoopPkg via scoop..."
    scoop install $scoopPkg
  } else {
    Write-Host "  MISSING: $cmd -- $hint"
  }
}

function Ensure-LlamaServer {
  if (Have 'llama-server') {
    Write-Host "  ok: llama-server"
    return
  }
  if (Have 'scoop') {
    Write-Host "  installing llama-cpp via scoop..."
    scoop install llama-cpp
  } elseif (Have 'winget') {
    Write-Host "  installing llama.cpp via winget..."
    winget install llama.cpp
  } else {
    $hasConda = (Have 'conda') -or (Have 'mamba') -or (Have 'pixi')
    Write-Host "  MISSING: llama-server (from llama.cpp >= b8530) is required." -ForegroundColor Yellow
    Write-Host "  Install it using one of these options:"
    Write-Host "    - Winget (Recommended): winget install llama.cpp"
    Write-Host "    - Scoop: scoop install llama-cpp"
    Write-Host "    - Conda-forge: conda install -c conda-forge llama-cpp"
    Write-Host "    - Manual: Download release from https://github.com/ggml-org/llama.cpp/releases"
  }
}

Write-Host "Runtime dependencies:"
Ensure 'pdftoppm' 'poppler' 'scoop install poppler  (or https://github.com/oschwartz10612/poppler-windows/releases)'
Ensure-LlamaServer

Write-Host "Done. Open a new terminal and run: $Name --help"
