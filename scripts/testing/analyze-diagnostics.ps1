param(
    [string]$LogsDir = "",
    [string]$SettingsPath = "",
    [int]$Limit = 30,
    [string]$RepoRoot = "E:\_workSpace\wipeout",
    [string]$OutputDir = "test-runs"
)

$ErrorActionPreference = "Stop"

if ($Limit -lt 1) {
    throw "-Limit must be greater than 0."
}

$repo = (Resolve-Path -LiteralPath $RepoRoot).Path
$targetDir = if ([IO.Path]::IsPathRooted($OutputDir)) {
    $OutputDir
} else {
    Join-Path $repo $OutputDir
}
New-Item -ItemType Directory -Force -Path $targetDir | Out-Null
$outputRoot = (Resolve-Path -LiteralPath $targetDir).Path

$summaryPath = Join-Path $outputRoot "latest-diagnostics-summary.json"
$exportPath = Join-Path $outputRoot "latest-diagnostics-records.jsonl"

$summaryArgs = @(
    "run", "--quiet", "--manifest-path", "src-tauri/Cargo.toml", "--bin", "aicleaner-logs", "--",
    "--json"
)
if (-not [string]::IsNullOrWhiteSpace($LogsDir)) {
    $summaryArgs += @("--logs-dir", $LogsDir)
} elseif (-not [string]::IsNullOrWhiteSpace($SettingsPath)) {
    $summaryArgs += @("--settings-path", $SettingsPath)
}
$summaryArgs += @("summary", "--family", "diagnostics", "--limit", "$Limit")

$summaryJson = & cargo @summaryArgs 2>&1
if ($LASTEXITCODE -ne 0) {
    throw ($summaryJson -join [Environment]::NewLine)
}
$summaryJson | Set-Content -LiteralPath $summaryPath -Encoding UTF8

$exportArgs = @(
    "run", "--quiet", "--manifest-path", "src-tauri/Cargo.toml", "--bin", "aicleaner-logs", "--"
)
if (-not [string]::IsNullOrWhiteSpace($LogsDir)) {
    $exportArgs += @("--logs-dir", $LogsDir)
} elseif (-not [string]::IsNullOrWhiteSpace($SettingsPath)) {
    $exportArgs += @("--settings-path", $SettingsPath)
}
$exportArgs += @("show", "--family", "diagnostics", "--format", "jsonl", "--output", $exportPath)

$exportResult = & cargo @exportArgs 2>&1
if ($LASTEXITCODE -ne 0) {
    throw ($exportResult -join [Environment]::NewLine)
}

Write-Host "[diagnostics-analyze] summary=$summaryPath"
Write-Host "[diagnostics-analyze] records=$exportPath"
