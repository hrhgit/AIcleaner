mod planner;
mod summary;

use crate::backend::{AppState, OrganizeSnapshot, OrganizeStartInput, TokenUsage};
use crate::file_representation::FileRepresentation;
use crate::llm_protocol::{
    apply_auth_headers, build_completion_payload, build_messages_url, detect_api_format,
    parse_completion_response, ParsedToolCall, DEFAULT_MAX_TOKENS,
};
use crate::persist;
use crate::web_search::{format_web_search_context, tavily_search, WebSearchRequest};
use parking_lot::Mutex;
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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
const SUMMARY_AGENT_LONG_CHARS: usize = 720;
const ORGANIZER_WEB_SEARCH_BUDGET: usize = 8;
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
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn system_time_to_iso(value: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(value)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn format_relative_age(duration: chrono::Duration) -> String {
    let seconds = duration.num_seconds().max(0);
    if seconds < 60 {
        "lt1m".to_string()
    } else if seconds < 60 * 60 {
        format!("{}m", (seconds / 60).max(1))
    } else if seconds < 60 * 60 * 24 {
        format!("{}h", (seconds / (60 * 60)).max(1))
    } else if seconds < 60 * 60 * 24 * 7 {
        format!("{}d", (seconds / (60 * 60 * 24)).max(1))
    } else if seconds < 60 * 60 * 24 * 30 {
        format!("{}w", (seconds / (60 * 60 * 24 * 7)).max(1))
    } else if seconds < 60 * 60 * 24 * 365 {
        format!("{}mo", (seconds / (60 * 60 * 24 * 30)).max(1))
    } else {
        format!("{}y", (seconds / (60 * 60 * 24 * 365)).max(1))
    }
}

fn compute_relative_age_at(
    value: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    let parsed = value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|value| value.with_timezone(&chrono::Utc))?;
    Some(format_relative_age(now.signed_duration_since(parsed)))
}

fn compute_relative_age(value: Option<&str>) -> Option<String> {
    compute_relative_age_at(value, chrono::Utc::now())
}

fn is_zh_language(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized == "zh" || normalized.starts_with("zh-") || normalized.starts_with("zh_")
}

fn localized_language_name(prompt_language: &str, output_language: &str) -> &'static str {
    if is_zh_language(prompt_language) {
        if is_zh_language(output_language) {
            "简体中文"
        } else {
            "英文"
        }
    } else if is_zh_language(output_language) {
        "Simplified Chinese"
    } else {
        "English"
    }
}

fn organizer_unknown_label(value: &str) -> &'static str {
    if is_zh_language(value) {
        "锛堟湭鐭ワ級"
    } else {
        "(unknown)"
    }
}

fn organizer_none_label(value: &str) -> &'static str {
    if is_zh_language(value) {
        "（无）"
    } else {
        "(none)"
    }
}

fn normalize_summary_mode(value: Option<&str>) -> String {
    match value.unwrap_or("").trim() {
        SUMMARY_MODE_LOCAL_SUMMARY => SUMMARY_MODE_LOCAL_SUMMARY.to_string(),
        SUMMARY_MODE_AGENT_SUMMARY => SUMMARY_MODE_AGENT_SUMMARY.to_string(),
        _ => SUMMARY_MODE_FILENAME_ONLY.to_string(),
    }
}

fn extraction_tool_config_from_settings(settings: &Value) -> ExtractionToolConfig {
    let tika = settings
        .get("contentExtraction")
        .and_then(|value| value.get("tika"));
    let configured_tika_enabled = tika
        .and_then(|value| value.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let configured_tika_auto_start = tika
        .and_then(|value| value.get("autoStart"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tika_jar_path = tika
        .and_then(|value| value.get("jarPath"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let tika_url = tika
        .and_then(|value| value.get("url"))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_TIKA_URL)
        .trim()
        .trim_end_matches('/')
        .to_string();
    let legacy_default_config = !configured_tika_enabled
        && !configured_tika_auto_start
        && tika_url == DEFAULT_TIKA_URL
        && tika_jar_path.is_empty();
    ExtractionToolConfig {
        tika_enabled: (configured_tika_enabled
            || configured_tika_auto_start
            || legacy_default_config)
            && !tika_url.is_empty(),
        tika_url,
        tika_auto_start: configured_tika_auto_start || legacy_default_config,
        tika_jar_path,
        tika_ready: false,
    }
}

fn force_enable_tika_for_summary_mode(config: &mut ExtractionToolConfig) {
    if config.tika_url.trim().is_empty() {
        config.tika_url = DEFAULT_TIKA_URL.to_string();
    }
    config.tika_enabled = true;
    config.tika_auto_start = true;
}

async fn is_tika_server_available(url: &str) -> bool {
    let normalized = url.trim().trim_end_matches('/');
    if normalized.is_empty() {
        return false;
    }
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    match client.get(format!("{normalized}/version")).send().await {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

fn looks_like_tika_server_jar(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|value| {
            let lower = value.to_ascii_lowercase();
            lower.starts_with("tika-server-standard-") && lower.ends_with(".jar")
        })
        .unwrap_or(false)
}

fn find_tika_server_jar_in_dir(dir: &Path) -> Option<PathBuf> {
    let mut candidates = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && looks_like_tika_server_jar(path))
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    candidates.into_iter().next()
}

fn resolve_tika_server_jar(state: &AppState, configured_path: &str) -> Option<PathBuf> {
    let configured = configured_path.trim();
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Some(path);
        }
    }

    if let Ok(value) = std::env::var("TIKA_SERVER_JAR") {
        let path = PathBuf::from(value.trim());
        if path.is_file() {
            return Some(path);
        }
    }

    let mut roots = Vec::<PathBuf>::new();
    if let Ok(dir) = std::env::current_dir() {
        roots.push(dir.clone());
        roots.push(dir.join("bin"));
        roots.push(dir.join("tools"));
        roots.push(dir.join("resources"));
    }
    let data_dir = state.data_dir();
    roots.push(data_dir.clone());
    roots.push(data_dir.join("bin"));
    roots.push(data_dir.join("tools"));
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            roots.push(exe_dir.to_path_buf());
            roots.push(exe_dir.join("bin"));
            roots.push(exe_dir.join("resources"));
            if let Some(parent) = exe_dir.parent() {
                roots.push(parent.to_path_buf());
                roots.push(parent.join("bin"));
                roots.push(parent.join("resources"));
            }
        }
    }

    let mut seen = HashSet::<PathBuf>::new();
    for root in roots {
        if !seen.insert(root.clone()) {
            continue;
        }
        if let Some(found) = find_tika_server_jar_in_dir(&root) {
            return Some(found);
        }
    }
    None
}

fn parse_tika_binding(url: &str) -> Option<(String, u16)> {
    let parsed = Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_string();
    let port = parsed.port_or_known_default()?;
    Some((host, port))
}

fn managed_tika_process_alive(process: &mut crate::backend::ManagedTikaProcess) -> bool {
    match process.child.try_wait() {
        Ok(None) => true,
        Ok(Some(_)) | Err(_) => false,
    }
}

async fn ensure_tika_server_running(state: &AppState, extraction_tool: &mut ExtractionToolConfig) {
    extraction_tool.tika_ready = false;
    if !extraction_tool.tika_enabled {
        return;
    }
    if is_tika_server_available(&extraction_tool.tika_url).await {
        extraction_tool.tika_ready = true;
        return;
    }
    if !extraction_tool.tika_auto_start {
        return;
    }
    if extraction_tool.tika_jar_path.trim().is_empty() {
        let Some(path) = resolve_tika_server_jar(state, &extraction_tool.tika_jar_path) else {
            return;
        };
        extraction_tool.tika_jar_path = path.to_string_lossy().to_string();
    }
    let waiting_for_existing_process = {
        let mut guard = state.tika_process.lock();
        if let Some(process) = guard.as_mut() {
            if process.url == extraction_tool.tika_url && managed_tika_process_alive(process) {
                true
            } else {
                *guard = None;
                false
            }
        } else {
            false
        }
    };
    if waiting_for_existing_process {
        for _ in 0..25 {
            if is_tika_server_available(&extraction_tool.tika_url).await {
                extraction_tool.tika_ready = true;
                return;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        return;
    }

    let mut command = Command::new("java");
    command.arg("-jar").arg(&extraction_tool.tika_jar_path);
    if let Some((host, port)) = parse_tika_binding(&extraction_tool.tika_url) {
        command.arg("--host").arg(host);
        command.arg("--port").arg(port.to_string());
    }
    let Ok(child) = command.stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return;
    };
    {
        let mut guard = state.tika_process.lock();
        *guard = Some(crate::backend::ManagedTikaProcess {
            url: extraction_tool.tika_url.clone(),
            child,
        });
    }
    for _ in 0..30 {
        if is_tika_server_available(&extraction_tool.tika_url).await {
            extraction_tool.tika_ready = true;
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn sanitize_summary_confidence(value: Option<&str>) -> Option<String> {
    match value.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "high" => Some("high".to_string()),
        "medium" => Some("medium".to_string()),
        "low" => Some("low".to_string()),
        _ => None,
    }
}

fn normalize_excluded(patterns: Option<Vec<String>>) -> Vec<String> {
    let mut set = DEFAULT_EXCLUDED_PATTERNS
        .iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>();
    for item in patterns.unwrap_or_default() {
        let trimmed = item.trim().to_lowercase();
        if !trimmed.is_empty() && !set.contains(&trimmed) {
            set.push(trimmed);
        }
    }
    set
}

fn normalize_batch_size(value: Option<u32>) -> u32 {
    value.unwrap_or(DEFAULT_BATCH_SIZE).clamp(1, 200)
}

fn supports_multimodal(model: &str, endpoint: &str) -> bool {
    let value = format!("{}|{}", endpoint.to_lowercase(), model.to_lowercase());
    ["gpt-4o", "gpt-4.1", "gemini", "claude", "glm-4v", "qwen-vl"]
        .iter()
        .any(|x| value.contains(x))
}

fn pick_modality(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if [".mp4", ".mov", ".mkv", ".avi", ".wmv", ".webm"]
        .iter()
        .any(|x| lower.ends_with(x))
    {
        "video"
    } else if [".mp3", ".wav", ".m4a", ".aac", ".flac", ".ogg"]
        .iter()
        .any(|x| lower.ends_with(x))
    {
        "audio"
    } else if [".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp"]
        .iter()
        .any(|x| lower.ends_with(x))
    {
        "image"
    } else {
        "text"
    }
}

fn sanitize_category_name(value: &str) -> String {
    let cleaned = value.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_");
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        CATEGORY_OTHER_PENDING.to_string()
    } else {
        trimmed.to_string()
    }
}

fn parse_routes(model_routing: &Option<Value>) -> HashMap<String, RouteConfig> {
    let mut routes = HashMap::new();
    let source = model_routing
        .as_ref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for modality in ["text", "image", "video", "audio"] {
        let config = source
            .get(modality)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let endpoint = config
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or("https://api.openai.com/v1")
            .trim()
            .to_string();
        let api_key = config
            .get("apiKey")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let model = config
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("gpt-4o-mini")
            .trim()
            .to_string();
        routes.insert(
            modality.to_string(),
            RouteConfig {
                endpoint,
                api_key,
                model,
            },
        );
    }
    routes
}

fn should_exclude(name: &str, patterns: &[String]) -> bool {
    let lower = name.to_lowercase();
    if lower.starts_with('.') {
        return true;
    }
    patterns.iter().any(|pattern| {
        let normalized = pattern.trim().to_lowercase();
        if normalized.is_empty() {
            return false;
        }
        if normalized.contains('*') {
            let needle = normalized.replace('*', "");
            !needle.is_empty() && lower.contains(&needle)
        } else {
            lower == normalized
        }
    })
}

fn extension_key(path: &Path) -> String {
    path.extension()
        .and_then(|x| x.to_str())
        .map(|x| format!(".{}", x.to_ascii_lowercase()))
        .unwrap_or_else(|| "(no_ext)".to_string())
}

fn relative_path_string(scan_root: &Path, path: &Path) -> String {
    path.strip_prefix(scan_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn classify_extension_family(ext: &str) -> &'static str {
    match ext {
        ".exe" | ".msi" | ".app" | ".apk" | ".dll" | ".bin" | ".pak" | ".pck" | ".3dsx"
        | ".firm" => "app",
        ".json" | ".yaml" | ".yml" | ".toml" | ".ini" | ".cfg" | ".conf" | ".config" | ".xml" => {
            "config"
        }
        ".md" | ".txt" | ".pdf" | ".doc" | ".docx" | ".rtf" | ".epub" | ".csv" | ".xlsx"
        | ".xls" | ".bib" => "document",
        ".png" | ".jpg" | ".jpeg" | ".webp" | ".gif" | ".bmp" | ".ani" | ".ico" => "image",
        ".mp4" | ".mov" | ".mkv" | ".avi" | ".wmv" | ".webm" => "video",
        ".mp3" | ".wav" | ".m4a" | ".aac" | ".flac" | ".ogg" => "audio",
        ".zip" | ".rar" | ".7z" | ".tar" | ".gz" | ".bz2" | ".xz" => "archive",
        ".ttf" | ".otf" | ".woff" | ".woff2" => "font",
        ".log" | ".tmp" | ".cache" | ".dat" | ".db" => "runtime",
        ".ps1" | ".bat" | ".cmd" | ".sh" => "script",
        _ => "other",
    }
}

fn normalize_name_family(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or(name)
        .to_ascii_lowercase();
    let mut value = stem.trim().to_string();
    loop {
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() {
            return "(empty)".to_string();
        }
        if let Some(inner) = trimmed.strip_suffix(')') {
            if let Some(pos) = inner.rfind(" (") {
                let suffix = &inner[pos + 2..];
                if !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()) {
                    value = inner[..pos].to_string();
                    continue;
                }
            }
        }
        let bytes = trimmed.as_bytes();
        let mut idx = bytes.len();
        while idx > 0 && bytes[idx - 1].is_ascii_digit() {
            idx -= 1;
        }
        if idx < bytes.len() && idx > 0 {
            let separator = bytes[idx - 1] as char;
            if matches!(separator, '-' | '_' | ' ') {
                value = trimmed[..idx - 1].to_string();
                continue;
            }
        }
        return trimmed
            .trim_matches(|ch: char| matches!(ch, '-' | '_' | ' ' | '.'))
            .to_string();
    }
}

fn strip_bundle_suffix_tokens(mut value: String) -> String {
    const SUFFIXES: [&str; 11] = [
        "x64",
        "x86",
        "arm64",
        "arm32",
        "amd64",
        "win64",
        "win32",
        "64bit",
        "32bit",
        "setup",
        "installer",
    ];
    loop {
        let trimmed = value
            .trim_end_matches(|ch: char| matches!(ch, '-' | '_' | ' ' | '.'))
            .to_string();
        let mut changed = false;
        for suffix in SUFFIXES {
            if let Some(stripped) = trimmed.strip_suffix(suffix) {
                let candidate = stripped
                    .trim_end_matches(|ch: char| matches!(ch, '-' | '_' | ' ' | '.'))
                    .to_string();
                if !candidate.is_empty() {
                    value = candidate;
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            return trimmed;
        }
    }
}

fn canonical_bundle_key(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or(name)
        .to_ascii_lowercase();
    let stripped = strip_bundle_suffix_tokens(stem);
    let cutoff = stripped
        .char_indices()
        .find_map(|(idx, ch)| (idx >= 3 && ch.is_ascii_digit()).then_some(idx))
        .unwrap_or(stripped.len());
    let base = stripped[..cutoff]
        .trim_end_matches(|ch: char| matches!(ch, '-' | '_' | ' ' | '.'))
        .to_string();
    let cleaned = base
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>();
    if cleaned.is_empty() {
        stripped
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .collect::<String>()
    } else {
        cleaned
    }
}

fn matches_bundle_root(root_key: &str, entry_name: &str) -> bool {
    if root_key.len() < 3 {
        return false;
    }
    let entry_key = canonical_bundle_key(entry_name);
    !entry_key.is_empty() && (entry_key.starts_with(root_key) || root_key.starts_with(&entry_key))
}

fn is_package_doc_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = extension_key(Path::new(name));
    let is_doc_ext = matches!(
        ext.as_str(),
        ".txt" | ".md" | ".pdf" | ".doc" | ".docx" | ".rtf"
    );
    is_doc_ext
        && [
            "readme",
            "guide",
            "manual",
            "install",
            "setup",
            "usage",
            "license",
            "璇存槑",
            "瀹夎",
            "浣跨敤",
            "鏁欑▼",
            "杩愯",
            "鐗堟潈",
        ]
        .iter()
        .any(|token| lower.contains(token))
}

fn format_ranked_entries(map: HashMap<String, u64>, limit: usize) -> Vec<String> {
    let mut rows = map.into_iter().collect::<Vec<_>>();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.into_iter()
        .take(limit)
        .map(|(key, count)| format!("{key}:{count}"))
        .collect()
}

fn summarize_name_families(file_names: &[String], limit: usize) -> (Vec<String>, usize) {
    let mut families = HashMap::<String, u64>::new();
    for name in file_names {
        let family = normalize_name_family(name);
        if family.is_empty() || family == "(empty)" {
            continue;
        }
        *families.entry(family).or_insert(0) += 1;
    }
    let max_family_count = families.values().copied().max().unwrap_or(0) as usize;
    let formatted = format_ranked_entries(
        families
            .into_iter()
            .filter(|(_, count)| *count >= 2)
            .collect::<HashMap<_, _>>(),
        limit,
    );
    (formatted, max_family_count)
}

fn summarize_sidecars(file_names: &[String], limit: usize) -> Vec<String> {
    let mut families = HashMap::<String, HashSet<String>>::new();
    for name in file_names {
        let family = normalize_name_family(name);
        if family.is_empty() || family == "(empty)" {
            continue;
        }
        families
            .entry(family)
            .or_default()
            .insert(extension_key(Path::new(name)));
    }
    let mut rows = families
        .into_iter()
        .filter_map(|(family, exts)| {
            if exts.len() < 2 {
                return None;
            }
            let mut ext_list = exts.into_iter().collect::<Vec<_>>();
            ext_list.sort();
            Some((family, ext_list))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    rows.into_iter()
        .take(limit)
        .map(|(family, exts)| format!("{family}=>{}", exts.join("+")))
        .collect()
}

fn summarize_directory_tree(
    path: &Path,
    stop: &AtomicBool,
) -> (u64, u64, u64, HashMap<String, u64>, u32) {
    let mut total_size = 0_u64;
    let mut file_count = 0_u64;
    let mut dir_count = 0_u64;
    let mut ext_counts = HashMap::new();
    let mut max_depth = 0_u32;
    for entry in WalkDir::new(path)
        .min_depth(1)
        .into_iter()
        .filter_map(Result::ok)
    {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let entry_path = entry.path();
        max_depth = max_depth.max(entry.depth() as u32);
        if entry.file_type().is_dir() {
            dir_count = dir_count.saturating_add(1);
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        file_count = file_count.saturating_add(1);
        if let Ok(meta) = entry.metadata() {
            total_size = total_size.saturating_add(meta.len());
        }
        let key = extension_key(entry_path);
        *ext_counts.entry(key).or_insert(0) += 1;
    }
    (total_size, file_count, dir_count, ext_counts, max_depth)
}

fn is_collection_root(path: &Path, excluded: &[String], stop: &AtomicBool) -> bool {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    let root_name = path
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let has_download_name = DOWNLOAD_ROOT_TOKENS
        .iter()
        .any(|token| root_name.contains(token));
    let mut direct_file_count = 0_u64;
    let mut direct_dir_count = 0_u64;
    let mut file_families = HashSet::<String>::new();

    for entry in entries.filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if should_exclude(&name, excluded) {
            continue;
        }
        let entry_path = entry.path();
        if entry_path.is_dir() {
            direct_dir_count = direct_dir_count.saturating_add(1);
            continue;
        }
        if entry_path.is_file() {
            direct_file_count = direct_file_count.saturating_add(1);
            file_families
                .insert(classify_extension_family(&extension_key(&entry_path)).to_string());
        }
    }

    (has_download_name && direct_dir_count >= 4)
        || (direct_dir_count >= 8 && direct_file_count >= 6 && file_families.len() >= 4)
        || (direct_dir_count >= 12 && file_families.len() >= 3)
}

fn evaluate_directory_assessment(
    path: &Path,
    stop: &AtomicBool,
    prefer_whole: bool,
) -> Option<DirectoryAssessment> {
    let mut marker_files = Vec::new();
    let mut evidence = Vec::new();
    let mut app_signals = Vec::new();
    let mut fragmentation_warnings = Vec::new();
    let mut top_level_entries = Vec::new();
    let mut direct_family_counts = HashMap::<String, u64>::new();
    let mut direct_file_names = Vec::new();
    let mut direct_dir_names = Vec::new();
    let mut direct_child_dirs = Vec::<PathBuf>::new();
    let mut has_readme = false;
    let mut has_src = false;
    let mut has_bin = false;
    let mut has_lib = false;
    let mut has_resources = false;
    let mut has_docs = false;
    let mut has_images = false;
    let mut has_labels = false;
    let mut has_annotations = false;
    let mut has_train = false;
    let mut has_val = false;
    let mut has_test = false;
    let mut has_mods = false;
    let mut junk_named_dirs = 0_u64;
    let mut direct_exe_count = 0_u32;
    let mut direct_dll_count = 0_u32;
    let mut direct_archive_count = 0_u32;
    let mut direct_font_count = 0_u32;
    let mut direct_text_count = 0_u32;
    let mut direct_image_count = 0_u32;
    let mut direct_video_count = 0_u32;
    let mut direct_audio_count = 0_u32;
    let mut direct_runtime_count = 0_u32;
    let mut direct_config_count = 0_u32;
    let mut direct_script_count = 0_u32;
    let mut direct_json_count = 0_u32;
    let mut direct_pck_count = 0_u32;
    let mut direct_pak_count = 0_u32;
    let mut direct_bin_payload_count = 0_u32;
    let mut direct_cursor_count = 0_u32;
    let mut direct_inf_count = 0_u32;
    let mut metadata_marker_count = 0_u32;

    let entries = fs::read_dir(path).ok()?;
    for entry in entries.filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if top_level_entries.len() < 18 {
            top_level_entries.push(name.clone());
        }
        if PROJECT_MARKER_NAMES.iter().any(|marker| lower == *marker) {
            marker_files.push(name.clone());
        }

        let entry_path = entry.path();
        if entry_path.is_dir() {
            direct_dir_names.push(name.clone());
            direct_child_dirs.push(entry_path.clone());
            match lower.as_str() {
                "src" | "app" => has_src = true,
                "bin" => has_bin = true,
                "lib" => has_lib = true,
                "resources" | "resource" => has_resources = true,
                "docs" | "doc" => has_docs = true,
                "images" | "image" | "img" | "imgs" => has_images = true,
                "labels" => has_labels = true,
                "annotations" | "annotation" => has_annotations = true,
                "train" => has_train = true,
                "val" | "valid" | "validation" => has_val = true,
                "test" | "tests" => has_test = true,
                "mods" | "plugins" | "plugin" => has_mods = true,
                _ => {}
            }
            if JUNK_DIR_NAMES.iter().any(|value| lower == *value) {
                junk_named_dirs = junk_named_dirs.saturating_add(1);
            }
            continue;
        }

        direct_file_names.push(name.clone());
        if lower == "readme.md" || lower == "readme.txt" || lower == "readme" {
            has_readme = true;
        }
        if lower.ends_with(".exe") {
            direct_exe_count += 1;
        }
        if lower.ends_with(".dll") {
            direct_dll_count += 1;
        }
        if lower.ends_with(".json") {
            direct_json_count += 1;
        }
        if lower.ends_with(".pck") {
            direct_pck_count += 1;
        }
        if lower.ends_with(".pak") {
            direct_pak_count += 1;
        }
        if lower.ends_with(".bin") {
            direct_bin_payload_count += 1;
        }
        if lower.ends_with(".ani") || lower.ends_with(".cur") {
            direct_cursor_count += 1;
        }
        if lower.ends_with(".inf") {
            direct_inf_count += 1;
        }
        if lower.contains("manifest")
            || lower.contains("metadata")
            || lower.contains("catalog")
            || lower.contains("index")
        {
            metadata_marker_count += 1;
        }

        let family = classify_extension_family(&extension_key(&entry_path)).to_string();
        *direct_family_counts.entry(family.clone()).or_insert(0) += 1;
        match family.as_str() {
            "archive" => direct_archive_count += 1,
            "font" => direct_font_count += 1,
            "document" => direct_text_count += 1,
            "image" => direct_image_count += 1,
            "video" => direct_video_count += 1,
            "audio" => direct_audio_count += 1,
            "runtime" => direct_runtime_count += 1,
            "config" => direct_config_count += 1,
            "script" => direct_script_count += 1,
            _ => {}
        }
    }

    let (total_size, file_count, dir_count, ext_counts, max_depth) =
        summarize_directory_tree(path, stop);
    let dominant_extensions = format_ranked_entries(ext_counts.clone(), 8);
    let (name_families, max_family_count) = summarize_name_families(&direct_file_names, 5);
    let paired_sidecars = summarize_sidecars(&direct_file_names, 5);
    let root_bundle_key = canonical_bundle_key(
        path.file_name()
            .and_then(|x| x.to_str())
            .unwrap_or_default(),
    );
    let root_named_file_count = direct_file_names
        .iter()
        .filter(|name| matches_bundle_root(&root_bundle_key, name))
        .count() as u64;
    let root_named_binary_count = direct_file_names
        .iter()
        .filter(|name| {
            matches_bundle_root(&root_bundle_key, name)
                && matches!(
                    classify_extension_family(&extension_key(Path::new(name))),
                    "app" | "config"
                )
        })
        .count() as u64;
    let package_doc_count = direct_file_names
        .iter()
        .filter(|name| is_package_doc_name(name))
        .count() as u64;
    let multi_variant_app_bundle = root_named_binary_count >= 2 && direct_exe_count >= 2;
    let direct_file_count = direct_file_names.len() as u64;
    let direct_dir_count = direct_dir_names.len() as u64;
    let document_collection_share =
        (direct_text_count + direct_archive_count) as u64 * 100 / direct_file_count.max(1);
    let document_collection_layout = direct_text_count >= 8
        && document_collection_share >= 85
        && direct_exe_count == 0
        && direct_dll_count == 0
        && direct_image_count <= 3
        && direct_video_count == 0
        && direct_audio_count == 0
        && direct_runtime_count == 0
        && direct_script_count == 0;
    let dominant_extension_count = ext_counts.values().copied().max().unwrap_or(0);
    let dominant_share = if file_count == 0 {
        0.0
    } else {
        dominant_extension_count as f64 / file_count as f64
    };
    let naming_cohesion = if max_family_count >= 3 {
        "high".to_string()
    } else if max_family_count == 2 || dominant_share >= 0.55 {
        "medium".to_string()
    } else {
        "low".to_string()
    };

    let wrapper_target_path = if direct_dir_count == 1
        && ((direct_file_count == 0)
            || (direct_file_count <= 2
                && direct_file_names.iter().all(|name| {
                    let lower = name.to_ascii_lowercase();
                    WRAPPER_FILE_EXTS.iter().any(|ext| lower.ends_with(ext))
                })))
        && marker_files.is_empty()
        && direct_exe_count == 0
        && direct_dll_count == 0
        && direct_archive_count == 0
        && direct_font_count == 0
        && direct_script_count == 0
    {
        direct_child_dirs
            .first()
            .map(|child| child.to_string_lossy().to_string())
    } else {
        None
    };

    let runtime_like_only = direct_file_count > 0
        && (direct_runtime_count + direct_config_count) as u64 * 100 / direct_file_count >= 70
        && direct_exe_count == 0
        && direct_dll_count == 0
        && direct_text_count <= 1
        && direct_image_count == 0
        && direct_video_count == 0
        && direct_audio_count == 0
        && direct_font_count == 0
        && direct_script_count <= 1
        && marker_files.is_empty();
    let staging_junk = if runtime_like_only {
        junk_named_dirs == direct_dir_count || direct_dir_count == 0
    } else {
        false
    };
    let dll_only_directory = direct_dll_count >= 2
        && u64::from(direct_dll_count) == direct_file_count
        && direct_dir_count == 0
        && direct_exe_count == 0
        && direct_json_count == 0
        && direct_pck_count == 0
        && direct_config_count == 0
        && direct_text_count == 0
        && direct_image_count == 0
        && direct_video_count == 0
        && direct_audio_count == 0
        && direct_script_count == 0;

    let mut integrity_kind = "mixed".to_string();
    let mut score = 0_i32;
    let mut strong_anchor = false;

    if !marker_files.is_empty() {
        score += 38;
        strong_anchor = true;
        integrity_kind = "project".to_string();
        evidence.push(format!("markerFiles={}", marker_files.join(",")));
        app_signals.push("project_markers".to_string());
    }
    if has_readme {
        score += 8;
        evidence.push("readme_present".to_string());
        app_signals.push("readme_present".to_string());
    }
    if package_doc_count > 0 {
        score += (package_doc_count.min(3) as i32) * 3;
        evidence.push(format!("packageDocs={package_doc_count}"));
        app_signals.push(format!("package_docs:{package_doc_count}"));
    }
    if has_readme && has_src {
        score += 30;
        strong_anchor = true;
        integrity_kind = "project".to_string();
        evidence.push("readme+src".to_string());
        app_signals.push("readme+src".to_string());
    }
    if ((has_train && has_val) || (has_train && has_test) || (has_val && has_test))
        || ((has_images || has_docs) && (has_labels || has_annotations))
    {
        score += 32;
        strong_anchor = true;
        integrity_kind = "dataset_bundle".to_string();
        evidence.push("dataset_skeleton".to_string());
        app_signals.push("dataset_skeleton".to_string());
    }
    if direct_exe_count > 0
        && (direct_dll_count > 0
            || has_resources
            || has_bin
            || has_lib
            || direct_pak_count > 0
            || direct_bin_payload_count > 0)
    {
        score += 36;
        strong_anchor = true;
        integrity_kind = "app_bundle".to_string();
        evidence.push("exe+companions".to_string());
        app_signals.push(format!("exe:{direct_exe_count}"));
    } else if direct_dll_count > 0
        && (direct_json_count > 0
            || direct_pck_count > 0
            || direct_config_count > 0
            || has_resources
            || has_bin
            || has_lib
            || has_mods)
    {
        score += 30;
        strong_anchor = true;
        integrity_kind = "app_bundle".to_string();
        evidence.push("dll+config_bundle".to_string());
        app_signals.push(format!("dll:{direct_dll_count}"));
    } else if direct_dll_count > 0 {
        score += 4;
        evidence.push("dll_weak_signal".to_string());
        app_signals.push(format!("dll:{direct_dll_count}"));
    }
    if direct_font_count >= 2 && direct_file_count <= 6 && direct_dir_count == 0 {
        score += 45;
        strong_anchor = true;
        integrity_kind = "doc_bundle".to_string();
        evidence.push("font_pack".to_string());
        app_signals.push("font_pack".to_string());
    }
    if direct_text_count >= 6 && document_collection_share >= 75 {
        score += 30;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = "doc_bundle".to_string();
        }
        evidence.push("document_bundle".to_string());
        app_signals.push("document_bundle".to_string());
    } else if direct_archive_count > 0 && direct_text_count >= 3 {
        score += 18;
        if integrity_kind == "mixed" {
            integrity_kind = "doc_bundle".to_string();
        }
        evidence.push("archive+documents".to_string());
        app_signals.push("archive+documents".to_string());
    }
    if (direct_image_count + direct_video_count + direct_audio_count) >= 3
        && !paired_sidecars.is_empty()
    {
        score += 22;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = "media_bundle".to_string();
        }
        evidence.push("media_sidecars".to_string());
        app_signals.push("media_sidecars".to_string());
    }
    if metadata_marker_count > 0 && (direct_dir_count > 0 || direct_file_count >= 3) {
        score += 22;
        if integrity_kind == "mixed" {
            integrity_kind = "export_backup_bundle".to_string();
        }
        evidence.push("metadata_markers".to_string());
        app_signals.push("metadata_markers".to_string());
    }
    if direct_exe_count >= 1
        && package_doc_count > 0
        && direct_file_count <= 6
        && direct_dir_count == 0
        && direct_dll_count == 0
        && direct_archive_count == 0
    {
        score += 30;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = "app_bundle".to_string();
        }
        evidence.push("installer_with_docs".to_string());
        app_signals.push(format!("installer_docs:{package_doc_count}"));
    }
    if document_collection_layout {
        score += 14;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = "doc_bundle".to_string();
        }
        evidence.push("document_collection_layout".to_string());
        app_signals.push(format!("document_files:{}", direct_text_count));
    }
    if direct_cursor_count >= 8
        && (direct_image_count > 0 || direct_text_count > 0 || direct_inf_count > 0)
    {
        score += 34;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = "theme_pack".to_string();
        }
        evidence.push("cursor_theme_pack".to_string());
        app_signals.push(format!("cursor_files:{direct_cursor_count}"));
        if direct_inf_count > 0 {
            evidence.push("install_manifest_present".to_string());
        }
    }
    if multi_variant_app_bundle {
        score += 42;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = "app_bundle".to_string();
        }
        evidence.push("multi_variant_app_bundle".to_string());
        app_signals.push(format!("root_named_binaries:{root_named_binary_count}"));
        if package_doc_count > 0 {
            evidence.push("package_docs_present".to_string());
        }
    } else if package_doc_count > 0
        && root_named_file_count >= 1
        && (direct_exe_count > 0 || direct_dll_count > 0 || direct_archive_count > 0)
        && direct_file_count <= 12
    {
        score += 20;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = if direct_exe_count > 0 || direct_dll_count > 0 {
                "app_bundle".to_string()
            } else {
                "doc_bundle".to_string()
            };
        }
        evidence.push("package_docs_bundle".to_string());
        app_signals.push(format!("package_docs:{package_doc_count}"));
    }
    if max_family_count >= 2 {
        score += ((max_family_count as i32 - 1) * 4).clamp(4, 16);
        evidence.push(format!("nameFamilyCount={max_family_count}"));
    }
    if dominant_share >= 0.75 {
        score += 14;
        evidence.push("dominantExtensionHigh".to_string());
    } else if dominant_share >= 0.6 {
        score += 10;
        evidence.push("dominantExtension".to_string());
    }
    if direct_file_count >= 2
        && direct_file_count <= 5
        && ((direct_exe_count == 1 && (direct_bin_payload_count > 0 || direct_dll_count > 0))
            || (direct_dll_count > 0
                && (direct_json_count > 0 || direct_pck_count > 0 || direct_config_count > 0))
            || (direct_font_count >= 2 && direct_dir_count == 0))
    {
        score += 15;
        evidence.push("small_strong_bundle".to_string());
    }
    if prefer_whole && score > 0 {
        score += 6;
        evidence.push("collection_root_bonus".to_string());
    }

    let app_bundle_layout = strong_anchor
        && integrity_kind == "app_bundle"
        && direct_exe_count > 0
        && (direct_dll_count > 0
            || has_resources
            || has_bin
            || has_lib
            || direct_pak_count > 0
            || direct_bin_payload_count > 0);
    if app_bundle_layout {
        score += 8;
        evidence.push("app_layout_bundle".to_string());
    }

    if direct_dir_count >= 3
        && direct_file_count >= 6
        && direct_family_counts.len() >= 4
        && !strong_anchor
    {
        score -= 25;
        fragmentation_warnings.push("heterogeneous_top_level".to_string());
    }
    if file_count >= 6
        && dominant_share < 0.45
        && ext_counts.len() >= 5
        && !app_bundle_layout
        && !multi_variant_app_bundle
    {
        score -= 18;
        fragmentation_warnings.push("low_content_cohesion".to_string());
    }
    if max_family_count <= 1
        && direct_file_count >= 8
        && !app_bundle_layout
        && !document_collection_layout
    {
        score -= 12;
        fragmentation_warnings.push("weak_name_families".to_string());
    }
    if dll_only_directory {
        score -= 24;
        fragmentation_warnings.push("dll_only_directory".to_string());
    }

    if wrapper_target_path.is_some() {
        evidence.push("single_child_wrapper".to_string());
    }
    if staging_junk {
        fragmentation_warnings.push("runtime_cache_shell".to_string());
    }

    let integrity_score = score.clamp(0, 100) as u8;
    let explicit_split = (!strong_anchor
        && direct_dir_count >= 3
        && direct_file_count >= 6
        && direct_family_counts.len() >= 4)
        || (!strong_anchor
            && file_count >= 6
            && dominant_share < 0.45
            && ext_counts.len() >= 5
            && direct_family_counts.len() >= 4)
        || dll_only_directory
        || (integrity_kind == "mixed"
            && direct_dir_count >= 2
            && direct_file_count >= 6
            && direct_family_counts.len() >= 4);
    let result_kind = if wrapper_target_path.is_some() {
        DirectoryResultKind::WholeWrapperPassthrough
    } else if staging_junk {
        DirectoryResultKind::StagingJunk
    } else if explicit_split {
        DirectoryResultKind::MixedSplit
    } else {
        DirectoryResultKind::Whole
    };

    Some(DirectoryAssessment {
        result_kind,
        integrity_score,
        integrity_kind,
        evidence,
        wrapper_target_path,
        top_level_entries,
        dominant_extensions,
        name_families,
        paired_sidecars,
        fragmentation_warnings,
        naming_cohesion,
        total_size,
        file_count,
        dir_count,
        direct_file_count,
        direct_dir_count,
        max_depth,
    })
}

fn create_directory_unit(
    scan_root: &Path,
    path: &Path,
    metadata: &fs::Metadata,
    assessment: DirectoryAssessment,
) -> OrganizeUnit {
    OrganizeUnit {
        name: path
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or_default()
            .to_string(),
        path: path.to_string_lossy().to_string(),
        relative_path: relative_path_string(scan_root, path),
        size: assessment.total_size,
        created_at: metadata.created().ok().map(system_time_to_iso),
        modified_at: metadata.modified().ok().map(system_time_to_iso),
        item_type: "directory".to_string(),
        modality: "directory".to_string(),
        directory_assessment: Some(assessment),
    }
}

fn create_file_unit(scan_root: &Path, path: &Path, metadata: &fs::Metadata) -> OrganizeUnit {
    OrganizeUnit {
        name: path
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or_default()
            .to_string(),
        path: path.to_string_lossy().to_string(),
        relative_path: relative_path_string(scan_root, path),
        size: metadata.len(),
        created_at: metadata.created().ok().map(system_time_to_iso),
        modified_at: metadata.modified().ok().map(system_time_to_iso),
        item_type: "file".to_string(),
        modality: pick_modality(&path.to_string_lossy()).to_string(),
        directory_assessment: None,
    }
}

fn collect_directory_candidate(
    scan_root: &Path,
    path: &Path,
    recursive: bool,
    excluded: &[String],
    stop: &AtomicBool,
    parent_is_collection_root: bool,
    staging_passthrough_budget: u8,
    out: &mut Vec<OrganizeUnit>,
) {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return,
    };
    let assessment = match evaluate_directory_assessment(path, stop, parent_is_collection_root) {
        Some(assessment) => assessment,
        None => return,
    };

    match assessment.result_kind {
        DirectoryResultKind::Whole => {
            out.push(create_directory_unit(
                scan_root, path, &metadata, assessment,
            ));
        }
        DirectoryResultKind::WholeWrapperPassthrough => {
            if let Some(target) = assessment.wrapper_target_path.as_ref() {
                collect_directory_candidate(
                    scan_root,
                    Path::new(target),
                    recursive,
                    excluded,
                    stop,
                    parent_is_collection_root,
                    staging_passthrough_budget,
                    out,
                );
            }
        }
        DirectoryResultKind::MixedSplit => {
            if recursive {
                let current_is_collection_root = is_collection_root(path, excluded, stop);
                collect_units_inner(
                    scan_root,
                    path,
                    true,
                    excluded,
                    stop,
                    current_is_collection_root,
                    staging_passthrough_budget,
                    out,
                );
            }
        }
        DirectoryResultKind::StagingJunk => {
            if recursive && staging_passthrough_budget > 0 {
                collect_units_inner(
                    scan_root,
                    path,
                    true,
                    excluded,
                    stop,
                    false,
                    staging_passthrough_budget.saturating_sub(1),
                    out,
                );
            }
        }
    }
}

fn collect_units_inner(
    scan_root: &Path,
    current_dir: &Path,
    recursive: bool,
    excluded: &[String],
    stop: &AtomicBool,
    current_is_collection_root: bool,
    staging_passthrough_budget: u8,
    out: &mut Vec<OrganizeUnit>,
) {
    let entries = match fs::read_dir(current_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_exclude(&name, excluded) {
            continue;
        }
        if path.is_dir() {
            collect_directory_candidate(
                scan_root,
                &path,
                recursive,
                excluded,
                stop,
                current_is_collection_root,
                staging_passthrough_budget,
                out,
            );
            continue;
        }
        if !path.is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            out.push(create_file_unit(scan_root, &path, &meta));
        }
    }
}

fn collect_units(
    root: &Path,
    recursive: bool,
    excluded: &[String],
    stop: &AtomicBool,
) -> Vec<OrganizeUnit> {
    let mut out = Vec::new();
    let root_is_collection_root = is_collection_root(root, excluded, stop);
    collect_units_inner(
        root,
        root,
        recursive,
        excluded,
        stop,
        root_is_collection_root,
        1,
        &mut out,
    );
    out.sort_by(|a, b| {
        a.relative_path
            .to_lowercase()
            .cmp(&b.relative_path.to_lowercase())
            .then_with(|| a.item_type.cmp(&b.item_type))
    });
    out
}

fn summarize_directory_for_prompt(unit: &OrganizeUnit, response_language: &str) -> String {
    let Some(assessment) = unit.directory_assessment.as_ref() else {
        return if is_zh_language(response_language) {
            "暂无目录摘要。".to_string()
        } else {
            "No directory summary available.".to_string()
        };
    };
    let mut lines = vec![
        format!("resultKind={}", assessment.result_kind.as_str()),
        format!("integrityKind={}", assessment.integrity_kind),
        format!("integrityScore={}", assessment.integrity_score),
        format!("relativePath={}", unit.relative_path),
        format!("totalSize={}", unit.size),
        format!(
            "createdAt={}",
            unit.created_at
                .clone()
                .unwrap_or_else(|| organizer_unknown_label(response_language).to_string())
        ),
        format!(
            "modifiedAt={}",
            unit.modified_at
                .clone()
                .unwrap_or_else(|| organizer_unknown_label(response_language).to_string())
        ),
        format!(
            "directoryShape=directFiles:{}|directDirs:{}|totalFiles:{}|totalDirectories:{}|maxDepth:{}",
            assessment.direct_file_count,
            assessment.direct_dir_count,
            assessment.file_count,
            assessment.dir_count,
            assessment.max_depth
        ),
        format!(
            "evidence={}",
            if assessment.evidence.is_empty() {
                organizer_none_label(response_language).to_string()
            } else {
                assessment.evidence.join(", ")
            }
        ),
        format!("namingCohesion={}", assessment.naming_cohesion),
        format!(
            "topLevelEntries={}",
            if assessment.top_level_entries.is_empty() {
                organizer_none_label(response_language).to_string()
            } else {
                assessment.top_level_entries.join(", ")
            }
        ),
        format!(
            "dominantExtensions={}",
            if assessment.dominant_extensions.is_empty() {
                organizer_none_label(response_language).to_string()
            } else {
                assessment.dominant_extensions.join(", ")
            }
        ),
        format!(
            "nameFamilies={}",
            if assessment.name_families.is_empty() {
                organizer_none_label(response_language).to_string()
            } else {
                assessment.name_families.join(", ")
            }
        ),
        format!(
            "pairedSidecars={}",
            if assessment.paired_sidecars.is_empty() {
                organizer_none_label(response_language).to_string()
            } else {
                assessment.paired_sidecars.join(", ")
            }
        ),
        format!(
            "fragmentationWarnings={}",
            if assessment.fragmentation_warnings.is_empty() {
                organizer_none_label(response_language).to_string()
            } else {
                assessment.fragmentation_warnings.join(", ")
            }
        ),
    ];
    if is_zh_language(response_language) {
        lines.push(
            "该目录已是整体候选，默认按整体归类，除非摘要明确显示存在多个无关主题。".to_string(),
        );
    } else {
        lines.push(
            "This directory is already a bundle candidate. Default to classifying it as a whole unit unless the summary clearly indicates multiple unrelated themes."
                .to_string(),
        );
    }
    lines.join("\n")
}

#[allow(dead_code)]
fn build_reference_structure_context(
    root: &Path,
    excluded: &[String],
    stop: &AtomicBool,
    response_language: &str,
) -> String {
    let mut lines = Vec::new();
    let mut total_dirs = 0_u64;
    let mut total_files = 0_u64;
    let mut truncated = false;
    let max_lines = 240_usize;
    let max_depth = 10_usize;

    let walker = WalkDir::new(root)
        .min_depth(1)
        .max_depth(max_depth)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            !should_exclude(&name, excluded)
        });

    for entry in walker.filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) {
            truncated = true;
            break;
        }
        if lines.len() >= max_lines {
            truncated = true;
            break;
        }

        let relative = entry
            .path()
            .strip_prefix(root)
            .unwrap_or_else(|_| entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        let depth = entry.depth().saturating_sub(1);
        let indent = "  ".repeat(depth);

        if entry.file_type().is_dir() {
            total_dirs = total_dirs.saturating_add(1);
            if is_zh_language(response_language) {
                lines.push(format!("{indent}[鐩綍] {relative}/"));
            } else {
                lines.push(format!("{indent}[D] {relative}/"));
            }
            continue;
        }
        if entry.file_type().is_file() {
            total_files = total_files.saturating_add(1);
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            if is_zh_language(response_language) {
                lines.push(format!("{indent}[鏂囦欢] {relative} ({size} bytes)"));
            } else {
                lines.push(format!("{indent}[F] {relative} ({size} bytes)"));
            }
        }
    }

    let mut out = if is_zh_language(response_language) {
        vec![
            format!("鏍硅矾寰?{}", root.to_string_lossy()),
            format!("参考树最大深度={max_depth}"),
            format!("参考树展示行数={}", lines.len()),
            format!("参考树目录数={total_dirs}"),
            format!("参考树文件数={total_files}"),
            format!("参考树是否截断={truncated}"),
            "参考树开始".to_string(),
        ]
    } else {
        vec![
            format!("rootPath={}", root.to_string_lossy()),
            format!("referenceTreeMaxDepth={max_depth}"),
            format!("referenceTreeLinesShown={}", lines.len()),
            format!("referenceTreeDirectoriesShown={total_dirs}"),
            format!("referenceTreeFilesShown={total_files}"),
            format!("referenceTreeTruncated={truncated}"),
            "referenceTreeStart".to_string(),
        ]
    };
    out.extend(lines);
    out.push(if is_zh_language(response_language) {
        "参考树结束".to_string()
    } else {
        "referenceTreeEnd".to_string()
    });
    out.join("\n")
}

fn default_tree() -> CategoryTreeNode {
    CategoryTreeNode {
        node_id: "root".to_string(),
        name: String::new(),
        children: Vec::new(),
    }
}

fn sanitize_node_name(value: &str) -> String {
    let cleaned = value.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_");
    cleaned.trim().to_string()
}

fn category_path_display(path: &[String]) -> String {
    if path.is_empty() {
        UNCATEGORIZED_NODE_NAME.to_string()
    } else {
        path.join(" / ")
    }
}

fn row_has_classification_error(row: &Value) -> bool {
    if row.get("reason").and_then(Value::as_str).map(str::trim)
        == Some(RESULT_REASON_CLASSIFICATION_ERROR)
    {
        return true;
    }
    !row.get("classificationError")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .is_empty()
}

fn category_tree_to_value(node: &CategoryTreeNode) -> Value {
    json!({
        "nodeId": node.node_id,
        "name": node.name,
        "children": node.children.iter().map(category_tree_to_value).collect::<Vec<_>>(),
    })
}

fn tree_from_value(value: &Value) -> CategoryTreeNode {
    fn parse_node(value: &Value) -> Option<CategoryTreeNode> {
        let node_id = value
            .get("nodeId")
            .and_then(Value::as_str)?
            .trim()
            .to_string();
        if node_id.is_empty() {
            return None;
        }
        Some(CategoryTreeNode {
            node_id,
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            children: value
                .get("children")
                .and_then(Value::as_array)
                .map(|children| children.iter().filter_map(parse_node).collect())
                .unwrap_or_default(),
        })
    }

    parse_node(value).unwrap_or_else(default_tree)
}

fn collect_existing_node_ids(node: &CategoryTreeNode, out: &mut HashSet<String>) {
    out.insert(node.node_id.clone());
    for child in &node.children {
        collect_existing_node_ids(child, out);
    }
}

fn normalize_ai_tree(value: &Value, current: &CategoryTreeNode) -> CategoryTreeNode {
    fn parse_node(
        value: &Value,
        existing_ids: &HashSet<String>,
        is_root: bool,
    ) -> Option<CategoryTreeNode> {
        let mut name = value
            .get("name")
            .and_then(Value::as_str)
            .map(sanitize_node_name)
            .unwrap_or_default();
        if !is_root && name.is_empty() {
            return None;
        }
        let provided_id = value
            .get("nodeId")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let node_id = if is_root {
            "root".to_string()
        } else if !provided_id.is_empty() && existing_ids.contains(provided_id) {
            provided_id.to_string()
        } else {
            Uuid::new_v4().to_string()
        };
        if is_root {
            name.clear();
        }
        Some(CategoryTreeNode {
            node_id,
            name,
            children: value
                .get("children")
                .and_then(Value::as_array)
                .map(|children| {
                    children
                        .iter()
                        .filter_map(|child| parse_node(child, existing_ids, false))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    let mut existing_ids = HashSet::new();
    collect_existing_node_ids(current, &mut existing_ids);
    value
        .get("tree")
        .and_then(|tree| parse_node(tree, &existing_ids, true))
        .unwrap_or_else(|| current.clone())
}

fn ensure_path(node: &mut CategoryTreeNode, path: &[String]) -> String {
    if path.is_empty() {
        return node.node_id.clone();
    }
    let name = sanitize_node_name(&path[0]);
    if name.is_empty() {
        return ensure_path(node, &path[1..]);
    }
    let idx = node
        .children
        .iter()
        .position(|child| child.name == name)
        .unwrap_or_else(|| {
            node.children.push(CategoryTreeNode {
                node_id: Uuid::new_v4().to_string(),
                name: name.clone(),
                children: Vec::new(),
            });
            node.children.len() - 1
        });
    ensure_path(&mut node.children[idx], &path[1..])
}

fn ensure_uncategorized_leaf(node: &mut CategoryTreeNode) -> String {
    ensure_path(node, &[UNCATEGORIZED_NODE_NAME.to_string()])
}

fn find_path_by_id(node: &CategoryTreeNode, target_id: &str, path: &mut Vec<String>) -> bool {
    if node.node_id == target_id {
        return true;
    }
    for child in &node.children {
        path.push(child.name.clone());
        if find_path_by_id(child, target_id, path) {
            return true;
        }
        path.pop();
    }
    false
}

fn category_path_for_id(node: &CategoryTreeNode, target_id: &str) -> Option<Vec<String>> {
    let mut path = Vec::new();
    if find_path_by_id(node, target_id, &mut path) {
        Some(path)
    } else {
        None
    }
}

fn category_path_from_value(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(sanitize_node_name)
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn trim_to_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect::<String>()
}

fn normalize_multiline_text(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut last_blank = false;
    for raw_line in value.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let line = raw_line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            if !last_blank && !out.is_empty() {
                out.push('\n');
            }
            last_blank = true;
            continue;
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&line);
        last_blank = false;
    }
    trim_to_chars(out.trim(), max_chars)
}

async fn emit_snapshot<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<OrganizeTaskRuntime>,
) -> Result<(), String> {
    let snap = task.snapshot.lock().clone();
    persist::save_organize_snapshot(&state.db_path(), &snap)?;
    app.emit(
        "organize_progress",
        serde_json::to_value(&snap).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

async fn run_organize_task<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<OrganizeTaskRuntime>,
) -> Result<(), String> {
    let task_started_at = Instant::now();
    let (
        root_path,
        recursive,
        excluded,
        batch_size,
        summary_strategy,
        max_cluster_depth,
        use_web_search,
    ) = {
        let snap = task.snapshot.lock();
        (
            snap.root_path.clone(),
            snap.recursive,
            snap.excluded_patterns.clone(),
            snap.batch_size,
            snap.summary_strategy.clone(),
            snap.max_cluster_depth,
            snap.use_web_search,
        )
    };
    task.diagnostics.record(
        "info",
        "organize_task_started",
        "organize task started",
        json!({
            "rootPath": root_path.clone(),
            "recursive": recursive,
            "excludedPatterns": excluded.clone(),
            "batchSize": batch_size,
            "summaryStrategy": summary_strategy.clone(),
            "maxClusterDepth": max_cluster_depth,
            "useWebSearch": use_web_search,
        }),
        None,
        None,
    );
    {
        let mut snap = task.snapshot.lock();
        snap.status = "scanning".to_string();
    }
    let scan_started_at = Instant::now();
    task.diagnostics.record(
        "info",
        "organize_scan_start",
        "organize scan started",
        json!({ "rootPath": root_path.clone() }),
        None,
        None,
    );
    emit_snapshot(app, state, task).await?;

    let units = collect_units(Path::new(&root_path), recursive, &excluded, &task.stop);
    task.diagnostics.record(
        "info",
        "organize_scan_done",
        "organize scan completed",
        json!({
            "rootPath": root_path.clone(),
            "unitCount": units.len(),
        }),
        None,
        Some(scan_started_at.elapsed()),
    );
    if task.stop.load(Ordering::Relaxed) {
        task.diagnostics.record(
            "info",
            "organize_task_stopped",
            "organize task stopped after scan",
            json!({ "stage": "scan" }),
            None,
            Some(task_started_at.elapsed()),
        );
        return Ok(());
    }
    let reference_structure = None;
    if task.stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let total_batches = if units.is_empty() {
        0
    } else {
        ((units.len() as u64) + batch_size as u64 - 1) / batch_size as u64
    };
    {
        let mut snap = task.snapshot.lock();
        snap.status = "classifying".to_string();
        snap.total_files = units.len() as u64;
        snap.total_batches = total_batches;
        snap.processed_files = 0;
        snap.processed_batches = 0;
        snap.results.clear();
        snap.preview.clear();
    }
    emit_snapshot(app, state, task).await?;

    let mut tree = {
        let snap = task.snapshot.lock();
        tree_from_value(&snap.tree)
    };
    ensure_uncategorized_leaf(&mut tree);
    let text_route = task.routes.get("text").cloned().unwrap_or(RouteConfig {
        endpoint: "https://api.openai.com/v1".to_string(),
        api_key: String::new(),
        model: "gpt-4o-mini".to_string(),
    });
    let task_id = task.snapshot.lock().id.clone();

    for (batch_idx, batch) in units.chunks(batch_size as usize).enumerate() {
        let batch_started_at = Instant::now();
        task.diagnostics.record(
            "info",
            "organize_batch_start",
            "organize batch started",
            json!({
                "batchIndex": batch_idx + 1,
                "batchSize": batch.len(),
                "totalBatches": total_batches,
            }),
            None,
            None,
        );
        if task.stop.load(Ordering::Relaxed) {
            task.diagnostics.record(
                "info",
                "organize_task_stopped",
                "organize task stopped before batch",
                json!({ "stage": "batch_start", "batchIndex": batch_idx + 1 }),
                None,
                Some(task_started_at.elapsed()),
            );
            return Ok(());
        }

        let mut batch_rows = Vec::new();
        let mut local_results = Vec::new();
        for (offset, unit) in batch.iter().enumerate() {
            if task.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let route_key = if unit.item_type == "directory" {
                "text"
            } else {
                unit.modality.as_str()
            };
            let route = task
                .routes
                .get(route_key)
                .or_else(|| task.routes.get("text"))
                .cloned()
                .unwrap_or(RouteConfig {
                    endpoint: "https://api.openai.com/v1".to_string(),
                    api_key: String::new(),
                    model: "gpt-4o-mini".to_string(),
                });
            let extracted = match summary_strategy.as_str() {
                SUMMARY_MODE_FILENAME_ONLY => None,
                _ => Some(
                    summary::extract_unit_content_for_summary_with_tools(
                        unit,
                        &task.response_language,
                        &task.stop,
                        &task.extraction_tool,
                    )
                    .await,
                ),
            };
            let local_result = match summary_strategy.as_str() {
                SUMMARY_MODE_FILENAME_ONLY => SummaryBuildResult {
                    representation: FileRepresentation {
                        metadata: Some(summary::build_representation_metadata(
                            unit,
                            &SummaryExtraction {
                                parser: SUMMARY_SOURCE_FILENAME_ONLY.to_string(),
                                ..SummaryExtraction::default()
                            },
                        )),
                        short: None,
                        long: None,
                        source: SUMMARY_SOURCE_FILENAME_ONLY.to_string(),
                        degraded: false,
                        confidence: None,
                        keywords: Vec::new(),
                    },
                    warnings: Vec::new(),
                },
                _ => summary::build_local_summary(
                    unit,
                    extracted.as_ref().unwrap_or(&SummaryExtraction::default()),
                ),
            };
            let extraction_json = extracted
                .as_ref()
                .map(|value| {
                    json!({
                        "parser": value.parser,
                        "title": value.title,
                        "excerpt": value.excerpt,
                        "keywords": value.keywords,
                        "metadata": value.metadata_lines,
                        "warnings": value.warnings,
                    })
                })
                .unwrap_or(Value::Null);
            batch_rows.push(json!({
                "itemId": format!("batch{}_{}", batch_idx + 1, offset + 1),
                "name": unit.name,
                "path": unit.path,
                "relativePath": unit.relative_path,
                "size": unit.size,
                "createdAt": unit.created_at,
                "modifiedAt": unit.modified_at,
                "itemType": unit.item_type,
                "modality": unit.modality,
                "summaryStrategy": summary_strategy.clone(),
                "representation": local_result.representation.to_value(),
                "summaryDegraded": local_result.representation.degraded,
                "summaryWarnings": local_result.warnings,
                "localExtraction": extraction_json,
                "provider": route.endpoint,
                "model": route.model,
            }));
            if summary_strategy == SUMMARY_MODE_LOCAL_SUMMARY {
                if let Some(row) = batch_rows.last() {
                    summary::emit_organize_summary_ready(
                        &app,
                        &task_id,
                        (batch_idx + 1) as u64,
                        row,
                    );
                }
            }
            local_results.push(local_result);
        }

        let mut summary_usage = TokenUsage::default();
        if summary_strategy == SUMMARY_MODE_AGENT_SUMMARY {
            let output = summary::summarize_batch_with_agent(
                &text_route,
                &task.response_language,
                &task.stop,
                &batch_rows,
                Some(&task.diagnostics),
                "summary_agent",
            )
            .await;
            summary_usage = output.usage;
            let batch_failed_warning = output
                .error
                .clone()
                .unwrap_or_else(|| "summary_agent_missing_items".to_string());
            for (idx, row) in batch_rows.iter_mut().enumerate() {
                let item_id = row.get("itemId").and_then(Value::as_str).unwrap_or("");
                let local_result = local_results.get(idx).cloned().unwrap_or_default();
                let mut warnings = row
                    .get("summaryWarnings")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|value| value.as_str().map(|item| item.to_string()))
                    .collect::<Vec<_>>();
                let fallback_source =
                    if local_result.representation.source == SUMMARY_SOURCE_FILENAME_ONLY {
                        SUMMARY_SOURCE_FILENAME_ONLY
                    } else {
                        SUMMARY_SOURCE_AGENT_FALLBACK_LOCAL
                    };

                if let Some(agent_item) = output
                    .error
                    .is_none()
                    .then(|| output.items.get(item_id))
                    .flatten()
                    .filter(|item| {
                        !item.summary_long.trim().is_empty()
                            || !item.summary_short.trim().is_empty()
                    })
                {
                    warnings.extend(agent_item.warnings.clone());
                    let mut representation = local_result.representation.clone();
                    representation.short = Some(agent_item.summary_short.clone());
                    representation.long = Some(agent_item.summary_long.clone());
                    representation.source = SUMMARY_SOURCE_AGENT_SUMMARY.to_string();
                    representation.confidence = agent_item.confidence.clone();
                    representation.keywords = agent_item.keywords.clone();
                    row["representation"] = representation.to_value();
                    row["summaryDegraded"] = Value::Bool(local_result.representation.degraded);
                    row["summaryWarnings"] =
                        Value::Array(warnings.into_iter().map(Value::String).collect::<Vec<_>>());
                } else {
                    warnings.push(batch_failed_warning.clone());
                    let mut representation = local_result.representation.clone();
                    representation.source = fallback_source.to_string();
                    representation.degraded = true;
                    representation.confidence = None;
                    row["representation"] = representation.to_value();
                    row["summaryDegraded"] = Value::Bool(true);
                    row["summaryWarnings"] =
                        Value::Array(warnings.into_iter().map(Value::String).collect::<Vec<_>>());
                }
            }
            for row in &batch_rows {
                summary::emit_organize_summary_ready(&app, &task_id, (batch_idx + 1) as u64, row);
            }
        }

        let mut cluster_usage = TokenUsage::default();
        let mut cluster_failed = false;
        let mut cluster_raw_output = String::new();
        let mut cluster_error = String::new();
        let mut assignment_map: HashMap<String, (String, Vec<String>, String)> = HashMap::new();

        if !text_route.api_key.trim().is_empty() {
            match summary::classify_organize_batch(
                &text_route,
                &task.response_language,
                &task.stop,
                &tree,
                &batch_rows,
                max_cluster_depth,
                reference_structure.as_ref(),
                use_web_search,
                &task.search_api_key,
                Some(&task.diagnostics),
                &format!("classification_batch_{}", batch_idx + 1),
            )
            .await
            {
                Ok(output) => {
                    cluster_usage = output.usage;
                    cluster_raw_output = output.raw_output;
                    if let Some(search_error) = output.error {
                        cluster_failed = true;
                        cluster_error = search_error;
                    }
                    if let Some(parsed) = output.parsed {
                        tree = normalize_ai_tree(&parsed, &tree);
                        ensure_uncategorized_leaf(&mut tree);
                        for assignment in parsed
                            .get("assignments")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                        {
                            let Some(item_id) = assignment.get("itemId").and_then(Value::as_str)
                            else {
                                continue;
                            };
                            let mut category_path = category_path_from_value(
                                assignment
                                    .get("categoryPath")
                                    .or_else(|| assignment.get("leafPath")),
                            );
                            let leaf_node_id = if let Some(node_id) =
                                assignment.get("leafNodeId").and_then(Value::as_str)
                            {
                                if let Some(path) = category_path_for_id(&tree, node_id) {
                                    category_path = path;
                                    node_id.to_string()
                                } else if !category_path.is_empty() {
                                    ensure_path(&mut tree, &category_path)
                                } else {
                                    ensure_uncategorized_leaf(&mut tree)
                                }
                            } else if !category_path.is_empty() {
                                ensure_path(&mut tree, &category_path)
                            } else {
                                ensure_uncategorized_leaf(&mut tree)
                            };
                            if category_path.is_empty() {
                                category_path = category_path_for_id(&tree, &leaf_node_id)
                                    .unwrap_or_else(|| vec![UNCATEGORIZED_NODE_NAME.to_string()]);
                            }
                            assignment_map.insert(
                                item_id.to_string(),
                                (
                                    leaf_node_id,
                                    category_path,
                                    assignment
                                        .get("reason")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string(),
                                ),
                            );
                        }
                        if assignment_map.len() < batch_rows.len() {
                            cluster_failed = true;
                            if cluster_error.is_empty() {
                                cluster_error = format!(
                                    "classification response missing assignments for {} item(s)",
                                    batch_rows.len().saturating_sub(assignment_map.len())
                                );
                            }
                            assignment_map.clear();
                        }
                    }
                }
                Err(err) => {
                    cluster_failed = true;
                    cluster_error = err;
                }
            }
        }
        if task.stop.load(Ordering::Relaxed) {
            task.diagnostics.record(
                "info",
                "organize_task_stopped",
                "organize task stopped after classification",
                json!({ "stage": "classification", "batchIndex": batch_idx + 1 }),
                None,
                Some(task_started_at.elapsed()),
            );
            return Ok(());
        }

        let batch_base_index = {
            let snap = task.snapshot.lock();
            snap.processed_files
        };
        let mut persisted_rows = Vec::with_capacity(batch_rows.len());
        for (row_offset, row) in batch_rows.into_iter().enumerate() {
            if task.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let item_id = row.get("itemId").and_then(Value::as_str).unwrap_or("");
            let (leaf_node_id, category_path, category, reason) = if cluster_failed {
                (
                    String::new(),
                    Vec::new(),
                    CATEGORY_CLASSIFICATION_ERROR.to_string(),
                    RESULT_REASON_CLASSIFICATION_ERROR.to_string(),
                )
            } else {
                let (leaf_node_id, category_path, reason) =
                    assignment_map.get(item_id).cloned().unwrap_or_else(|| {
                        let leaf = ensure_uncategorized_leaf(&mut tree);
                        let path = category_path_for_id(&tree, &leaf)
                            .unwrap_or_else(|| vec![UNCATEGORIZED_NODE_NAME.to_string()]);
                        (leaf, path, "fallback_uncategorized".to_string())
                    });
                (
                    leaf_node_id,
                    category_path.clone(),
                    category_path_display(&category_path),
                    reason,
                )
            };
            let warnings = row
                .get("summaryWarnings")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let result_row = json!({
                "taskId": task_id.clone(),
                "index": batch_base_index + row_offset as u64 + 1,
                "batchIndex": (batch_idx + 1) as u64,
                "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
                "path": row.get("path").and_then(Value::as_str).unwrap_or(""),
                "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
                "size": row.get("size").and_then(Value::as_u64).unwrap_or(0),
                "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
                "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
                "summaryStrategy": row.get("summaryStrategy").cloned().unwrap_or(Value::String(SUMMARY_MODE_FILENAME_ONLY.to_string())),
                "representation": row.get("representation").cloned().unwrap_or_else(|| FileRepresentation::default().to_value()),
                "localExtraction": row.get("localExtraction").cloned().unwrap_or(Value::Null),
                "leafNodeId": leaf_node_id,
                "categoryPath": category_path,
                "category": category,
                "reason": reason,
                "degraded": cluster_failed || row.get("summaryDegraded").and_then(Value::as_bool).unwrap_or(false),
                "warnings": warnings,
                "provider": row.get("provider").and_then(Value::as_str).unwrap_or(""),
                "model": row.get("model").and_then(Value::as_str).unwrap_or(""),
                "classificationError": if row_offset == 0 { cluster_error.clone() } else { String::new() },
                "modelRawOutput": if row_offset == 0 { cluster_raw_output.clone() } else { String::new() },
            });
            persisted_rows.push(result_row.clone());
        }
        persist::upsert_organize_results(&state.db_path(), &task_id, &persisted_rows)?;
        {
            let mut snap = task.snapshot.lock();
            snap.processed_files = snap
                .processed_files
                .saturating_add(persisted_rows.len() as u64);
            snap.results.extend(persisted_rows.iter().cloned());
        }
        let persisted_row_count = persisted_rows.len();
        for row in persisted_rows {
            app.emit("organize_file_done", row)
                .map_err(|e| e.to_string())?;
        }

        {
            let mut snap = task.snapshot.lock();
            snap.tree = category_tree_to_value(&tree);
            snap.tree_version = snap.tree_version.saturating_add(1);
            snap.processed_batches = (batch_idx + 1) as u64;
            snap.token_usage.prompt = snap
                .token_usage
                .prompt
                .saturating_add(summary_usage.prompt)
                .saturating_add(cluster_usage.prompt);
            snap.token_usage.completion = snap
                .token_usage
                .completion
                .saturating_add(summary_usage.completion)
                .saturating_add(cluster_usage.completion);
            snap.token_usage.total = snap
                .token_usage
                .total
                .saturating_add(summary_usage.total)
                .saturating_add(cluster_usage.total);
        }
        emit_snapshot(app, state, task).await?;
        task.diagnostics.record(
            if cluster_failed { "warn" } else { "info" },
            "organize_batch_done",
            "organize batch completed",
            json!({
                "batchIndex": batch_idx + 1,
                "persistedRows": persisted_row_count,
                "clusterFailed": cluster_failed,
                "clusterError": cluster_error.clone(),
                "summaryUsage": {
                    "prompt": summary_usage.prompt,
                    "completion": summary_usage.completion,
                    "total": summary_usage.total,
                },
                "clusterUsage": {
                    "prompt": cluster_usage.prompt,
                    "completion": cluster_usage.completion,
                    "total": cluster_usage.total,
                },
            }),
            None,
            Some(batch_started_at.elapsed()),
        );
    }

    let final_snapshot = {
        let mut snap = task.snapshot.lock();
        snap.results
            .sort_by_key(|x| x.get("index").and_then(Value::as_u64).unwrap_or(0));
        snap.preview = planner::build_preview(&snap.root_path, &snap.results);
        snap.tree = category_tree_to_value(&tree);
        snap.status = "completed".to_string();
        snap.completed_at = Some(now_iso());
        snap.clone()
    };
    persist::save_organize_snapshot(&state.db_path(), &final_snapshot)?;
    persist::save_latest_organize_tree(
        &state.db_path(),
        &final_snapshot.root_path,
        &final_snapshot.tree,
        final_snapshot.tree_version,
    )?;
    app.emit(
        "organize_done",
        serde_json::to_value(final_snapshot.clone()).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    task.diagnostics.record(
        "info",
        "organize_task_completed",
        "organize task completed",
        json!({
            "totalFiles": final_snapshot.total_files,
            "processedFiles": final_snapshot.processed_files,
            "totalBatches": final_snapshot.total_batches,
            "processedBatches": final_snapshot.processed_batches,
            "previewCount": final_snapshot.preview.len(),
            "tokenUsage": {
                "prompt": final_snapshot.token_usage.prompt,
                "completion": final_snapshot.token_usage.completion,
                "total": final_snapshot.token_usage.total,
            },
        }),
        None,
        Some(task_started_at.elapsed()),
    );
    Ok(())
}

pub async fn organize_get_capability(state: State<'_, AppState>) -> Result<Value, String> {
    let settings = crate::backend::read_settings(&state.settings_path());
    let (endpoint, model) =
        crate::backend::resolve_provider_endpoint_and_model(state.inner(), None, None);
    Ok(json!({
        "selectedModel": model,
        "selectedModels": { "text": model, "image": model, "video": model, "audio": model },
        "selectedProviders": { "text": endpoint, "image": endpoint, "video": endpoint, "audio": endpoint },
        "supportsMultimodal": supports_multimodal(&model, &endpoint),
        "useWebSearch": settings.pointer("/searchApi/scopes/organizer").and_then(Value::as_bool).unwrap_or(false),
        "webSearchEnabled": settings.pointer("/searchApi/enabled").and_then(Value::as_bool).unwrap_or(false),
    }))
}

pub async fn organize_start<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    input: OrganizeStartInput,
    operation_id: String,
) -> Result<Value, String> {
    if input.root_path.trim().is_empty() {
        return Err("rootPath is required".to_string());
    }
    let task_id = format!("org_{}", Uuid::new_v4().simple());
    let settings = crate::backend::read_settings(&state.settings_path());
    let mut extraction_tool = extraction_tool_config_from_settings(&settings);
    let normalized_summary_strategy = normalize_summary_mode(input.summary_strategy.as_deref());
    if normalized_summary_strategy != SUMMARY_MODE_FILENAME_ONLY {
        force_enable_tika_for_summary_mode(&mut extraction_tool);
        ensure_tika_server_running(state.inner(), &mut extraction_tool).await;
    }
    let routes = parse_routes(&input.model_routing);
    let text_route = routes.get("text").cloned().unwrap_or(RouteConfig {
        endpoint: "https://api.openai.com/v1".to_string(),
        api_key: String::new(),
        model: "gpt-4o-mini".to_string(),
    });
    let (tree, tree_version) =
        persist::load_latest_organize_tree(&state.db_path(), &input.root_path)?
            .unwrap_or_else(|| (category_tree_to_value(&default_tree()), 0));
    let snapshot = OrganizeSnapshot {
        id: task_id.clone(),
        status: "idle".to_string(),
        error: None,
        root_path: input.root_path.clone(),
        recursive: true,
        excluded_patterns: normalize_excluded(input.excluded_patterns.clone()),
        batch_size: normalize_batch_size(input.batch_size),
        summary_strategy: normalized_summary_strategy,
        max_cluster_depth: input.max_cluster_depth.filter(|value| *value > 0),
        use_web_search: input.use_web_search.unwrap_or(false),
        web_search_enabled: input.use_web_search.unwrap_or(false)
            && input.search_api_key.as_deref().unwrap_or("").trim().len() > 0,
        selected_model: text_route.model.clone(),
        selected_models: json!({
            "text": routes.get("text").map(|x| x.model.clone()).unwrap_or_else(|| text_route.model.clone()),
            "image": routes.get("image").map(|x| x.model.clone()).unwrap_or_else(|| text_route.model.clone()),
            "video": routes.get("video").map(|x| x.model.clone()).unwrap_or_else(|| text_route.model.clone()),
            "audio": routes.get("audio").map(|x| x.model.clone()).unwrap_or_else(|| text_route.model.clone()),
        }),
        selected_providers: json!({
            "text": routes.get("text").map(|x| x.endpoint.clone()).unwrap_or_else(|| text_route.endpoint.clone()),
            "image": routes.get("image").map(|x| x.endpoint.clone()).unwrap_or_else(|| text_route.endpoint.clone()),
            "video": routes.get("video").map(|x| x.endpoint.clone()).unwrap_or_else(|| text_route.endpoint.clone()),
            "audio": routes.get("audio").map(|x| x.endpoint.clone()).unwrap_or_else(|| text_route.endpoint.clone()),
        }),
        supports_multimodal: supports_multimodal(&text_route.model, &text_route.endpoint),
        tree,
        tree_version,
        total_files: 0,
        processed_files: 0,
        total_batches: 0,
        processed_batches: 0,
        token_usage: TokenUsage::default(),
        results: Vec::new(),
        preview: Vec::new(),
        created_at: now_iso(),
        completed_at: None,
        job_id: None,
    };
    persist::init_organize_task(&state.db_path(), &snapshot)?;
    let task = Arc::new(OrganizeTaskRuntime {
        stop: AtomicBool::new(false),
        snapshot: Mutex::new(snapshot.clone()),
        routes,
        search_api_key: input.search_api_key.unwrap_or_default(),
        response_language: input.response_language.unwrap_or_else(|| "zh".to_string()),
        extraction_tool,
        diagnostics: OrganizerDiagnostics {
            data_dir: state.data_dir(),
            operation_id,
            task_id: task_id.clone(),
        },
        job: Mutex::new(None),
    });
    state
        .organize_tasks
        .lock()
        .insert(task_id.clone(), task.clone());
    let state_clone = state.inner().clone();
    let task_id_clone = task_id.clone();
    let app_clone = app.clone();
    let runtime = task.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let result = run_organize_task(&app_clone, &state_clone, &runtime).await;
        if runtime.stop.load(Ordering::Relaxed) {
            let mut snap = runtime.snapshot.lock();
            snap.status = "stopped".to_string();
            snap.completed_at = Some(now_iso());
            let _ = persist::save_organize_snapshot(&state_clone.db_path(), &snap);
            let payload = serde_json::to_value(&*snap).unwrap_or_else(|_| json!({}));
            drop(snap);
            runtime.diagnostics.record(
                "info",
                "organize_task_stopped",
                "organize task stopped",
                payload.clone(),
                None,
                None,
            );
            let _ = app_clone.emit("organize_stopped", payload);
        } else if let Err(err) = result {
            let mut snap = runtime.snapshot.lock();
            snap.status = "error".to_string();
            snap.error = Some(err.clone());
            snap.completed_at = Some(now_iso());
            let _ = persist::save_organize_snapshot(&state_clone.db_path(), &snap);
            let payload = json!({ "taskId": task_id_clone, "message": err, "snapshot": &*snap });
            drop(snap);
            runtime.diagnostics.record(
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
            let _ = app_clone.emit("organize_error", payload);
        }
        state_clone.organize_tasks.lock().remove(&task_id_clone);
    });
    *task.job.lock() = Some(handle);
    Ok(json!({
        "taskId": task_id,
        "summaryStrategy": snapshot.summary_strategy,
        "selectedModel": snapshot.selected_model,
        "selectedModels": snapshot.selected_models,
        "selectedProviders": snapshot.selected_providers,
        "supportsMultimodal": snapshot.supports_multimodal
    }))
}

pub async fn organize_stop<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Value, String> {
    let task = state
        .organize_tasks
        .lock()
        .get(&task_id)
        .cloned()
        .ok_or_else(|| "Task not found".to_string())?;
    task.stop.store(true, Ordering::Relaxed);
    {
        let mut snapshot = task.snapshot.lock();
        if matches!(snapshot.status.as_str(), "scanning" | "classifying") {
            snapshot.status = "stopping".to_string();
        }
    }
    emit_snapshot(&app, state.inner(), &task).await?;
    Ok(json!({ "success": true }))
}

pub async fn organize_get_result(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Value, String> {
    if let Some(task) = state.organize_tasks.lock().get(&task_id).cloned() {
        let snap = task.snapshot.lock().clone();
        return serde_json::to_value(snap).map_err(|e| e.to_string());
    }
    persist::prepare_organizer_module_access(&state.db_path())?;
    let snapshot = persist::load_organize_snapshot(&state.db_path(), &task_id)?
        .ok_or_else(|| "Task not found".to_string())?;
    serde_json::to_value(planner::hydrate_loaded_snapshot(snapshot)).map_err(|e| e.to_string())
}

pub async fn organize_apply(state: State<'_, AppState>, task_id: String) -> Result<Value, String> {
    let mut snapshot = planner::hydrate_loaded_snapshot(
        persist::load_organize_snapshot(&state.db_path(), &task_id)?
            .ok_or_else(|| "Task not found".to_string())?,
    );
    if snapshot.status != "completed" && snapshot.status != "done" {
        return Err(format!(
            "task status is {}, cannot apply move",
            snapshot.status
        ));
    }
    snapshot.status = "moving".to_string();
    persist::save_organize_snapshot(&state.db_path(), &snapshot)?;

    let plan = planner::build_apply_plan(&snapshot);
    let mut entries = Vec::new();
    for row in &plan {
        let source = PathBuf::from(row.get("sourcePath").and_then(Value::as_str).unwrap_or(""));
        let item_type = row
            .get("itemType")
            .and_then(Value::as_str)
            .unwrap_or("file")
            .to_string();
        let category = sanitize_category_name(
            row.get("category")
                .and_then(Value::as_str)
                .unwrap_or(CATEGORY_OTHER_PENDING),
        );
        let planned_target =
            PathBuf::from(row.get("targetPath").and_then(Value::as_str).unwrap_or(""));
        let target_base = if planned_target.as_os_str().is_empty() {
            let fallback_name = source
                .file_name()
                .and_then(|x| x.to_str())
                .unwrap_or("item");
            PathBuf::from(&snapshot.root_path)
                .join(&category)
                .join(fallback_name)
        } else {
            planned_target
        };
        let target_dir = target_base
            .parent()
            .unwrap_or_else(|| Path::new(&snapshot.root_path))
            .to_path_buf();
        if !source.exists() {
            entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target_base.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "failed",
                "error": "source_not_found"
            }));
            continue;
        }
        if let Err(err) = fs::create_dir_all(&target_dir) {
            entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target_base.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "failed",
                "error": err.to_string()
            }));
            continue;
        }
        let target = planner::resolve_apply_target_path(&source, &target_base);
        if planner::normalize_path_key(&source) == planner::normalize_path_key(&target) {
            entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "skipped",
                "error": Value::Null
            }));
            continue;
        }
        match fs::rename(&source, &target) {
            Ok(_) => entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "moved",
                "error": Value::Null
            })),
            Err(err) => entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "failed",
                "error": err.to_string()
            })),
        }
    }
    let moved = entries
        .iter()
        .filter(|x| x.get("status").and_then(Value::as_str) == Some("moved"))
        .count();
    let skipped = entries
        .iter()
        .filter(|x| x.get("status").and_then(Value::as_str) == Some("skipped"))
        .count();
    let failed = entries
        .iter()
        .filter(|x| x.get("status").and_then(Value::as_str) == Some("failed"))
        .count();
    let job_id = format!("job_{}", Uuid::new_v4().simple());
    let manifest = json!({
        "jobId": job_id,
        "taskId": task_id,
        "rootPath": snapshot.root_path,
        "createdAt": now_iso(),
        "batchSize": snapshot.batch_size,
        "maxClusterDepth": snapshot.max_cluster_depth,
        "recursive": snapshot.recursive,
        "entries": entries,
        "summary": {
            "moved": moved,
            "skipped": skipped,
            "failed": failed,
            "total": entries.len()
        }
    });
    crate::diagnostics::record_state_event(
        state.inner(),
        if failed > 0 { "warn" } else { "info" },
        "organizer",
        "organize_apply_manifest",
        None,
        "organize apply move results",
        json!({
            "taskId": manifest.get("taskId").cloned().unwrap_or(Value::Null),
            "jobId": manifest.get("jobId").cloned().unwrap_or(Value::Null),
            "manifest": manifest.clone(),
        }),
        None,
        None,
    );
    persist::save_organize_manifest(&state.db_path(), &manifest)?;
    snapshot.status = "done".to_string();
    snapshot.job_id = manifest
        .get("jobId")
        .and_then(Value::as_str)
        .map(|x| x.to_string());
    persist::save_organize_snapshot(&state.db_path(), &snapshot)?;
    if let Some(task) = state.organize_tasks.lock().get(&task_id).cloned() {
        *task.snapshot.lock() = snapshot;
    }
    Ok(json!({ "success": true, "manifest": manifest }))
}

pub async fn organize_rollback(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<Value, String> {
    let manifest = persist::load_organize_job(&state.db_path(), &job_id)?
        .ok_or_else(|| "job manifest not found".to_string())?;
    let task_id = manifest
        .get("taskId")
        .and_then(Value::as_str)
        .map(|value| value.to_string());
    let root_path = PathBuf::from(
        manifest
            .get("rootPath")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let mut entries = persist::load_organize_job_entries(&state.db_path(), &job_id)?;
    entries.reverse();
    let mut rollback_entries = Vec::new();
    for entry in entries {
        let item_type = entry
            .get("itemType")
            .and_then(Value::as_str)
            .unwrap_or("file")
            .to_string();
        let source = PathBuf::from(
            entry
                .get("sourcePath")
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        let target = PathBuf::from(
            entry
                .get("targetPath")
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        if entry.get("status").and_then(Value::as_str) != Some("moved") {
            rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "status": "skipped",
                "error": "not_moved_in_apply"
            }));
            continue;
        }
        if !target.exists() {
            rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "status": "failed",
                "error": "target_not_found"
            }));
            continue;
        }
        if source.exists() {
            rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "status": "failed",
                "error": "source_already_exists"
            }));
            continue;
        }
        if let Some(parent) = source.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match fs::rename(&target, &source) {
            Ok(_) => {
                if let Some(target_parent) = target.parent() {
                    planner::prune_empty_dirs_upward(target_parent, &root_path);
                }
                rollback_entries.push(json!({
                    "sourcePath": source.to_string_lossy().to_string(),
                    "targetPath": target.to_string_lossy().to_string(),
                    "itemType": item_type,
                    "status": "rolled_back",
                    "error": Value::Null
                }))
            }
            Err(err) => rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "status": "failed",
                "error": err.to_string()
            })),
        }
    }
    let rollback = json!({
        "at": now_iso(),
        "entries": rollback_entries,
        "summary": {
            "rolledBack": rollback_entries.iter().filter(|x| x.get("status").and_then(Value::as_str) == Some("rolled_back")).count(),
            "failed": rollback_entries.iter().filter(|x| x.get("status").and_then(Value::as_str) == Some("failed")).count(),
            "skipped": rollback_entries.iter().filter(|x| x.get("status").and_then(Value::as_str) == Some("skipped")).count(),
            "total": rollback_entries.len()
        }
    });
    let rollback_failed = rollback
        .get("summary")
        .and_then(|summary| summary.get("failed"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    crate::diagnostics::record_state_event(
        state.inner(),
        if rollback_failed > 0 { "warn" } else { "info" },
        "organizer",
        "organize_rollback_result",
        None,
        "organize rollback results",
        json!({
            "jobId": job_id.clone(),
            "taskId": task_id.clone(),
            "rollback": rollback.clone(),
        }),
        None,
        None,
    );
    persist::save_organize_rollback(&state.db_path(), &job_id, &rollback)?;

    let failed = rollback
        .get("summary")
        .and_then(|summary| summary.get("failed"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if failed == 0 {
        if let Some(task_id) = task_id {
            if let Some(mut snapshot) = persist::load_organize_snapshot(&state.db_path(), &task_id)?
            {
                snapshot = planner::hydrate_loaded_snapshot(snapshot);
                snapshot.status = "completed".to_string();
                snapshot.job_id = None;
                persist::save_organize_snapshot(&state.db_path(), &snapshot)?;
                if let Some(task) = state.organize_tasks.lock().get(&task_id).cloned() {
                    *task.snapshot.lock() = snapshot;
                }
            }
        }
    }

    Ok(json!({
        "success": true,
        "jobId": manifest.get("jobId").and_then(Value::as_str).unwrap_or(&job_id),
        "rollback": rollback
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use uuid::Uuid;

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wipeout-organizer-{name}-{}", Uuid::new_v4()))
    }

    fn write_file(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, b"test").expect("write file");
    }

    fn write_text_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, content.as_bytes()).expect("write text file");
    }

    fn make_test_unit(path: &Path) -> OrganizeUnit {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string();
        OrganizeUnit {
            name,
            path: path.to_string_lossy().to_string(),
            relative_path: path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_string(),
            size: fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
            created_at: None,
            modified_at: None,
            item_type: "file".to_string(),
            modality: "text".to_string(),
            directory_assessment: None,
        }
    }

    #[test]
    fn ensure_path_creates_nested_tree() {
        let mut tree = default_tree();
        let leaf = ensure_path(&mut tree, &["group".to_string(), "leaf".to_string()]);
        let path = category_path_for_id(&tree, &leaf).expect("path");
        assert_eq!(path, vec!["group".to_string(), "leaf".to_string()]);
    }

    #[test]
    fn build_preview_uses_nested_category_path() {
        let preview = planner::build_preview(
            r"C:\root",
            &[json!({
                "name": "foo.txt",
                "path": r"C:\root\foo.txt",
                "itemType": "file",
                "leafNodeId": "leaf",
                "categoryPath": ["group", "leaf"]
            })],
        );
        assert_eq!(
            preview[0].get("targetPath").and_then(Value::as_str),
            Some(r"C:\root\group\leaf\foo.txt")
        );
    }

    #[test]
    fn build_preview_skips_classification_error_rows() {
        let preview = planner::build_preview(
            r"C:\root",
            &[
                json!({
                    "name": "bad.txt",
                    "path": r"C:\root\bad.txt",
                    "itemType": "file",
                    "reason": RESULT_REASON_CLASSIFICATION_ERROR,
                    "category": CATEGORY_CLASSIFICATION_ERROR
                }),
                json!({
                    "name": "good.txt",
                    "path": r"C:\root\good.txt",
                    "itemType": "file",
                    "leafNodeId": "leaf",
                    "categoryPath": ["group", "leaf"]
                }),
            ],
        );
        assert_eq!(preview.len(), 1);
        assert_eq!(
            preview[0].get("sourcePath").and_then(Value::as_str),
            Some(r"C:\root\good.txt")
        );
    }

    #[test]
    fn build_apply_plan_skips_classification_error_rows_even_if_preview_is_stale() {
        let snapshot = OrganizeSnapshot {
            id: "task_1".to_string(),
            status: "completed".to_string(),
            error: None,
            root_path: r"C:\root".to_string(),
            recursive: true,
            excluded_patterns: Vec::new(),
            batch_size: 20,
            summary_strategy: SUMMARY_MODE_FILENAME_ONLY.to_string(),
            max_cluster_depth: None,
            use_web_search: false,
            web_search_enabled: false,
            selected_model: "deepseek-chat".to_string(),
            selected_models: json!({}),
            selected_providers: json!({}),
            supports_multimodal: false,
            tree: json!({}),
            tree_version: 0,
            processed_files: 1,
            total_files: 1,
            processed_batches: 1,
            total_batches: 1,
            token_usage: TokenUsage::default(),
            results: vec![json!({
                "name": "bad.txt",
                "path": r"C:\root\bad.txt",
                "itemType": "file",
                "reason": RESULT_REASON_CLASSIFICATION_ERROR,
                "category": CATEGORY_CLASSIFICATION_ERROR
            })],
            preview: vec![json!({
                "sourcePath": r"C:\root\bad.txt",
                "category": CATEGORY_OTHER_PENDING,
                "categoryPath": ["鍏朵粬寰呭畾"],
                "leafNodeId": "leaf",
                "targetPath": r"C:\root\鍏朵粬寰呭畾\bad.txt",
                "itemType": "file"
            })],
            created_at: "2026-03-28T00:00:00Z".to_string(),
            completed_at: None,
            job_id: None,
        };
        let plan = planner::build_apply_plan(&snapshot);
        assert!(plan.is_empty());
    }

    #[test]
    fn normalize_summary_strategy_defaults_to_filename_only() {
        assert_eq!(
            normalize_summary_mode(None),
            SUMMARY_MODE_FILENAME_ONLY.to_string()
        );
        assert_eq!(
            normalize_summary_mode(Some("local_summary")),
            SUMMARY_MODE_LOCAL_SUMMARY.to_string()
        );
        assert_eq!(
            normalize_summary_mode(Some("agent_summary")),
            SUMMARY_MODE_AGENT_SUMMARY.to_string()
        );
        assert_eq!(
            normalize_summary_mode(Some("bad-mode")),
            SUMMARY_MODE_FILENAME_ONLY.to_string()
        );
    }

    #[test]
    fn relative_age_uses_compact_backend_format() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-06T12:00:00Z")
            .expect("parse now")
            .with_timezone(&chrono::Utc);

        assert_eq!(
            compute_relative_age_at(Some("2026-04-06T11:59:45Z"), now).as_deref(),
            Some("lt1m")
        );
        assert_eq!(
            compute_relative_age_at(Some("2026-04-06T09:00:00Z"), now).as_deref(),
            Some("3h")
        );
        assert_eq!(
            compute_relative_age_at(Some("2026-03-30T12:00:00Z"), now).as_deref(),
            Some("1w")
        );
        assert_eq!(
            compute_relative_age_at(Some("2025-12-06T12:00:00Z"), now).as_deref(),
            Some("4mo")
        );
        assert_eq!(
            compute_relative_age_at(Some("2024-04-06T12:00:00Z"), now).as_deref(),
            Some("2y")
        );
        assert_eq!(compute_relative_age_at(Some("bad-date"), now), None);
        assert_eq!(compute_relative_age_at(None, now), None);
    }

    #[test]
    fn local_summary_skips_large_plain_text_inputs() {
        let root = temp_dir("large-text-summary");
        let path = root.join("notes.txt");
        write_text_file(&path, "small content");
        let mut unit = make_test_unit(&path);
        unit.size = LOCAL_SUMMARY_MAX_PLAIN_TEXT_BYTES + 1;
        let extracted = summary::extract_plain_text_summary(&unit);
        assert!(extracted.excerpt.is_empty());
        assert!(extracted
            .warnings
            .iter()
            .any(|warning| warning.starts_with("summary_input_too_large:")));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn local_summary_falls_back_to_filename_for_unsupported_file() {
        let root = temp_dir("filename-fallback");
        let path = root.join("archive.bin");
        write_text_file(&path, "not actually parsed");
        let mut unit = make_test_unit(&path);
        unit.modality = "binary".to_string();
        let stop = AtomicBool::new(false);
        let extracted = summary::extract_unit_content_for_summary(&unit, "zh-CN", &stop);
        let summary = summary::build_local_summary(&unit, &extracted);
        assert!(extracted.excerpt.is_empty());
        assert_eq!(summary.representation.source, SUMMARY_SOURCE_FILENAME_ONLY);
        assert!(summary.representation.degraded);
        assert_eq!(
            summary.representation.metadata.as_deref(),
            Some("archive.bin")
        );
        assert!(summary.representation.short.is_none());
        assert!(summary.representation.long.is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn legacy_tika_defaults_are_upgraded_to_auto_start() {
        let config = extraction_tool_config_from_settings(&json!({
            "contentExtraction": {
                "tika": {
                    "enabled": false,
                    "autoStart": false,
                    "url": DEFAULT_TIKA_URL,
                    "jarPath": ""
                }
            }
        }));
        assert!(config.tika_enabled);
        assert!(config.tika_auto_start);
        assert!(!config.tika_ready);
    }

    #[test]
    fn summary_modes_force_enable_tika_runtime() {
        let mut config = ExtractionToolConfig {
            tika_enabled: false,
            tika_url: String::new(),
            tika_auto_start: false,
            tika_jar_path: String::new(),
            tika_ready: false,
        };

        force_enable_tika_for_summary_mode(&mut config);

        assert!(config.tika_enabled);
        assert!(config.tika_auto_start);
        assert_eq!(config.tika_url, DEFAULT_TIKA_URL.to_string());
    }

    #[test]
    fn local_summary_does_not_parse_pdf_binary_as_plain_text() {
        let root = temp_dir("pdf-fallback");
        let path = root.join("paper.pdf");
        write_text_file(&path, "%PDF-1.7\nstream\n\x00\x01");
        let mut unit = make_test_unit(&path);
        unit.modality = "text".to_string();
        let stop = AtomicBool::new(false);

        let extracted = summary::extract_unit_content_for_summary(&unit, "zh-CN", &stop);
        assert_eq!(extracted.parser, "unavailable");
        assert!(extracted.excerpt.is_empty());
        assert!(extracted
            .warnings
            .iter()
            .any(|warning| warning == "filename_only_fallback"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_summary_agent_output_reads_items() {
        let parsed = summary::parse_summary_agent_output(
            r#"{
                "items": [
                    {
                        "itemId": "batch1_1",
                        "summaryShort": "预算表，包含负责人和金额。",
                        "summaryLong": "预算表，包含项目负责人、金额等预算信息。",
                        "keywords": ["预算", "项目", "金额"],
                        "confidence": "high",
                        "warnings": ["source_sparse"]
                    }
                ]
            }"#,
        )
        .expect("parse summary agent output");
        let item = parsed.get("batch1_1").expect("item exists");
        assert_eq!(item.summary_short, "预算表，包含负责人和金额。");
        assert_eq!(
            item.summary_long,
            "预算表，包含项目负责人、金额等预算信息。"
        );
        assert_eq!(item.keywords, vec!["预算", "项目", "金额"]);
        assert_eq!(item.confidence.as_deref(), Some("high"));
        assert_eq!(item.warnings, vec!["source_sparse"]);
    }

    #[test]
    fn classification_batch_items_exclude_raw_extraction_fields() {
        let items = summary::build_classification_batch_items(&[json!({
            "itemId": "batch1_1",
            "name": "report.pdf",
            "path": "E:\\docs\\report.pdf",
            "relativePath": "docs\\report.pdf",
            "size": 1234,
            "createdAt": "2026-04-01T00:00:00Z",
            "modifiedAt": "2026-04-05T00:00:00Z",
            "itemType": "file",
            "modality": "text",
            "representation": {
                "metadata": "report.pdf，document，1.2 KB",
                "short": "季度财务报告",
                "long": "季度财务报告，包含季度财务指标与结论。",
                "source": "agent_summary",
                "degraded": false,
                "confidence": "high",
                "keywords": ["财务", "季度"]
            },
            "summaryKeywords": ["财务", "季度"],
            "summaryWarnings": ["source_sparse"],
            "localExtraction": {
                "parser": "tika",
                "excerpt": "very long raw extraction text"
            }
        })]);

        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(
            item.get("summaryText").and_then(Value::as_str),
            Some("季度财务报告，包含季度财务指标与结论。")
        );
        assert_eq!(
            item.pointer("/representation/keywords")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2),
        );
        assert_eq!(
            item.pointer("/representation/source")
                .and_then(Value::as_str),
            Some("agent_summary")
        );
        assert!(item.get("createdAge").is_some());
        assert!(item.get("modifiedAge").is_some());
        assert!(item.get("localExtraction").is_none());
        assert!(item.get("path").is_none());
        assert!(item.get("size").is_none());
        assert!(item.get("createdAt").is_none());
        assert!(item.get("modifiedAt").is_none());
    }

    #[test]
    fn collection_root_detects_download_like_root() {
        let root = temp_dir("download-root").join("Download");
        fs::create_dir_all(root.join("Buzz-1.4.2-Windows-X64")).expect("create buzz dir");
        fs::create_dir_all(root.join("QuickRestart")).expect("create quick dir");
        fs::create_dir_all(root.join("Fonts")).expect("create fonts dir");
        fs::create_dir_all(root.join("Docs")).expect("create docs dir");
        write_file(&root.join("setup.exe"));
        write_file(&root.join("paper.pdf"));
        write_file(&root.join("archive.zip"));
        write_file(&root.join("image.png"));

        let stop = AtomicBool::new(false);
        assert!(is_collection_root(&root, &normalize_excluded(None), &stop));

        let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
    }

    #[test]
    fn evaluates_whole_for_app_bundle_directory() {
        let root = temp_dir("app-bundle");
        fs::create_dir_all(&root).expect("create root");
        write_file(&root.join("Buzz-1.4.2-windows.exe"));
        write_file(&root.join("Buzz-1.4.2-windows-1.bin"));
        write_file(&root.join("Buzz-1.4.2-windows-2.bin"));

        let stop = AtomicBool::new(false);
        let assessment =
            evaluate_directory_assessment(&root, &stop, true).expect("assessment exists");
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_whole_for_plugin_bundle_directory() {
        let root = temp_dir("plugin-bundle");
        fs::create_dir_all(&root).expect("create root");
        write_file(&root.join("QuickRestart.dll"));
        write_file(&root.join("QuickRestart.json"));
        write_file(&root.join("QuickRestart.pck"));

        let stop = AtomicBool::new(false);
        let assessment =
            evaluate_directory_assessment(&root, &stop, true).expect("assessment exists");
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_whole_for_font_pack_directory() {
        let root = temp_dir("font-pack");
        fs::create_dir_all(&root).expect("create root");
        write_file(&root.join("generica.otf"));
        write_file(&root.join("generica bold.otf"));

        let stop = AtomicBool::new(false);
        let assessment =
            evaluate_directory_assessment(&root, &stop, false).expect("assessment exists");
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_whole_for_document_bundle_directory() {
        let root = temp_dir("doc-bundle");
        fs::create_dir_all(root.join("我的女友是冒险游戏（待续）")).expect("create child dir");
        for idx in 0..8 {
            write_file(&root.join(format!("chapter-{idx}.txt")));
        }
        write_file(&root.join("collection.zip"));
        write_file(&root.join("extras.zip"));

        let stop = AtomicBool::new(false);
        let assessment =
            evaluate_directory_assessment(&root, &stop, true).expect("assessment exists");
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_wrapper_passthrough_for_single_child_shell() {
        let root = temp_dir("wrapper");
        let shell = root.join("DwnlData");
        let target = shell.join("32858");
        fs::create_dir_all(&target).expect("create target");
        write_file(&target.join("app.exe"));
        write_file(&target.join("payload.bin"));

        let stop = AtomicBool::new(false);
        let assessment =
            evaluate_directory_assessment(&shell, &stop, true).expect("assessment exists");
        assert_eq!(
            assessment.result_kind,
            DirectoryResultKind::WholeWrapperPassthrough
        );
        assert_eq!(
            assessment.wrapper_target_path.as_deref(),
            Some(target.to_string_lossy().as_ref())
        );

        let units = collect_units(&root, true, &normalize_excluded(None), &stop);
        assert!(units
            .iter()
            .any(|unit| unit.item_type == "directory"
                && unit.relative_path.ends_with("DwnlData\\32858")));
        assert!(!units
            .iter()
            .any(|unit| unit.item_type == "directory" && unit.relative_path == "DwnlData"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_mixed_split_for_mixed_directory() {
        let root = temp_dir("mixed");
        fs::create_dir_all(root.join("photos")).expect("create photos dir");
        fs::create_dir_all(root.join("docs")).expect("create docs dir");
        fs::create_dir_all(root.join("tools")).expect("create tools dir");
        write_file(&root.join("setup.exe"));
        write_file(&root.join("paper.pdf"));
        write_file(&root.join("cover.png"));
        write_file(&root.join("song.mp3"));
        write_file(&root.join("font.ttf"));
        write_file(&root.join("notes.txt"));

        let stop = AtomicBool::new(false);
        let assessment =
            evaluate_directory_assessment(&root, &stop, false).expect("assessment exists");
        assert_eq!(assessment.result_kind, DirectoryResultKind::MixedSplit);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_staging_junk_for_runtime_cache_shell() {
        let root = temp_dir("junk");
        fs::create_dir_all(root.join("logs")).expect("create logs dir");
        fs::create_dir_all(root.join("cache")).expect("create cache dir");
        write_file(&root.join("telemetry_cache.json"));
        write_file(&root.join("update_cache.json"));
        write_file(&root.join("session.dat"));

        let stop = AtomicBool::new(false);
        let assessment =
            evaluate_directory_assessment(&root, &stop, false).expect("assessment exists");
        assert_eq!(assessment.result_kind, DirectoryResultKind::StagingJunk);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_whole_for_complex_windows_app_directory() {
        let root = temp_dir("windows-app");
        fs::create_dir_all(root.join("Config")).expect("create config dir");
        fs::create_dir_all(root.join("Images")).expect("create images dir");
        write_file(&root.join("App.exe"));
        for idx in 0..10 {
            write_file(&root.join(format!("runtime-{idx}.dll")));
        }
        for idx in 0..6 {
            write_file(&root.join(format!("asset-{idx}.png")));
        }
        for idx in 0..6 {
            write_file(&root.join(format!("strings-{idx}.res")));
        }
        write_file(&root.join("App.exe.config"));
        write_file(&root.join("readme.txt"));

        let stop = AtomicBool::new(false);
        let assessment =
            evaluate_directory_assessment(&root, &stop, true).expect("assessment exists");
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
        assert_eq!(assessment.integrity_kind, "app_bundle");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_whole_for_multi_variant_package_with_docs() {
        let root = temp_dir("dism-bundle").join("Dism++10.1.1002.1B");
        fs::create_dir_all(root.join("Config")).expect("create config dir");
        write_file(&root.join("Dism++ARM64.exe"));
        write_file(&root.join("Dism++x64.exe"));
        write_file(&root.join("Dism++x86.exe"));
        write_file(&root.join("ReadMe for NCleaner.txt"));
        write_file(&root.join("ReadMe for Dism++x86.txt"));
        write_file(&root.join("Dism++x86 usage notes.txt"));

        let stop = AtomicBool::new(false);
        let assessment =
            evaluate_directory_assessment(&root, &stop, true).expect("assessment exists");
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
        assert_eq!(assessment.integrity_kind, "app_bundle");

        let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
    }

    #[test]
    fn evaluates_whole_for_cursor_theme_pack_directory() {
        let root = temp_dir("cursor-pack").join("Nagasaki Soyo-theme-pack");
        fs::create_dir_all(root.join("optional-replacements")).expect("create alt dir");
        for name in [
            "Alternate.ani",
            "Busy.ani",
            "Diagonal Resize 1.ani",
            "Diagonal Resize 2.ani",
            "Help Select.ani",
            "Horizontal Resize.ani",
            "Link.ani",
            "Location Select.ani",
            "Move.ani",
            "Normal Select.ani",
            "Person Select.ani",
            "Precision Select.ani",
            "Text Select.ani",
            "Unavailable.ani",
            "Vertical Resize.ani",
            "Work.ani",
            "Arrow.cur",
            "Hand.cur",
        ] {
            write_file(&root.join(name));
        }
        write_file(&root.join("cursor-preview.jpg"));
        write_file(&root.join("license.txt"));
        write_file(&root.join("install.inf"));

        let stop = AtomicBool::new(false);
        let assessment =
            evaluate_directory_assessment(&root, &stop, true).expect("assessment exists");
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
        assert_eq!(assessment.integrity_kind, "theme_pack");

        let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
    }

    #[test]
    fn evaluates_whole_for_document_collection_with_weak_name_families() {
        let root = temp_dir("doc-collection").join("MC-article-collection-1");
        fs::create_dir_all(root.join("我的女友是冒险游戏（待续）")).expect("create child dir");
        for name in [
            "article-01-prologue.txt",
            "article-02-background.txt",
            "article-03-character-notes.txt",
            "article-04-worldbuilding.txt",
            "article-05-chapter-outline.txt",
            "article-06-dialogue-draft.txt",
            "article-07-side-story.txt",
            "article-08-ending-notes.txt",
            "article-09-reading-guide.txt",
            "article-10-author-commentary.txt",
            "article-11-extra-scenes.txt",
            "article-12-appendix.txt",
        ] {
            write_file(&root.join(name));
        }
        for name in [
            "MC-article-collection-1.zip",
            "article-drafts-backup.zip",
            "reading-materials.zip",
            "game-notes-archive.zip",
            "extras.7z",
        ] {
            write_file(&root.join(name));
        }

        let stop = AtomicBool::new(false);
        let assessment =
            evaluate_directory_assessment(&root, &stop, true).expect("assessment exists");
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
        assert_eq!(assessment.integrity_kind, "doc_bundle");

        let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
    }

    #[test]
    fn evaluates_whole_for_single_installer_with_readme() {
        let root = temp_dir("installer-docs").join("IDM-main");
        fs::create_dir_all(&root).expect("create root");
        write_file(&root.join("IDM_v6.41.2_Setup_by-System3206.exe"));
        write_file(&root.join("README.md"));

        let stop = AtomicBool::new(false);
        let assessment =
            evaluate_directory_assessment(&root, &stop, true).expect("assessment exists");
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
        assert_eq!(assessment.integrity_kind, "app_bundle");

        let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
    }

    #[test]
    fn dll_only_directory_does_not_become_whole() {
        let root = temp_dir("dll-only");
        fs::create_dir_all(&root).expect("create root");
        write_file(&root.join("a.dll"));
        write_file(&root.join("b.dll"));
        write_file(&root.join("c.dll"));

        let stop = AtomicBool::new(false);
        let assessment =
            evaluate_directory_assessment(&root, &stop, true).expect("assessment exists");
        assert_ne!(assessment.result_kind, DirectoryResultKind::Whole);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_chat_completion_http_body_extracts_content_and_usage() {
        let raw_body = r#"{
          "choices": [
            {
              "message": {
                "content": "{\"tree\":{\"name\":\"\",\"nodeId\":\"root\",\"children\":[]},\"assignments\":[]}"
              }
            }
          ],
          "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 34,
            "total_tokens": 46
          }
        }"#;
        let parsed = summary::parse_chat_completion_http_body(
            "https://api.openai.com/v1",
            StatusCode::OK,
            raw_body,
        )
        .expect("parse success");
        assert!(parsed.content.contains("\"assignments\":[]"));
        assert_eq!(parsed.usage.prompt, 12);
        assert_eq!(parsed.usage.completion, 34);
        assert_eq!(parsed.usage.total, 46);
    }

    #[test]
    fn parse_chat_completion_http_body_keeps_raw_body_on_decode_error() {
        let raw_body = "<html>upstream gateway error</html>";
        let err = summary::parse_chat_completion_http_body(
            "https://api.openai.com/v1",
            StatusCode::OK,
            raw_body,
        )
        .expect_err("decode error");
        assert!(err.message.contains("error decoding response body"));
        assert!(err.message.contains("upstream gateway error"));
        assert_eq!(err.raw_body, raw_body);
    }

    #[test]
    fn parse_chat_completion_http_body_accepts_tool_calls_without_text() {
        let raw_body = r#"{
          "choices": [
            {
              "message": {
                "content": null,
                "tool_calls": [
                  {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                      "name": "submit_organize_result",
                      "arguments": "{\"tree\":{\"nodeId\":\"root\",\"name\":\"\",\"children\":[]},\"assignments\":[]}"
                    }
                  }
                ]
              }
            }
          ],
          "usage": {
            "prompt_tokens": 3,
            "completion_tokens": 5,
            "total_tokens": 8
          }
        }"#;
        let parsed = summary::parse_chat_completion_http_body(
            "https://api.openai.com/v1",
            StatusCode::OK,
            raw_body,
        )
        .expect("parse success");
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "submit_organize_result");
        assert_eq!(parsed.usage.total, 8);
    }
}
