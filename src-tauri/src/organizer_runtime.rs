mod planner;
mod summary;

use crate::backend::{
    AppState, OrganizeProgress, OrganizeSnapshot, OrganizeStartInput, TokenUsage,
};
use crate::file_representation::FileRepresentation;
use crate::llm_protocol::{
    apply_auth_headers, apply_llm_transport_headers, build_completion_payload,
    build_llm_http_client, build_messages_url, detect_api_format, parse_completion_response,
    ParsedToolCall, DEFAULT_MAX_TOKENS,
};
use crate::model_boundary::ModelIdMap;
use crate::persist;
use parking_lot::Mutex;
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Runtime, State};
use uuid::Uuid;
use walkdir::WalkDir;

const UNCATEGORIZED_NODE_NAME: &str = "\u{5176}\u{4ED6}\u{5F85}\u{5B9A}";
const DEFAULT_BATCH_SIZE: u32 = 20;
const CATEGORY_OTHER_PENDING: &str = "\u{5176}\u{4ED6}\u{5F85}\u{5B9A}";
const CATEGORY_CLASSIFICATION_ERROR: &str = "\u{5206}\u{7C7B}\u{9519}\u{8BEF}";
const RESULT_REASON_CLASSIFICATION_ERROR: &str = "classification_error";
const CHAT_COMPLETION_TIMEOUT_SECS: u64 = 180;
const TIKA_EXTRACT_TIMEOUT_SECS: u64 = 90;
const RESPONSE_ERROR_SNIPPET_CHARS: usize = 400;
const LOCAL_TEXT_EXCERPT_CHARS: usize = 1200;
const LOCAL_SUMMARY_EXCERPT_CHARS: usize = 480;
const SUMMARY_AGENT_SUMMARY_CHARS: usize = 320;
const SUMMARY_PREFETCH_BATCHES: usize = 2;
const CLASSIFICATION_BATCH_CONCURRENCY: usize = 8;
const ORGANIZER_SEARCH_CONCURRENCY: usize = 8;
pub(crate) const ORGANIZER_WEB_SEARCH_BUDGET: usize = 20;
const LOCAL_SUMMARY_MAX_PLAIN_TEXT_BYTES: u64 = 2 * 1024 * 1024;
const TIKA_MAX_UPLOAD_BYTES: u64 = 32 * 1024 * 1024;
const DEFAULT_TIKA_URL: &str = "http://127.0.0.1:9998";

const SUMMARY_MODE_FILENAME_ONLY: &str = "filename_only";
const SUMMARY_MODE_LOCAL_SUMMARY: &str = "local_summary";
const SUMMARY_MODE_AGENT_SUMMARY: &str = "agent_summary";

const SUMMARY_SOURCE_FILENAME_ONLY: &str = "filename_only";
const SUMMARY_SOURCE_LOCAL_SUMMARY: &str = "local_summary";
const SUMMARY_SOURCE_AGENT_SUMMARY: &str = "agent_summary";
const SUMMARY_SOURCE_AGENT_FALLBACK_LOCAL: &str = "agent_fallback_local";

const DEFAULT_EXCLUDED_PATTERNS: [&str; 11] = [
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "dist",
    "build",
    "out",
    "tmp",
    "temp",
    "$recycle.bin",
    "windows",
];

#[derive(Clone)]
struct RouteConfig {
    endpoint: String,
    api_key: String,
    model: String,
}

#[derive(Debug)]
struct ChatCompletionOutput {
    raw_body: String,
    content: String,
    raw_message: Value,
    tool_calls: Vec<ParsedToolCall>,
    usage: TokenUsage,
}

#[derive(Debug)]
struct ChatCompletionError {
    message: String,
    raw_body: String,
}

struct ClassifyOrganizeBatchOutput {
    parsed: Option<Value>,
    usage: TokenUsage,
    raw_output: String,
    error: Option<String>,
    search_calls: usize,
}

struct InitialTreeOutput {
    parsed: Option<Value>,
    usage: TokenUsage,
    raw_output: String,
    error: Option<String>,
}

struct ReconcileOrganizeOutput {
    parsed: Option<Value>,
    usage: TokenUsage,
    raw_output: String,
    error: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct PreparedClassificationBatch {
    batch_idx: usize,
    batch_rows: Vec<Value>,
}

struct SummaryAgentBatchOutput {
    items: HashMap<String, SummaryAgentItem>,
    usage: TokenUsage,
    error: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct SummaryAgentItem {
    summary_short: String,
    summary_long: String,
    keywords: Vec<String>,
    confidence: Option<String>,
    warnings: Vec<String>,
}

fn organize_progress(
    stage: &str,
    label: &str,
    detail: Option<String>,
    current: Option<u64>,
    total: Option<u64>,
    unit: Option<&str>,
    indeterminate: bool,
) -> OrganizeProgress {
    OrganizeProgress {
        stage: stage.to_string(),
        label: label.to_string(),
        detail,
        current,
        total,
        unit: unit.map(str::to_string),
        indeterminate,
    }
}

fn set_organize_progress(
    snapshot: &mut OrganizeSnapshot,
    stage: &str,
    label: &str,
    detail: Option<String>,
    current: Option<u64>,
    total: Option<u64>,
    unit: Option<&str>,
    indeterminate: bool,
) {
    snapshot.progress =
        organize_progress(stage, label, detail, current, total, unit, indeterminate);
}

#[derive(Clone, Debug, Default)]
struct SummaryExtraction {
    parser: String,
    title: Option<String>,
    excerpt: String,
    keywords: Vec<String>,
    metadata_lines: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct SummaryBuildResult {
    representation: FileRepresentation,
    warnings: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct ExtractionToolConfig {
    tika_enabled: bool,
    tika_url: String,
    tika_auto_start: bool,
    tika_jar_path: String,
    tika_ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectoryResultKind {
    Whole,
    WholeWrapperPassthrough,
    MixedSplit,
    StagingJunk,
}

impl DirectoryResultKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Whole => "whole",
            Self::WholeWrapperPassthrough => "whole_wrapper_passthrough",
            Self::MixedSplit => "mixed_split",
            Self::StagingJunk => "staging_junk",
        }
    }
}

#[derive(Clone)]
struct DirectoryAssessment {
    result_kind: DirectoryResultKind,
    integrity_score: u8,
    integrity_kind: String,
    evidence: Vec<String>,
    wrapper_target_path: Option<String>,
    top_level_entries: Vec<String>,
    dominant_extensions: Vec<String>,
    name_families: Vec<String>,
    paired_sidecars: Vec<String>,
    fragmentation_warnings: Vec<String>,
    naming_cohesion: String,
    total_size: u64,
    file_count: u64,
    dir_count: u64,
    direct_file_count: u64,
    direct_dir_count: u64,
    max_depth: u32,
}

#[derive(Clone)]
struct OrganizeUnit {
    name: String,
    path: String,
    relative_path: String,
    size: u64,
    created_at: Option<String>,
    modified_at: Option<String>,
    item_type: String,
    modality: String,
    directory_assessment: Option<DirectoryAssessment>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CategoryTreeNode {
    node_id: String,
    name: String,
    #[serde(default)]
    children: Vec<CategoryTreeNode>,
}

const PROJECT_MARKER_NAMES: [&str; 17] = [
    ".git",
    "package.json",
    "pnpm-workspace.yaml",
    "yarn.lock",
    "pyproject.toml",
    "requirements.txt",
    "cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "dockerfile",
    "docker-compose.yml",
    "docker-compose.yaml",
    ".sln",
];

const DOWNLOAD_ROOT_TOKENS: [&str; 4] = ["download", "downloads", "涓嬭浇", "dwnldata"];
const JUNK_DIR_NAMES: [&str; 8] = [
    "log", "logs", "cache", "caches", "temp", "tmp", "updates", "update",
];
const WRAPPER_FILE_EXTS: [&str; 7] = [".txt", ".md", ".json", ".nfo", ".url", ".sfv", ".crc"];

pub struct OrganizeTaskRuntime {
    pub stop: AtomicBool,
    pub snapshot: Mutex<OrganizeSnapshot>,
    routes: HashMap<String, RouteConfig>,
    search_api_key: String,
    response_language: String,
    extraction_tool: ExtractionToolConfig,
    diagnostics: OrganizerDiagnostics,
    pub job: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone, Debug)]
pub(crate) struct OrganizerDiagnostics {
    pub data_dir: PathBuf,
    pub operation_id: String,
    pub task_id: String,
}

impl OrganizerDiagnostics {
    fn record(
        &self,
        level: &str,
        event: &str,
        message: &str,
        details: Value,
        error: Option<Value>,
        duration: Option<Duration>,
    ) {
        crate::diagnostics::record_event(
            &self.data_dir,
            level,
            "organizer",
            event,
            Some(&self.operation_id),
            message,
            crate::diagnostics::merge_details(details, json!({ "taskId": self.task_id })),
            error,
            duration,
        );
    }

    fn task_started(&self, details: Value) {
        self.record(
            "info",
            "organize_task_started",
            "organize task started",
            details,
            None,
            None,
        );
    }

    fn task_stopped(&self, message: &str, details: Value, duration: Option<Duration>) {
        self.record(
            "info",
            "organize_task_stopped",
            message,
            details,
            None,
            duration,
        );
    }

    fn task_failed(&self, payload: Value) {
        self.record(
            "error",
            "organize_task_failed",
            "organize task failed",
            payload.clone(),
            payload
                .get("message")
                .cloned()
                .map(|message| json!({ "message": message })),
            None,
        );
    }

    fn task_completed(&self, details: Value, duration: Duration) {
        self.record(
            "info",
            "organize_task_completed",
            "organize task completed",
            details,
            None,
            Some(duration),
        );
    }

    fn collection_started(&self, root_path: &str) {
        self.record(
            "info",
            "organize_collection_start",
            "organize directory collection started",
            json!({ "rootPath": root_path }),
            None,
            None,
        );
    }

    fn collection_completed(
        &self,
        root_path: &str,
        unit_count: usize,
        report: &CollectionReport,
        duration: Duration,
    ) {
        self.record(
            "info",
            "organize_collection_done",
            "organize directory collection completed",
            json!({
                "rootPath": root_path,
                "unitCount": unit_count,
                "report": report,
            }),
            None,
            Some(duration),
        );
    }

    fn batch_started(&self, batch_index: usize, batch_size: usize, total_batches: u64) {
        self.record(
            "info",
            "organize_batch_start",
            "organize batch started",
            json!({
                "batchIndex": batch_index,
                "batchSize": batch_size,
                "totalBatches": total_batches,
            }),
            None,
            None,
        );
    }

    fn batch_completed(&self, cluster_failed: bool, details: Value, duration: Duration) {
        self.record(
            if cluster_failed { "warn" } else { "info" },
            "organize_batch_done",
            "organize batch completed",
            details,
            None,
            Some(duration),
        );
    }

    fn stage_completed(&self, stage: &str, details: Value, duration: Duration) {
        self.record(
            "info",
            "organize_stage_done",
            "organize stage completed",
            crate::diagnostics::merge_details(json!({ "stage": stage }), details),
            None,
            Some(duration),
        );
    }

    fn model_request(
        &self,
        stage: &str,
        route: &RouteConfig,
        url: &str,
        messages: &[Value],
        tools: &[Value],
        payload: &Value,
    ) {
        self.record(
            "info",
            "organizer_model_request",
            "organizer model request started",
            json!({
                "stage": stage,
                "endpoint": route.endpoint.clone(),
                "model": route.model.clone(),
                "url": url,
                "messages": messages,
                "tools": tools,
                "payload": payload,
            }),
            None,
            None,
        );
    }

    fn model_error(
        &self,
        stage: &str,
        endpoint: &str,
        model: &str,
        message: &str,
        details: Value,
        duration: Duration,
    ) {
        let (details, error) = match details {
            Value::Object(mut map) => {
                let error = map.remove("error");
                (Value::Object(map), error)
            }
            other => (other, None),
        };
        self.record(
            "error",
            "organizer_model_error",
            message,
            crate::diagnostics::merge_details(
                json!({
                    "stage": stage,
                    "endpoint": endpoint,
                    "model": model,
                }),
                details,
            ),
            error,
            Some(duration),
        );
    }

    fn model_response(
        &self,
        stage: &str,
        endpoint: &str,
        model: &str,
        status: u16,
        raw_body: &str,
        duration: Duration,
    ) {
        self.record(
            if (200..300).contains(&status) {
                "info"
            } else {
                "error"
            },
            "organizer_model_response",
            "organizer model response received",
            json!({
                "stage": stage,
                "endpoint": endpoint,
                "model": model,
                "status": status,
                "rawBody": raw_body,
            }),
            None,
            Some(duration),
        );
    }

    pub(crate) fn web_search_succeeded(
        &self,
        stage: &str,
        trace: &crate::web_search::WebSearchTrace,
    ) {
        self.record(
            "info",
            "organizer_web_search",
            "organizer web search succeeded",
            json!({
                "stage": stage,
                "query": trace.query.clone(),
                "reason": trace.reason.clone(),
                "resultCount": trace.results.len(),
                "results": trace.results.clone(),
            }),
            None,
            None,
        );
    }

    pub(crate) fn web_search_failed(&self, stage: &str, arguments: &Value, message: &str) {
        self.record(
            "error",
            "organizer_web_search",
            "organizer web search failed",
            json!({
                "stage": stage,
                "arguments": arguments,
            }),
            Some(json!({ "message": message })),
            None,
        );
    }
}

include!("organizer_runtime/support.rs");

include!("organizer_runtime/task_runner.rs");

include!("organizer_runtime/commands.rs");

#[cfg(test)]
include!("organizer_runtime/tests.rs");
