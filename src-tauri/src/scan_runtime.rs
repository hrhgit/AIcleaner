use crate::backend::{
    self, AppError, AppResult, AppState, ScanPersistentRuleRecord, ScanPersistentRules,
    ScanResultItem, ScanRuleTopChild, ScanSnapshot, ScanStartInput, TokenUsage,
};
use crate::persist;
use crate::web_search::{
    format_web_search_context, parse_web_search_request, tavily_search, web_search_trace_to_value,
};
use parking_lot::Mutex;
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use uuid::Uuid;

const TARGET_SIZE_MIN_GB: f64 = 0.1;
const TARGET_SIZE_DEFAULT_MAX_GB: f64 = 20.0;
const SCAN_PROGRESS_PERSIST_INTERVAL: Duration = Duration::from_millis(1000);
const CHAT_COMPLETION_TIMEOUT_SECS: u64 = 180;
const RESPONSE_ERROR_SNIPPET_CHARS: usize = 400;
const SOURCE_LOCAL_RULE: &str = "local_rule";
const SOURCE_PERSISTENT_RULE: &str = "persistent_rule";
const SOURCE_BASELINE_REUSE: &str = "baseline_reuse";
const SOURCE_AI: &str = "ai";
const CLASS_DELETE_ALL: &str = "delete_all";
const CLASS_KEEP_ALL: &str = "keep_all";
const CLASS_EXPAND_ANALYSIS: &str = "expand_analysis";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanMode {
    FullRescanIncremental,
    DeepenIncremental,
}

impl ScanMode {
    fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or("").trim() {
            "deepen_incremental" => Self::DeepenIncremental,
            _ => Self::FullRescanIncremental,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::FullRescanIncremental => "full_rescan_incremental",
            Self::DeepenIncremental => "deepen_incremental",
        }
    }
}

fn round_up_to_tenth(value: f64) -> f64 {
    (value * 10.0).ceil() / 10.0
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ScanPruneRule {
    SkipSubtree {
        path: String,
    },
    CapSubtreeDepth {
        path: String,
        max_relative_depth: u32,
    },
    SkipReparsePoints,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PruneRuleJson {
    SkipSubtree {
        path: String,
    },
    CapSubtreeDepth {
        path: String,
        max_relative_depth: u32,
    },
    SkipReparsePoints,
}

impl From<&ScanPruneRule> for PruneRuleJson {
    fn from(value: &ScanPruneRule) -> Self {
        match value {
            ScanPruneRule::SkipSubtree { path } => Self::SkipSubtree { path: path.clone() },
            ScanPruneRule::CapSubtreeDepth {
                path,
                max_relative_depth,
            } => Self::CapSubtreeDepth {
                path: path.clone(),
                max_relative_depth: *max_relative_depth,
            },
            ScanPruneRule::SkipReparsePoints => Self::SkipReparsePoints,
        }
    }
}

fn normalize_scan_path_key(value: &str) -> String {
    let mut normalized = value.trim().replace('/', "\\").to_lowercase();
    while normalized.len() > 3 && normalized.ends_with('\\') {
        normalized.pop();
    }
    normalized
}

fn is_same_or_descendant_path(path: &str, parent: &str) -> bool {
    path == parent || path.starts_with(&format!("{parent}\\"))
}

fn relative_path_depth(path: &str, parent: &str) -> Option<u32> {
    if !is_same_or_descendant_path(path, parent) {
        return None;
    }
    if path == parent {
        return Some(0);
    }
    let suffix = path.strip_prefix(parent)?.strip_prefix('\\')?;
    Some(suffix.split('\\').count() as u32)
}

fn is_c_drive_root(path: &str) -> bool {
    normalize_scan_path_key(path) == "c:\\"
}

fn build_scan_prune_rules(target_path: &str) -> Vec<ScanPruneRule> {
    if !is_c_drive_root(target_path) {
        return Vec::new();
    }

    vec![
        ScanPruneRule::SkipSubtree {
            path: "C:\\System Volume Information".to_string(),
        },
        ScanPruneRule::SkipSubtree {
            path: "C:\\Recovery".to_string(),
        },
        ScanPruneRule::SkipSubtree {
            path: "C:\\$Recycle.Bin".to_string(),
        },
        ScanPruneRule::SkipSubtree {
            path: "C:\\Documents and Settings".to_string(),
        },
        ScanPruneRule::SkipSubtree {
            path: "C:\\Config.Msi".to_string(),
        },
        ScanPruneRule::SkipSubtree {
            path: "C:\\PerfLogs".to_string(),
        },
        ScanPruneRule::CapSubtreeDepth {
            path: "C:\\Windows".to_string(),
            max_relative_depth: 2,
        },
        ScanPruneRule::CapSubtreeDepth {
            path: "C:\\Program Files".to_string(),
            max_relative_depth: 2,
        },
        ScanPruneRule::CapSubtreeDepth {
            path: "C:\\Program Files (x86)".to_string(),
            max_relative_depth: 2,
        },
        ScanPruneRule::CapSubtreeDepth {
            path: "C:\\ProgramData".to_string(),
            max_relative_depth: 3,
        },
        ScanPruneRule::SkipReparsePoints,
    ]
}

fn should_exclude_deepen_boundary_node(
    node: &persist::ScanNode,
    prune_rules: &[ScanPruneRule],
) -> bool {
    if node.child_count == 0 {
        return true;
    }

    let path_key = normalize_scan_path_key(&node.path);
    prune_rules.iter().any(|rule| match rule {
        ScanPruneRule::SkipSubtree { path } => {
            let rule_path = normalize_scan_path_key(path);
            is_same_or_descendant_path(&path_key, &rule_path)
        }
        ScanPruneRule::CapSubtreeDepth {
            path,
            max_relative_depth,
        } => {
            let rule_path = normalize_scan_path_key(path);
            relative_path_depth(&path_key, &rule_path)
                .map(|depth| depth >= *max_relative_depth)
                .unwrap_or(false)
        }
        ScanPruneRule::SkipReparsePoints => false,
    })
}

fn resolve_deepen_boundary_nodes(
    db_path: &Path,
    baseline: &ScanSnapshot,
    requested_max_depth: Option<u32>,
    prune_rules: &[ScanPruneRule],
) -> Result<(u32, Vec<persist::ScanNode>), String> {
    let boundary_depth = baseline.max_scanned_depth;
    if requested_max_depth
        .map(|value| value <= boundary_depth)
        .unwrap_or(false)
    {
        return Err("New maxDepth must be greater than the current depth".to_string());
    }
    let nodes = persist::list_boundary_scan_nodes(db_path, &baseline.id, boundary_depth)?
        .into_iter()
        .filter(|node| !should_exclude_deepen_boundary_node(node, prune_rules))
        .collect::<Vec<_>>();
    if nodes.is_empty() {
        return Err("No boundary directories available for deeper scan".to_string());
    }
    Ok((boundary_depth, nodes))
}

#[derive(Clone, Debug, serde::Serialize)]
struct SidecarRoot {
    path: String,
    parent_id: Option<String>,
    depth: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DirectoryExtensionCount {
    extension: String,
    count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DirectoryPortrait {
    direct_file_count: u64,
    direct_dir_count: u64,
    top_children: Vec<ScanRuleTopChild>,
    top_file_extensions: Vec<DirectoryExtensionCount>,
    name_tags: Vec<String>,
    freshness_bucket: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NodeFingerprint {
    size: u64,
    self_size: u64,
    child_count: u64,
    name_tags: Vec<String>,
    top_children: Vec<ScanRuleTopChild>,
}

#[derive(Clone, Debug)]
struct LocalRuleDecision {
    classification: String,
    reason: String,
    risk: String,
    should_expand: bool,
    source: &'static str,
}

#[derive(Clone)]
struct IncrementalContext {
    baseline_nodes: HashMap<String, persist::ScanNode>,
    baseline_findings: HashMap<String, persist::ScanFindingRecord>,
    baseline_children_by_parent: HashMap<String, Vec<persist::ScanNode>>,
    analyze_roots: Vec<persist::ScanNode>,
    deleted_count: u64,
}

#[derive(Clone)]
struct InPlaceBaselineCache {
    nodes: HashMap<String, persist::ScanNode>,
    findings: HashMap<String, persist::ScanFindingRecord>,
}

#[derive(Clone)]
struct ScanAiConfig {
    endpoint: String,
    api_key: String,
    model: String,
    use_web_search: bool,
    search_api_key: String,
    response_language: String,
}

pub struct ScanTaskRuntime {
    pub stop: AtomicBool,
    pub child_pid: Mutex<Option<u32>>,
    pub snapshot: Mutex<ScanSnapshot>,
    pub max_depth: Option<u32>,
    scan_mode: ScanMode,
    baseline_task_id: Option<String>,
    sidecar_roots: Vec<SidecarRoot>,
    ignored_paths: Vec<String>,
    prune_rules: Vec<ScanPruneRule>,
    in_place_baseline: Option<InPlaceBaselineCache>,
    persistent_rules: Mutex<ScanPersistentRules>,
    last_progress_persist_at: Mutex<Option<Instant>>,
    ai: ScanAiConfig,
    pub job: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone, Debug)]
struct ScanReview {
    classification: String,
    reason: String,
    risk: String,
    token_usage: TokenUsage,
    trace: Value,
}

#[derive(Clone, Debug)]
struct ChatResponse {
    raw_body: String,
    content: String,
    token_usage: TokenUsage,
}

#[derive(Debug)]
struct ChatCompletionError {
    message: String,
    raw_body: String,
}

impl ChatCompletionError {
    fn into_app_error(self, ai: &ScanAiConfig, stage: &str) -> AppError {
        let code = if self.raw_body.is_empty() {
            "HTTP_REQUEST_FAILED"
        } else if self.message.contains("HTTP ") {
            "HTTP_BAD_STATUS"
        } else {
            "MODEL_RESPONSE_INVALID"
        };
        AppError::new(code, self.message.clone(), "扫描分析服务调用失败")
            .with_operation("scan_task")
            .with_endpoint(ai.endpoint.clone())
            .with_model(ai.model.clone())
            .with_stage(stage)
            .retryable(code == "HTTP_REQUEST_FAILED")
    }
}

fn build_scan_task_error(task: &ScanTaskRuntime, task_id: &str, root_path: &str, err: String) -> AppError {
    let trimmed = err.trim();
    let stage = if trimmed.is_empty() {
        "scan_task"
    } else if trimmed.contains("chat completion")
        || trimmed.contains("response body")
        || trimmed.contains("HTTP ")
        || trimmed.contains("web search")
    {
        "analysis"
    } else if trimmed.contains("scanner.exe")
        || trimmed.contains("scan_data")
        || trimmed.contains("scan stream")
    {
        "scan"
    } else if trimmed.contains("prepare_incremental") || trimmed.contains("baseline") {
        "prepare"
    } else {
        "scan_task"
    };

    AppError::internal(err)
        .with_operation("scan_task")
        .with_task_id(task_id.to_string())
        .with_path(root_path.to_string())
        .with_endpoint(task.ai.endpoint.clone())
        .with_model(task.ai.model.clone())
        .with_stage(stage)
}

fn log_scan_task_error(error: &AppError) {
    log::error!(
        "scan task failed: task_id={}, operation={}, root_path={}, stage={}, error_code={}, error_message={}, endpoint={}, model={}, http_status={}",
        error.context.task_id.as_deref().unwrap_or(""),
        error.context.operation,
        error.context.path.as_deref().unwrap_or(""),
        error.context.stage.as_deref().unwrap_or(""),
        error.code,
        error.message,
        error.context.endpoint.as_deref().unwrap_or(""),
        error.context.model.as_deref().unwrap_or(""),
        error
            .context
            .http_status
            .map(|value| value.to_string())
            .unwrap_or_default()
    );
}

fn format_size(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else if n < 1024 * 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GB", n as f64 / 1024.0 / 1024.0 / 1024.0)
    }
}

fn extract_json_text(content: &str) -> String {
    let mut clean = content.trim().to_string();
    if clean.starts_with("```json") {
        clean = clean.replacen("```json", "", 1);
    } else if clean.starts_with("```") {
        clean = clean.replacen("```", "", 1);
    }
    if clean.ends_with("```") {
        clean.truncate(clean.len().saturating_sub(3));
    }
    clean.trim().to_string()
}

fn summarize_response_body_for_error(raw_body: &str) -> String {
    let trimmed = raw_body.trim();
    if trimmed.is_empty() {
        return "empty body".to_string();
    }
    let snippet: String = trimmed.chars().take(RESPONSE_ERROR_SNIPPET_CHARS).collect();
    if trimmed.chars().count() > RESPONSE_ERROR_SNIPPET_CHARS {
        format!("{snippet}...")
    } else {
        snippet
    }
}

fn extract_message_content(value: Option<&Value>) -> String {
    value
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(s.to_string())
            } else {
                v.as_array().map(|parts| {
                    parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            }
        })
        .unwrap_or_default()
}

fn parse_chat_completion_http_body(
    status: StatusCode,
    raw_body: &str,
) -> Result<ChatResponse, ChatCompletionError> {
    let body: Value = serde_json::from_str(raw_body).map_err(|e| ChatCompletionError {
        message: format!(
            "error decoding response body: {} | body: {}",
            e,
            summarize_response_body_for_error(raw_body)
        ),
        raw_body: raw_body.to_string(),
    })?;
    if !status.is_success() {
        let api_message = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("chat completion failed");
        return Err(ChatCompletionError {
            message: format!("{} (HTTP {})", api_message, status.as_u16()),
            raw_body: raw_body.to_string(),
        });
    }
    let content = extract_message_content(body.pointer("/choices/0/message/content"));
    if content.trim().is_empty() {
        return Err(ChatCompletionError {
            message: format!(
                "chat completion response missing choices[0].message.content | body: {}",
                summarize_response_body_for_error(raw_body)
            ),
            raw_body: raw_body.to_string(),
        });
    }
    Ok(ChatResponse {
        raw_body: raw_body.to_string(),
        content,
        token_usage: TokenUsage {
            prompt: body
                .pointer("/usage/prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            completion: body
                .pointer("/usage/completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            total: body
                .pointer("/usage/total_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        },
    })
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

fn default_unclear_reason(value: &str) -> &'static str {
    if is_zh_language(value) {
        "用途不明确，建议保留并人工复核。"
    } else {
        "Unclear purpose, keep for manual review."
    }
}

fn default_analysis_failed_reason(value: &str, err: &str) -> String {
    if is_zh_language(value) {
        format!("分析失败，建议人工复核：{err}")
    } else {
        format!("Analysis failed, manual review recommended: {err}")
    }
}

fn localized_local_rule_reason(language: &str, message_en: &str, message_zh: &str) -> String {
    if is_zh_language(language) {
        message_zh.to_string()
    } else {
        message_en.to_string()
    }
}

fn split_normalized_path(path: &str) -> Vec<&str> {
    path.split('\\')
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn path_has_segment(path: &str, needle: &str) -> bool {
    split_normalized_path(path)
        .iter()
        .any(|segment| *segment == needle)
}

fn path_contains_segment_sequence(path: &str, sequence: &[&str]) -> bool {
    if sequence.is_empty() {
        return false;
    }
    let segments = split_normalized_path(path);
    if segments.len() < sequence.len() {
        return false;
    }
    segments
        .windows(sequence.len())
        .any(|window| window == sequence)
}

fn local_appdata_suffix<'a>(path: &'a str) -> Option<Vec<&'a str>> {
    let segments = split_normalized_path(path);
    if segments.len() < 5
        || !segments[0].ends_with(':')
        || segments[1] != "users"
        || segments[2].is_empty()
        || segments[3] != "appdata"
        || segments[4] != "local"
    {
        return None;
    }
    Some(segments[5..].to_vec())
}

fn matches_browser_cache_suffix(suffix: &[&str], prefix: &[&str]) -> bool {
    suffix.len() >= prefix.len() + 2
        && suffix.starts_with(prefix)
        && !suffix[prefix.len()].is_empty()
        && matches!(suffix[prefix.len() + 1], "cache" | "code cache")
}

fn is_known_browser_cache_path(path: &str) -> bool {
    let Some(suffix) = local_appdata_suffix(path) else {
        return false;
    };
    matches_browser_cache_suffix(&suffix, &["google", "chrome", "user data"])
        || matches_browser_cache_suffix(&suffix, &["microsoft", "edge", "user data"])
}

fn is_local_appdata_expand_root(path: &str) -> bool {
    let segments = split_normalized_path(path);
    if segments.len() < 5
        || !segments[0].ends_with(':')
        || segments[1] != "users"
        || segments[3] != "appdata"
        || segments[4] != "local"
    {
        return false;
    }
    if segments.len() == 5 {
        return true;
    }
    if segments.len() != 6 {
        return false;
    }
    matches!(
        segments[5],
        "packages" | "microsoft" | "google" | "adobe" | "jetbrains" | "nvidia" | "crashdumps"
    )
}

fn extract_file_extension(name: &str) -> Option<String> {
    let trimmed = name.trim();
    let (_, extension) = trimmed.rsplit_once('.')?;
    let normalized = extension.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    Some(format!(".{normalized}"))
}

fn freshness_bucket(mtime_ms: Option<i64>) -> String {
    let Some(mtime_ms) = mtime_ms else {
        return "older".to_string();
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(mtime_ms);
    let age_ms = now_ms.saturating_sub(mtime_ms);
    let recent_7d = 7_i64 * 24 * 60 * 60 * 1000;
    let recent_30d = 30_i64 * 24 * 60 * 60 * 1000;
    if age_ms <= recent_7d {
        "recent_7d".to_string()
    } else if age_ms <= recent_30d {
        "recent_30d".to_string()
    } else {
        "older".to_string()
    }
}

fn extract_name_tags(path: &str) -> Vec<String> {
    let normalized = normalize_scan_path_key(path);
    let mut tags = Vec::new();
    let mut push_tag = |tag: &str| {
        if !tags.iter().any(|existing| existing == tag) {
            tags.push(tag.to_string());
        }
    };

    if path_has_segment(&normalized, "cache")
        || path_has_segment(&normalized, "caches")
        || normalized.contains("\\npm-cache")
        || normalized.contains("\\pip\\cache")
    {
        push_tag("cache");
    }
    if path_has_segment(&normalized, "temp")
        || path_has_segment(&normalized, "tmp")
        || normalized.ends_with("\\windows\\temp")
        || normalized.contains("\\appdata\\local\\temp")
    {
        push_tag("temp");
    }
    if path_has_segment(&normalized, "downloads") || path_has_segment(&normalized, "download") {
        push_tag("download");
    }
    if path_has_segment(&normalized, "backup") || path_has_segment(&normalized, "backups") {
        push_tag("backup");
    }
    if path_has_segment(&normalized, ".git")
        || path_has_segment(&normalized, "node_modules")
        || path_has_segment(&normalized, ".venv")
        || path_has_segment(&normalized, "venv")
    {
        push_tag("dev");
    }
    if path_has_segment(&normalized, "dist")
        || path_has_segment(&normalized, "build")
        || path_has_segment(&normalized, "target")
        || path_has_segment(&normalized, "out")
        || path_has_segment(&normalized, ".next")
        || path_has_segment(&normalized, ".nuxt")
    {
        push_tag("build");
    }
    if is_same_or_descendant_path(&normalized, "c:\\windows")
        || is_same_or_descendant_path(&normalized, "c:\\program files")
        || is_same_or_descendant_path(&normalized, "c:\\program files (x86)")
        || is_same_or_descendant_path(&normalized, "c:\\programdata")
    {
        push_tag("system");
    }
    let segments = split_normalized_path(&normalized);
    if segments.len() >= 4
        && segments[0].ends_with(':')
        && segments[1] == "users"
        && matches!(
            segments[3],
            "desktop" | "documents" | "pictures" | "videos" | "music"
        )
    {
        push_tag("user_content");
    }
    tags
}

fn build_directory_portrait(
    node: &persist::ScanNode,
    children: &[persist::ScanNode],
) -> DirectoryPortrait {
    let mut direct_file_count = 0_u64;
    let mut direct_dir_count = 0_u64;
    let mut extension_counts = HashMap::<String, u64>::new();
    let mut sorted_children = children.to_vec();
    sorted_children.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });

    for child in children {
        if child.node_type == "directory" {
            direct_dir_count = direct_dir_count.saturating_add(1);
        } else {
            direct_file_count = direct_file_count.saturating_add(1);
            if let Some(extension) = extract_file_extension(&child.name) {
                *extension_counts.entry(extension).or_insert(0) += 1;
            }
        }
    }

    let mut top_file_extensions = extension_counts
        .into_iter()
        .map(|(extension, count)| DirectoryExtensionCount { extension, count })
        .collect::<Vec<_>>();
    top_file_extensions.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.extension.cmp(&b.extension))
    });
    top_file_extensions.truncate(5);

    DirectoryPortrait {
        direct_file_count,
        direct_dir_count,
        top_children: sorted_children
            .into_iter()
            .take(5)
            .map(|child| ScanRuleTopChild {
                name: child.name,
                item_type: child.node_type,
                size: child.size,
            })
            .collect(),
        top_file_extensions,
        name_tags: extract_name_tags(&node.path),
        freshness_bucket: freshness_bucket(node.mtime_ms),
    }
}

fn build_node_fingerprint(
    node: &persist::ScanNode,
    portrait: Option<&DirectoryPortrait>,
) -> NodeFingerprint {
    NodeFingerprint {
        size: node.size,
        self_size: node.self_size,
        child_count: node.child_count,
        name_tags: portrait
            .map(|item| item.name_tags.clone())
            .unwrap_or_else(|| extract_name_tags(&node.path)),
        top_children: portrait
            .map(|item| item.top_children.clone())
            .unwrap_or_default(),
    }
}

fn portrait_summary_lines(portrait: &DirectoryPortrait, response_language: &str) -> String {
    let top_children = if portrait.top_children.is_empty() {
        if is_zh_language(response_language) {
            "（无）".to_string()
        } else {
            "(none)".to_string()
        }
    } else {
        portrait
            .top_children
            .iter()
            .map(|child| {
                format!(
                    "- {} [{}] ({})",
                    child.name,
                    child.item_type,
                    format_size(child.size)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let top_extensions = if portrait.top_file_extensions.is_empty() {
        if is_zh_language(response_language) {
            "（无）".to_string()
        } else {
            "(none)".to_string()
        }
    } else {
        portrait
            .top_file_extensions
            .iter()
            .map(|item| format!("{} x{}", item.extension, item.count))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let name_tags = if portrait.name_tags.is_empty() {
        if is_zh_language(response_language) {
            "（无）".to_string()
        } else {
            "(none)".to_string()
        }
    } else {
        portrait.name_tags.join(", ")
    };
    if is_zh_language(response_language) {
        format!(
            "目录画像：\n- 直接文件数：{}\n- 直接子目录数：{}\n- 名称标签：{}\n- 新旧程度：{}\n- 主要子节点：\n{}\n- 主要文件扩展名：{}",
            portrait.direct_file_count,
            portrait.direct_dir_count,
            name_tags,
            portrait.freshness_bucket,
            top_children,
            top_extensions,
        )
    } else {
        format!(
            "Directory portrait:\n- Direct files: {}\n- Direct directories: {}\n- Name tags: {}\n- Freshness: {}\n- Top child nodes:\n{}\n- Top file extensions: {}",
            portrait.direct_file_count,
            portrait.direct_dir_count,
            name_tags,
            portrait.freshness_bucket,
            top_children,
            top_extensions,
        )
    }
}

fn is_exact_top_children_match(left: &[ScanRuleTopChild], right: &[ScanRuleTopChild]) -> bool {
    left == right
}

fn top_child_overlap(left: &[ScanRuleTopChild], right: &[ScanRuleTopChild], limit: usize) -> usize {
    let mut matched = 0;
    for candidate in left.iter().take(limit) {
        if right.iter().take(limit).any(|other| {
            other.name.eq_ignore_ascii_case(&candidate.name)
                && other.item_type == candidate.item_type
        }) {
            matched += 1;
        }
    }
    matched
}

fn within_delta(current: u64, baseline: u64, absolute_delta: u64, percent_delta: f64) -> bool {
    let diff = current.abs_diff(baseline);
    let percent_limit = ((baseline as f64) * percent_delta).ceil() as u64;
    diff <= absolute_delta.max(percent_limit)
}

fn matches_keep_directory_similarity(
    current: &NodeFingerprint,
    baseline: &NodeFingerprint,
    current_child_count: u64,
    baseline_child_count: u64,
) -> bool {
    current.name_tags == baseline.name_tags
        && within_delta(current.size, baseline.size, 32 * 1024 * 1024, 0.05)
        && within_delta(current.self_size, baseline.self_size, 8 * 1024 * 1024, 0.10)
        && current_child_count.abs_diff(baseline_child_count) <= 2
        && top_child_overlap(&current.top_children, &baseline.top_children, 3) >= 2
}

fn matches_expand_analysis_directory_similarity(
    current: &NodeFingerprint,
    baseline: &NodeFingerprint,
    current_child_count: u64,
    baseline_child_count: u64,
) -> bool {
    current.name_tags == baseline.name_tags
        && within_delta(current.size, baseline.size, 64 * 1024 * 1024, 0.15)
        && within_delta(
            current.self_size,
            baseline.self_size,
            16 * 1024 * 1024,
            0.20,
        )
        && current_child_count.abs_diff(baseline_child_count) <= 4
        && top_child_overlap(&current.top_children, &baseline.top_children, 3) >= 1
}

fn matches_loose_file_similarity(
    current: &persist::ScanNode,
    baseline: &persist::ScanNode,
) -> bool {
    current.node_type == baseline.node_type
        && current.name.eq_ignore_ascii_case(&baseline.name)
        && within_delta(current.size, baseline.size, 4 * 1024 * 1024, 0.10)
        && extract_file_extension(&current.name) == extract_file_extension(&baseline.name)
}

fn build_scan_result_item(
    node: &persist::ScanNode,
    classification: &str,
    reason: String,
    risk: &str,
    source: &str,
) -> ScanResultItem {
    ScanResultItem {
        name: node.name.clone(),
        path: node.path.clone(),
        size: node.size,
        item_type: if node.node_type == "directory" {
            "directory".to_string()
        } else {
            "file".to_string()
        },
        purpose: String::new(),
        reason,
        risk: risk.to_string(),
        classification: classification.to_string(),
        source: source.to_string(),
    }
}

fn maybe_local_rule_decision(
    language: &str,
    node: &persist::ScanNode,
    portrait: Option<&DirectoryPortrait>,
) -> Option<LocalRuleDecision> {
    let normalized = normalize_scan_path_key(&node.path);
    let is_keep = node.node_type == "directory"
        && (normalized == "c:\\windows"
            || normalized == "c:\\program files"
            || normalized == "c:\\program files (x86)"
            || normalized == "c:\\programdata"
            || split_normalized_path(&normalized).last().copied() == Some(".git")
            || {
                let segments = split_normalized_path(&normalized);
                segments.len() >= 4
                    && segments[0].ends_with(':')
                    && segments[1] == "users"
                    && matches!(
                        segments[3],
                        "desktop" | "documents" | "pictures" | "videos" | "music"
                    )
            });
    if is_keep {
        return Some(LocalRuleDecision {
            classification: CLASS_KEEP_ALL.to_string(),
            reason: localized_local_rule_reason(
                language,
                "Protected system or user-content root; keep it.",
                "命中受保护的系统或用户内容根目录，保留。",
            ),
            risk: "high".to_string(),
            should_expand: false,
            source: SOURCE_LOCAL_RULE,
        });
    }

    let is_expand_dir = node.node_type == "directory" && is_local_appdata_expand_root(&normalized);
    if is_expand_dir {
        return Some(LocalRuleDecision {
            classification: CLASS_EXPAND_ANALYSIS.to_string(),
            reason: localized_local_rule_reason(
                language,
                "Matched a Local AppData container path; inspect direct child folders before deciding.",
                "命中 Local AppData 容器目录，先展开直接子文件夹再判断。",
            ),
            risk: "medium".to_string(),
            should_expand: true,
            source: SOURCE_LOCAL_RULE,
        });
    }

    let is_safe_dir = node.node_type == "directory"
        && (normalized == "c:\\windows\\temp"
            || normalized.contains("\\appdata\\local\\temp")
            || normalized.contains("\\appdata\\local\\npm-cache")
            || normalized.contains("\\appdata\\local\\pip\\cache")
            || path_contains_segment_sequence(&normalized, &[".next", "cache"])
            || path_contains_segment_sequence(&normalized, &[".nuxt"])
            || path_contains_segment_sequence(&normalized, &["node_modules", ".cache"])
            || is_known_browser_cache_path(&normalized)
            || portrait
                .map(|item| {
                    item.name_tags
                        .iter()
                        .any(|tag| tag == "cache" || tag == "temp")
                })
                .unwrap_or(false)
                && (normalized.contains("\\appdata\\local\\temp")
                    || normalized.contains("\\appdata\\local\\npm-cache")
                    || normalized.contains("\\appdata\\local\\pip\\cache")));
    if is_safe_dir {
        return Some(LocalRuleDecision {
            classification: CLASS_DELETE_ALL.to_string(),
            reason: localized_local_rule_reason(
                language,
                "Matched a known temporary or package-cache path; safe to delete.",
                "命中已知临时文件或包管理缓存路径，可安全删除。",
            ),
            risk: "low".to_string(),
            should_expand: false,
            source: SOURCE_LOCAL_RULE,
        });
    }

    let is_safe_file = node.node_type == "file"
        && extract_file_extension(&node.name)
            .map(|extension| {
                matches!(
                    extension.as_str(),
                    ".tmp" | ".temp" | ".crdownload" | ".part"
                )
            })
            .unwrap_or(false);
    if is_safe_file {
        return Some(LocalRuleDecision {
            classification: CLASS_DELETE_ALL.to_string(),
            reason: localized_local_rule_reason(
                language,
                "Matched a known temporary or partial-download file type; safe to delete.",
                "命中已知临时文件或未完成下载文件类型，可安全删除。",
            ),
            risk: "low".to_string(),
            should_expand: false,
            source: SOURCE_LOCAL_RULE,
        });
    }

    None
}

fn persistent_rule_record_matches(
    rule: &ScanPersistentRuleRecord,
    node: &persist::ScanNode,
    fingerprint: &NodeFingerprint,
) -> bool {
    let normalized_rule_path = normalize_scan_path_key(&rule.path);
    if normalized_rule_path != normalize_scan_path_key(&node.path)
        || rule.node_type != node.node_type
    {
        return false;
    }
    match rule.classification.as_str() {
        CLASS_DELETE_ALL => {
            rule.size == node.size
                && rule.self_size == node.self_size
                && rule.child_count == node.child_count
                && rule.name_tags == fingerprint.name_tags
                && is_exact_top_children_match(&rule.top_children, &fingerprint.top_children)
        }
        CLASS_KEEP_ALL => {
            if node.node_type == "file" {
                rule.size == node.size && rule.self_size == node.self_size
            } else {
                rule.name_tags == fingerprint.name_tags
                    && within_delta(node.size, rule.size, 32 * 1024 * 1024, 0.05)
                    && within_delta(node.self_size, rule.self_size, 8 * 1024 * 1024, 0.10)
                    && node.child_count.abs_diff(rule.child_count) <= 2
                    && top_child_overlap(&fingerprint.top_children, &rule.top_children, 3) >= 2
            }
        }
        _ => false,
    }
}

fn resolve_persistent_rule(
    rules: &ScanPersistentRules,
    node: &persist::ScanNode,
    portrait: Option<&DirectoryPortrait>,
) -> Option<(ScanResultItem, bool)> {
    let fingerprint = build_node_fingerprint(node, portrait);
    let candidate = rules
        .keep_all_exact
        .iter()
        .find(|rule| persistent_rule_record_matches(rule, node, &fingerprint))
        .or_else(|| {
            rules
                .delete_all_exact
                .iter()
                .find(|rule| persistent_rule_record_matches(rule, node, &fingerprint))
        })?;

    let item = build_scan_result_item(
        node,
        &candidate.classification,
        if candidate.reason.trim().is_empty() {
            "Matched persistent exact-path rule.".to_string()
        } else {
            candidate.reason.clone()
        },
        &candidate.risk,
        SOURCE_PERSISTENT_RULE,
    );
    let should_expand =
        node.node_type == "directory" && item.classification == CLASS_EXPAND_ANALYSIS;
    Some((item, should_expand))
}

fn make_persistent_rule_record(
    node: &persist::ScanNode,
    portrait: Option<&DirectoryPortrait>,
    classification: &str,
    reason: &str,
    risk: &str,
    source: &str,
) -> ScanPersistentRuleRecord {
    let fingerprint = build_node_fingerprint(node, portrait);
    ScanPersistentRuleRecord {
        path: node.path.clone(),
        node_type: node.node_type.clone(),
        classification: classification.to_string(),
        reason: reason.to_string(),
        risk: risk.to_string(),
        source: source.to_string(),
        size: fingerprint.size,
        self_size: fingerprint.self_size,
        child_count: fingerprint.child_count,
        name_tags: fingerprint.name_tags,
        top_children: fingerprint.top_children,
    }
}

fn should_promote_ai_rule(review: &ScanReview) -> bool {
    matches!(review.classification.as_str(), CLASS_KEEP_ALL)
        || (review.classification == CLASS_DELETE_ALL && review.risk == "low")
}

fn insert_persistent_rule(
    rules: &mut ScanPersistentRules,
    record: ScanPersistentRuleRecord,
) -> bool {
    let path_key = normalize_scan_path_key(&record.path);
    rules
        .keep_all_exact
        .retain(|existing| normalize_scan_path_key(&existing.path) != path_key);
    rules
        .delete_all_exact
        .retain(|existing| normalize_scan_path_key(&existing.path) != path_key);
    let target = if record.classification == CLASS_KEEP_ALL {
        &mut rules.keep_all_exact
    } else if record.classification == CLASS_DELETE_ALL {
        &mut rules.delete_all_exact
    } else {
        return false;
    };
    let changed = !target.iter().any(|existing| existing == &record);
    if changed {
        target.push(record);
    }
    changed
}

fn maybe_store_persistent_rule(
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
    record: ScanPersistentRuleRecord,
) -> Result<bool, String> {
    let mut rules = task.persistent_rules.lock();
    let changed = insert_persistent_rule(&mut rules, record);
    if changed {
        backend::save_scan_persistent_rules(&state.settings_path(), &rules)?;
    }
    Ok(changed)
}

fn build_children_by_parent(
    nodes: &HashMap<String, persist::ScanNode>,
) -> HashMap<String, Vec<persist::ScanNode>> {
    let mut groups = HashMap::<String, Vec<persist::ScanNode>>::new();
    for node in nodes.values() {
        if let Some(parent_id) = node.parent_id.clone() {
            groups.entry(parent_id).or_default().push(node.clone());
        }
    }
    groups
}

fn resolve_reusable_finding(
    node: &persist::ScanNode,
    portrait: Option<&DirectoryPortrait>,
    baseline_node: Option<&persist::ScanNode>,
    baseline_finding: Option<&persist::ScanFindingRecord>,
    baseline_children: &[persist::ScanNode],
) -> Option<(ScanResultItem, bool)> {
    let baseline_node = baseline_node?;
    let baseline_finding = baseline_finding?;
    let baseline_portrait = if baseline_node.node_type == "directory" {
        Some(build_directory_portrait(baseline_node, baseline_children))
    } else {
        None
    };
    let current_fp = build_node_fingerprint(node, portrait);
    let baseline_fp = build_node_fingerprint(baseline_node, baseline_portrait.as_ref());

    let reusable = match baseline_finding.item.classification.as_str() {
        CLASS_DELETE_ALL => {
            if node.node_type == "directory" {
                nodes_match(node, baseline_node)
                    && current_fp.name_tags == baseline_fp.name_tags
                    && is_exact_top_children_match(
                        &current_fp.top_children,
                        &baseline_fp.top_children,
                    )
            } else {
                nodes_match(node, baseline_node)
            }
        }
        CLASS_KEEP_ALL => {
            if node.node_type == "directory" {
                matches_keep_directory_similarity(
                    &current_fp,
                    &baseline_fp,
                    node.child_count,
                    baseline_node.child_count,
                )
            } else {
                nodes_match(node, baseline_node)
            }
        }
        CLASS_EXPAND_ANALYSIS => {
            if node.node_type == "directory" {
                matches_expand_analysis_directory_similarity(
                    &current_fp,
                    &baseline_fp,
                    node.child_count,
                    baseline_node.child_count,
                )
            } else {
                matches_loose_file_similarity(node, baseline_node)
            }
        }
        _ => false,
    };

    if !reusable {
        return None;
    }

    let item = ScanResultItem {
        source: SOURCE_BASELINE_REUSE.to_string(),
        ..baseline_finding.item.clone()
    };
    let should_expand = if node.node_type != "directory" {
        false
    } else if item.classification == CLASS_EXPAND_ANALYSIS {
        true
    } else {
        baseline_finding.should_expand
    };
    Some((item, should_expand))
}

fn scanner_binary_candidates<R: Runtime>(app: &AppHandle<R>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        out.push(resource_dir.join("scanner.exe"));
        out.push(resource_dir.join("bin").join("scanner.exe"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            out.push(parent.join("scanner.exe"));
            out.push(parent.join("bin").join("scanner.exe"));
            out.push(parent.join("..").join("..").join("bin").join("scanner.exe"));
            out.push(
                parent
                    .join("..")
                    .join("..")
                    .join("..")
                    .join("bin")
                    .join("scanner.exe"),
            );
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join("bin").join("scanner.exe"));
    }
    out
}

fn scanner_binary_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    scanner_binary_candidates(app)
        .into_iter()
        .find(|p| p.exists())
        .ok_or_else(|| "scanner.exe not found in dev or bundle paths".to_string())
}

fn kill_pid(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .status()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

async fn chat_completion(
    ai: &ScanAiConfig,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<ChatResponse, ChatCompletionError> {
    let url = format!("{}/chat/completions", ai.endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(CHAT_COMPLETION_TIMEOUT_SECS))
        .build()
        .map_err(|e| ChatCompletionError {
            message: e.to_string(),
            raw_body: String::new(),
        })?;
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&json!({
            "model": ai.model,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": user_prompt }
            ],
            "temperature": 0.1
        }));
    if !ai.api_key.trim().is_empty() {
        req = req
            .header("Authorization", format!("Bearer {}", ai.api_key))
            .header("x-api-key", ai.api_key.clone())
            .header("api-key", ai.api_key.clone());
    }
    let resp = req.send().await.map_err(|e| ChatCompletionError {
        message: e.to_string(),
        raw_body: String::new(),
    })?;
    let status = resp.status();
    let raw_body = resp.text().await.map_err(|e| ChatCompletionError {
        message: format!("error reading response body: {}", e),
        raw_body: String::new(),
    })?;
    parse_chat_completion_http_body(status, &raw_body)
}

fn build_scan_system_prompt(
    response_language: &str,
    _is_directory: bool,
    allow_web_search: bool,
) -> String {
    let response_language_name = localized_language_name(response_language, response_language);
    let (delete_all, keep_all, expand_analysis) =
        scan_prompt_classification_tokens(response_language);
    let final_schema = format!(
        "{{\"classification\":\"{delete_all}|{keep_all}|{expand_analysis}\",\"reason\":\"...\",\"risk\":\"low|medium|high\"}}"
    );

    let mut lines = if is_zh_language(response_language) {
        vec![
            "你是一个磁盘清理安全分析助手。".to_string(),
            "只能返回 JSON。".to_string(),
            format!("输出结构：{final_schema}"),
            format!(
                "分类含义：{delete_all} = 整个项目都可以删除，{keep_all} = 整个项目都应该保留，{expand_analysis} = 需要继续深入分析或人工复核。"
            ),
            format!("保持保守判断；如果不确定，优先使用 {expand_analysis}，不要使用 {delete_all}。"),
            format!("`reason` 字段只能使用 {response_language_name}。"),
        ]
    } else {
        vec![
            "You are a disk cleanup safety assistant.".to_string(),
            "Return JSON only.".to_string(),
            format!("Final schema: {final_schema}"),
            format!(
                "Classification meanings: {delete_all} = delete the whole item, {keep_all} = keep the whole item, {expand_analysis} = inspect deeper or require manual review."
            ),
            format!(
                "Be conservative. If unsure, prefer {expand_analysis} over {delete_all}."
            ),
            format!("The \"reason\" field must be written in {response_language_name} only."),
        ]
    };
    if allow_web_search {
        lines.push(if is_zh_language(response_language) {
            "如果本地元数据不足且确实需要外部上下文，你可以返回 {\"action\":\"web_search\",\"query\":\"...\",\"reason\":\"...\"} 来替代最终结构。只使用一条简洁查询。"
                .to_string()
        } else {
            "If local metadata is insufficient and external context is necessary, you may return {\"action\":\"web_search\",\"query\":\"...\",\"reason\":\"...\"} instead of the final schema. Use one concise query only."
                .to_string()
        });
    }
    lines.join("\n")
}

fn scan_prompt_classification_tokens(
    response_language: &str,
) -> (&'static str, &'static str, &'static str) {
    if is_zh_language(response_language) {
        ("全部删除", "全部保留", "展开分析")
    } else {
        ("delete_all", "keep_all", "expand_analysis")
    }
}

fn normalize_scan_ai_classification(raw: Option<&str>) -> String {
    let normalized = raw.unwrap_or("").trim();
    let lowered = normalized.to_ascii_lowercase();
    match normalized {
        "全部删除" => CLASS_DELETE_ALL,
        "全部保留" => CLASS_KEEP_ALL,
        "展开分析" => CLASS_EXPAND_ANALYSIS,
        _ => match lowered.as_str() {
            "safe_to_delete" | "delete_all" | "all_delete" => CLASS_DELETE_ALL,
            "keep" | "keep_all" | "all_keep" => CLASS_KEEP_ALL,
            "suspicious" | "expand_analysis" | "analyze_further" | "manual_review" => {
                CLASS_EXPAND_ANALYSIS
            }
            _ => CLASS_EXPAND_ANALYSIS,
        },
    }
    .to_string()
}

async fn analyze_scan_node(
    ai: &ScanAiConfig,
    node: &persist::ScanNode,
    child_dirs: &[persist::ScanNode],
    portrait: Option<&DirectoryPortrait>,
) -> ScanReview {
    let child_summary = if child_dirs.is_empty() {
        if is_zh_language(&ai.response_language) {
            "（无）".to_string()
        } else {
            "(none)".to_string()
        }
    } else {
        child_dirs
            .iter()
            .take(24)
            .map(|child| format!("- {} ({})", child.name, format_size(child.size)))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let user_prompt = if node.node_type == "directory" {
        let portrait_summary = portrait
            .map(|value| portrait_summary_lines(value, &ai.response_language))
            .unwrap_or_else(|| {
                if is_zh_language(&ai.response_language) {
                    "目录画像：\n（无）".to_string()
                } else {
                    "Directory portrait:\n(none)".to_string()
                }
            });
        let (delete_all, keep_all, expand_analysis) =
            scan_prompt_classification_tokens(&ai.response_language);
        if is_zh_language(&ai.response_language) {
            format!(
                "类型：目录\n路径：{}\n名称：{}\n大小：{}\n{}\n直接子目录：\n{}\n只能选择一个 classification：{}、{}、{}。只有当整个目录都可以安全删除时，才能使用 {}。当整个目录都应保留时，使用 {}。只要任一子项需要继续深入判断，或者你无法确定，就使用 {}。如果不确定，优先使用 {}，不要使用 {}。",
                node.path,
                node.name,
                format_size(node.size),
                portrait_summary,
                child_summary,
                delete_all,
                keep_all,
                expand_analysis,
                delete_all,
                keep_all,
                expand_analysis,
                expand_analysis,
                delete_all
            )
        } else {
            format!(
                "Type: directory\nPath: {}\nName: {}\nSize: {}\n{}\nDirect child directories:\n{}\nChoose one classification only: {}, {}, or {}. Use {} only when the whole directory can be deleted safely. Use {} when the whole directory should be kept. Use {} when any child needs deeper inspection or when you are uncertain. If unsure, prefer {} over {}.",
                node.path,
                node.name,
                format_size(node.size),
                portrait_summary,
                child_summary,
                delete_all,
                keep_all,
                expand_analysis,
                delete_all,
                keep_all,
                expand_analysis,
                expand_analysis,
                delete_all
            )
        }
    } else {
        let (delete_all, keep_all, expand_analysis) =
            scan_prompt_classification_tokens(&ai.response_language);
        if is_zh_language(&ai.response_language) {
            format!(
                "类型：文件\n路径：{}\n名称：{}\n大小：{}\n只能选择一个 classification：{}、{}、{}。只有当文件可以安全删除时，才能使用 {}。当文件应保留时，使用 {}。如果无法确定，就使用 {}。如果不确定，优先使用 {}，不要使用 {}。",
                node.path,
                node.name,
                format_size(node.size),
                delete_all,
                keep_all,
                expand_analysis,
                delete_all,
                keep_all,
                expand_analysis,
                expand_analysis,
                delete_all
            )
        } else {
            format!(
                "Type: file\nPath: {}\nName: {}\nSize: {}\nChoose one classification only: {}, {}, or {}. Use {} only when the file can be deleted safely. Use {} when the file should be kept. Use {} when you are uncertain. If unsure, prefer {} over {}.",
                node.path,
                node.name,
                format_size(node.size),
                delete_all,
                keep_all,
                expand_analysis,
                delete_all,
                keep_all,
                expand_analysis,
                expand_analysis,
                delete_all
            )
        }
    };

    let search_allowed = ai.use_web_search && !ai.search_api_key.trim().is_empty();
    let mut total_usage = TokenUsage::default();
    let mut search_context = None::<String>;
    let mut search_trace = Value::Null;
    let max_rounds = if search_allowed { 2 } else { 1 };

    for round in 0..max_rounds {
        let system_prompt = build_scan_system_prompt(
            &ai.response_language,
            node.node_type == "directory",
            search_allowed && search_context.is_none(),
        );
        let current_user_prompt = if let Some(context) = search_context.as_ref() {
            let (delete_all, keep_all, expand_analysis) =
                scan_prompt_classification_tokens(&ai.response_language);
            if is_zh_language(&ai.response_language) {
                format!(
                    "{user_prompt}\n\n网页搜索上下文：\n{context}\n\n只返回最终分类 JSON，classification 只能是 {delete_all}|{keep_all}|{expand_analysis}。"
                )
            } else {
                format!(
                    "{user_prompt}\n\nWeb search context:\n{context}\n\nReturn the final classification JSON only using classification={delete_all}|{keep_all}|{expand_analysis}."
                )
            }
        } else {
            user_prompt.clone()
        };

        match chat_completion(ai, &system_prompt, &current_user_prompt).await {
            Ok(resp) => {
                total_usage.prompt = total_usage.prompt.saturating_add(resp.token_usage.prompt);
                total_usage.completion = total_usage
                    .completion
                    .saturating_add(resp.token_usage.completion);
                total_usage.total = total_usage.total.saturating_add(resp.token_usage.total);

                let parsed: Value = serde_json::from_str(&extract_json_text(&resp.content))
                    .unwrap_or_else(|_| json!({}));
                if search_allowed && search_context.is_none() {
                    if let Some(request) = parse_web_search_request(&parsed) {
                        match tavily_search(&ai.search_api_key, &request).await {
                            Ok(trace) => {
                                search_context =
                                    Some(format_web_search_context(&trace, &ai.response_language));
                                search_trace = web_search_trace_to_value(&trace);
                                continue;
                            }
                            Err(err) => {
                                return ScanReview {
                                    classification: CLASS_EXPAND_ANALYSIS.to_string(),
                                    reason: default_analysis_failed_reason(
                                        &ai.response_language,
                                        &format!("web search failed: {err}"),
                                    ),
                                    risk: "high".to_string(),
                                    token_usage: total_usage,
                                    trace: json!({
                                        "model": ai.model,
                                        "responseLanguage": ai.response_language,
                                        "systemPrompt": system_prompt,
                                        "userPrompt": current_user_prompt,
                                        "rawContent": resp.content,
                                        "rawHttpBody": resp.raw_body,
                                        "search": {
                                            "request": {
                                                "query": request.query,
                                                "reason": request.reason,
                                            },
                                            "error": err,
                                        },
                                    }),
                                };
                            }
                        }
                    }
                }

                return ScanReview {
                    classification: normalize_scan_ai_classification(
                        parsed.get("classification").and_then(Value::as_str),
                    ),
                    reason: parsed
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or(default_unclear_reason(&ai.response_language))
                        .to_string(),
                    risk: parsed
                        .get("risk")
                        .and_then(Value::as_str)
                        .unwrap_or("medium")
                        .to_string(),
                    token_usage: total_usage,
                    trace: json!({
                        "model": ai.model,
                        "responseLanguage": ai.response_language,
                        "systemPrompt": system_prompt,
                        "userPrompt": current_user_prompt,
                        "rawContent": resp.content,
                        "rawHttpBody": resp.raw_body,
                        "search": search_trace,
                        "round": round + 1,
                    }),
                };
            }
            Err(err) => {
                return ScanReview {
                    classification: CLASS_EXPAND_ANALYSIS.to_string(),
                    reason: default_analysis_failed_reason(&ai.response_language, &err.message),
                    risk: "high".to_string(),
                    token_usage: total_usage,
                    trace: json!({
                        "model": ai.model,
                        "responseLanguage": ai.response_language,
                        "systemPrompt": system_prompt,
                        "userPrompt": current_user_prompt,
                        "error": err.message,
                        "errorRawBody": err.raw_body,
                        "search": search_trace,
                    }),
                };
            }
        }
    }

    ScanReview {
        classification: CLASS_EXPAND_ANALYSIS.to_string(),
        reason: default_analysis_failed_reason(
            &ai.response_language,
            "model requested web search but did not return a final classification",
        ),
        risk: "high".to_string(),
        token_usage: total_usage,
        trace: json!({
            "model": ai.model,
            "responseLanguage": ai.response_language,
            "userPrompt": user_prompt,
            "search": search_trace,
            "error": "missing_final_classification",
        }),
    }
}

fn sort_queue(queue: &mut Vec<persist::ScanNode>) {
    queue.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
}

fn nodes_match(current: &persist::ScanNode, baseline: &persist::ScanNode) -> bool {
    current.node_type == baseline.node_type
        && current.self_size == baseline.self_size
        && current.size == baseline.size
        && current.child_count == baseline.child_count
        && current.mtime_ms == baseline.mtime_ms
}

fn build_finding_item(node: &persist::ScanNode, review: &ScanReview) -> ScanResultItem {
    build_scan_result_item(
        node,
        &review.classification,
        review.reason.clone(),
        &review.risk,
        SOURCE_AI,
    )
}

fn should_expand_node(node: &persist::ScanNode, review: &ScanReview) -> bool {
    node.node_type == "directory"
        && review.classification != CLASS_DELETE_ALL
        && review.classification != CLASS_KEEP_ALL
}

fn apply_finding_to_snapshot<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    snap: &mut ScanSnapshot,
    item: &ScanResultItem,
) -> Result<(), String> {
    if item.classification == CLASS_DELETE_ALL
        && !snap
            .deletable
            .iter()
            .any(|existing| existing.path.eq_ignore_ascii_case(&item.path))
    {
        snap.total_cleanable = snap.total_cleanable.saturating_add(item.size);
        snap.deletable.push(item.clone());
        snap.deletable_count = snap.deletable.len() as u64;
        app.emit(
            "scan_found",
            json!({
                "taskId": task_id,
                "name": item.name,
                "path": item.path,
                "size": item.size,
                "type": item.item_type,
                "reason": item.reason,
                "risk": item.risk
            }),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn should_stop_for_target(snapshot: &ScanSnapshot) -> bool {
    snapshot.target_size > 0 && snapshot.total_cleanable >= snapshot.target_size
}

async fn emit_cache_event<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    action: &str,
    node: Option<&persist::ScanNode>,
    count: Option<u64>,
) -> Result<(), String> {
    let payload = json!({
        "taskId": task_id,
        "action": action,
        "path": node.map(|item| item.path.clone()).unwrap_or_default(),
        "name": node.map(|item| item.name.clone()).unwrap_or_default(),
        "nodeType": node.map(|item| item.node_type.clone()).unwrap_or_default(),
        "count": count.unwrap_or(0),
    });
    app.emit("scan_cache", payload).map_err(|e| e.to_string())
}

async fn emit_progress<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
) -> Result<(), String> {
    emit_progress_with_options(app, state, task, true).await
}

async fn emit_progress_with_options<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
    persist_snapshot: bool,
) -> Result<(), String> {
    let snap = task.snapshot.lock().clone();
    if persist_snapshot {
        persist::save_scan_snapshot(&state.db_path(), &snap)?;
    }
    app.emit(
        "scan_progress",
        serde_json::to_value(&snap).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn should_persist_scan_progress(task: &Arc<ScanTaskRuntime>, force: bool) -> bool {
    let mut last_persisted_at = task.last_progress_persist_at.lock();
    if force {
        *last_persisted_at = Some(Instant::now());
        return true;
    }
    let now = Instant::now();
    match *last_persisted_at {
        Some(previous) if now.duration_since(previous) < SCAN_PROGRESS_PERSIST_INTERVAL => false,
        _ => {
            *last_persisted_at = Some(now);
            true
        }
    }
}

fn prepare_incremental_context(
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
) -> Result<Option<IncrementalContext>, String> {
    let snapshot = task.snapshot.lock().clone();
    let task_id = snapshot.id.clone();
    let current_nodes = persist::load_scan_node_map(&state.db_path(), &task_id)?;
    let analyze_roots = if task.scan_mode == ScanMode::DeepenIncremental {
        task.sidecar_roots
            .iter()
            .filter_map(|root| current_nodes.get(&root.path.to_lowercase()).cloned())
            .collect::<Vec<_>>()
    } else {
        persist::load_scan_children_exact_for_task(
            &state.db_path(),
            &task_id,
            &snapshot.root_node_id,
            false,
        )?
    };
    if task.in_place_baseline.as_ref().is_none()
        && task
            .baseline_task_id
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        return Ok(Some(IncrementalContext {
            baseline_nodes: HashMap::new(),
            baseline_findings: HashMap::new(),
            baseline_children_by_parent: HashMap::new(),
            analyze_roots,
            deleted_count: 0,
        }));
    }
    let (baseline_nodes, baseline_findings) = if let Some(cache) = task.in_place_baseline.as_ref() {
        (cache.nodes.clone(), cache.findings.clone())
    } else {
        let baseline_task_id = task.baseline_task_id.clone().unwrap_or_default();
        (
            persist::load_effective_scan_node_map(&state.db_path(), &baseline_task_id)?,
            persist::load_effective_scan_findings_map(&state.db_path(), &baseline_task_id)?,
        )
    };
    let deleted_count = if task.scan_mode == ScanMode::FullRescanIncremental {
        baseline_nodes
            .keys()
            .filter(|path| !current_nodes.contains_key(*path))
            .count() as u64
    } else {
        0
    };
    let baseline_children_by_parent = build_children_by_parent(&baseline_nodes);
    Ok(Some(IncrementalContext {
        baseline_nodes,
        baseline_findings,
        baseline_children_by_parent,
        analyze_roots,
        deleted_count,
    }))
}

async fn run_auto_analyze<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
) -> Result<(), String> {
    let Some(ctx) = prepare_incremental_context(state, task)? else {
        return Ok(());
    };
    let task_id = task.snapshot.lock().id.clone();
    let mut queue = ctx.analyze_roots;
    let mut queued: HashSet<String> = queue.iter().map(|x| x.path.to_lowercase()).collect();
    let mut analyzed = HashSet::new();
    sort_queue(&mut queue);

    if ctx.deleted_count > 0 {
        emit_cache_event(app, &task_id, "skip_deleted", None, Some(ctx.deleted_count)).await?;
    }

    while !queue.is_empty() {
        if task.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let node = queue.remove(0);
        queued.remove(&node.path.to_lowercase());
        if !analyzed.insert(node.path.to_lowercase()) {
            continue;
        }
        {
            let mut snap = task.snapshot.lock();
            snap.status = "analyzing".to_string();
            snap.current_path = node.path.clone();
            snap.current_depth = node.depth;
        }
        emit_progress(app, state, task).await?;

        let direct_children = if node.node_type == "directory" {
            persist::load_scan_children_exact_for_task(&state.db_path(), &task_id, &node.id, false)?
        } else {
            Vec::new()
        };
        let child_dirs = direct_children
            .iter()
            .filter(|child| child.node_type == "directory")
            .cloned()
            .collect::<Vec<_>>();
        let portrait = if node.node_type == "directory" {
            Some(build_directory_portrait(&node, &direct_children))
        } else {
            None
        };

        if let Some(local_rule) =
            maybe_local_rule_decision(&task.ai.response_language, &node, portrait.as_ref())
        {
            let item = build_scan_result_item(
                &node,
                &local_rule.classification,
                local_rule.reason.clone(),
                &local_rule.risk,
                local_rule.source,
            );
            persist::upsert_scan_finding(
                &state.db_path(),
                &task_id,
                &item,
                local_rule.should_expand,
            )?;
            let reached_target = {
                let mut snap = task.snapshot.lock();
                snap.processed_entries = snap.processed_entries.saturating_add(1);
                snap.current_path = node.path.clone();
                snap.current_depth = node.depth;
                apply_finding_to_snapshot(app, &task_id, &mut snap, &item)?;
                should_stop_for_target(&snap)
            };
            emit_cache_event(app, &task_id, "local_rule", Some(&node), None).await?;
            emit_progress(app, state, task).await?;
            if local_rule.should_expand {
                for child in &direct_children {
                    let key = child.path.to_lowercase();
                    if analyzed.contains(&key) || queued.contains(&key) {
                        continue;
                    }
                    queued.insert(key);
                    queue.push(child.clone());
                }
                sort_queue(&mut queue);
            }
            if reached_target {
                break;
            }
            continue;
        }

        let persistent_match = {
            let rules = task.persistent_rules.lock().clone();
            resolve_persistent_rule(&rules, &node, portrait.as_ref())
        };
        if let Some((item, should_expand)) = persistent_match {
            persist::upsert_scan_finding(&state.db_path(), &task_id, &item, should_expand)?;
            let reached_target = {
                let mut snap = task.snapshot.lock();
                snap.processed_entries = snap.processed_entries.saturating_add(1);
                snap.current_path = node.path.clone();
                snap.current_depth = node.depth;
                apply_finding_to_snapshot(app, &task_id, &mut snap, &item)?;
                should_stop_for_target(&snap)
            };
            emit_cache_event(app, &task_id, "persistent_rule", Some(&node), None).await?;
            emit_progress(app, state, task).await?;
            if should_expand {
                for child in &direct_children {
                    let key = child.path.to_lowercase();
                    if analyzed.contains(&key) || queued.contains(&key) {
                        continue;
                    }
                    queued.insert(key);
                    queue.push(child.clone());
                }
                sort_queue(&mut queue);
            }
            if reached_target {
                break;
            }
            continue;
        }

        let node_key = node.path.to_lowercase();
        let baseline_node = ctx.baseline_nodes.get(&node_key);
        let baseline_children = baseline_node
            .and_then(|baseline| ctx.baseline_children_by_parent.get(&baseline.id))
            .map(|children| children.as_slice())
            .unwrap_or(&[]);
        let reusable_finding = resolve_reusable_finding(
            &node,
            portrait.as_ref(),
            baseline_node,
            ctx.baseline_findings.get(&node_key),
            baseline_children,
        );

        if let Some((item, should_expand)) = reusable_finding {
            persist::upsert_scan_finding(&state.db_path(), &task_id, &item, should_expand)?;
            let reached_target = {
                let mut snap = task.snapshot.lock();
                snap.processed_entries = snap.processed_entries.saturating_add(1);
                snap.current_path = node.path.clone();
                snap.current_depth = node.depth;
                apply_finding_to_snapshot(app, &task_id, &mut snap, &item)?;
                should_stop_for_target(&snap)
            };
            emit_cache_event(app, &task_id, "reuse", Some(&node), None).await?;
            emit_progress(app, state, task).await?;
            if should_expand {
                for child in &direct_children {
                    let key = child.path.to_lowercase();
                    if analyzed.contains(&key) || queued.contains(&key) {
                        continue;
                    }
                    queued.insert(key);
                    queue.push(child.clone());
                }
                sort_queue(&mut queue);
            }
            if reached_target {
                break;
            }
            continue;
        }

        if ctx.baseline_nodes.contains_key(&node_key) {
            emit_cache_event(app, &task_id, "rescan_changed", Some(&node), None).await?;
        }

        app.emit(
            "scan_agent_call",
            json!({
                "taskId": task_id,
                "nodeType": node.node_type,
                "nodePath": node.path,
                "nodeName": node.name,
                "nodeSize": node.size,
                "directoryPortrait": portrait.as_ref().map(|item| json!({
                    "directFileCount": item.direct_file_count,
                    "directDirCount": item.direct_dir_count,
                    "freshnessBucket": item.freshness_bucket,
                    "nameTags": item.name_tags,
                    "topChildren": item.top_children,
                })).unwrap_or(Value::Null),
                "childDirectories": child_dirs.iter().map(|x| json!({"name": x.name, "path": x.path, "size": x.size})).collect::<Vec<_>>(),
            }),
        )
        .map_err(|e| e.to_string())?;

        let review = analyze_scan_node(&task.ai, &node, &child_dirs, portrait.as_ref()).await;
        let should_expand = should_expand_node(&node, &review);
        app.emit(
            "scan_agent_response",
            json!({
                "taskId": task_id,
                "nodeType": node.node_type,
                "nodePath": node.path,
                "nodeName": node.name,
                "nodeSize": node.size,
                "model": task.ai.model,
                "classification": review.classification,
                "reason": review.reason,
                "risk": review.risk,
                "shouldExpand": should_expand,
                "tokenUsage": review.token_usage,
                "elapsed": review.trace.get("elapsed").cloned().unwrap_or(Value::Null),
                "reasoning": review.trace.get("reasoning").cloned().unwrap_or(Value::Null),
                "rawContent": review.trace.get("rawContent").cloned().unwrap_or(Value::Null),
                "userPrompt": review.trace.get("userPrompt").cloned().unwrap_or(Value::Null),
                "search": review.trace.get("search").cloned().unwrap_or(Value::Null),
                "error": review.trace.get("error").cloned().unwrap_or(Value::Null),
            }),
        )
        .map_err(|e| e.to_string())?;

        let item = build_finding_item(&node, &review);
        persist::upsert_scan_finding(&state.db_path(), &task_id, &item, should_expand)?;
        if should_promote_ai_rule(&review) {
            let _ = maybe_store_persistent_rule(
                state,
                task,
                make_persistent_rule_record(
                    &node,
                    portrait.as_ref(),
                    &review.classification,
                    &review.reason,
                    &review.risk,
                    "ai_promoted",
                ),
            )?;
        }

        let reached_target = {
            let mut snap = task.snapshot.lock();
            snap.processed_entries = snap.processed_entries.saturating_add(1);
            snap.token_usage.prompt = snap
                .token_usage
                .prompt
                .saturating_add(review.token_usage.prompt);
            snap.token_usage.completion = snap
                .token_usage
                .completion
                .saturating_add(review.token_usage.completion);
            snap.token_usage.total = snap
                .token_usage
                .total
                .saturating_add(review.token_usage.total);
            snap.current_path = node.path.clone();
            snap.current_depth = node.depth;
            apply_finding_to_snapshot(app, &task_id, &mut snap, &item)?;
            should_stop_for_target(&snap)
        };

        emit_progress(app, state, task).await?;

        if should_expand {
            for child in &direct_children {
                let key = child.path.to_lowercase();
                if analyzed.contains(&key) || queued.contains(&key) {
                    continue;
                }
                queued.insert(key);
                queue.push(child.clone());
            }
            sort_queue(&mut queue);
        }

        if reached_target {
            break;
        }
    }

    Ok(())
}

async fn handle_sidecar_line<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
    payload: &Value,
) -> Result<(), String> {
    let task_id = task.snapshot.lock().id.clone();
    match payload.get("type").and_then(Value::as_str).unwrap_or("") {
        "task_started" | "scan_progress" | "scan_completed" => {
            let force_persist = matches!(
                payload.get("type").and_then(Value::as_str).unwrap_or(""),
                "task_started" | "scan_completed"
            );
            {
                let mut snap = task.snapshot.lock();
                snap.status = "scanning".to_string();
                if let Some(path) = payload.get("current_path").and_then(Value::as_str) {
                    snap.current_path = path.to_string();
                }
                snap.current_depth = payload
                    .get("current_depth")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32;
                let scanned_delta = payload
                    .get("scanned_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let total_delta = payload
                    .get("total_entries")
                    .and_then(Value::as_u64)
                    .unwrap_or(scanned_delta);
                snap.scanned_count = scanned_delta;
                snap.total_entries = total_delta;
                snap.max_scanned_depth = snap.max_scanned_depth.max(snap.current_depth);
            }
            emit_progress_with_options(
                app,
                state,
                task,
                should_persist_scan_progress(task, force_persist),
            )
            .await?;
        }
        "permission_denied" => {
            let warning = {
                let mut snap = task.snapshot.lock();
                snap.permission_denied_count = snap.permission_denied_count.saturating_add(1);
                if let Some(path) = payload.get("path").and_then(Value::as_str) {
                    if !snap
                        .permission_denied_paths
                        .iter()
                        .any(|x| x.eq_ignore_ascii_case(path))
                    {
                        snap.permission_denied_paths.push(path.to_string());
                        if snap.permission_denied_paths.len() > 20 {
                            let drain_to = snap.permission_denied_paths.len() - 20;
                            snap.permission_denied_paths.drain(..drain_to);
                        }
                    }
                }
                json!({
                    "taskId": task_id,
                    "type": "permission_denied",
                    "path": payload.get("path").and_then(Value::as_str).unwrap_or(""),
                    "message": payload.get("message").and_then(Value::as_str).unwrap_or("Access denied"),
                    "count": snap.permission_denied_count,
                })
            };
            emit_progress_with_options(app, state, task, should_persist_scan_progress(task, false))
                .await?;
            app.emit("scan_warning", warning)
                .map_err(|e| e.to_string())?;
        }
        _ => {}
    }
    Ok(())
}

async fn run_sidecar_scan<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
) -> Result<(), String> {
    let bin = scanner_binary_path(app)?;
    let snap = task.snapshot.lock().clone();
    let mut child = Command::new(bin);
    child
        .arg("scan")
        .arg("--db")
        .arg(state.db_path())
        .arg("--task-id")
        .arg(&snap.id)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let roots_path = if task.sidecar_roots.len() > 1
        || task
            .sidecar_roots
            .first()
            .map(|root| {
                root.depth > 0
                    || root.parent_id.is_some()
                    || !root.path.eq_ignore_ascii_case(&snap.target_path)
            })
            .unwrap_or(false)
    {
        let file_path = std::env::temp_dir().join(format!("wipeout-scan-roots-{}.json", snap.id));
        fs::write(
            &file_path,
            serde_json::to_vec(&task.sidecar_roots).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        child.arg("--roots-json").arg(&file_path);
        Some(file_path)
    } else {
        child.arg("--root").arg(&snap.target_path);
        None
    };
    let ignore_path_file = if task.ignored_paths.is_empty() {
        None
    } else {
        let file_path = std::env::temp_dir().join(format!("wipeout-scan-ignore-{}.json", snap.id));
        fs::write(
            &file_path,
            serde_json::to_vec(&task.ignored_paths).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        child.arg("--ignore-json").arg(&file_path);
        Some(file_path)
    };
    let prune_rule_file = if task.prune_rules.is_empty() {
        None
    } else {
        let file_path = std::env::temp_dir().join(format!("wipeout-scan-prune-{}.json", snap.id));
        let payload = task
            .prune_rules
            .iter()
            .map(PruneRuleJson::from)
            .collect::<Vec<_>>();
        fs::write(
            &file_path,
            serde_json::to_vec(&payload).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        child.arg("--prune-rules-json").arg(&file_path);
        Some(file_path)
    };
    if let Some(max_depth) = task.max_depth {
        child.arg("--max-depth").arg(max_depth.to_string());
    }
    let mut child = child.spawn().map_err(|e| e.to_string())?;

    *task.child_pid.lock() = child.id().into();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "scanner stdout unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "scanner stderr unavailable".to_string())?;
    let stderr_last = Arc::new(Mutex::new(String::new()));
    let stderr_last_clone = stderr_last.clone();
    let stderr_handle = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let trimmed = line.trim().to_string();
            if !trimmed.is_empty() {
                *stderr_last_clone.lock() = trimmed;
            }
        }
    });

    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if task.stop.load(Ordering::Relaxed) {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(payload) = serde_json::from_str::<Value>(&line) {
            handle_sidecar_line(app, state, task, &payload).await?;
        }
    }

    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = stderr_handle.join();
    *task.child_pid.lock() = None;
    if let Some(file_path) = roots_path {
        let _ = fs::remove_file(file_path);
    }
    if let Some(file_path) = ignore_path_file {
        let _ = fs::remove_file(file_path);
    }
    if let Some(file_path) = prune_rule_file {
        let _ = fs::remove_file(file_path);
    }
    if task.stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    if !status.success() {
        let msg = stderr_last.lock().clone();
        return Err(if msg.is_empty() {
            format!("scanner.exe exited with {status}")
        } else {
            msg
        });
    }
    Ok(())
}

async fn run_scan_task<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
) -> Result<(), String> {
    {
        let mut snap = task.snapshot.lock();
        snap.status = "scanning".to_string();
    }
    emit_progress(app, state, task).await?;
    prepare_runtime_scan_data(app, state, task).await?;
    run_sidecar_scan(app, state, task).await?;
    if task.stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    if task.snapshot.lock().auto_analyze {
        run_auto_analyze(app, state, task).await?;
    }
    if !task.stop.load(Ordering::Relaxed) {
        let mut snap = task.snapshot.lock();
        if task.scan_mode == ScanMode::FullRescanIncremental {
            snap.visible_latest = true;
        }
        snap.status = "done".to_string();
        persist::save_scan_snapshot(&state.db_path(), &snap)?;
        if task.scan_mode == ScanMode::FullRescanIncremental {
            persist::finalize_full_scan_draft(&state.db_path(), &snap.id)?;
        }
        let payload = serde_json::to_value(&*snap).map_err(|e| e.to_string())?;
        drop(snap);
        app.emit("scan_done", payload).map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn prepare_runtime_scan_data<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
) -> Result<(), String> {
    if task.scan_mode != ScanMode::DeepenIncremental {
        return Ok(());
    }

    let task_id = task.snapshot.lock().id.clone();
    let affected_paths = task
        .sidecar_roots
        .iter()
        .map(|root| root.path.clone())
        .collect::<Vec<_>>();

    emit_cache_event(
        app,
        &task_id,
        "prepare_incremental_reuse_tree",
        None,
        Some(affected_paths.len() as u64),
    )
    .await?;

    emit_cache_event(
        app,
        &task_id,
        "prepare_incremental_prune",
        None,
        Some(affected_paths.len() as u64),
    )
    .await?;

    if !affected_paths.is_empty() {
        persist::delete_scan_descendants_for_paths(&state.db_path(), &task_id, &affected_paths)?;
    }
    if !task.ignored_paths.is_empty() {
        persist::delete_scan_data_for_paths(&state.db_path(), &task_id, &task.ignored_paths)?;
    }

    if let Some(prepared_snapshot) = persist::load_scan_snapshot(&state.db_path(), &task_id)? {
        let mut snap = task.snapshot.lock();
        snap.deletable = prepared_snapshot.deletable;
        snap.deletable_count = prepared_snapshot.deletable_count;
        snap.total_cleanable = prepared_snapshot.total_cleanable;
        snap.token_usage = prepared_snapshot.token_usage;
        snap.permission_denied_count = prepared_snapshot.permission_denied_count;
        snap.permission_denied_paths = prepared_snapshot.permission_denied_paths;
        snap.last_error = prepared_snapshot.last_error;
        snap.error_message = prepared_snapshot.error_message;
    }

    emit_cache_event(app, &task_id, "prepare_incremental_ready", None, None).await?;

    Ok(())
}

fn resolve_latest_baseline_task_id(
    state: &AppState,
    target_path: &str,
    requested: Option<&str>,
) -> Result<Option<String>, String> {
    let requested = requested.unwrap_or("").trim();
    if !requested.is_empty() {
        return Ok(Some(requested.to_string()));
    }
    persist::find_latest_visible_scan_task_id_for_path(&state.db_path(), target_path)
}

pub async fn scan_start<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    input: ScanStartInput,
) -> Result<Value, String> {
    if input.target_path.trim().is_empty() {
        return Err("targetPath is required".to_string());
    }
    if input.auto_analyze.unwrap_or(true)
        && input.api_key.as_deref().unwrap_or("").trim().is_empty()
    {
        return Err("API key is required for scan analysis".to_string());
    }
    let target_path = PathBuf::from(&input.target_path)
        .to_string_lossy()
        .to_string();
    let ignored_paths = crate::backend::read_scan_ignore_paths(&state.settings_path());
    let scan_mode = ScanMode::parse(input.scan_mode.as_deref());
    let prune_rules = build_scan_prune_rules(&target_path);
    let target_root_key = persist::create_root_path_key(&target_path);
    if state.scan_tasks.lock().values().any(|task| {
        let snapshot = task.snapshot.lock();
        matches!(snapshot.status.as_str(), "idle" | "scanning" | "analyzing")
            && snapshot.root_path_key == target_root_key
    }) {
        return Err("A scan task for this path is already running".to_string());
    }
    let baseline_task_id = resolve_latest_baseline_task_id(
        state.inner(),
        &target_path,
        input.baseline_task_id.as_deref(),
    )?;
    let baseline_snapshot = if let Some(task_id) = baseline_task_id.as_deref() {
        persist::load_scan_snapshot(&state.db_path(), task_id)?
    } else {
        None
    };
    if matches!(scan_mode, ScanMode::DeepenIncremental) && baseline_snapshot.is_none() {
        return Err("A baseline task is required for deepen_incremental".to_string());
    }
    let task_id = if matches!(scan_mode, ScanMode::DeepenIncremental) {
        let existing_task_id = baseline_task_id
            .as_ref()
            .ok_or_else(|| "Missing baseline snapshot".to_string())?
            .clone();
        if state.scan_tasks.lock().contains_key(&existing_task_id) {
            return Err("The selected scan task is already running".to_string());
        }
        existing_task_id
    } else {
        format!("scan_{}", Uuid::new_v4().simple())
    };
    let deepen_boundary_nodes = if matches!(scan_mode, ScanMode::DeepenIncremental) {
        let baseline = baseline_snapshot
            .as_ref()
            .ok_or_else(|| "Missing baseline snapshot".to_string())?;
        Some(resolve_deepen_boundary_nodes(
            &state.db_path(),
            baseline,
            input.max_depth,
            &prune_rules,
        )?)
    } else {
        None
    };
    let in_place_baseline = if matches!(scan_mode, ScanMode::DeepenIncremental) {
        Some(InPlaceBaselineCache {
            nodes: persist::load_scan_node_map(&state.db_path(), &task_id)?,
            findings: persist::load_scan_findings_map(&state.db_path(), &task_id)?,
        })
    } else {
        None
    };
    let max_depth = input.max_depth.map(|value| value.clamp(1, 16));
    let min_target_size_gb = if matches!(scan_mode, ScanMode::DeepenIncremental) {
        baseline_snapshot
            .as_ref()
            .map(|snapshot| {
                round_up_to_tenth(snapshot.total_cleanable as f64 / 1024.0 / 1024.0 / 1024.0)
            })
            .unwrap_or(TARGET_SIZE_MIN_GB)
            .max(TARGET_SIZE_MIN_GB)
    } else {
        TARGET_SIZE_MIN_GB
    };
    let max_target_size_gb = TARGET_SIZE_DEFAULT_MAX_GB.max(min_target_size_gb);
    let target_size_gb = input
        .target_size_gb
        .unwrap_or(1.0)
        .clamp(min_target_size_gb, max_target_size_gb);
    let mut snapshot = if matches!(scan_mode, ScanMode::DeepenIncremental) {
        let mut existing = baseline_snapshot
            .clone()
            .ok_or_else(|| "Missing baseline snapshot".to_string())?;
        existing.status = "idle".to_string();
        existing.scan_mode = scan_mode.as_str().to_string();
        existing.baseline_task_id = None;
        existing.visible_latest = true;
        existing.root_path_key = persist::create_root_path_key(&target_path);
        existing.target_path = target_path.clone();
        existing.auto_analyze = input.auto_analyze.unwrap_or(true);
        existing.root_node_id = persist::create_node_id(&target_path);
        existing.configured_max_depth = max_depth;
        existing.current_path = target_path.clone();
        existing.current_depth = 0;
        existing.scanned_count = 0;
        existing.total_entries = 0;
        existing.processed_entries = 0;
        existing.target_size = (target_size_gb * 1024.0 * 1024.0 * 1024.0) as u64;
        existing.last_error = None;
        existing.error_message.clear();
        existing.id = task_id.clone();
        existing
    } else {
        persist::init_full_scan_draft(
            &state.db_path(),
            &task_id,
            &target_path,
            (target_size_gb * 1024.0 * 1024.0 * 1024.0) as u64,
            max_depth,
            input.auto_analyze.unwrap_or(true),
            baseline_task_id.as_deref(),
        )?;
        ScanSnapshot {
            id: task_id.clone(),
            status: "idle".to_string(),
            scan_mode: scan_mode.as_str().to_string(),
            baseline_task_id: baseline_task_id.clone(),
            visible_latest: false,
            root_path_key: persist::create_root_path_key(&target_path),
            target_path: target_path.clone(),
            auto_analyze: input.auto_analyze.unwrap_or(true),
            root_node_id: persist::create_node_id(&target_path),
            configured_max_depth: max_depth,
            max_scanned_depth: baseline_snapshot
                .as_ref()
                .map(|item| item.max_scanned_depth)
                .unwrap_or(0),
            current_path: target_path.clone(),
            current_depth: 0,
            scanned_count: 0,
            total_entries: 0,
            processed_entries: 0,
            deletable_count: 0,
            total_cleanable: 0,
            target_size: (target_size_gb * 1024.0 * 1024.0 * 1024.0) as u64,
            token_usage: TokenUsage::default(),
            deletable: Vec::new(),
            permission_denied_count: 0,
            permission_denied_paths: Vec::new(),
            last_error: None,
            error_message: String::new(),
        }
    };
    let mut sidecar_roots = vec![SidecarRoot {
        path: target_path.clone(),
        parent_id: None,
        depth: 0,
    }];
    if matches!(scan_mode, ScanMode::DeepenIncremental) {
        let (boundary_depth, boundary_nodes) = deepen_boundary_nodes
            .clone()
            .ok_or_else(|| "No boundary directories available for deeper scan".to_string())?;
        snapshot.current_depth = boundary_depth;
        snapshot.current_path = boundary_nodes
            .first()
            .map(|node| node.path.clone())
            .unwrap_or_else(|| target_path.clone());
        sidecar_roots = boundary_nodes
            .into_iter()
            .map(|node| SidecarRoot {
                path: node.path,
                parent_id: node.parent_id,
                depth: node.depth,
            })
            .collect();
    }
    persist::save_scan_snapshot(&state.db_path(), &snapshot)?;
    let (persistent_rules, persistent_rules_cleaned) =
        crate::backend::read_scan_persistent_rules_with_cleanup(&state.settings_path());
    if persistent_rules_cleaned {
        let _ =
            crate::backend::save_scan_persistent_rules(&state.settings_path(), &persistent_rules)?;
    }
    let task = Arc::new(ScanTaskRuntime {
        stop: AtomicBool::new(false),
        child_pid: Mutex::new(None),
        snapshot: Mutex::new(snapshot),
        max_depth,
        scan_mode,
        baseline_task_id,
        sidecar_roots,
        ignored_paths,
        prune_rules,
        in_place_baseline,
        persistent_rules: Mutex::new(persistent_rules),
        last_progress_persist_at: Mutex::new(None),
        ai: ScanAiConfig {
            endpoint: input
                .api_endpoint
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            api_key: input.api_key.unwrap_or_default(),
            model: input.model.unwrap_or_else(|| "gpt-4o-mini".to_string()),
            use_web_search: input.use_web_search.unwrap_or(false),
            search_api_key: input.search_api_key.unwrap_or_default(),
            response_language: input.response_language.unwrap_or_else(|| "zh".to_string()),
        },
        job: Mutex::new(None),
    });
    state
        .scan_tasks
        .lock()
        .insert(task_id.clone(), task.clone());

    let state_clone = state.inner().clone();
    let task_id_clone = task_id.clone();
    let app_clone = app.clone();
    let runtime = task.clone();
    let handle = std::thread::spawn(move || {
        tauri::async_runtime::block_on(async move {
            let result = run_scan_task(&app_clone, &state_clone, &runtime).await;
            if let Err(err) = result {
                let mut snap = runtime.snapshot.lock();
                let app_error = build_scan_task_error(&runtime, &task_id_clone, &snap.target_path, err);
                log_scan_task_error(&app_error);
                snap.status = "error".to_string();
                snap.last_error = Some(app_error.clone());
                snap.error_message = app_error.user_message.clone();
                let payload = json!({
                    "taskId": task_id_clone,
                    "error": app_error,
                    "snapshot": &*snap
                });
                if runtime.scan_mode == ScanMode::FullRescanIncremental {
                    let _ =
                        persist::discard_full_scan_draft(&state_clone.db_path(), &task_id_clone);
                } else {
                    let _ = persist::save_scan_snapshot(&state_clone.db_path(), &snap);
                }
                drop(snap);
                if let Err(emit_err) = app_clone.emit("scan_error", payload) {
                    log::error!(
                        "scan task error event emit failed: task_id={}, event=scan_error, error={}",
                        task_id_clone,
                        emit_err
                    );
                }
            } else if runtime.stop.load(Ordering::Relaxed) {
                let mut snap = runtime.snapshot.lock();
                snap.status = "stopped".to_string();
                let payload = serde_json::to_value(&*snap).unwrap_or_else(|_| json!({}));
                if runtime.scan_mode == ScanMode::FullRescanIncremental {
                    let _ =
                        persist::discard_full_scan_draft(&state_clone.db_path(), &task_id_clone);
                } else {
                    let _ = persist::save_scan_snapshot(&state_clone.db_path(), &snap);
                }
                drop(snap);
                let _ = app_clone.emit("scan_stopped", payload);
            }
            state_clone.scan_tasks.lock().remove(&task_id_clone);
        });
    });
    *task.job.lock() = Some(handle);
    Ok(json!({ "taskId": task_id, "status": "started" }))
}

pub async fn scan_stop(state: State<'_, AppState>, task_id: String) -> AppResult<Value> {
    let task = state
        .scan_tasks
        .lock()
        .get(&task_id)
        .cloned()
        .ok_or_else(|| AppError::task_not_found("Task not found"))?;
    task.stop.store(true, Ordering::Relaxed);
    if let Some(pid) = *task.child_pid.lock() {
        let _ = kill_pid(pid);
    }
    Ok(json!({ "success": true }))
}

pub async fn scan_get_active(state: State<'_, AppState>) -> Result<Vec<Value>, String> {
    let map = state.scan_tasks.lock();
    Ok(map
        .values()
        .map(|task| task.snapshot.lock().clone())
        .filter(|snap| matches!(snap.status.as_str(), "idle" | "scanning" | "analyzing"))
        .map(|snap| {
            json!({
                "taskId": snap.id,
                "id": snap.id,
                "status": snap.status,
                "scanMode": snap.scan_mode,
                "baselineTaskId": snap.baseline_task_id,
                "visibleLatest": snap.visible_latest,
                "rootPathKey": snap.root_path_key,
                "targetPath": snap.target_path,
                "autoAnalyze": snap.auto_analyze,
                "rootNodeId": snap.root_node_id,
                "configuredMaxDepth": snap.configured_max_depth,
                "maxScannedDepth": snap.max_scanned_depth,
                "currentPath": snap.current_path,
                "currentDepth": snap.current_depth,
                "scannedCount": snap.scanned_count,
                "totalEntries": snap.total_entries,
                "processedEntries": snap.processed_entries,
                "deletableCount": snap.deletable_count,
                "totalCleanable": snap.total_cleanable,
                "targetSize": snap.target_size,
                "tokenUsage": snap.token_usage,
                "deletable": snap.deletable,
                "permissionDeniedCount": snap.permission_denied_count,
                "permissionDeniedPaths": snap.permission_denied_paths,
                "lastError": snap.last_error,
                "errorMessage": snap.error_message
            })
        })
        .collect())
}

pub async fn scan_list_history(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<Value>, String> {
    persist::list_scan_history(&state.db_path(), limit.unwrap_or(20).clamp(1, 200))
}

pub async fn scan_find_latest_for_path(
    state: State<'_, AppState>,
    path: String,
) -> Result<Value, String> {
    let Some(task_id) =
        persist::find_latest_visible_scan_task_id_for_path(&state.db_path(), &path)?
    else {
        return Ok(Value::Null);
    };
    let snapshot = persist::load_scan_snapshot_summary(&state.db_path(), &task_id)?
        .ok_or_else(|| "Task not found".to_string())?;
    serde_json::to_value(snapshot).map_err(|e| e.to_string())
}

pub async fn scan_delete_history(
    state: State<'_, AppState>,
    task_id: String,
) -> AppResult<Value> {
    if let Some(task) = state.scan_tasks.lock().get(&task_id).cloned() {
        let status = task.snapshot.lock().status.clone();
        if matches!(status.as_str(), "idle" | "scanning" | "analyzing") {
            return Err(AppError::task_running("Task is still running"));
        }
    }
    if !persist::delete_committed_scan_task(&state.db_path(), &task_id)? {
        return Err(AppError::task_not_found("Task not found"));
    }
    Ok(json!({ "success": true }))
}

pub async fn scan_get_result(state: State<'_, AppState>, task_id: String) -> AppResult<Value> {
    if let Some(task) = state.scan_tasks.lock().get(&task_id).cloned() {
        let snap = task.snapshot.lock().clone();
        return serde_json::to_value(snap).map_err(|e| AppError::internal(e.to_string()));
    }
    let snapshot = persist::load_scan_snapshot(&state.db_path(), &task_id)?
        .ok_or_else(|| AppError::task_not_found("Task not found"))?;
    serde_json::to_value(snapshot).map_err(|e| AppError::internal(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use uuid::Uuid;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wipeout-{name}-{}", Uuid::new_v4()))
    }

    fn create_scan_fixture(root: &Path) {
        fs::create_dir_all(root.join("A").join("nested")).expect("create nested dir");
        fs::create_dir_all(root.join("B")).expect("create B dir");
        fs::write(root.join("root.log"), b"root").expect("write root file");
        fs::write(root.join("A").join("a.txt"), b"aaa").expect("write a.txt");
        fs::write(root.join("A").join("nested").join("deep.txt"), b"deep").expect("write deep.txt");
        fs::write(root.join("B").join("b.txt"), b"bbb").expect("write b.txt");
    }

    fn test_scanner_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("native")
            .join("scanner")
            .join("target")
            .join("debug")
            .join("scanner.exe")
    }

    fn make_snapshot(
        task_id: &str,
        root: &Path,
        max_depth: Option<u32>,
        scan_mode: &str,
        baseline_task_id: Option<String>,
    ) -> ScanSnapshot {
        let root_string = root.to_string_lossy().to_string();
        ScanSnapshot {
            id: task_id.to_string(),
            status: "idle".to_string(),
            scan_mode: scan_mode.to_string(),
            baseline_task_id,
            visible_latest: true,
            root_path_key: persist::create_root_path_key(&root_string),
            target_path: root_string.clone(),
            auto_analyze: false,
            root_node_id: persist::create_node_id(&root_string),
            configured_max_depth: max_depth,
            max_scanned_depth: 0,
            current_path: root_string,
            current_depth: 0,
            scanned_count: 0,
            total_entries: 0,
            processed_entries: 0,
            deletable_count: 0,
            total_cleanable: 0,
            target_size: 0,
            token_usage: TokenUsage::default(),
            deletable: Vec::new(),
            permission_denied_count: 0,
            permission_denied_paths: Vec::new(),
            last_error: None,
            error_message: String::new(),
        }
    }

    fn apply_sidecar_output_to_db_internal(
        db_path: &Path,
        task_id: &str,
        root: &Path,
        scan_mode: &str,
        baseline_task_id: Option<String>,
        max_depth: Option<u32>,
        stdout_lines: &[String],
        finalize_full_scan: bool,
    ) {
        let mut snapshot = make_snapshot(task_id, root, max_depth, scan_mode, baseline_task_id);

        for line in stdout_lines {
            let payload: Value = serde_json::from_str(line).expect("parse sidecar event");
            match payload.get("type").and_then(Value::as_str).unwrap_or("") {
                "task_started" | "scan_progress" | "scan_completed" => {
                    if let Some(path) = payload.get("current_path").and_then(Value::as_str) {
                        snapshot.current_path = path.to_string();
                    }
                    snapshot.current_depth = payload
                        .get("current_depth")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32;
                    snapshot.scanned_count = payload
                        .get("scanned_count")
                        .and_then(Value::as_u64)
                        .unwrap_or(snapshot.scanned_count);
                    snapshot.total_entries = payload
                        .get("total_entries")
                        .and_then(Value::as_u64)
                        .unwrap_or(snapshot.total_entries);
                    snapshot.max_scanned_depth =
                        snapshot.max_scanned_depth.max(snapshot.current_depth);
                    snapshot.status = "scanning".to_string();
                }
                "permission_denied" => {
                    snapshot.permission_denied_count =
                        snapshot.permission_denied_count.saturating_add(1);
                    if let Some(path) = payload.get("path").and_then(Value::as_str) {
                        snapshot.permission_denied_paths.push(path.to_string());
                    }
                }
                other => panic!("unexpected sidecar event: {other}"),
            }
        }

        snapshot.status = "done".to_string();
        persist::save_scan_snapshot(db_path, &snapshot).expect("save final snapshot");
        if finalize_full_scan && scan_mode == "full_rescan_incremental" {
            persist::finalize_full_scan_draft(db_path, task_id).expect("finalize full scan draft");
        }
    }

    fn apply_sidecar_output_to_db(
        db_path: &Path,
        task_id: &str,
        root: &Path,
        scan_mode: &str,
        baseline_task_id: Option<String>,
        max_depth: Option<u32>,
        stdout_lines: &[String],
    ) {
        apply_sidecar_output_to_db_internal(
            db_path,
            task_id,
            root,
            scan_mode,
            baseline_task_id,
            max_depth,
            stdout_lines,
            true,
        )
    }

    fn run_scanner_command(args: &[String]) -> Vec<String> {
        let scanner_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("native")
            .join("scanner");
        let build_status = Command::new("cargo")
            .arg("build")
            .current_dir(&scanner_dir)
            .status()
            .expect("build scanner");
        assert!(
            build_status.success(),
            "scanner build failed with {build_status}"
        );

        let scanner = test_scanner_path();
        assert!(
            scanner.exists(),
            "scanner.exe missing at {}",
            scanner.display()
        );

        let mut child = Command::new(scanner)
            .args(args)
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn scanner");
        let stdout = child.stdout.take().expect("scanner stdout");
        let lines = BufReader::new(stdout)
            .lines()
            .map(|line| line.expect("read scanner line"))
            .collect::<Vec<_>>();
        let status = child.wait().expect("wait scanner");
        assert!(status.success(), "scanner exited with {status}");
        lines
    }

    #[test]
    fn full_scan_sidecar_output_persists_snapshot_and_nodes() {
        let root = temp_path("scan-root");
        let db_path = temp_path("scan-db.sqlite");
        fs::create_dir_all(&root).expect("create root");
        create_scan_fixture(&root);
        persist::init_db(&db_path).expect("init db");

        let task_id = format!("scan_{}", Uuid::new_v4().simple());
        let root_string = root.to_string_lossy().to_string();
        persist::init_full_scan_draft(&db_path, &task_id, &root_string, 0, Some(1), false, None)
            .expect("init full scan task");

        let lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            task_id.clone(),
            "--root".to_string(),
            root_string.clone(),
            "--max-depth".to_string(),
            "1".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &task_id,
            &root,
            "full_rescan_incremental",
            None,
            Some(1),
            &lines,
        );

        let snapshot = persist::load_scan_snapshot(&db_path, &task_id)
            .expect("load snapshot")
            .expect("snapshot exists");
        assert_eq!(snapshot.status, "done");
        assert_eq!(snapshot.configured_max_depth, Some(1));
        assert_eq!(snapshot.max_scanned_depth, 1);
        assert!(snapshot.scanned_count >= 4);

        let root_children =
            persist::load_scan_children(&db_path, &task_id, &snapshot.root_node_id, false)
                .expect("load root children");
        assert!(root_children.iter().any(|node| node.name == "A"));
        assert!(root_children.iter().any(|node| node.name == "B"));

        let boundaries =
            persist::list_boundary_scan_nodes(&db_path, &task_id, 1).expect("list boundaries");
        assert!(boundaries.iter().any(|node| node.name == "A"));
        assert!(boundaries.iter().any(|node| node.name == "B"));

        let latest = persist::find_latest_visible_scan_task_id_for_path(&db_path, &root_string)
            .expect("find latest")
            .expect("latest task");
        assert_eq!(latest, task_id);

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn deepen_incremental_scan_updates_existing_tree_in_place() {
        let root = temp_path("deepen-root");
        let db_path = temp_path("deepen-db.sqlite");
        fs::create_dir_all(&root).expect("create root");
        create_scan_fixture(&root);
        persist::init_db(&db_path).expect("init db");

        let baseline_task_id = format!("scan_{}", Uuid::new_v4().simple());
        let root_string = root.to_string_lossy().to_string();
        persist::init_full_scan_draft(
            &db_path,
            &baseline_task_id,
            &root_string,
            0,
            Some(1),
            false,
            None,
        )
        .expect("init baseline task");
        let baseline_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            baseline_task_id.clone(),
            "--root".to_string(),
            root_string.clone(),
            "--max-depth".to_string(),
            "1".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &baseline_task_id,
            &root,
            "full_rescan_incremental",
            None,
            Some(1),
            &baseline_lines,
        );

        let boundary_nodes =
            persist::list_boundary_scan_nodes(&db_path, &baseline_task_id, 1).expect("boundaries");
        assert!(
            !boundary_nodes.is_empty(),
            "baseline boundary nodes missing"
        );

        persist::delete_scan_descendants_for_paths(
            &db_path,
            &baseline_task_id,
            &boundary_nodes
                .iter()
                .map(|node| node.path.clone())
                .collect::<Vec<_>>(),
        )
        .expect("delete boundary descendants");

        let roots_json = temp_path("deepen-roots.json");
        let roots_payload = boundary_nodes
            .iter()
            .map(|node| {
                json!({
                    "path": node.path,
                    "parent_id": node.parent_id,
                    "depth": node.depth,
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            &roots_json,
            serde_json::to_vec(&roots_payload).expect("serialize roots"),
        )
        .expect("write roots payload");

        let deepen_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            baseline_task_id.clone(),
            "--roots-json".to_string(),
            roots_json.to_string_lossy().to_string(),
            "--max-depth".to_string(),
            "2".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &baseline_task_id,
            &root,
            "deepen_incremental",
            None,
            Some(2),
            &deepen_lines,
        );

        let deepen_snapshot = persist::load_scan_snapshot(&db_path, &baseline_task_id)
            .expect("load deepen snapshot")
            .expect("deepen snapshot exists");
        assert_eq!(deepen_snapshot.status, "done");
        assert_eq!(deepen_snapshot.id, baseline_task_id);
        assert_eq!(deepen_snapshot.configured_max_depth, Some(2));
        assert_eq!(deepen_snapshot.max_scanned_depth, 2);

        let node_map =
            persist::load_scan_node_map(&db_path, &baseline_task_id).expect("load node map");
        assert!(node_map.contains_key(&root.join("A").to_string_lossy().to_lowercase()));
        assert!(node_map.contains_key(
            &root
                .join("A")
                .join("nested")
                .to_string_lossy()
                .to_lowercase()
        ));
        assert!(!node_map.contains_key(
            &root
                .join("A")
                .join("nested")
                .join("deep.txt")
                .to_string_lossy()
                .to_lowercase()
        ));

        let dir_a_id = persist::create_node_id(&root.join("A").to_string_lossy());
        let dir_a_children =
            persist::load_scan_children(&db_path, &baseline_task_id, &dir_a_id, false)
                .expect("load A children");
        assert!(dir_a_children.iter().any(|node| node.name == "nested"));
        assert!(dir_a_children.iter().any(|node| node.name == "a.txt"));

        let latest = persist::find_latest_visible_scan_task_id_for_path(&db_path, &root_string)
            .expect("find latest task")
            .expect("latest task exists");
        assert_eq!(latest, baseline_task_id);

        let _ = fs::remove_file(&roots_json);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn deepen_incremental_uses_actual_scanned_depth_for_stopped_baseline() {
        let root = temp_path("deepen-stopped-root");
        let db_path = temp_path("deepen-stopped-db.sqlite");
        fs::create_dir_all(&root).expect("create root");
        create_scan_fixture(&root);
        persist::init_db(&db_path).expect("init db");

        let baseline_task_id = format!("scan_{}", Uuid::new_v4().simple());
        let root_string = root.to_string_lossy().to_string();
        persist::init_full_scan_draft(
            &db_path,
            &baseline_task_id,
            &root_string,
            0,
            Some(1),
            false,
            None,
        )
        .expect("init baseline task");
        let baseline_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            baseline_task_id.clone(),
            "--root".to_string(),
            root_string.clone(),
            "--max-depth".to_string(),
            "1".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &baseline_task_id,
            &root,
            "full_rescan_incremental",
            None,
            Some(1),
            &baseline_lines,
        );

        let mut stopped_snapshot = persist::load_scan_snapshot(&db_path, &baseline_task_id)
            .expect("load stopped snapshot")
            .expect("stopped snapshot exists");
        stopped_snapshot.scan_mode = "deepen_incremental".to_string();
        stopped_snapshot.configured_max_depth = Some(3);
        stopped_snapshot.status = "stopped".to_string();
        stopped_snapshot.current_path = root_string.clone();
        stopped_snapshot.current_depth = 1;
        persist::save_scan_snapshot(&db_path, &stopped_snapshot).expect("save stopped snapshot");

        let stopped_snapshot = persist::load_scan_snapshot(&db_path, &baseline_task_id)
            .expect("reload stopped snapshot")
            .expect("reloaded stopped snapshot exists");
        assert_eq!(stopped_snapshot.configured_max_depth, Some(3));
        assert_eq!(stopped_snapshot.max_scanned_depth, 1);

        let (boundary_depth, boundary_nodes) =
            resolve_deepen_boundary_nodes(&db_path, &stopped_snapshot, Some(4), &[])
                .expect("resolve deepen boundary nodes");
        assert_eq!(boundary_depth, 1);
        assert!(boundary_nodes.iter().any(|node| node.name == "A"));
        assert!(boundary_nodes.iter().any(|node| node.name == "B"));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn full_scan_draft_finalize_prunes_ancestor_cache_and_switches_owner() {
        let root = temp_path("child-owner-root");
        let db_path = temp_path("child-owner-db.sqlite");
        fs::create_dir_all(&root).expect("create root");
        create_scan_fixture(&root);
        persist::init_db(&db_path).expect("init db");

        let root_task_id = format!("scan_{}", Uuid::new_v4().simple());
        let root_string = root.to_string_lossy().to_string();
        persist::init_full_scan_draft(
            &db_path,
            &root_task_id,
            &root_string,
            0,
            Some(3),
            false,
            None,
        )
        .expect("init root draft");
        let root_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            root_task_id.clone(),
            "--root".to_string(),
            root_string.clone(),
            "--max-depth".to_string(),
            "3".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &root_task_id,
            &root,
            "full_rescan_incremental",
            None,
            Some(3),
            &root_lines,
        );

        let deep_txt = root.join("A").join("nested").join("deep.txt");
        fs::remove_file(&deep_txt).expect("remove deep file");
        fs::write(root.join("A").join("nested").join("fresh.txt"), b"fresh")
            .expect("write fresh file");

        let child_root = root.join("A");
        let child_task_id = format!("scan_{}", Uuid::new_v4().simple());
        let child_root_string = child_root.to_string_lossy().to_string();
        persist::init_full_scan_draft(
            &db_path,
            &child_task_id,
            &child_root_string,
            0,
            Some(2),
            false,
            Some(&root_task_id),
        )
        .expect("init child draft");
        let child_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            child_task_id.clone(),
            "--root".to_string(),
            child_root_string.clone(),
            "--max-depth".to_string(),
            "2".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &child_task_id,
            &child_root,
            "full_rescan_incremental",
            Some(root_task_id.clone()),
            Some(2),
            &child_lines,
        );

        let root_node_map =
            persist::load_scan_node_map(&db_path, &root_task_id).expect("root node map");
        assert!(root_node_map.contains_key(&child_root_string.to_lowercase()));
        assert!(!root_node_map.contains_key(&deep_txt.to_string_lossy().to_lowercase()));
        assert!(!root_node_map.contains_key(
            &root
                .join("A")
                .join("nested")
                .join("fresh.txt")
                .to_string_lossy()
                .to_lowercase()
        ));

        let child_node_map =
            persist::load_scan_node_map(&db_path, &child_task_id).expect("child node map");
        assert_eq!(
            root_node_map
                .get(&child_root_string.to_lowercase())
                .expect("ancestor boundary node")
                .size,
            child_node_map
                .get(&child_root_string.to_lowercase())
                .expect("child root node")
                .size
        );

        let dir_a_id = persist::create_node_id(&child_root_string);
        let handed_off_children =
            persist::load_scan_children(&db_path, &root_task_id, &dir_a_id, false)
                .expect("load handed off children");
        assert!(handed_off_children.iter().any(|node| node.name == "nested"));
        assert!(handed_off_children.iter().any(|node| node.name == "a.txt"));

        let nested_children = persist::load_scan_children(
            &db_path,
            &child_task_id,
            &persist::create_node_id(&root.join("A").join("nested").to_string_lossy()),
            false,
        )
        .expect("load nested children");
        assert!(nested_children.iter().any(|node| node.name == "fresh.txt"));
        assert!(!nested_children.iter().any(|node| node.name == "deep.txt"));

        let latest_child =
            persist::find_latest_visible_scan_task_id_for_path(&db_path, &child_root_string)
                .expect("find child owner")
                .expect("child owner exists");
        assert_eq!(latest_child, child_task_id);

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn discarding_full_scan_draft_keeps_ancestor_cache_unchanged() {
        let root = temp_path("discard-draft-root");
        let db_path = temp_path("discard-draft-db.sqlite");
        fs::create_dir_all(&root).expect("create root");
        create_scan_fixture(&root);
        persist::init_db(&db_path).expect("init db");

        let root_task_id = format!("scan_{}", Uuid::new_v4().simple());
        let root_string = root.to_string_lossy().to_string();
        persist::init_full_scan_draft(
            &db_path,
            &root_task_id,
            &root_string,
            0,
            Some(2),
            false,
            None,
        )
        .expect("init root draft");
        let root_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            root_task_id.clone(),
            "--root".to_string(),
            root_string.clone(),
            "--max-depth".to_string(),
            "2".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &root_task_id,
            &root,
            "full_rescan_incremental",
            None,
            Some(2),
            &root_lines,
        );

        let child_root = root.join("A");
        let child_root_string = child_root.to_string_lossy().to_string();
        let root_before_discard =
            persist::load_scan_node_map(&db_path, &root_task_id).expect("load root baseline");
        let baseline_child_size = root_before_discard
            .get(&child_root_string.to_lowercase())
            .expect("baseline child node")
            .size;

        fs::remove_file(root.join("A").join("nested").join("deep.txt")).expect("remove deep file");
        let draft_task_id = format!("scan_{}", Uuid::new_v4().simple());
        persist::init_full_scan_draft(
            &db_path,
            &draft_task_id,
            &child_root_string,
            0,
            Some(2),
            false,
            Some(&root_task_id),
        )
        .expect("init child draft");
        let child_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            draft_task_id.clone(),
            "--root".to_string(),
            child_root_string.clone(),
            "--max-depth".to_string(),
            "2".to_string(),
        ]);
        apply_sidecar_output_to_db_internal(
            &db_path,
            &draft_task_id,
            &child_root,
            "full_rescan_incremental",
            Some(root_task_id.clone()),
            Some(2),
            &child_lines,
            false,
        );

        assert!(
            persist::load_full_scan_draft_snapshot(&db_path, &draft_task_id)
                .expect("load draft snapshot")
                .is_some()
        );
        persist::discard_full_scan_draft(&db_path, &draft_task_id).expect("discard child draft");

        let root_after_discard =
            persist::load_scan_node_map(&db_path, &root_task_id).expect("load root after discard");
        assert_eq!(
            root_after_discard
                .get(&child_root_string.to_lowercase())
                .expect("child node after discard")
                .size,
            baseline_child_size
        );
        assert!(
            persist::load_full_scan_draft_snapshot(&db_path, &draft_task_id)
                .expect("reload draft snapshot")
                .is_none()
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn parse_chat_completion_http_body_extracts_content_and_usage() {
        let raw_body = r#"{
          "choices": [
            {
              "message": {
                "content": "{\"classification\":\"keep\",\"reason\":\"ok\",\"risk\":\"low\"}"
              }
            }
          ],
          "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 34,
            "total_tokens": 46
          }
        }"#;
        let parsed =
            parse_chat_completion_http_body(StatusCode::OK, raw_body).expect("parse success");
        assert!(parsed.content.contains("\"classification\":\"keep\""));
        assert_eq!(parsed.token_usage.prompt, 12);
        assert_eq!(parsed.token_usage.completion, 34);
        assert_eq!(parsed.token_usage.total, 46);
        assert_eq!(parsed.raw_body, raw_body);
    }

    #[test]
    fn parse_chat_completion_http_body_keeps_raw_body_on_decode_error() {
        let raw_body = "<html>upstream gateway error</html>";
        let err =
            parse_chat_completion_http_body(StatusCode::OK, raw_body).expect_err("decode error");
        assert!(err.message.contains("error decoding response body"));
        assert!(err.message.contains("upstream gateway error"));
        assert_eq!(err.raw_body, raw_body);
    }

    fn make_scan_node(
        path: &str,
        node_type: &str,
        size: u64,
        child_count: u64,
    ) -> persist::ScanNode {
        let normalized_path = PathBuf::from(path).to_string_lossy().to_string();
        let name = PathBuf::from(path)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(path)
            .to_string();
        persist::ScanNode {
            id: persist::create_node_id(&normalized_path),
            parent_id: None,
            path: normalized_path,
            name,
            node_type: node_type.to_string(),
            depth: 1,
            self_size: size,
            size,
            child_count,
            mtime_ms: None,
        }
    }

    #[test]
    fn local_rules_match_protected_and_temp_paths() {
        let keep_node = make_scan_node(r"C:\Windows", "directory", 0, 1);
        let temp_node = make_scan_node(r"C:\Users\tester\AppData\Local\Temp", "directory", 1024, 1);
        let temp_file = make_scan_node(r"C:\Users\tester\Downloads\foo.tmp", "file", 64, 0);

        let keep_rule =
            maybe_local_rule_decision("zh", &keep_node, None).expect("keep rule should match");
        let delete_rule =
            maybe_local_rule_decision("zh", &temp_node, None).expect("temp rule should match");
        let file_rule =
            maybe_local_rule_decision("zh", &temp_file, None).expect("tmp file rule should match");

        assert_eq!(keep_rule.classification, CLASS_KEEP_ALL);
        assert!(!keep_rule.should_expand);
        assert_eq!(delete_rule.classification, CLASS_DELETE_ALL);
        assert!(!delete_rule.should_expand);
        assert_eq!(file_rule.classification, CLASS_DELETE_ALL);
    }

    #[test]
    fn local_rules_match_build_and_browser_cache_paths() {
        let next_cache = make_scan_node(r"E:\repo\frontend\.next\cache", "directory", 1024, 1);
        let node_modules_cache = make_scan_node(
            r"E:\repo\frontend\node_modules\.cache",
            "directory",
            1024,
            1,
        );
        let chrome_cache = make_scan_node(
            r"C:\Users\tester\AppData\Local\Google\Chrome\User Data\Default\Cache",
            "directory",
            1024,
            1,
        );
        let chrome_code_cache = make_scan_node(
            r"C:\Users\tester\AppData\Local\Google\Chrome\User Data\Profile 1\Code Cache",
            "directory",
            1024,
            1,
        );
        let edge_cache = make_scan_node(
            r"C:\Users\tester\AppData\Local\Microsoft\Edge\User Data\Default\Cache",
            "directory",
            1024,
            1,
        );
        let nuxt_dir = make_scan_node(r"E:\repo\frontend\.nuxt", "directory", 1024, 1);

        for node in [
            &next_cache,
            &node_modules_cache,
            &chrome_cache,
            &chrome_code_cache,
            &edge_cache,
            &nuxt_dir,
        ] {
            let rule = maybe_local_rule_decision("zh", node, None)
                .expect("expected known build or browser cache path to match");
            assert_eq!(rule.classification, CLASS_DELETE_ALL);
            assert!(!rule.should_expand);
        }
    }

    #[test]
    fn local_rules_do_not_match_generic_or_unsupported_cache_paths() {
        let generic_cache = make_scan_node(r"E:\repo\frontend\Cache", "directory", 1024, 1);
        let firefox_cache = make_scan_node(
            r"C:\Users\tester\AppData\Local\Mozilla\Firefox\Profiles\abc.default-release\cache2",
            "directory",
            1024,
            1,
        );
        let downloads = make_scan_node(r"C:\Users\tester\Downloads", "directory", 1024, 1);
        let logs = make_scan_node(r"E:\repo\logs", "directory", 1024, 1);
        let archives = make_scan_node(r"E:\repo\archives", "directory", 1024, 1);

        for node in [&generic_cache, &firefox_cache, &downloads, &logs, &archives] {
            assert!(
                maybe_local_rule_decision("zh", node, None).is_none(),
                "path should not match static local rule: {}",
                node.path
            );
        }
    }

    #[test]
    fn build_directory_portrait_summarizes_children() {
        let dir = make_scan_node(r"C:\Users\tester\AppData\Local\Temp", "directory", 0, 3);
        let mut child_dir = make_scan_node(
            r"C:\Users\tester\AppData\Local\Temp\cache",
            "directory",
            2048,
            0,
        );
        child_dir.parent_id = Some(dir.id.clone());
        let mut file_a = make_scan_node(r"C:\Users\tester\AppData\Local\Temp\a.tmp", "file", 64, 0);
        file_a.parent_id = Some(dir.id.clone());
        let mut file_b = make_scan_node(r"C:\Users\tester\AppData\Local\Temp\b.tmp", "file", 32, 0);
        file_b.parent_id = Some(dir.id.clone());

        let portrait =
            build_directory_portrait(&dir, &[child_dir.clone(), file_a.clone(), file_b.clone()]);
        assert_eq!(portrait.direct_dir_count, 1);
        assert_eq!(portrait.direct_file_count, 2);
        assert!(portrait.name_tags.iter().any(|tag| tag == "temp"));
        assert_eq!(portrait.top_children[0].name, child_dir.name);
        assert_eq!(portrait.top_file_extensions[0].extension, ".tmp");
        assert_eq!(portrait.top_file_extensions[0].count, 2);
    }

    #[test]
    fn scan_prompt_classification_tokens_follow_response_language() {
        assert_eq!(
            scan_prompt_classification_tokens("zh-CN"),
            ("全部删除", "全部保留", "展开分析")
        );
        assert_eq!(
            scan_prompt_classification_tokens("en"),
            ("delete_all", "keep_all", "expand_analysis")
        );
    }

    #[test]
    fn normalize_scan_ai_classification_accepts_localized_values() {
        assert_eq!(
            normalize_scan_ai_classification(Some("全部删除")),
            CLASS_DELETE_ALL
        );
        assert_eq!(normalize_scan_ai_classification(Some("全部保留")), CLASS_KEEP_ALL);
        assert_eq!(
            normalize_scan_ai_classification(Some("展开分析")),
            CLASS_EXPAND_ANALYSIS
        );
        assert_eq!(
            normalize_scan_ai_classification(Some("delete_all")),
            CLASS_DELETE_ALL
        );
        assert_eq!(normalize_scan_ai_classification(Some("keep_all")), CLASS_KEEP_ALL);
        assert_eq!(
            normalize_scan_ai_classification(Some("expand_analysis")),
            CLASS_EXPAND_ANALYSIS
        );
    }

    #[test]
    fn expand_analysis_directories_always_expand() {
        let dir = make_scan_node(r"C:\Users\tester\Misc", "directory", 1024, 2);
        let review = ScanReview {
            classification: CLASS_EXPAND_ANALYSIS.to_string(),
            reason: "manual review".to_string(),
            risk: "medium".to_string(),
            token_usage: TokenUsage::default(),
            trace: Value::Null,
        };
        assert!(should_expand_node(&dir, &review));
    }

    #[test]
    fn ai_rule_promotion_only_allows_keep_and_low_risk_safe_delete() {
        let keep = ScanReview {
            classification: CLASS_KEEP_ALL.to_string(),
            reason: "keep".to_string(),
            risk: "high".to_string(),
            token_usage: TokenUsage::default(),
            trace: Value::Null,
        };
        let safe_low = ScanReview {
            classification: CLASS_DELETE_ALL.to_string(),
            reason: "safe".to_string(),
            risk: "low".to_string(),
            token_usage: TokenUsage::default(),
            trace: Value::Null,
        };
        let safe_medium = ScanReview {
            classification: CLASS_DELETE_ALL.to_string(),
            reason: "review".to_string(),
            risk: "medium".to_string(),
            token_usage: TokenUsage::default(),
            trace: Value::Null,
        };
        let expand_analysis = ScanReview {
            classification: CLASS_EXPAND_ANALYSIS.to_string(),
            reason: "expand_analysis".to_string(),
            risk: "medium".to_string(),
            token_usage: TokenUsage::default(),
            trace: Value::Null,
        };

        assert!(should_promote_ai_rule(&keep));
        assert!(should_promote_ai_rule(&safe_low));
        assert!(!should_promote_ai_rule(&safe_medium));
        assert!(!should_promote_ai_rule(&expand_analysis));
    }

    #[test]
    fn expand_analysis_directory_reuse_forces_expand() {
        let current = make_scan_node(r"C:\Users\tester\Misc", "directory", 1024, 2);
        let baseline = current.clone();
        let child = make_scan_node(r"C:\Users\tester\Misc\cache", "directory", 512, 0);
        let reused = resolve_reusable_finding(
            &current,
            Some(&build_directory_portrait(
                &current,
                std::slice::from_ref(&child),
            )),
            Some(&baseline),
            Some(&persist::ScanFindingRecord {
                item: ScanResultItem {
                    name: current.name.clone(),
                    path: current.path.clone(),
                    size: current.size,
                    item_type: "directory".to_string(),
                    purpose: String::new(),
                    reason: "old expand_analysis".to_string(),
                    risk: "medium".to_string(),
                    classification: CLASS_EXPAND_ANALYSIS.to_string(),
                    source: SOURCE_AI.to_string(),
                },
                should_expand: false,
            }),
            std::slice::from_ref(&child),
        )
        .expect("reuse should match");

        assert_eq!(reused.0.source, SOURCE_BASELINE_REUSE);
        assert!(reused.1);
    }

    #[test]
    fn build_scan_prune_rules_only_applies_to_c_drive_root() {
        let c_rules = build_scan_prune_rules("C:\\");
        assert!(c_rules.iter().any(|rule| matches!(
            rule,
            ScanPruneRule::SkipSubtree { path } if path == "C:\\System Volume Information"
        )));
        assert!(c_rules.iter().any(|rule| matches!(
            rule,
            ScanPruneRule::CapSubtreeDepth {
                path,
                max_relative_depth
            } if path == "C:\\Windows" && *max_relative_depth == 2
        )));
        assert!(c_rules
            .iter()
            .any(|rule| matches!(rule, ScanPruneRule::SkipReparsePoints)));

        assert!(build_scan_prune_rules("D:\\").is_empty());
        assert!(build_scan_prune_rules("C:\\Users\\32858\\AppData").is_empty());
    }

    #[test]
    fn deepen_boundary_filter_skips_pruned_and_non_expandable_nodes() {
        let prune_rules = build_scan_prune_rules("C:\\");
        let windows_cap = persist::ScanNode {
            id: "1".to_string(),
            parent_id: Some("root".to_string()),
            path: "C:\\Windows\\System32\\Config".to_string(),
            name: "Config".to_string(),
            node_type: "directory".to_string(),
            depth: 3,
            self_size: 0,
            size: 1024,
            child_count: 2,
            mtime_ms: None,
        };
        let users_node = persist::ScanNode {
            id: "2".to_string(),
            parent_id: Some("root".to_string()),
            path: "C:\\Users\\32858".to_string(),
            name: "32858".to_string(),
            node_type: "directory".to_string(),
            depth: 2,
            self_size: 0,
            size: 2048,
            child_count: 3,
            mtime_ms: None,
        };
        let empty_leaf = persist::ScanNode {
            id: "3".to_string(),
            parent_id: Some("root".to_string()),
            path: "C:\\Temp\\Empty".to_string(),
            name: "Empty".to_string(),
            node_type: "directory".to_string(),
            depth: 2,
            self_size: 0,
            size: 0,
            child_count: 0,
            mtime_ms: None,
        };

        assert!(should_exclude_deepen_boundary_node(
            &windows_cap,
            &prune_rules
        ));
        assert!(!should_exclude_deepen_boundary_node(
            &users_node,
            &prune_rules
        ));
        assert!(should_exclude_deepen_boundary_node(
            &empty_leaf,
            &prune_rules
        ));
    }
}
