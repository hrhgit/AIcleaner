param(
  [switch]$SkipInstall,
  [switch]$CheckOnly
)

$ErrorActionPreference = 'Stop'

Set-Location -Path $PSScriptRoot

function Write-Step([string]$Message) {
  Write-Host ""
  Write-Host "==> $Message" -ForegroundColor Cyan
}

function Test-Command([string]$Name) {
  return $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

Write-Host "AIcleaner Tauri dev launcher" -ForegroundColor Green
Write-Host "Root: $PSScriptRoot"

if (-not (Test-Command "node")) {
  throw "node is not installed or not in PATH."
}

if (-not (Test-Command "npm")) {
  throw "npm is not installed or not in PATH."
}

if (-not (Test-Command "cargo")) {
  throw "cargo is not installed or not in PATH."
}

if (-not (Test-Command "rustc")) {
  throw "rustc is not installed or not in PATH."
}

if (-not $SkipInstall) {
  if (-not (Test-Path (Join-Path $PSScriptRoot "node_modules"))) {
    Write-Step "Installing npm dependencies"
    npm install
    if ($LASTEXITCODE -ne 0) {
      throw "npm install failed."
    }
  } else {
    Write-Step "npm dependencies already present"
  }
}

if ($CheckOnly) {
  Write-Step "Environment check passed"
  exit 0
}

Write-Step "Starting Tauri dev app"
Write-Host "This will launch Vite through Tauri beforeDevCommand." -ForegroundColor DarkGray

npm run tauri:dev
if ($LASTEXITCODE -ne 0) {
  throw "npm run tauri:dev failed."
}
