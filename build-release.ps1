param(
  [switch]$OpenOutput,
  [switch]$Force
)

$ErrorActionPreference = 'Stop'
Set-Location -Path $PSScriptRoot

function Write-Step([string]$Message) {
  Write-Host ""
  Write-Host "==> $Message" -ForegroundColor Cyan
}

function Write-Skip([string]$Message) {
  Write-Host "  [skip] $Message" -ForegroundColor DarkGray
}

function Write-Ok([string]$Message) {
  Write-Host "  [ok]   $Message" -ForegroundColor Green
}

function Test-Command([string]$Name) {
  return $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

# Returns $true if any file under SrcDir is newer than TargetFile
function Test-SourceNewer([string]$SrcDir, [string]$TargetFile) {
  if (-not (Test-Path $TargetFile)) { return $true }
  $targetTime = (Get-Item $TargetFile).LastWriteTime
  $newer = Get-ChildItem $SrcDir -Recurse -File |
    Where-Object { $_.LastWriteTime -gt $targetTime }
  return ($newer.Count -gt 0)
}

$startTime = Get-Date

Write-Host ""
Write-Host "  AIcleaner Release Builder" -ForegroundColor Green
Write-Host "  Root: $PSScriptRoot"
Write-Host "  Time: $($startTime.ToString('yyyy-MM-dd HH:mm:ss'))"
if ($Force) { Write-Host "  Mode: FORCE (full rebuild)" -ForegroundColor Yellow }
Write-Host ""

# --- Environment check ---

Write-Step "Checking environment"

foreach ($cmd in @("node", "npm", "cargo", "rustc")) {
  if (-not (Test-Command $cmd)) { throw "'$cmd' is not installed or not in PATH." }
  Write-Ok "$cmd $(& $cmd --version 2>&1 | Select-Object -First 1)"
}

# --- npm dependencies: reinstall only when package-lock.json is newer than node_modules ---

Write-Step "npm dependencies"

$nodeModules  = Join-Path $PSScriptRoot "node_modules"
$lockFile     = Join-Path $PSScriptRoot "package-lock.json"
$needsInstall = $Force -or
                (-not (Test-Path $nodeModules)) -or
                (-not (Test-Path $lockFile)) -or
                ((Get-Item $lockFile).LastWriteTime -gt (Get-Item $nodeModules).LastWriteTime)

if ($needsInstall) {
  Write-Host "  package-lock.json changed or node_modules missing - installing..." -ForegroundColor DarkGray
  npm install
  if ($LASTEXITCODE -ne 0) { throw "npm install failed." }
  (Get-Item $nodeModules).LastWriteTime = Get-Date
  Write-Ok "npm install done"
} else {
  Write-Skip "node_modules up to date"
}

# --- Tauri release build (Cargo handles incremental compilation automatically) ---

Write-Step "Tauri release build (Cargo handles incremental compilation)"
Write-Host "  Vite bundles frontend, then Cargo compiles Tauri backend." -ForegroundColor DarkGray

npm run tauri:build
if ($LASTEXITCODE -ne 0) { throw "npm run tauri:build failed." }

# --- Print output locations ---

$nsisDir = Join-Path $PSScriptRoot "src-tauri\target\release\bundle\nsis"
$exePath = Join-Path $PSScriptRoot "src-tauri\target\release\AIcleaner.exe"
$elapsed = [math]::Round(((Get-Date) - $startTime).TotalSeconds)

Write-Host ""
Write-Host "  Build completed in ${elapsed}s" -ForegroundColor Green
Write-Host ""
Write-Host "  Output files:" -ForegroundColor White

if (Test-Path $nsisDir) {
  foreach ($f in (Get-ChildItem $nsisDir -Filter "*.exe")) {
    Write-Host "    [Installer] $($f.FullName)  ($([math]::Round($f.Length/1MB,1)) MB)" -ForegroundColor Cyan
  }
} else {
  Write-Host "  [!] NSIS bundle dir not found: $nsisDir" -ForegroundColor Yellow
}

if (Test-Path $exePath) {
  Write-Host "    [Portable]  $exePath  ($([math]::Round((Get-Item $exePath).Length/1MB,1)) MB)" -ForegroundColor Cyan
}

Write-Host ""

if ($OpenOutput -and (Test-Path $nsisDir)) {
  Start-Process explorer.exe $nsisDir
}
