param(
    [string]$LogsDir = "",
    [string]$SettingsPath = "",
    [int]$Limit = 30,
    [string]$RepoRoot = "E:\_workSpace\wipeout",
    [string]$OutputDir = "test-runs"
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
        $candidate = Join-Path $env:AICLEANER_DATA_DIR "settings.json"
        if (Test-Path -LiteralPath $candidate) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    $defaultPath = "E:\Cache\AIcleaner\settings.json"
    if (Test-Path -LiteralPath $defaultPath) {
        return (Resolve-Path -LiteralPath $defaultPath).Path
    }
    return $null
}

function Get-AicleanerLogsDir {
    param([string]$ExplicitLogsDir, [string]$ExplicitSettingsPath)

    if (-not [string]::IsNullOrWhiteSpace($ExplicitLogsDir)) {
        return (Resolve-Path -LiteralPath $ExplicitLogsDir).Path
    }

    $settings = Get-AicleanerSettingsPath $ExplicitSettingsPath
    if ($settings) {
        $candidate = Join-Path (Split-Path -Parent $settings) "logs"
        if (Test-Path -LiteralPath $candidate) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }

    $defaultLogs = "E:\Cache\AIcleaner\logs"
    if (Test-Path -LiteralPath $defaultLogs) {
        return (Resolve-Path -LiteralPath $defaultLogs).Path
    }

    throw "AIcleaner logs directory not found. Pass -LogsDir or -SettingsPath."
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

function Measure-JsonChars {
    param($Value)
    if ($null -eq $Value) {
        return 0
    }
    return (($Value | ConvertTo-Json -Depth 100 -Compress).Length)
}

function Measure-MessageContentChars {
    param($Messages)
    $total = 0
    foreach ($message in @($Messages)) {
        if ($null -eq $message.content) {
            continue
        }
        if ($message.content -is [string]) {
            $total += $message.content.Length
        } else {
            $total += Measure-JsonChars $message.content
        }
    }
    return $total
}

function ConvertTo-JsonLine {
    param($Value)
    return ($Value | ConvertTo-Json -Depth 32 -Compress)
}

if ($Limit -lt 1) {
    throw "-Limit must be greater than 0."
}

$repo = (Resolve-Path -LiteralPath $RepoRoot).Path
$timestamp = [DateTime]::UtcNow.ToString("yyyyMMdd-HHmmss-fffZ")
$runId = "diagnostics-$timestamp"
$outputRoot = Resolve-OutputDirectory -Root $repo -Dir $OutputDir
$jsonlPath = Join-Path $outputRoot "$timestamp-diagnostics.jsonl"
$summaryPath = Join-Path $outputRoot "$timestamp-diagnostics-summary.md"

$resolvedLogsDir = Get-AicleanerLogsDir $LogsDir $SettingsPath
$candidates = @(Get-ChildItem -LiteralPath $resolvedLogsDir -Filter "aicleaner-diagnostics*.jsonl" -Force |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 20)

if (-not $candidates) {
    throw "No AIcleaner diagnostics JSONL files found in $resolvedLogsDir."
}

$latest = $null
$records = @()
foreach ($candidate in $candidates) {
    $candidateRecords = @(Get-Content -LiteralPath $candidate.FullName |
        ForEach-Object {
            try {
                $_ | ConvertFrom-Json
            } catch {
                $null
            }
        } |
        Where-Object { $_ -and ($_.event -like "organizer_*" -or $_.category -eq "organizer") })
    if ($candidateRecords.Count -gt 0) {
        $latest = $candidate
        $records = $candidateRecords
        break
    }
}

if (-not $latest) {
    throw "No organizer diagnostics events found in the latest $($candidates.Count) diagnostics file(s) under $resolvedLogsDir."
}

$requests = @($records | Where-Object { $_.event -eq "organizer_model_request" })
$responses = @($records | Where-Object {
    $_.event -eq "organizer_model_response" -or $_.event -eq "organizer_model_error"
})
$stageCompletions = @($records | Where-Object { $_.event -eq "organizer_stage_completed" })
$errors = @($records | Where-Object { $_.level -eq "error" -or $_.event -like "*error*" })

$rows = New-Object System.Collections.Generic.List[object]
$rows.Add([ordered]@{
    runId = $runId
    mode = "diagnostics"
    timestamp = $timestamp
    logPath = $latest.FullName
    logsDir = $resolvedLogsDir
    organizerEvents = $records.Count
    requestCount = $requests.Count
    responseOrErrorCount = $responses.Count
    stageCompletionCount = $stageCompletions.Count
    errorCount = $errors.Count
    eventType = "summary"
})

$idx = 0
foreach ($request in $requests) {
    $idx += 1
    $rows.Add([ordered]@{
        runId = $runId
        mode = "diagnostics"
        timestamp = $timestamp
        logPath = $latest.FullName
        eventType = "request"
        index = $idx
        stage = $request.details.stage
        model = $request.details.model
        payloadChars = Measure-JsonChars $request.details.payload
        messageChars = Measure-JsonChars $request.details.messages
        contentChars = Measure-MessageContentChars $request.details.messages
        toolChars = Measure-JsonChars $request.details.tools
    })
}

$idx = 0
foreach ($response in ($responses | Select-Object -First $Limit)) {
    $idx += 1
    $raw = if ($response.details.rawBody) { [string]$response.details.rawBody } else { "" }
    $rows.Add([ordered]@{
        runId = $runId
        mode = "diagnostics"
        timestamp = $timestamp
        logPath = $latest.FullName
        eventType = "response"
        index = $idx
        event = $response.event
        stage = $response.details.stage
        status = $response.details.status
        durationMs = $response.durationMs
        rawBodyChars = $raw.Length
        message = $response.message
    })
}

$rows | ForEach-Object { ConvertTo-JsonLine $_ } | Set-Content -LiteralPath $jsonlPath -Encoding UTF8

Write-Host "[diagnostics-analyze] log=$($latest.FullName)"
Write-Host "[diagnostics-analyze] organizer_events=$($records.Count) request_count=$($requests.Count) response_or_error_count=$($responses.Count)"

$summary = @(
    "# Organizer Diagnostics Summary",
    "",
    "- Run ID: $runId",
    "- Logs dir: $resolvedLogsDir",
    "- Selected log: $($latest.FullName)",
    "- Organizer events: $($records.Count)",
    "- Requests: $($requests.Count)",
    "- Responses/errors: $($responses.Count)",
    "- Stage completions: $($stageCompletions.Count)",
    "- Error events: $($errors.Count)",
    "",
    "## Files",
    "",
    "- JSONL: $jsonlPath",
    "",
    "## Requests",
    "",
    "| index | stage | model | payloadChars | messageChars | contentChars | toolChars |",
    "| --- | --- | --- | --- | --- | --- | --- |"
)

$requestIdx = 0
foreach ($request in $requests) {
    $requestIdx += 1
    $summary += "| $requestIdx | $($request.details.stage) | $($request.details.model) | $(Measure-JsonChars $request.details.payload) | $(Measure-JsonChars $request.details.messages) | $(Measure-MessageContentChars $request.details.messages) | $(Measure-JsonChars $request.details.tools) |"
}

$summary += @(
    "",
    "## Responses And Errors",
    "",
    "| index | event | stage | status | durationMs | rawBodyChars | message |",
    "| --- | --- | --- | --- | --- | --- | --- |"
)

$responseIdx = 0
foreach ($response in ($responses | Select-Object -First $Limit)) {
    $responseIdx += 1
    $raw = if ($response.details.rawBody) { [string]$response.details.rawBody } else { "" }
    $message = ([string]$response.message).Replace("|", "/")
    $summary += "| $responseIdx | $($response.event) | $($response.details.stage) | $($response.details.status) | $($response.durationMs) | $($raw.Length) | $message |"
}

$summary | Set-Content -LiteralPath $summaryPath -Encoding UTF8

Write-Host "[diagnostics-analyze] jsonl=$jsonlPath"
Write-Host "[diagnostics-analyze] summary=$summaryPath"
