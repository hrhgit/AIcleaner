param(
    [string]$RepoRoot = "E:\_workSpace\wipeout",
    [string]$RealFolder = "E:\Download",
    [string]$Endpoint = "",
    [string]$ApiKey = "",
    [string]$Model = "",
    [string]$SettingsPath = "",
    [ValidateSet("filename_only", "local_summary")]
    [string]$SummaryStrategy = "filename_only",
    [int]$MaxItems = 24,
    [int]$RealBatchSize = 8,
    [int]$RealConcurrency = 2,
    [string]$OutputDir = "test-runs",
    [switch]$AllRealItems
)

$ErrorActionPreference = "Stop"

function Get-AicleanerSettingsPath {
    param([string]$ExplicitPath)

    if (-not [string]::IsNullOrWhiteSpace($ExplicitPath)) {
        return (Resolve-Path -LiteralPath $ExplicitPath).Path
    }
    if (-not [string]::IsNullOrWhiteSpace($env:AICLEANER_SETTINGS_PATH)) {
        return (Resolve-Path -LiteralPath $env:AICLEANER_SETTINGS_PATH).Path
    }
    if (-not [string]::IsNullOrWhiteSpace($env:AICLEANER_DATA_DIR)) {
        $fromEnvDir = Join-Path $env:AICLEANER_DATA_DIR "settings.json"
        if (Test-Path -LiteralPath $fromEnvDir) {
            return (Resolve-Path -LiteralPath $fromEnvDir).Path
        }
    }
    $defaultPath = "E:\Cache\AIcleaner\settings.json"
    if (Test-Path -LiteralPath $defaultPath) {
        return (Resolve-Path -LiteralPath $defaultPath).Path
    }
    throw "AIcleaner settings not found. Pass -SettingsPath, or set AICLEANER_SETTINGS_PATH/AICLEANER_DATA_DIR."
}

function Add-CredManNativeType {
    if ("CredManNative" -as [type]) {
        return
    }
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class CredManNative {
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  public struct CREDENTIAL {
    public UInt32 Flags;
    public UInt32 Type;
    public IntPtr TargetName;
    public IntPtr Comment;
    public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
    public UInt32 CredentialBlobSize;
    public IntPtr CredentialBlob;
    public UInt32 Persist;
    public UInt32 AttributeCount;
    public IntPtr Attributes;
    public IntPtr TargetAlias;
    public IntPtr UserName;
  }
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
  public static extern bool CredRead(string target, UInt32 type, UInt32 reservedFlag, out IntPtr credentialPtr);
  [DllImport("advapi32.dll", SetLastError=true)]
  public static extern void CredFree(IntPtr buffer);
}
"@
}

function Get-WindowsCredentialSecret {
    param([string]$Target)

    Add-CredManNativeType
    $ptr = [IntPtr]::Zero
    if (-not [CredManNative]::CredRead($Target, 1, 0, [ref]$ptr)) {
        $code = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        if ($code -eq 1168) {
            return $null
        }
        throw "CredRead failed for target '$Target' (Win32=$code)."
    }

    try {
        $credential = [Runtime.InteropServices.Marshal]::PtrToStructure(
            $ptr,
            [type][CredManNative+CREDENTIAL]
        )
        if ($credential.CredentialBlobSize -eq 0) {
            return ""
        }
        $bytes = New-Object byte[] $credential.CredentialBlobSize
        [Runtime.InteropServices.Marshal]::Copy(
            $credential.CredentialBlob,
            $bytes,
            0,
            [int]$credential.CredentialBlobSize
        )
        return [Text.Encoding]::Unicode.GetString($bytes).TrimEnd([char]0)
    } finally {
        [CredManNative]::CredFree($ptr)
    }
}

function Resolve-AicleanerRealModelConfig {
    param(
        [string]$ExplicitEndpoint,
        [string]$ExplicitApiKey,
        [string]$ExplicitModel,
        [string]$ExplicitSettingsPath
    )

    $resolvedSettingsPath = Get-AicleanerSettingsPath $ExplicitSettingsPath
    $settings = Get-Content -LiteralPath $resolvedSettingsPath -Raw | ConvertFrom-Json

    $resolvedEndpoint = $ExplicitEndpoint
    if ([string]::IsNullOrWhiteSpace($resolvedEndpoint)) {
        $resolvedEndpoint = [string]$settings.defaultProviderEndpoint
    }
    if ([string]::IsNullOrWhiteSpace($resolvedEndpoint)) {
        throw "AIcleaner settings do not contain defaultProviderEndpoint. Pass -Endpoint explicitly."
    }

    $providerConfig = $settings.providerConfigs.$resolvedEndpoint
    if ($null -eq $providerConfig) {
        $providerConfig = $settings.providerConfigs.PSObject.Properties |
            Where-Object { $_.Value.endpoint -eq $resolvedEndpoint } |
            Select-Object -First 1 -ExpandProperty Value
    }

    $resolvedModel = $ExplicitModel
    if ([string]::IsNullOrWhiteSpace($resolvedModel) -and $null -ne $providerConfig) {
        $resolvedModel = [string]$providerConfig.model
    }
    if ([string]::IsNullOrWhiteSpace($resolvedModel)) {
        throw "AIcleaner settings do not contain a model for '$resolvedEndpoint'. Pass -Model explicitly."
    }

    $resolvedApiKey = $ExplicitApiKey
    if ([string]::IsNullOrWhiteSpace($resolvedApiKey) -and $null -ne $providerConfig) {
        $resolvedApiKey = [string]$providerConfig.apiKey
    }
    if ([string]::IsNullOrWhiteSpace($resolvedApiKey)) {
        $account = "provider:$($resolvedEndpoint.Trim()):apiKey"
        $target = "$account.aicleaner"
        $resolvedApiKey = Get-WindowsCredentialSecret $target
    }
    if ([string]::IsNullOrWhiteSpace($resolvedApiKey)) {
        throw "No API key found for '$resolvedEndpoint'. Save it in AIcleaner credentials or pass -ApiKey explicitly."
    }

    [pscustomobject]@{
        SettingsPath = $resolvedSettingsPath
        Endpoint = $resolvedEndpoint
        Model = $resolvedModel
        ApiKey = $resolvedApiKey
    }
}

function Resolve-OutputDirectory {
    param([string]$Root, [string]$Dir)

    if ([IO.Path]::IsPathRooted($Dir)) {
        $resolved = $Dir
    } else {
        $resolved = Join-Path $Root $Dir
    }
    New-Item -ItemType Directory -Force -Path $resolved | Out-Null
    return (Resolve-Path -LiteralPath $resolved).Path
}

function Invoke-CargoAndCapture {
    param([string[]]$Arguments)

    $lines = New-Object System.Collections.Generic.List[string]
    & cargo @Arguments 2>&1 | ForEach-Object {
        $line = $_.ToString()
        $lines.Add($line)
        Write-Host $line
    }
    [pscustomobject]@{
        ExitCode = $LASTEXITCODE
        Lines = @($lines)
    }
}

function ConvertTo-JsonLine {
    param($Value)
    return ($Value | ConvertTo-Json -Depth 32 -Compress)
}

function Find-ValueLine {
    param([string[]]$Lines, [string]$Name)

    foreach ($line in $Lines) {
        if ($line -match "^$([regex]::Escape($Name))=(.*)$") {
            return $Matches[1]
        }
    }
    return $null
}

function Parse-IntOrNull {
    param([string]$Value)
    if ([string]::IsNullOrWhiteSpace($Value)) {
        return $null
    }
    $parsed = 0
    if ([int]::TryParse($Value, [ref]$parsed)) {
        return $parsed
    }
    return $null
}

$repo = (Resolve-Path -LiteralPath $RepoRoot).Path
$cargoToml = Join-Path $repo "src-tauri\Cargo.toml"
if (-not (Test-Path -LiteralPath $cargoToml)) {
    throw "Cargo manifest not found: $cargoToml"
}

$realRoot = (Resolve-Path -LiteralPath $RealFolder).Path
if (-not (Test-Path -LiteralPath $realRoot -PathType Container)) {
    throw "RealFolder must be a directory: $realRoot"
}
if ($MaxItems -lt 1) {
    throw "-MaxItems must be greater than 0"
}
if ($RealBatchSize -lt 1) {
    throw "-RealBatchSize must be greater than 0"
}
if ($RealConcurrency -lt 1) {
    throw "-RealConcurrency must be greater than 0"
}

$resolvedConfig = Resolve-AicleanerRealModelConfig `
    -ExplicitEndpoint $Endpoint `
    -ExplicitApiKey $ApiKey `
    -ExplicitModel $Model `
    -ExplicitSettingsPath $SettingsPath

$timestamp = [DateTime]::UtcNow.ToString("yyyyMMdd-HHmmss-fffZ")
$runId = "smoke-$timestamp"
$outputRoot = Resolve-OutputDirectory -Root $repo -Dir $OutputDir
$jsonlPath = Join-Path $outputRoot "$timestamp-smoke.jsonl"
$summaryPath = Join-Path $outputRoot "$timestamp-smoke-summary.md"
$rawPath = Join-Path $outputRoot "$timestamp-smoke-raw.log"

$env:WIPEOUT_CLASSIFICATION_SMOKE_ROOT = $realRoot
$env:WIPEOUT_CLASSIFICATION_SMOKE_ENDPOINT = $resolvedConfig.Endpoint
$env:WIPEOUT_CLASSIFICATION_SMOKE_API_KEY = $resolvedConfig.ApiKey
$env:WIPEOUT_CLASSIFICATION_SMOKE_MODEL = $resolvedConfig.Model
$env:WIPEOUT_CLASSIFICATION_SMOKE_SUMMARY_STRATEGY = $SummaryStrategy
$env:WIPEOUT_CLASSIFICATION_SMOKE_MAX_ITEMS = if ($AllRealItems) { [string][int]::MaxValue } else { [string]$MaxItems }
$env:WIPEOUT_CLASSIFICATION_SMOKE_CHUNK_SIZE = [string]$RealBatchSize
$env:WIPEOUT_CLASSIFICATION_SMOKE_CONCURRENCY = [string]$RealConcurrency

Write-Host "[classification-smoke] settings=$($resolvedConfig.SettingsPath)"
Write-Host "[classification-smoke] endpoint=$($resolvedConfig.Endpoint) model=$($resolvedConfig.Model) apiKey=<redacted>"
Write-Host "[classification-smoke] folder=$realRoot"
Write-Host "[classification-smoke] summaryStrategy=$SummaryStrategy maxItems=$($env:WIPEOUT_CLASSIFICATION_SMOKE_MAX_ITEMS) batchSize=$RealBatchSize concurrency=$RealConcurrency"

$cargoArgs = @(
    "test",
    "--manifest-path", $cargoToml,
    "real_folder_classification_smoke_with_real_model",
    "--lib",
    "--",
    "--ignored",
    "--nocapture"
)
$result = Invoke-CargoAndCapture -Arguments $cargoArgs
$result.Lines | Set-Content -LiteralPath $rawPath -Encoding UTF8

$items = Parse-IntOrNull (Find-ValueLine -Lines $result.Lines -Name "items")
$assigned = Parse-IntOrNull (Find-ValueLine -Lines $result.Lines -Name "assigned")
$collectedItems = Parse-IntOrNull (Find-ValueLine -Lines $result.Lines -Name "collected_items")
$chunkSize = Parse-IntOrNull (Find-ValueLine -Lines $result.Lines -Name "chunk_size")
$concurrency = Parse-IntOrNull (Find-ValueLine -Lines $result.Lines -Name "concurrency")
$chunks = Parse-IntOrNull (Find-ValueLine -Lines $result.Lines -Name "chunks")
$promptTokens = $null
$completionTokens = $null
$totalTokens = $null
$collectMs = $null
$summaryMs = $null
$modelSumMs = $null
$wallMs = $null
$totalMs = $null

foreach ($line in $result.Lines) {
    if ($line -match "^usage=prompt:(\d+),completion:(\d+),total:(\d+)$") {
        $promptTokens = [int]$Matches[1]
        $completionTokens = [int]$Matches[2]
        $totalTokens = [int]$Matches[3]
    }
    if ($line -match "^timing=collect:(\d+)ms,summary:(\d+)ms,classify_model_sum:(\d+)ms,classify_wall:(\d+)ms,total:(\d+)ms$") {
        $collectMs = [int]$Matches[1]
        $summaryMs = [int]$Matches[2]
        $modelSumMs = [int]$Matches[3]
        $wallMs = [int]$Matches[4]
        $totalMs = [int]$Matches[5]
    }
}

$missing = $null
if ($null -ne $items -and $null -ne $assigned) {
    $missing = [Math]::Max(0, $items - $assigned)
}
$tail = ($result.Lines | Select-Object -Last 30) -join "`n"
$errorText = if ($result.ExitCode -eq 0) { "" } else { $tail }

$row = [ordered]@{
    runId = $runId
    mode = "smoke"
    timestamp = $timestamp
    rootPath = $realRoot
    endpoint = $resolvedConfig.Endpoint
    model = $resolvedConfig.Model
    summaryStrategy = $SummaryStrategy
    batchSize = $chunkSize
    concurrency = $concurrency
    repeat = $null
    ok = ($result.ExitCode -eq 0 -and ($null -eq $missing -or $missing -eq 0))
    durationMs = $totalMs
    wallMs = $wallMs
    modelSumMs = $modelSumMs
    collectMs = $collectMs
    summaryMs = $summaryMs
    chunks = $chunks
    collectedItems = $collectedItems
    items = $items
    assigned = $assigned
    missing = $missing
    duplicates = 0
    unknown = 0
    promptTokens = $promptTokens
    completionTokens = $completionTokens
    totalTokens = $totalTokens
    error = $errorText
}
ConvertTo-JsonLine $row | Set-Content -LiteralPath $jsonlPath -Encoding UTF8

$summary = @(
    "# Classification Smoke Summary",
    "",
    "- Run ID: $runId",
    "- Exit code: $($result.ExitCode)",
    "- Root: $realRoot",
    "- Endpoint: $($resolvedConfig.Endpoint)",
    "- Model: $($resolvedConfig.Model)",
    "- Summary strategy: $SummaryStrategy",
    "- Items: $items",
    "- Assigned: $assigned",
    "- Missing: $missing",
    "- Chunks: $chunks",
    "- Batch size: $chunkSize",
    "- Concurrency: $concurrency",
    "- Total ms: $totalMs",
    "- Classify wall ms: $wallMs",
    "- Classify model sum ms: $modelSumMs",
    "- Tokens: prompt $promptTokens, completion $completionTokens, total $totalTokens",
    "",
    "## Files",
    "",
    "- JSONL: $jsonlPath",
    "- Raw log: $rawPath"
)
if ($result.ExitCode -ne 0) {
    $summary += @("", "## Error Tail", "", '```text', $tail, '```')
}
$summary | Set-Content -LiteralPath $summaryPath -Encoding UTF8

Write-Host "[classification-smoke] jsonl=$jsonlPath"
Write-Host "[classification-smoke] summary=$summaryPath"
Write-Host "[classification-smoke] raw=$rawPath"

if ($result.ExitCode -ne 0) {
    exit $result.ExitCode
}
