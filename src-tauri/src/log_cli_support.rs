use crate::app_paths::{
    default_data_dir, default_legacy_app_log_dir, resolve_storage_data_dir, AppPaths,
    APP_LOG_FILE_STEM, STORAGE_LOCATION_FILE,
};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

const PREVIEW_CHAR_LIMIT: usize = 300;
#[derive(Clone, Debug, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LogFamily {
    Diagnostics,
    WebSearch,
    App,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPaths {
    pub data_dir: PathBuf,
    pub settings_path: PathBuf,
    pub logs_dir: PathBuf,
    pub legacy_app_logs_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct ResolveOptions {
    pub data_dir: Option<PathBuf>,
    pub settings_path: Option<PathBuf>,
    pub logs_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFileInfo {
    pub path: PathBuf,
    pub family: LogFamily,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub correlation_id: Option<String>,
    pub source: String,
}

#[derive(Clone, Debug, Default)]
pub struct RecordFilter {
    pub family: Option<LogFamily>,
    pub level: Option<String>,
    pub event: Option<String>,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub job_id: Option<String>,
    pub operation_id: Option<String>,
    pub since: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedRecord {
    pub family: LogFamily,
    pub source_path: PathBuf,
    pub raw_line: String,
    pub timestamp: Option<String>,
    pub level: Option<String>,
    pub event: Option<String>,
    pub module: Option<String>,
    pub message: Option<String>,
    pub operation_id: Option<String>,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub job_id: Option<String>,
    pub details: Option<Value>,
    pub error: Option<Value>,
    pub duration_ms: Option<u64>,
    pub raw_json: Option<Value>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AiRunIdKind {
    Task,
    Session,
    Job,
    Operation,
    File,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompactedText {
    pub chars: usize,
    pub sha1: String,
    pub preview: String,
}

#[derive(Clone, Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RequestStats {
    pub count: usize,
    pub max_payload_chars: usize,
    pub avg_payload_chars: usize,
    pub max_message_chars: usize,
    pub max_content_chars: usize,
    pub max_tool_chars: usize,
    pub max_raw_body_chars: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageDigest {
    pub stage: String,
    pub event_count: usize,
    pub error_count: usize,
    pub total_duration_ms: u64,
    pub request_count: usize,
    pub response_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDigest {
    pub signature: String,
    pub count: usize,
    pub stage: Option<String>,
    pub module: Option<String>,
    pub event: Option<String>,
    pub message_preview: Option<CompactedText>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiHotspot {
    pub kind: String,
    pub label: String,
    pub stage: Option<String>,
    pub module: Option<String>,
    pub value: Option<u64>,
    pub preview: Option<CompactedText>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSample {
    pub kind: String,
    pub stage: Option<String>,
    pub module: Option<String>,
    pub preview: CompactedText,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawAccessHints {
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub job_id: Option<String>,
    pub operation_id: Option<String>,
    pub source_files: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiRunSummary {
    pub id: String,
    pub id_kind: AiRunIdKind,
    pub family: LogFamily,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub event_count: usize,
    pub error_count: usize,
    pub top_module: Option<String>,
    pub top_events: Vec<String>,
    pub hotspot_summary: Vec<String>,
    pub source_files: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTaskPackage {
    pub id: String,
    pub id_kind: AiRunIdKind,
    pub family: LogFamily,
    pub source_files: Vec<PathBuf>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub event_count: usize,
    pub error_count: usize,
    pub request_count: usize,
    pub response_count: usize,
    pub parse_error_count: usize,
    pub stage_timeline: Vec<StageDigest>,
    pub request_stats: RequestStats,
    pub error_digest: Vec<ErrorDigest>,
    pub hotspots: Vec<AiHotspot>,
    pub samples: Vec<AiSample>,
    pub raw_access_hints: RawAccessHints,
}

#[derive(Clone, Debug)]
pub struct AggregatedRun {
    pub family: LogFamily,
    pub id_kind: AiRunIdKind,
    pub id: String,
    pub records: Vec<ParsedRecord>,
    pub source_files: Vec<PathBuf>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub started_at_display: Option<String>,
    pub finished_at_display: Option<String>,
}

#[derive(Default)]
struct StageStats {
    event_count: usize,
    error_count: usize,
    total_duration_ms: u64,
    request_count: usize,
    response_count: usize,
}

struct RequestObservation {
    stage: Option<String>,
    payload_chars: usize,
    message_chars: usize,
    content_chars: usize,
    tool_chars: usize,
    payload_preview: Option<CompactedText>,
}

struct ResponseObservation {
    stage: Option<String>,
    duration_ms: u64,
    raw_body_chars: usize,
    raw_body_preview: Option<CompactedText>,
}

struct WebSearchObservation {
    stage: Option<String>,
    result_count: u64,
    query_preview: Option<CompactedText>,
}

pub fn resolve_paths(options: &ResolveOptions) -> Result<ResolvedPaths> {
    if let Some(logs_dir) = options.logs_dir.clone() {
        let data_dir = logs_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(default_data_dir);
        return Ok(ResolvedPaths {
            settings_path: options
                .settings_path
                .clone()
                .unwrap_or_else(|| data_dir.join("settings.json")),
            legacy_app_logs_dir: default_legacy_app_log_dir(),
            data_dir,
            logs_dir,
        });
    }

    let explicit_data_dir = options.data_dir.clone();
    let explicit_settings = options.settings_path.clone();
    let env_data_dir = std::env::var_os("AICLEANER_DATA_DIR").map(PathBuf::from);
    let env_settings_path = std::env::var_os("AICLEANER_SETTINGS_PATH").map(PathBuf::from);

    let settings_path = explicit_settings
        .or(env_settings_path)
        .or_else(|| {
            env_data_dir
                .as_ref()
                .map(|dir| dir.join("settings.json"))
                .filter(|candidate| candidate.exists())
        });

    let base_data_dir = explicit_data_dir
        .or_else(|| settings_path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)))
        .or(env_data_dir)
        .unwrap_or_else(default_data_dir);

    let bootstrap_path = base_data_dir.join(STORAGE_LOCATION_FILE);
    let data_dir = resolve_storage_data_dir(&base_data_dir, &bootstrap_path);
    let app_paths = AppPaths::from_data_dir(data_dir.clone());
    Ok(ResolvedPaths {
        settings_path: settings_path.unwrap_or_else(|| app_paths.settings_path.clone()),
        logs_dir: app_paths.logs_dir(),
        data_dir,
        legacy_app_logs_dir: default_legacy_app_log_dir(),
    })
}

pub fn discover_log_files(paths: &ResolvedPaths) -> Result<Vec<LogFileInfo>> {
    let mut files = Vec::new();
    collect_logs_from_dir(&paths.logs_dir, "canonical", &mut files)?;

    let canonical_has_app = files.iter().any(|entry| entry.family == LogFamily::App);
    if !canonical_has_app {
        if let Some(legacy_dir) = &paths.legacy_app_logs_dir {
            collect_logs_from_dir(legacy_dir, "legacy_app_log_dir", &mut files)?;
        }
    }

    files.sort_by(|a, b| b.modified_at.cmp(&a.modified_at).then_with(|| a.path.cmp(&b.path)));
    Ok(files)
}

fn collect_logs_from_dir(dir: &Path, source: &str, files: &mut Vec<LogFileInfo>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let family = classify_log_family(file_name);
        let Some(family) = family else {
            continue;
        };
        let metadata = entry.metadata()?;
        let modified_at = metadata.modified().ok().map(|time| {
            DateTime::<Utc>::from(time)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        });
        files.push(LogFileInfo {
            correlation_id: infer_correlation_id(file_name, &family),
            family,
            path: path.clone(),
            size_bytes: metadata.len(),
            modified_at,
            source: source.to_string(),
        });
    }
    Ok(())
}

fn classify_log_family(file_name: &str) -> Option<LogFamily> {
    if file_name.starts_with("aicleaner-diagnostics-") && file_name.ends_with(".jsonl") {
        return Some(LogFamily::Diagnostics);
    }
    if file_name == "web_search.jsonl" {
        return Some(LogFamily::WebSearch);
    }
    if file_name == format!("{APP_LOG_FILE_STEM}.log")
        || (file_name.starts_with(&format!("{APP_LOG_FILE_STEM}_")) && file_name.ends_with(".log"))
    {
        return Some(LogFamily::App);
    }
    None
}

fn infer_correlation_id(file_name: &str, family: &LogFamily) -> Option<String> {
    match family {
        LogFamily::Diagnostics => file_name
            .strip_prefix("aicleaner-diagnostics-")
            .and_then(|value| value.strip_suffix(".jsonl"))
            .and_then(|value| value.splitn(2, '-').nth(1))
            .map(|value| value.to_string()),
        _ => None,
    }
}

pub fn read_records(files: &[LogFileInfo], filter: &RecordFilter) -> Result<Vec<ParsedRecord>> {
    let (records, _) = read_records_with_options(files, filter, false)?;
    Ok(records)
}

pub fn read_records_lossy(
    files: &[LogFileInfo],
    filter: &RecordFilter,
) -> Result<(Vec<ParsedRecord>, Vec<String>)> {
    read_records_with_options(files, filter, true)
}

fn read_records_with_options(
    files: &[LogFileInfo],
    filter: &RecordFilter,
    allow_parse_errors: bool,
) -> Result<(Vec<ParsedRecord>, Vec<String>)> {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for file in files {
        if let Some(family) = &filter.family {
            if &file.family != family {
                continue;
            }
        }
        let raw = fs::read_to_string(&file.path)
            .with_context(|| format!("failed to read {}", file.path.display()))?;
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parsed = match parse_record(file, line) {
                Ok(parsed) => parsed,
                Err(err) if allow_parse_errors => {
                    errors.push(format!("{}: {}", file.path.display(), err));
                    continue;
                }
                Err(err) => return Err(err),
            };
            if matches_record(&parsed, filter) {
                out.push(parsed);
            }
        }
    }
    Ok((out, errors))
}

fn parse_record(file: &LogFileInfo, line: &str) -> Result<ParsedRecord> {
    match file.family {
        LogFamily::Diagnostics | LogFamily::WebSearch => {
            let json_value: Value =
                serde_json::from_str(line).with_context(|| "failed to parse json log line")?;
            Ok(ParsedRecord {
                family: file.family.clone(),
                source_path: file.path.clone(),
                raw_line: line.to_string(),
                timestamp: json_value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                level: json_value
                    .get("level")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                event: json_value
                    .get("event")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                module: json_value
                    .get("module")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                message: json_value
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                operation_id: json_string_field(&json_value, "operationId"),
                task_id: json_string_field(&json_value, "taskId"),
                session_id: json_string_field(&json_value, "sessionId"),
                job_id: json_string_field(&json_value, "jobId"),
                details: json_value.get("details").cloned(),
                error: json_value.get("error").cloned().filter(|value| !value.is_null()),
                duration_ms: json_value.get("durationMs").and_then(Value::as_u64),
                raw_json: Some(json_value),
            })
        }
        LogFamily::App => parse_app_log_line(file, line),
    }
}

fn parse_app_log_line(file: &LogFileInfo, line: &str) -> Result<ParsedRecord> {
    let (timestamp, level, module, message) = if let Some(rest) = line.strip_prefix('[') {
        let parts = rest.splitn(4, ']').collect::<Vec<_>>();
        if parts.len() == 4 {
            let timestamp = parts[0].to_string();
            let level = parts[1].trim_start_matches('[').to_ascii_lowercase();
            let module = parts[2].trim_start_matches('[').to_string();
            let message = parts[3].trim_start().to_string();
            (Some(timestamp), Some(level), Some(module), Some(message))
        } else {
            (None, None, None, Some(line.to_string()))
        }
    } else {
        (None, None, None, Some(line.to_string()))
    };

    Ok(ParsedRecord {
        family: file.family.clone(),
        source_path: file.path.clone(),
        raw_line: line.to_string(),
        timestamp,
        level,
        event: None,
        module,
        message,
        operation_id: None,
        task_id: None,
        session_id: None,
        job_id: None,
        details: None,
        error: None,
        duration_ms: None,
        raw_json: None,
    })
}

fn json_string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn matches_record(record: &ParsedRecord, filter: &RecordFilter) -> bool {
    if let Some(level) = &filter.level {
        if record.level.as_deref() != Some(level.as_str()) {
            return false;
        }
    }
    if let Some(event) = &filter.event {
        if record.event.as_deref() != Some(event.as_str()) {
            return false;
        }
    }
    if let Some(task_id) = &filter.task_id {
        if record.task_id.as_deref() != Some(task_id.as_str()) {
            return false;
        }
    }
    if let Some(session_id) = &filter.session_id {
        if record.session_id.as_deref() != Some(session_id.as_str()) {
            return false;
        }
    }
    if let Some(job_id) = &filter.job_id {
        if record.job_id.as_deref() != Some(job_id.as_str()) {
            return false;
        }
    }
    if let Some(operation_id) = &filter.operation_id {
        if record.operation_id.as_deref() != Some(operation_id.as_str()) {
            return false;
        }
    }
    if let Some(since) = filter.since {
        let Some(record_ts) = record.timestamp.as_deref().and_then(parse_timestamp) else {
            return false;
        };
        if record_ts < since {
            return false;
        }
    }
    true
}

pub fn aggregate_runs(records: &[ParsedRecord]) -> Vec<AggregatedRun> {
    let mut grouped = HashMap::<String, AggregatedRunBuilder>::new();

    for record in records {
        let (id_kind, id) = run_identity(record);
        let key = format!("{:?}|{:?}|{}", record.family, id_kind, id);
        grouped
            .entry(key)
            .or_insert_with(|| AggregatedRunBuilder::new(record.family.clone(), id_kind, id.clone()))
            .push(record.clone());
    }

    let mut runs = grouped
        .into_values()
        .map(AggregatedRunBuilder::build)
        .collect::<Vec<_>>();
    runs.sort_by(compare_runs_desc);
    runs
}

pub fn summarize_runs(runs: &[AggregatedRun], hotspot_limit: usize) -> Vec<AiRunSummary> {
    runs.iter()
        .map(|run| {
            let package = build_ai_task_package(run, 0, hotspot_limit.max(1));
            AiRunSummary {
                id: run.id.clone(),
                id_kind: run.id_kind,
                family: run.family.clone(),
                started_at: run.started_at_display.clone(),
                finished_at: run.finished_at_display.clone(),
                duration_ms: run_duration_ms(run),
                event_count: run.records.len(),
                error_count: package.error_count,
                top_module: top_label(&module_counts(&run.records)).map(|(label, _)| label),
                top_events: top_label_strings(event_counts(&run.records), hotspot_limit.max(1)),
                hotspot_summary: package
                    .hotspots
                    .iter()
                    .take(hotspot_limit.max(1))
                    .map(|item| item.label.clone())
                    .collect(),
                source_files: run.source_files.clone(),
            }
        })
        .collect()
}

pub fn newest_run<'a>(runs: &'a [AggregatedRun]) -> Option<&'a AggregatedRun> {
    runs.first()
}

pub fn build_ai_task_package(
    run: &AggregatedRun,
    parse_error_count: usize,
    limit: usize,
) -> AiTaskPackage {
    let mut stage_stats = BTreeMap::<String, StageStats>::new();
    let mut request_observations = Vec::new();
    let mut response_observations = Vec::new();
    let mut web_search_observations = Vec::new();
    let mut error_entries = HashMap::<String, ErrorDigestAccumulator>::new();
    let mut module_errors = HashMap::<String, u64>::new();
    let mut stage_errors = HashMap::<String, u64>::new();

    let mut request_count = 0;
    let mut response_count = 0;
    let mut error_count = 0;

    for record in &run.records {
        let stage = extract_stage(record);
        if let Some(stage_name) = &stage {
            let entry = stage_stats.entry(stage_name.clone()).or_default();
            entry.event_count += 1;
            entry.total_duration_ms = entry
                .total_duration_ms
                .saturating_add(record.duration_ms.unwrap_or(0));
            if is_request_record(record) {
                entry.request_count += 1;
            }
            if is_response_record(record) {
                entry.response_count += 1;
            }
            if is_error_record(record) {
                entry.error_count += 1;
            }
        }

        if is_request_record(record) {
            request_count += 1;
            request_observations.push(RequestObservation {
                stage: stage.clone(),
                payload_chars: detail_json_chars(record, "payload"),
                message_chars: detail_json_chars(record, "messages"),
                content_chars: detail_message_content_chars(record, "messages"),
                tool_chars: detail_json_chars(record, "tools"),
                payload_preview: detail_compacted_preview(record, "payload"),
            });
        }

        if is_response_record(record) {
            response_count += 1;
            response_observations.push(ResponseObservation {
                stage: stage.clone(),
                duration_ms: record.duration_ms.unwrap_or(0),
                raw_body_chars: detail_text_chars(record, "rawBody"),
                raw_body_preview: detail_compacted_text(record, "rawBody"),
            });
        }

        if run.family == LogFamily::WebSearch {
            let result_count = record
                .details
                .as_ref()
                .and_then(|details| details.get("resultCount"))
                .and_then(value_to_u64)
                .or_else(|| {
                    record
                        .details
                        .as_ref()
                        .and_then(|details| details.get("results"))
                        .and_then(Value::as_array)
                        .map(|items| items.len() as u64)
                })
                .unwrap_or(0);
            let query_preview = record
                .details
                .as_ref()
                .and_then(|details| details.get("query"))
                .and_then(Value::as_str)
                .map(compact_text);
            if query_preview.is_some() || result_count > 0 {
                web_search_observations.push(WebSearchObservation {
                    stage: stage.clone(),
                    result_count,
                    query_preview,
                });
            }
        }

        if is_error_record(record) {
            error_count += 1;
            if let Some(module) = &record.module {
                *module_errors.entry(module.clone()).or_insert(0) += 1;
            }
            if let Some(stage_name) = &stage {
                *stage_errors.entry(stage_name.clone()).or_insert(0) += 1;
            }

            let signature = error_signature(record, stage.as_deref());
            let digest = error_entries
                .entry(signature.clone())
                .or_insert_with(|| ErrorDigestAccumulator::new(signature, record, stage.clone()));
            digest.count += 1;
        }
    }

    let stage_timeline = stage_stats
        .into_iter()
        .map(|(stage, stats)| StageDigest {
            stage,
            event_count: stats.event_count,
            error_count: stats.error_count,
            total_duration_ms: stats.total_duration_ms,
            request_count: stats.request_count,
            response_count: stats.response_count,
        })
        .collect::<Vec<_>>();

    let request_stats = summarize_request_stats(&request_observations, &response_observations);
    let mut error_digest = error_entries
        .into_values()
        .map(ErrorDigestAccumulator::finish)
        .collect::<Vec<_>>();
    error_digest.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.signature.cmp(&b.signature)));
    error_digest.truncate(limit.max(1));

    let hotspots = collect_hotspots(
        run,
        &request_observations,
        &response_observations,
        &web_search_observations,
        &stage_errors,
        &module_errors,
        limit.max(1),
    );
    let samples = collect_samples(&request_observations, &response_observations, &error_digest);

    AiTaskPackage {
        id: run.id.clone(),
        id_kind: run.id_kind,
        family: run.family.clone(),
        source_files: run.source_files.clone(),
        started_at: run.started_at_display.clone(),
        finished_at: run.finished_at_display.clone(),
        duration_ms: run_duration_ms(run),
        event_count: run.records.len(),
        error_count,
        request_count,
        response_count,
        parse_error_count,
        stage_timeline,
        request_stats,
        error_digest,
        hotspots,
        samples,
        raw_access_hints: RawAccessHints {
            task_id: first_non_empty(run.records.iter().filter_map(|record| record.task_id.clone())),
            session_id: first_non_empty(
                run.records.iter().filter_map(|record| record.session_id.clone()),
            ),
            job_id: first_non_empty(run.records.iter().filter_map(|record| record.job_id.clone())),
            operation_id: first_non_empty(
                run.records.iter().filter_map(|record| record.operation_id.clone()),
            ),
            source_files: run.source_files.clone(),
        },
    }
}

pub fn serialize_ai_package_jsonl(package: &AiTaskPackage) -> Result<String> {
    let mut lines = Vec::new();
    lines.push(serde_json::to_string(&json!({
        "section": "run",
        "data": {
            "id": package.id,
            "idKind": package.id_kind,
            "family": package.family,
            "startedAt": package.started_at,
            "finishedAt": package.finished_at,
            "durationMs": package.duration_ms,
            "eventCount": package.event_count,
            "errorCount": package.error_count,
            "requestCount": package.request_count,
            "responseCount": package.response_count,
            "parseErrorCount": package.parse_error_count,
            "sourceFiles": package.source_files,
        }
    }))?);
    lines.push(serde_json::to_string(&json!({
        "section": "requestStats",
        "data": package.request_stats,
    }))?);
    lines.push(serde_json::to_string(&json!({
        "section": "rawAccessHints",
        "data": package.raw_access_hints,
    }))?);

    for stage in &package.stage_timeline {
        lines.push(serde_json::to_string(&json!({
            "section": "stage",
            "data": stage,
        }))?);
    }
    for error in &package.error_digest {
        lines.push(serde_json::to_string(&json!({
            "section": "error",
            "data": error,
        }))?);
    }
    for hotspot in &package.hotspots {
        lines.push(serde_json::to_string(&json!({
            "section": "hotspot",
            "data": hotspot,
        }))?);
    }
    for sample in &package.samples {
        lines.push(serde_json::to_string(&json!({
            "section": "sample",
            "data": sample,
        }))?);
    }

    Ok(lines.join("\n"))
}

fn summarize_request_stats(
    requests: &[RequestObservation],
    responses: &[ResponseObservation],
) -> RequestStats {
    let count = requests.len();
    let total_payload_chars = requests.iter().map(|item| item.payload_chars).sum::<usize>();
    RequestStats {
        count,
        max_payload_chars: requests
            .iter()
            .map(|item| item.payload_chars)
            .max()
            .unwrap_or(0),
        avg_payload_chars: if count == 0 {
            0
        } else {
            total_payload_chars / count
        },
        max_message_chars: requests
            .iter()
            .map(|item| item.message_chars)
            .max()
            .unwrap_or(0),
        max_content_chars: requests
            .iter()
            .map(|item| item.content_chars)
            .max()
            .unwrap_or(0),
        max_tool_chars: requests.iter().map(|item| item.tool_chars).max().unwrap_or(0),
        max_raw_body_chars: responses
            .iter()
            .map(|item| item.raw_body_chars)
            .max()
            .unwrap_or(0),
    }
}

fn collect_hotspots(
    run: &AggregatedRun,
    requests: &[RequestObservation],
    responses: &[ResponseObservation],
    web_search: &[WebSearchObservation],
    stage_errors: &HashMap<String, u64>,
    module_errors: &HashMap<String, u64>,
    limit: usize,
) -> Vec<AiHotspot> {
    let mut hotspots = Vec::new();

    if let Some(request) = requests
        .iter()
        .max_by_key(|item| (item.payload_chars, item.message_chars))
    {
        hotspots.push(AiHotspot {
            kind: "largest_request".to_string(),
            label: format!(
                "largest request {} chars{}",
                request.payload_chars,
                request
                    .stage
                    .as_deref()
                    .map(|stage| format!(" in {}", stage))
                    .unwrap_or_default()
            ),
            stage: request.stage.clone(),
            module: None,
            value: Some(request.payload_chars as u64),
            preview: request.payload_preview.clone(),
        });
    }

    if let Some(response) = responses.iter().max_by_key(|item| item.duration_ms) {
        hotspots.push(AiHotspot {
            kind: "slowest_response".to_string(),
            label: format!(
                "slowest response {} ms{}",
                response.duration_ms,
                response
                    .stage
                    .as_deref()
                    .map(|stage| format!(" in {}", stage))
                    .unwrap_or_default()
            ),
            stage: response.stage.clone(),
            module: None,
            value: Some(response.duration_ms),
            preview: response.raw_body_preview.clone(),
        });
    }

    if let Some((stage, count)) = top_label(stage_errors) {
        hotspots.push(AiHotspot {
            kind: "highest_error_stage".to_string(),
            label: format!("highest error stage {} ({})", stage, count),
            stage: Some(stage),
            module: None,
            value: Some(count),
            preview: None,
        });
    }

    if let Some((module, count)) = top_label(module_errors) {
        hotspots.push(AiHotspot {
            kind: "highest_error_module".to_string(),
            label: format!("highest error module {} ({})", module, count),
            stage: None,
            module: Some(module),
            value: Some(count),
            preview: None,
        });
    }

    if run.family == LogFamily::WebSearch {
        if let Some(search) = web_search.iter().max_by_key(|item| item.result_count) {
            hotspots.push(AiHotspot {
                kind: "largest_web_search".to_string(),
                label: format!(
                    "largest web search {} results{}",
                    search.result_count,
                    search
                        .stage
                        .as_deref()
                        .map(|stage| format!(" in {}", stage))
                        .unwrap_or_default()
                ),
                stage: search.stage.clone(),
                module: None,
                value: Some(search.result_count),
                preview: search.query_preview.clone(),
            });
        }
    }

    hotspots.truncate(limit);
    hotspots
}

fn collect_samples(
    requests: &[RequestObservation],
    responses: &[ResponseObservation],
    errors: &[ErrorDigest],
) -> Vec<AiSample> {
    let mut samples = Vec::new();

    if let Some(request) = requests.iter().max_by_key(|item| item.payload_chars) {
        if let Some(preview) = &request.payload_preview {
            samples.push(AiSample {
                kind: "largest_request_payload".to_string(),
                stage: request.stage.clone(),
                module: None,
                preview: preview.clone(),
            });
        }
    }

    if let Some(response) = responses.iter().max_by_key(|item| item.duration_ms) {
        if let Some(preview) = &response.raw_body_preview {
            samples.push(AiSample {
                kind: "slowest_response_body".to_string(),
                stage: response.stage.clone(),
                module: None,
                preview: preview.clone(),
            });
        }
    }

    if let Some(error) = errors.first() {
        if let Some(preview) = &error.message_preview {
            samples.push(AiSample {
                kind: "representative_error".to_string(),
                stage: error.stage.clone(),
                module: error.module.clone(),
                preview: preview.clone(),
            });
        }
    }

    samples
}

fn run_identity(record: &ParsedRecord) -> (AiRunIdKind, String) {
    if let Some(value) = &record.task_id {
        return (AiRunIdKind::Task, value.clone());
    }
    if let Some(value) = &record.session_id {
        return (AiRunIdKind::Session, value.clone());
    }
    if let Some(value) = &record.job_id {
        return (AiRunIdKind::Job, value.clone());
    }
    if let Some(value) = &record.operation_id {
        return (AiRunIdKind::Operation, value.clone());
    }
    (AiRunIdKind::File, record.source_path.to_string_lossy().to_string())
}

#[derive(Default)]
struct AggregatedRunBuilder {
    family: Option<LogFamily>,
    id_kind: Option<AiRunIdKind>,
    id: Option<String>,
    records: Vec<ParsedRecord>,
    source_files: BTreeSet<PathBuf>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    started_at_display: Option<String>,
    finished_at_display: Option<String>,
}

impl AggregatedRunBuilder {
    fn new(family: LogFamily, id_kind: AiRunIdKind, id: String) -> Self {
        Self {
            family: Some(family),
            id_kind: Some(id_kind),
            id: Some(id),
            ..Self::default()
        }
    }

    fn push(&mut self, record: ParsedRecord) {
        self.source_files.insert(record.source_path.clone());
        let parsed = record.timestamp.as_deref().and_then(parse_timestamp);
        let display = record.timestamp.clone();
        if let Some(ts) = parsed {
            if self.started_at.map(|current| ts < current).unwrap_or(true) {
                self.started_at = Some(ts);
                self.started_at_display = display.clone();
            }
            if self.finished_at.map(|current| ts > current).unwrap_or(true) {
                self.finished_at = Some(ts);
                self.finished_at_display = display.clone();
            }
        } else {
            if self.started_at_display.is_none() {
                self.started_at_display = display.clone();
            }
            if display.is_some() {
                self.finished_at_display = display.clone();
            }
        }
        self.records.push(record);
    }

    fn build(self) -> AggregatedRun {
        AggregatedRun {
            family: self.family.expect("family"),
            id_kind: self.id_kind.expect("id kind"),
            id: self.id.expect("id"),
            records: self.records,
            source_files: self.source_files.into_iter().collect(),
            started_at: self.started_at,
            finished_at: self.finished_at,
            started_at_display: self.started_at_display,
            finished_at_display: self.finished_at_display,
        }
    }
}

fn compare_runs_desc(a: &AggregatedRun, b: &AggregatedRun) -> std::cmp::Ordering {
    let a_ts = a.finished_at.or(a.started_at);
    let b_ts = b.finished_at.or(b.started_at);
    b_ts.cmp(&a_ts).then_with(|| a.id.cmp(&b.id))
}

fn run_duration_ms(run: &AggregatedRun) -> Option<u64> {
    match (run.started_at, run.finished_at) {
        (Some(started), Some(finished)) if finished >= started => {
            Some((finished - started).num_milliseconds() as u64)
        }
        _ => None,
    }
}

fn extract_stage(record: &ParsedRecord) -> Option<String> {
    record
        .details
        .as_ref()
        .and_then(|details| details.get("stage"))
        .and_then(value_to_string)
}

fn is_request_record(record: &ParsedRecord) -> bool {
    record
        .event
        .as_deref()
        .is_some_and(|event| event.ends_with("_model_request"))
}

fn is_response_record(record: &ParsedRecord) -> bool {
    record.event.as_deref().is_some_and(|event| {
        event.ends_with("_model_response") || event.ends_with("_model_error")
    })
}

fn is_error_record(record: &ParsedRecord) -> bool {
    record.level.as_deref() == Some("error")
        || record
            .event
            .as_deref()
            .is_some_and(|event| event.contains("error"))
}

fn detail_json_chars(record: &ParsedRecord, key: &str) -> usize {
    record
        .details
        .as_ref()
        .and_then(|details| details.get(key))
        .map(measure_json_chars)
        .unwrap_or(0)
}

fn detail_message_content_chars(record: &ParsedRecord, key: &str) -> usize {
    record
        .details
        .as_ref()
        .and_then(|details| details.get(key))
        .map(measure_message_content_chars)
        .unwrap_or(0)
}

fn detail_text_chars(record: &ParsedRecord, key: &str) -> usize {
    record
        .details
        .as_ref()
        .and_then(|details| details.get(key))
        .and_then(Value::as_str)
        .map(|text| text.chars().count())
        .unwrap_or(0)
}

fn detail_compacted_preview(record: &ParsedRecord, key: &str) -> Option<CompactedText> {
    record
        .details
        .as_ref()
        .and_then(|details| details.get(key))
        .map(compact_json_value)
}

fn detail_compacted_text(record: &ParsedRecord, key: &str) -> Option<CompactedText> {
    record
        .details
        .as_ref()
        .and_then(|details| details.get(key))
        .and_then(Value::as_str)
        .map(compact_text)
}

fn module_counts(records: &[ParsedRecord]) -> HashMap<String, u64> {
    let mut counts = HashMap::new();
    for record in records {
        if let Some(module) = &record.module {
            *counts.entry(module.clone()).or_insert(0) += 1;
        }
    }
    counts
}

fn event_counts(records: &[ParsedRecord]) -> HashMap<String, u64> {
    let mut counts = HashMap::new();
    for record in records {
        if let Some(event) = &record.event {
            *counts.entry(event.clone()).or_insert(0) += 1;
        }
    }
    counts
}

fn top_label(map: &HashMap<String, u64>) -> Option<(String, u64)> {
    map.iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(label, value)| (label.clone(), *value))
}

fn top_label_strings(map: HashMap<String, u64>, limit: usize) -> Vec<String> {
    let mut items = map.into_iter().collect::<Vec<_>>();
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    items
        .into_iter()
        .take(limit)
        .map(|(label, count)| format!("{label} x{count}"))
        .collect()
}

fn first_non_empty<I>(mut values: I) -> Option<String>
where
    I: Iterator<Item = String>,
{
    values.find(|value| !value.trim().is_empty())
}

fn compact_json_value(value: &Value) -> CompactedText {
    compact_text(&serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()))
}

fn compact_text(value: &str) -> CompactedText {
    let chars = value.chars().count();
    CompactedText {
        chars,
        sha1: sha1_hex(value),
        preview: truncate_text(value, PREVIEW_CHAR_LIMIT),
    }
}

fn truncate_text(value: &str, limit: usize) -> String {
    let mut preview = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index >= limit {
            preview.push_str("...");
            break;
        }
        preview.push(ch);
    }
    preview
}

fn sha1_hex(value: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn error_signature(record: &ParsedRecord, stage: Option<&str>) -> String {
    let normalized = normalize_text(
        &extract_error_message(record)
            .or_else(|| record.message.clone())
            .unwrap_or_default(),
    );
    sha1_hex(&format!(
        "{}|{}|{}|{}",
        record.event.as_deref().unwrap_or(""),
        stage.unwrap_or(""),
        record.module.as_deref().unwrap_or(""),
        normalized
    ))
}

fn extract_error_message(record: &ParsedRecord) -> Option<String> {
    record
        .error
        .as_ref()
        .and_then(|error| error.get("message"))
        .and_then(value_to_string)
        .or_else(|| record.error.as_ref().map(|value| value.to_string()))
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(boolean) => Some(boolean.to_string()),
        _ => None,
    }
}

fn value_to_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
}

struct ErrorDigestAccumulator {
    signature: String,
    count: usize,
    stage: Option<String>,
    module: Option<String>,
    event: Option<String>,
    message_preview: Option<CompactedText>,
}

impl ErrorDigestAccumulator {
    fn new(signature: String, record: &ParsedRecord, stage: Option<String>) -> Self {
        Self {
            signature,
            count: 0,
            stage,
            module: record.module.clone(),
            event: record.event.clone(),
            message_preview: extract_error_message(record)
                .or_else(|| record.message.clone())
                .map(|text| compact_text(&text)),
        }
    }

    fn finish(self) -> ErrorDigest {
        ErrorDigest {
            signature: self.signature,
            count: self.count,
            stage: self.stage,
            module: self.module,
            event: self.event,
            message_preview: self.message_preview,
        }
    }
}

fn measure_json_chars(value: &Value) -> usize {
    serde_json::to_string(value)
        .map(|text| text.chars().count())
        .unwrap_or(0)
}

fn measure_message_content_chars(value: &Value) -> usize {
    value
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .map(|message| match message.get("content") {
                    Some(Value::String(text)) => text.chars().count(),
                    Some(other) => measure_json_chars(other),
                    None => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|_| {
            DateTime::<FixedOffset>::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f %z")
                .map(|dt| dt.with_timezone(&Utc))
        })
        .or_else(|_| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map(|date| DateTime::<Utc>::from_naive_utc_and_offset(date.and_hms_opt(0, 0, 0).expect("midnight"), Utc))
        })
        .ok()
}

pub fn find_file_by_path<'a>(files: &'a [LogFileInfo], path: &Path) -> Option<&'a LogFileInfo> {
    files.iter().find(|entry| entry.path == path)
}

pub fn parse_family(value: &str) -> Result<LogFamily> {
    match value {
        "diagnostics" => Ok(LogFamily::Diagnostics),
        "web_search" => Ok(LogFamily::WebSearch),
        "app" => Ok(LogFamily::App),
        other => Err(anyhow!("unknown log family '{}'", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use uuid::Uuid;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("aicleaner-logs-test-{}", Uuid::new_v4()))
    }

    fn diagnostics_record(
        task_id: Option<&str>,
        session_id: Option<&str>,
        job_id: Option<&str>,
        operation_id: Option<&str>,
        timestamp: &str,
        event: &str,
        details: Value,
    ) -> ParsedRecord {
        ParsedRecord {
            family: LogFamily::Diagnostics,
            source_path: PathBuf::from("diag.jsonl"),
            raw_line: String::new(),
            timestamp: Some(timestamp.to_string()),
            level: Some(if event.contains("error") {
                "error".to_string()
            } else {
                "info".to_string()
            }),
            event: Some(event.to_string()),
            module: Some("organizer".to_string()),
            message: Some(event.to_string()),
            operation_id: operation_id.map(str::to_string),
            task_id: task_id.map(str::to_string),
            session_id: session_id.map(str::to_string),
            job_id: job_id.map(str::to_string),
            details: Some(details),
            error: None,
            duration_ms: Some(25),
            raw_json: None,
        }
    }

    #[test]
    fn resolve_paths_prefers_explicit_logs_dir() {
        let dir = temp_dir();
        let logs = dir.join("logs");
        let resolved = resolve_paths(&ResolveOptions {
            logs_dir: Some(logs.clone()),
            ..ResolveOptions::default()
        })
        .expect("resolve paths");
        assert_eq!(resolved.logs_dir, logs);
        assert_eq!(resolved.data_dir, dir);
    }

    #[test]
    fn discover_and_parse_diagnostics_records() {
        let dir = temp_dir();
        let logs = dir.join("logs");
        fs::create_dir_all(&logs).expect("create logs");
        let path = logs.join("aicleaner-diagnostics-20260506-000000-000Z-task_1.jsonl");
        fs::write(
            &path,
            "{\"timestamp\":\"2026-05-06T00:00:00.000Z\",\"level\":\"info\",\"event\":\"organizer_model_request\",\"module\":\"organizer\",\"taskId\":\"task_1\",\"details\":{\"stage\":\"classify\"}}\n",
        )
        .expect("write log");

        let files = discover_log_files(&ResolvedPaths {
            data_dir: dir.clone(),
            settings_path: dir.join("settings.json"),
            logs_dir: logs,
            legacy_app_logs_dir: None,
        })
        .expect("discover");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].family, LogFamily::Diagnostics);

        let records = read_records(&files, &RecordFilter::default()).expect("read");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].task_id.as_deref(), Some("task_1"));
    }

    #[test]
    fn parse_app_log_line_extracts_metadata() {
        let file = LogFileInfo {
            path: PathBuf::from("app.log"),
            family: LogFamily::App,
            size_bytes: 1,
            modified_at: None,
            correlation_id: None,
            source: "canonical".to_string(),
        };
        let record = parse_record(
            &file,
            "[2026-05-06][INFO][backend] organize_apply starting move phase",
        )
        .expect("parse");
        assert_eq!(record.level.as_deref(), Some("info"));
        assert_eq!(record.module.as_deref(), Some("backend"));
    }

    #[test]
    fn filter_by_task_id_matches() {
        let dir = temp_dir();
        let logs = dir.join("logs");
        fs::create_dir_all(&logs).expect("create logs");
        let path = logs.join("aicleaner-diagnostics-20260506-000000-000Z-task_1.jsonl");
        let mut file = fs::File::create(&path).expect("create log");
        writeln!(
            file,
            "{{\"timestamp\":\"2026-05-06T00:00:00.000Z\",\"level\":\"info\",\"event\":\"organizer_model_request\",\"module\":\"organizer\",\"taskId\":\"task_1\"}}"
        )
        .expect("write");

        let files = discover_log_files(&ResolvedPaths {
            data_dir: dir.clone(),
            settings_path: dir.join("settings.json"),
            logs_dir: logs,
            legacy_app_logs_dir: None,
        })
        .expect("discover");
        let records = read_records(
            &files,
            &RecordFilter {
                task_id: Some("task_1".to_string()),
                ..RecordFilter::default()
            },
        )
        .expect("read");
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn aggregate_runs_prefers_task_then_fallbacks() {
        let runs = aggregate_runs(&[
            diagnostics_record(
                Some("task_1"),
                Some("session_1"),
                None,
                None,
                "2026-05-06T00:00:00.000Z",
                "organizer_model_request",
                json!({ "stage": "classification" }),
            ),
            diagnostics_record(
                None,
                Some("session_2"),
                None,
                None,
                "2026-05-06T00:01:00.000Z",
                "advisor_model_request",
                json!({ "stage": "answer" }),
            ),
            diagnostics_record(
                None,
                None,
                Some("job_3"),
                None,
                "2026-05-06T00:02:00.000Z",
                "organizer_model_request",
                json!({ "stage": "classification" }),
            ),
            diagnostics_record(
                None,
                None,
                None,
                Some("op_4"),
                "2026-05-06T00:03:00.000Z",
                "organizer_model_request",
                json!({ "stage": "classification" }),
            ),
        ]);
        assert_eq!(runs.len(), 4);
        assert_eq!(runs[0].id_kind, AiRunIdKind::Operation);
        assert_eq!(runs[1].id_kind, AiRunIdKind::Job);
        assert_eq!(runs[2].id_kind, AiRunIdKind::Session);
        assert_eq!(runs[3].id_kind, AiRunIdKind::Task);
    }

    #[test]
    fn newest_run_selects_latest_matching_run() {
        let runs = aggregate_runs(&[
            diagnostics_record(
                Some("task_old"),
                None,
                None,
                None,
                "2026-05-06T00:00:00.000Z",
                "organizer_model_request",
                json!({ "stage": "classification" }),
            ),
            diagnostics_record(
                Some("task_new"),
                None,
                None,
                None,
                "2026-05-06T00:05:00.000Z",
                "organizer_model_request",
                json!({ "stage": "classification" }),
            ),
        ]);
        assert_eq!(newest_run(&runs).expect("latest").id, "task_new");
    }

    #[test]
    fn ai_package_compacts_large_fields_and_dedupes_errors() {
        let long_payload = "x".repeat(PREVIEW_CHAR_LIMIT + 50);
        let long_body = "y".repeat(PREVIEW_CHAR_LIMIT + 30);
        let records = vec![
            diagnostics_record(
                Some("task_1"),
                None,
                None,
                None,
                "2026-05-06T00:00:00.000Z",
                "organizer_model_request",
                json!({
                    "stage": "classification",
                    "payload": { "blob": long_payload.clone() },
                    "messages": [{ "content": long_payload.clone() }],
                    "tools": [{ "name": "tool" }],
                }),
            ),
            ParsedRecord {
                family: LogFamily::Diagnostics,
                source_path: PathBuf::from("diag.jsonl"),
                raw_line: String::new(),
                timestamp: Some("2026-05-06T00:00:01.000Z".to_string()),
                level: Some("error".to_string()),
                event: Some("organizer_model_error".to_string()),
                module: Some("organizer".to_string()),
                message: Some("organizer model failed".to_string()),
                operation_id: None,
                task_id: Some("task_1".to_string()),
                session_id: None,
                job_id: None,
                details: Some(json!({
                    "stage": "classification",
                    "rawBody": long_body.clone(),
                })),
                error: Some(json!({ "message": "provider timeout" })),
                duration_ms: Some(42),
                raw_json: None,
            },
            ParsedRecord {
                family: LogFamily::Diagnostics,
                source_path: PathBuf::from("diag.jsonl"),
                raw_line: String::new(),
                timestamp: Some("2026-05-06T00:00:02.000Z".to_string()),
                level: Some("error".to_string()),
                event: Some("organizer_model_error".to_string()),
                module: Some("organizer".to_string()),
                message: Some("organizer model failed".to_string()),
                operation_id: None,
                task_id: Some("task_1".to_string()),
                session_id: None,
                job_id: None,
                details: Some(json!({
                    "stage": "classification",
                    "rawBody": long_body.clone(),
                })),
                error: Some(json!({ "message": "provider timeout" })),
                duration_ms: Some(45),
                raw_json: None,
            },
        ];
        let runs = aggregate_runs(&records);
        let package = build_ai_task_package(&runs[0], 2, 5);
        let serialized = serde_json::to_string(&package).expect("serialize");
        assert_eq!(package.error_digest.len(), 1);
        assert_eq!(package.error_digest[0].count, 2);
        assert!(package.samples.iter().any(|sample| sample.preview.preview.ends_with("...")));
        assert!(!serialized.contains(&long_payload));
        assert!(!serialized.contains(&long_body));
    }

    #[test]
    fn web_search_package_includes_query_summary() {
        let records = vec![ParsedRecord {
            family: LogFamily::WebSearch,
            source_path: PathBuf::from("web_search.jsonl"),
            raw_line: String::new(),
            timestamp: Some("2026-05-06T00:00:00.000Z".to_string()),
            level: Some("info".to_string()),
            event: Some("web_search_result".to_string()),
            module: Some("web_search".to_string()),
            message: Some("search completed".to_string()),
            operation_id: Some("op_1".to_string()),
            task_id: None,
            session_id: None,
            job_id: None,
            details: Some(json!({
                "query": "latest organizer latency",
                "resultCount": 8,
                "stage": "research",
            })),
            error: None,
            duration_ms: Some(5),
            raw_json: None,
        }];
        let runs = aggregate_runs(&records);
        let package = build_ai_task_package(&runs[0], 0, 5);
        assert!(package
            .hotspots
            .iter()
            .any(|item| item.kind == "largest_web_search"));
    }

    #[test]
    fn app_family_summary_uses_file_fallback() {
        let record = ParsedRecord {
            family: LogFamily::App,
            source_path: PathBuf::from("app.log"),
            raw_line: String::new(),
            timestamp: Some("2026-05-06".to_string()),
            level: Some("info".to_string()),
            event: None,
            module: Some("backend".to_string()),
            message: Some("boot".to_string()),
            operation_id: None,
            task_id: None,
            session_id: None,
            job_id: None,
            details: None,
            error: None,
            duration_ms: None,
            raw_json: None,
        };
        let runs = aggregate_runs(&[record]);
        let summaries = summarize_runs(&runs, 3);
        assert_eq!(summaries[0].id_kind, AiRunIdKind::File);
        assert_eq!(summaries[0].top_module.as_deref(), Some("backend"));
    }

    #[test]
    fn serialize_ai_package_jsonl_emits_section_lines() {
        let runs = aggregate_runs(&[diagnostics_record(
            Some("task_1"),
            None,
            None,
            None,
            "2026-05-06T00:00:00.000Z",
            "organizer_model_request",
            json!({
                "stage": "classification",
                "payload": { "a": 1 },
                "messages": [],
                "tools": []
            }),
        )]);
        let package = build_ai_task_package(&runs[0], 0, 5);
        let jsonl = serialize_ai_package_jsonl(&package).expect("jsonl");
        assert!(jsonl.contains("\"section\":\"run\""));
        assert!(jsonl.contains("\"section\":\"requestStats\""));
    }
}
