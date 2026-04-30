param(
    [ValidateSet("capacity", "concurrency")]
    [string]$Mode = "capacity",
    [string]$RepoRoot = "E:\_workSpace\wipeout",
    [string]$RealFolder = "E:\Download",
    [string]$Endpoint = "",
    [string]$ApiKey = "",
    [string]$Model = "",
    [string]$SettingsPath = "",
    [ValidateSet("filename_only", "local_summary")]
    [string]$SummaryStrategy = "filename_only",
    [string]$OutputDir = "test-runs",
    [string]$BatchSizes = "4,8,12,16,24",
    [int]$Repeats = 1,
    [int]$RequestConcurrency = 0,
    [int]$BatchSize = 10,
    [int]$MaxItems = 240,
    [string]$ConcurrencyValues = "1,2,4,8"
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

function Split-PositiveIntList {
    param([string]$Raw, [string]$Name)

    $values = @($Raw -split "[,; ]+" |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
        ForEach-Object {
            $parsed = 0
            if (-not [int]::TryParse($_.Trim(), [ref]$parsed) -or $parsed -lt 1) {
                throw "$Name must contain only positive integers. Invalid value: $_"
            }
            $parsed
        })
    if ($values.Count -lt 1) {
        throw "$Name must contain at least one positive integer."
    }
    return $values
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

$batchSizeValues = Split-PositiveIntList -Raw $BatchSizes -Name "-BatchSizes"
$concurrencyValueList = Split-PositiveIntList -Raw $ConcurrencyValues -Name "-ConcurrencyValues"
if ($Repeats -lt 1) {
    throw "-Repeats must be greater than 0."
}
if ($RequestConcurrency -lt 0) {
    throw "-RequestConcurrency must be 0 or greater."
}
if ($BatchSize -lt 1) {
    throw "-BatchSize must be greater than 0."
}
if ($MaxItems -lt 1) {
    throw "-MaxItems must be greater than 0."
}

$resolvedConfig = Resolve-AicleanerRealModelConfig `
    -ExplicitEndpoint $Endpoint `
    -ExplicitApiKey $ApiKey `
    -ExplicitModel $Model `
    -ExplicitSettingsPath $SettingsPath

$timestamp = [DateTime]::UtcNow.ToString("yyyyMMdd-HHmmss-fffZ")
$runId = "$Mode-$timestamp"
$outputRoot = Resolve-OutputDirectory -Root $repo -Dir $OutputDir
$jsonlPath = Join-Path $outputRoot "$timestamp-$Mode.jsonl"
$summaryPath = Join-Path $outputRoot "$timestamp-$Mode-summary.md"
$rawPath = Join-Path $outputRoot "$timestamp-$Mode-raw.log"

if ($Mode -eq "capacity") {
    $effectiveRequestConcurrency = if ($RequestConcurrency -gt 0) { $RequestConcurrency } else { $batchSizeValues.Count }
    $env:WIPEOUT_CAPACITY_SWEEP_ROOT = $realRoot
    $env:WIPEOUT_CAPACITY_SWEEP_ENDPOINT = $resolvedConfig.Endpoint
    $env:WIPEOUT_CAPACITY_SWEEP_API_KEY = $resolvedConfig.ApiKey
    $env:WIPEOUT_CAPACITY_SWEEP_MODEL = $resolvedConfig.Model
    $env:WIPEOUT_CAPACITY_SWEEP_SUMMARY_STRATEGY = $SummaryStrategy
    $env:WIPEOUT_CAPACITY_SWEEP_BATCH_SIZES = ($batchSizeValues -join ",")
    $env:WIPEOUT_CAPACITY_SWEEP_REPEATS = [string]$Repeats
    $env:WIPEOUT_CAPACITY_SWEEP_REQUEST_CONCURRENCY = [string]$effectiveRequestConcurrency
    $testName = "real_folder_single_batch_capacity_sweep_with_real_model"
    $experimentConcurrency = $effectiveRequestConcurrency
} else {
    $env:WIPEOUT_CONCURRENCY_SWEEP_ROOT = $realRoot
    $env:WIPEOUT_CONCURRENCY_SWEEP_ENDPOINT = $resolvedConfig.Endpoint
    $env:WIPEOUT_CONCURRENCY_SWEEP_API_KEY = $resolvedConfig.ApiKey
    $env:WIPEOUT_CONCURRENCY_SWEEP_MODEL = $resolvedConfig.Model
    $env:WIPEOUT_CONCURRENCY_SWEEP_SUMMARY_STRATEGY = $SummaryStrategy
    $env:WIPEOUT_CONCURRENCY_SWEEP_BATCH_SIZE = [string]$BatchSize
    $env:WIPEOUT_CONCURRENCY_SWEEP_MAX_ITEMS = [string]$MaxItems
    $env:WIPEOUT_CONCURRENCY_SWEEP_VALUES = ($concurrencyValueList -join ",")
    $testName = "real_folder_small_batch_concurrency_sweep_with_real_model"
    $experimentConcurrency = $null
}

Write-Host "[organizer-experiment] mode=$Mode settings=$($resolvedConfig.SettingsPath)"
Write-Host "[organizer-experiment] endpoint=$($resolvedConfig.Endpoint) model=$($resolvedConfig.Model) apiKey=<redacted>"
Write-Host "[organizer-experiment] folder=$realRoot summaryStrategy=$SummaryStrategy"
if ($Mode -eq "capacity") {
    Write-Host "[organizer-experiment] batchSizes=$($batchSizeValues -join ',') repeats=$Repeats requestConcurrency=$experimentConcurrency"
} else {
    Write-Host "[organizer-experiment] batchSize=$BatchSize maxItems=$MaxItems concurrencyValues=$($concurrencyValueList -join ',')"
}

$cargoArgs = @(
    "test",
    "--manifest-path", $cargoToml,
    $testName,
    "--lib",
    "--",
    "--ignored",
    "--nocapture"
)
$result = Invoke-CargoAndCapture -Arguments $cargoArgs
$result.Lines | Set-Content -LiteralPath $rawPath -Encoding UTF8

$rows = New-Object System.Collections.Generic.List[object]
foreach ($line in $result.Lines) {
    if ($line.StartsWith("capacity_result,")) {
        $parts = $line.Split(",", 17)
        if ($parts.Count -ge 17 -and $parts[1] -ne "repeat") {
            $rows.Add([ordered]@{
                runId = $runId
                mode = "capacity"
                timestamp = $timestamp
                rootPath = $realRoot
                endpoint = $resolvedConfig.Endpoint
                model = $resolvedConfig.Model
                summaryStrategy = $SummaryStrategy
                batchSize = [int]$parts[2]
                concurrency = $experimentConcurrency
                repeat = [int]$parts[1]
                ok = [bool]::Parse($parts[3])
                durationMs = [int64]$parts[4]
                wallMs = $null
                modelSumMs = $null
                summaryMs = [int64]$parts[5]
                p50Ms = $null
                p95Ms = $null
                maxMs = $null
                batches = $null
                failedBatches = $null
                items = [int]$parts[6]
                assigned = [int]$parts[7]
                uniqueAssigned = [int]$parts[8]
                missing = [int]$parts[9]
                duplicates = [int]$parts[10]
                unknown = [int]$parts[11]
                promptTokens = [int]$parts[12]
                completionTokens = [int]$parts[13]
                totalTokens = [int]$parts[14]
                rawChars = [int]$parts[15]
                error = $parts[16]
            })
        }
    } elseif ($line.StartsWith("concurrency_result,")) {
        $parts = $line.Split(",", 20)
        if ($parts.Count -ge 20 -and $parts[1] -ne "concurrency") {
            $rows.Add([ordered]@{
                runId = $runId
                mode = "concurrency"
                timestamp = $timestamp
                rootPath = $realRoot
                endpoint = $resolvedConfig.Endpoint
                model = $resolvedConfig.Model
                summaryStrategy = $SummaryStrategy
                batchSize = $BatchSize
                concurrency = [int]$parts[1]
                repeat = $null
                ok = [bool]::Parse($parts[2])
                durationMs = $null
                wallMs = [int64]$parts[3]
                modelSumMs = [int64]$parts[4]
                summaryMs = $null
                p50Ms = [int64]$parts[5]
                p95Ms = [int64]$parts[6]
                maxMs = [int64]$parts[7]
                batches = [int]$parts[8]
                failedBatches = [int]$parts[9]
                items = [int]$parts[10]
                assigned = [int]$parts[11]
                uniqueAssigned = [int]$parts[12]
                missing = [int]$parts[13]
                duplicates = [int]$parts[14]
                unknown = [int]$parts[15]
                promptTokens = [int]$parts[16]
                completionTokens = [int]$parts[17]
                totalTokens = [int]$parts[18]
                rawChars = $null
                error = $parts[19]
            })
        }
    }
}

if ($rows.Count -eq 0) {
    $tail = ($result.Lines | Select-Object -Last 30) -join "`n"
    $rows.Add([ordered]@{
        runId = $runId
        mode = $Mode
        timestamp = $timestamp
        rootPath = $realRoot
        endpoint = $resolvedConfig.Endpoint
        model = $resolvedConfig.Model
        summaryStrategy = $SummaryStrategy
        batchSize = if ($Mode -eq "concurrency") { $BatchSize } else { $null }
        concurrency = if ($Mode -eq "capacity") { $experimentConcurrency } else { $null }
        repeat = $null
        ok = $false
        durationMs = $null
        wallMs = $null
        modelSumMs = $null
        items = $null
        assigned = $null
        missing = $null
        duplicates = $null
        unknown = $null
        promptTokens = $null
        completionTokens = $null
        totalTokens = $null
        error = $tail
    })
}

$rows | ForEach-Object { ConvertTo-JsonLine $_ } | Set-Content -LiteralPath $jsonlPath -Encoding UTF8

$successRows = @($rows | Where-Object { $_.ok -eq $true })
$failedRows = @($rows | Where-Object { $_.ok -ne $true })
$totalTokens = ($rows | Measure-Object -Property totalTokens -Sum).Sum
$totalItems = ($rows | Measure-Object -Property items -Sum).Sum
$summary = @(
    "# Organizer Experiment Summary",
    "",
    "- Run ID: $runId",
    "- Mode: $Mode",
    "- Exit code: $($result.ExitCode)",
    "- Root: $realRoot",
    "- Endpoint: $($resolvedConfig.Endpoint)",
    "- Model: $($resolvedConfig.Model)",
    "- Summary strategy: $SummaryStrategy",
    "- Result rows: $($rows.Count)",
    "- Successful rows: $($successRows.Count)",
    "- Failed rows: $($failedRows.Count)",
    "- Total items: $totalItems",
    "- Total tokens: $totalTokens",
    "",
    "## Files",
    "",
    "- JSONL: $jsonlPath",
    "- Raw log: $rawPath",
    "",
    "## Results",
    ""
)

if ($Mode -eq "capacity") {
    $summary += "| batchSize | repeat | ok | durationMs | summaryMs | items | missing | duplicates | unknown | totalTokens | error |"
    $summary += "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    foreach ($row in $rows) {
        $err = ([string]$row.error).Replace("|", "/")
        $summary += "| $($row.batchSize) | $($row.repeat) | $($row.ok) | $($row.durationMs) | $($row.summaryMs) | $($row.items) | $($row.missing) | $($row.duplicates) | $($row.unknown) | $($row.totalTokens) | $err |"
    }
} else {
    $summary += "| concurrency | ok | wallMs | modelSumMs | p50Ms | p95Ms | batches | failedBatches | items | missing | totalTokens | error |"
    $summary += "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    foreach ($row in $rows) {
        $err = ([string]$row.error).Replace("|", "/")
        $summary += "| $($row.concurrency) | $($row.ok) | $($row.wallMs) | $($row.modelSumMs) | $($row.p50Ms) | $($row.p95Ms) | $($row.batches) | $($row.failedBatches) | $($row.items) | $($row.missing) | $($row.totalTokens) | $err |"
    }
}

if ($result.ExitCode -ne 0) {
    $tail = ($result.Lines | Select-Object -Last 30) -join "`n"
    $summary += @("", "## Error Tail", "", '```text', $tail, '```')
}
$summary | Set-Content -LiteralPath $summaryPath -Encoding UTF8

Write-Host "[organizer-experiment] jsonl=$jsonlPath"
Write-Host "[organizer-experiment] summary=$summaryPath"
Write-Host "[organizer-experiment] raw=$rawPath"

if ($result.ExitCode -ne 0) {
    exit $result.ExitCode
}
