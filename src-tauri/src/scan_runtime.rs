use crate::backend::{AppState, ScanResultItem, ScanSnapshot, ScanStartInput, TokenUsage};
use crate::persist;
use crate::web_search::{
    format_web_search_context, parse_web_search_request, tavily_search, web_search_trace_to_value,
};
use parking_lot::Mutex;
use reqwest::StatusCode;
use serde::Deserialize;
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
const SCAN_NODE_FLUSH_THRESHOLD: usize = 1024;
const SCAN_PROGRESS_PERSIST_INTERVAL: Duration = Duration::from_millis(750);
const CHAT_COMPLETION_TIMEOUT_SECS: u64 = 180;
const RESPONSE_ERROR_SNIPPET_CHARS: usize = 400;

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

#[derive(Clone)]
struct IncrementalContext {
    baseline_nodes: HashMap<String, persist::ScanNode>,
    baseline_findings: HashMap<String, persist::ScanFindingRecord>,
    analyze_roots: Vec<persist::ScanNode>,
    deleted_count: u64,
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
    scan_count_offset: u64,
    pending_sidecar_nodes: Mutex<Vec<persist::ScanNodeUpsert>>,
    last_progress_persist_at: Mutex<Option<Instant>>,
    ai: ScanAiConfig,
    pub job: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone, Debug)]
struct ScanReview {
    classification: String,
    reason: String,
    risk: String,
    has_potential_deletable_subfolders: bool,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SidecarNodeBatchEvent {
    nodes: Vec<SidecarNodeRecord>,
}

#[derive(Clone, Debug, Deserialize)]
struct SidecarNodeRecord {
    node_id: String,
    parent_id: Option<String>,
    path: String,
    name: String,
    node_type: String,
    depth: u32,
    self_size: i64,
    total_size: i64,
    child_count: i64,
    mtime_ms: Option<i64>,
    ext: String,
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
    is_directory: bool,
    allow_web_search: bool,
) -> String {
    let response_language_name = localized_language_name(response_language, response_language);
    let final_schema = if is_directory {
        "{\"classification\":\"safe_to_delete|suspicious|keep\",\"reason\":\"...\",\"risk\":\"low|medium|high\",\"hasPotentialDeletableSubfolders\":true}"
    } else {
        "{\"classification\":\"safe_to_delete|suspicious|keep\",\"reason\":\"...\",\"risk\":\"low|medium|high\"}"
    };

    let mut lines = vec![
        "You are a disk cleanup safety assistant.".to_string(),
        "Return JSON only.".to_string(),
        format!("Final schema: {final_schema}"),
        "Be conservative. If unsure, use suspicious.".to_string(),
        format!("The \"reason\" field must be written in {response_language_name} only."),
    ];
    if allow_web_search {
        lines.push(
            "If local metadata is insufficient and external context is necessary, you may return {\"action\":\"web_search\",\"query\":\"...\",\"reason\":\"...\"} instead of the final schema. Use one concise query only."
                .to_string(),
        );
    }
    lines.join("\n")
}

async fn analyze_scan_node(
    ai: &ScanAiConfig,
    node: &persist::ScanNode,
    child_dirs: &[persist::ScanNode],
) -> ScanReview {
    let prompt_in_zh = is_zh_language(&ai.response_language);
    let child_summary = if child_dirs.is_empty() {
        if prompt_in_zh {
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
        if prompt_in_zh {
            format!(
                "类型：目录\n路径：{}\n名称：{}\n大小：{}\n直接子目录：\n{}\n请判断整个目录是否可以安全删除，以及它是否可能包含可删除的子目录。",
                node.path,
                node.name,
                format_size(node.size),
                child_summary
            )
        } else {
            format!(
                "Type: directory\nPath: {}\nName: {}\nSize: {}\nDirect child directories:\n{}\nJudge whether the whole directory can be deleted safely, and whether it may contain deletable subfolders.",
                node.path,
                node.name,
                format_size(node.size),
                child_summary
            )
        }
    } else if prompt_in_zh {
        format!(
            "类型：文件\n路径：{}\n名称：{}\n大小：{}\n请判断该文件是否可以安全删除。",
            node.path,
            node.name,
            format_size(node.size)
        )
    } else {
        format!(
            "Type: file\nPath: {}\nName: {}\nSize: {}\nJudge whether the file can be deleted safely.",
            node.path,
            node.name,
            format_size(node.size)
        )
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
            format!(
                "{user_prompt}\n\nWeb search context:\n{context}\n\nReturn the final classification JSON only."
            )
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
                                    classification: "suspicious".to_string(),
                                    reason: default_analysis_failed_reason(
                                        &ai.response_language,
                                        &format!("web search failed: {err}"),
                                    ),
                                    risk: "high".to_string(),
                                    has_potential_deletable_subfolders: node.node_type
                                        == "directory",
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
                    classification: parsed
                        .get("classification")
                        .and_then(Value::as_str)
                        .unwrap_or("suspicious")
                        .to_string(),
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
                    has_potential_deletable_subfolders: parsed
                        .get("hasPotentialDeletableSubfolders")
                        .and_then(Value::as_bool)
                        .unwrap_or(node.node_type == "directory"),
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
                    classification: "suspicious".to_string(),
                    reason: default_analysis_failed_reason(&ai.response_language, &err.message),
                    risk: "high".to_string(),
                    has_potential_deletable_subfolders: node.node_type == "directory",
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
        classification: "suspicious".to_string(),
        reason: default_analysis_failed_reason(
            &ai.response_language,
            "model requested web search but did not return a final classification",
        ),
        risk: "high".to_string(),
        has_potential_deletable_subfolders: node.node_type == "directory",
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
        reason: review.reason.clone(),
        risk: review.risk.clone(),
        classification: review.classification.clone(),
        source: "priority_queue".to_string(),
    }
}

fn should_expand_node(node: &persist::ScanNode, review: &ScanReview) -> bool {
    node.node_type == "directory"
        && review.classification != "safe_to_delete"
        && review.has_potential_deletable_subfolders
}

fn apply_finding_to_snapshot<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    snap: &mut ScanSnapshot,
    item: &ScanResultItem,
) -> Result<(), String> {
    if item.classification == "safe_to_delete"
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
    if persist_snapshot {
        flush_pending_sidecar_nodes(state, task)?;
    }
    let snap = task.snapshot.lock().clone();
    if persist_snapshot {
        persist::save_scan_snapshot(&state.db_path, &snap)?;
    }
    app.emit(
        "scan_progress",
        serde_json::to_value(&snap).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn queue_sidecar_nodes(
    task: &Arc<ScanTaskRuntime>,
    mut rows: Vec<persist::ScanNodeUpsert>,
) -> usize {
    let mut pending = task.pending_sidecar_nodes.lock();
    pending.append(&mut rows);
    pending.len()
}

fn flush_pending_sidecar_nodes(
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
) -> Result<usize, String> {
    let rows = {
        let mut pending = task.pending_sidecar_nodes.lock();
        if pending.is_empty() {
            return Ok(0);
        }
        std::mem::take(&mut *pending)
    };
    let task_id = task.snapshot.lock().id.clone();
    let row_count = rows.len();
    persist::upsert_scan_nodes(&state.db_path, &task_id, &rows)?;
    Ok(row_count)
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
    let current_nodes = persist::load_scan_node_map(&state.db_path, &task_id)?;
    let analyze_roots = if task.scan_mode == ScanMode::DeepenIncremental {
        task.sidecar_roots
            .iter()
            .filter_map(|root| current_nodes.get(&root.path.to_lowercase()).cloned())
            .collect::<Vec<_>>()
    } else {
        persist::load_scan_children(&state.db_path, &task_id, &snapshot.root_node_id, false)?
    };
    if task
        .baseline_task_id
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        return Ok(Some(IncrementalContext {
            baseline_nodes: HashMap::new(),
            baseline_findings: HashMap::new(),
            analyze_roots,
            deleted_count: 0,
        }));
    }
    let baseline_task_id = task.baseline_task_id.clone().unwrap_or_default();
    let baseline_nodes = persist::load_scan_node_map(&state.db_path, &baseline_task_id)?;
    let baseline_findings = persist::load_scan_findings_map(&state.db_path, &baseline_task_id)?;
    let deleted_count = if task.scan_mode == ScanMode::FullRescanIncremental {
        baseline_nodes
            .keys()
            .filter(|path| !current_nodes.contains_key(*path))
            .count() as u64
    } else {
        0
    };
    Ok(Some(IncrementalContext {
        baseline_nodes,
        baseline_findings,
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

        let child_dirs = if node.node_type == "directory" {
            persist::load_scan_children(&state.db_path, &task_id, &node.id, true)?
        } else {
            Vec::new()
        };

        let node_key = node.path.to_lowercase();
        let reusable_finding = ctx
            .baseline_nodes
            .get(&node_key)
            .filter(|baseline| nodes_match(&node, baseline))
            .and_then(|_| ctx.baseline_findings.get(&node_key).cloned());

        if let Some(reused) = reusable_finding {
            persist::upsert_scan_finding(
                &state.db_path,
                &task_id,
                &reused.item,
                reused.should_expand,
            )?;
            let (should_enqueue_children, reached_target) = {
                let mut snap = task.snapshot.lock();
                snap.processed_entries = snap.processed_entries.saturating_add(1);
                snap.current_path = node.path.clone();
                snap.current_depth = node.depth;
                apply_finding_to_snapshot(app, &task_id, &mut snap, &reused.item)?;
                (reused.should_expand, should_stop_for_target(&snap))
            };
            emit_cache_event(app, &task_id, "reuse", Some(&node), None).await?;
            emit_progress(app, state, task).await?;
            if should_enqueue_children {
                for child in persist::load_scan_children(&state.db_path, &task_id, &node.id, false)?
                {
                    let key = child.path.to_lowercase();
                    if analyzed.contains(&key) || queued.contains(&key) {
                        continue;
                    }
                    queued.insert(key);
                    queue.push(child);
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
                "childDirectories": child_dirs.iter().map(|x| json!({"name": x.name, "path": x.path, "size": x.size})).collect::<Vec<_>>(),
            }),
        )
        .map_err(|e| e.to_string())?;

        let review = analyze_scan_node(&task.ai, &node, &child_dirs).await;
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
                "hasPotentialDeletableSubfolders": review.has_potential_deletable_subfolders,
                "tokenUsage": review.token_usage,
                "rawContent": review.trace.get("rawContent").cloned().unwrap_or(Value::Null),
                "userPrompt": review.trace.get("userPrompt").cloned().unwrap_or(Value::Null),
                "search": review.trace.get("search").cloned().unwrap_or(Value::Null),
                "error": review.trace.get("error").cloned().unwrap_or(Value::Null),
            }),
        )
        .map_err(|e| e.to_string())?;

        let item = build_finding_item(&node, &review);
        persist::upsert_scan_finding(&state.db_path, &task_id, &item, should_expand)?;

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
            for child in persist::load_scan_children(&state.db_path, &task_id, &node.id, false)? {
                let key = child.path.to_lowercase();
                if analyzed.contains(&key) || queued.contains(&key) {
                    continue;
                }
                queued.insert(key);
                queue.push(child);
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
        "node_batch" => {
            let batch: SidecarNodeBatchEvent =
                serde_json::from_value(payload.clone()).map_err(|e| e.to_string())?;
            let rows = batch
                .nodes
                .into_iter()
                .map(|node| persist::ScanNodeUpsert {
                    node_id: node.node_id,
                    parent_id: node.parent_id,
                    path: node.path,
                    name: node.name,
                    node_type: node.node_type,
                    depth: node.depth,
                    self_size: node.self_size.max(0) as u64,
                    size: node.total_size.max(0) as u64,
                    child_count: node.child_count.max(0) as u64,
                    mtime_ms: node.mtime_ms,
                    ext: node.ext,
                })
                .collect::<Vec<_>>();
            if queue_sidecar_nodes(task, rows) >= SCAN_NODE_FLUSH_THRESHOLD {
                flush_pending_sidecar_nodes(state, task)?;
            }
        }
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
                snap.scanned_count = task.scan_count_offset.saturating_add(scanned_delta);
                snap.total_entries = task.scan_count_offset.saturating_add(total_delta);
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
        .arg(&state.db_path)
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
    flush_pending_sidecar_nodes(state, task)?;
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
    flush_pending_sidecar_nodes(state, task)?;
    if task.stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    if task.snapshot.lock().auto_analyze {
        run_auto_analyze(app, state, task).await?;
    }
    if !task.stop.load(Ordering::Relaxed) {
        let mut snap = task.snapshot.lock();
        snap.status = "done".to_string();
        persist::save_scan_snapshot(&state.db_path, &snap)?;
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

    let baseline_task_id = task
        .baseline_task_id
        .clone()
        .ok_or_else(|| "Missing baseline snapshot".to_string())?;
    let task_id = task.snapshot.lock().id.clone();
    let affected_paths = task
        .sidecar_roots
        .iter()
        .map(|root| root.path.clone())
        .collect::<Vec<_>>();

    emit_cache_event(
        app,
        &task_id,
        "prepare_incremental_clone",
        None,
        Some(affected_paths.len() as u64),
    )
    .await?;

    persist::clone_scan_task_data(&state.db_path, &baseline_task_id, &task_id)?;

    emit_cache_event(
        app,
        &task_id,
        "prepare_incremental_prune",
        None,
        Some(affected_paths.len() as u64),
    )
    .await?;

    if !affected_paths.is_empty() {
        persist::delete_scan_data_for_paths(&state.db_path, &task_id, &affected_paths)?;
    }
    if !task.ignored_paths.is_empty() {
        persist::delete_scan_data_for_paths(&state.db_path, &task_id, &task.ignored_paths)?;
    }

    if let Some(prepared_snapshot) = persist::load_scan_snapshot(&state.db_path, &task_id)? {
        let mut snap = task.snapshot.lock();
        snap.deletable = prepared_snapshot.deletable;
        snap.deletable_count = prepared_snapshot.deletable_count;
        snap.total_cleanable = prepared_snapshot.total_cleanable;
        snap.token_usage = prepared_snapshot.token_usage;
        snap.permission_denied_count = prepared_snapshot.permission_denied_count;
        snap.permission_denied_paths = prepared_snapshot.permission_denied_paths;
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
    persist::find_latest_visible_scan_task_id_for_path(&state.db_path, target_path)
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
    let task_id = format!("scan_{}", Uuid::new_v4().simple());
    let target_path = PathBuf::from(&input.target_path)
        .to_string_lossy()
        .to_string();
    let ignored_paths = crate::backend::read_scan_ignore_paths(&state.settings_path);
    let scan_mode = ScanMode::parse(input.scan_mode.as_deref());
    let prune_rules = build_scan_prune_rules(&target_path);
    let baseline_task_id = resolve_latest_baseline_task_id(
        state.inner(),
        &target_path,
        input.baseline_task_id.as_deref(),
    )?;
    let baseline_snapshot = if let Some(task_id) = baseline_task_id.as_deref() {
        persist::load_scan_snapshot(&state.db_path, task_id)?
    } else {
        None
    };
    if matches!(scan_mode, ScanMode::DeepenIncremental) && baseline_snapshot.is_none() {
        return Err("A baseline task is required for deepen_incremental".to_string());
    }
    let deepen_boundary_nodes = if matches!(scan_mode, ScanMode::DeepenIncremental) {
        let baseline = baseline_snapshot
            .as_ref()
            .ok_or_else(|| "Missing baseline snapshot".to_string())?;
        Some(resolve_deepen_boundary_nodes(
            &state.db_path,
            baseline,
            input.max_depth,
            &prune_rules,
        )?)
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
    let mut snapshot = ScanSnapshot {
        id: task_id.clone(),
        status: "idle".to_string(),
        scan_mode: scan_mode.as_str().to_string(),
        baseline_task_id: baseline_task_id.clone(),
        visible_latest: true,
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
        error_message: String::new(),
    };
    persist::init_scan_task(
        &state.db_path,
        &task_id,
        &target_path,
        snapshot.target_size,
        max_depth,
        snapshot.auto_analyze,
        baseline_task_id.as_deref(),
        scan_mode.as_str(),
    )?;
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
    persist::save_scan_snapshot(&state.db_path, &snapshot)?;
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
        scan_count_offset: 0,
        pending_sidecar_nodes: Mutex::new(Vec::new()),
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
                snap.status = "error".to_string();
                snap.error_message = err.clone();
                let _ = persist::save_scan_snapshot(&state_clone.db_path, &snap);
                let payload =
                    json!({ "taskId": task_id_clone, "message": err, "snapshot": &*snap });
                drop(snap);
                let _ = app_clone.emit("scan_error", payload);
            } else if runtime.stop.load(Ordering::Relaxed) {
                let mut snap = runtime.snapshot.lock();
                snap.status = "stopped".to_string();
                let _ = persist::save_scan_snapshot(&state_clone.db_path, &snap);
                let payload = serde_json::to_value(&*snap).unwrap_or_else(|_| json!({}));
                drop(snap);
                let _ = app_clone.emit("scan_stopped", payload);
            }
            state_clone.scan_tasks.lock().remove(&task_id_clone);
        });
    });
    *task.job.lock() = Some(handle);
    Ok(json!({ "taskId": task_id, "status": "started" }))
}

pub async fn scan_stop(state: State<'_, AppState>, task_id: String) -> Result<Value, String> {
    let task = state
        .scan_tasks
        .lock()
        .get(&task_id)
        .cloned()
        .ok_or_else(|| "Task not found".to_string())?;
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
                "errorMessage": snap.error_message
            })
        })
        .collect())
}

pub async fn scan_list_history(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<Value>, String> {
    persist::list_scan_history(&state.db_path, limit.unwrap_or(20).clamp(1, 200))
}

pub async fn scan_find_latest_for_path(
    state: State<'_, AppState>,
    path: String,
) -> Result<Value, String> {
    let Some(task_id) = persist::find_latest_visible_scan_task_id_for_path(&state.db_path, &path)?
    else {
        return Ok(Value::Null);
    };
    let snapshot = persist::load_scan_snapshot_summary(&state.db_path, &task_id)?
        .ok_or_else(|| "Task not found".to_string())?;
    serde_json::to_value(snapshot).map_err(|e| e.to_string())
}

pub async fn scan_delete_history(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Value, String> {
    if let Some(task) = state.scan_tasks.lock().get(&task_id).cloned() {
        let status = task.snapshot.lock().status.clone();
        if matches!(status.as_str(), "idle" | "scanning" | "analyzing") {
            return Err("Task is still running".to_string());
        }
    }
    if !persist::delete_scan_task(&state.db_path, &task_id)? {
        return Err("Task not found".to_string());
    }
    Ok(json!({ "success": true }))
}

pub async fn scan_get_result(state: State<'_, AppState>, task_id: String) -> Result<Value, String> {
    if let Some(task) = state.scan_tasks.lock().get(&task_id).cloned() {
        let snap = task.snapshot.lock().clone();
        return serde_json::to_value(snap).map_err(|e| e.to_string());
    }
    let snapshot = persist::load_scan_snapshot(&state.db_path, &task_id)?
        .ok_or_else(|| "Task not found".to_string())?;
    serde_json::to_value(snapshot).map_err(|e| e.to_string())
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
            .join("bin")
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
            error_message: String::new(),
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
        let mut snapshot = make_snapshot(task_id, root, max_depth, scan_mode, baseline_task_id);
        let mut pending = Vec::<persist::ScanNodeUpsert>::new();

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
                "node_batch" => {
                    let batch: SidecarNodeBatchEvent =
                        serde_json::from_value(payload).expect("parse node batch");
                    pending.extend(batch.nodes.into_iter().map(|node| persist::ScanNodeUpsert {
                        node_id: node.node_id,
                        parent_id: node.parent_id,
                        path: node.path,
                        name: node.name,
                        node_type: node.node_type,
                        depth: node.depth,
                        self_size: node.self_size.max(0) as u64,
                        size: node.total_size.max(0) as u64,
                        child_count: node.child_count.max(0) as u64,
                        mtime_ms: node.mtime_ms,
                        ext: node.ext,
                    }));
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

        if !pending.is_empty() {
            persist::upsert_scan_nodes(db_path, task_id, &pending).expect("persist node batch");
        }
        snapshot.status = "done".to_string();
        persist::save_scan_snapshot(db_path, &snapshot).expect("save final snapshot");
    }

    fn run_scanner_command(args: &[String]) -> Vec<String> {
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
        persist::init_scan_task(
            &db_path,
            &task_id,
            &root_string,
            0,
            Some(1),
            false,
            None,
            "full_rescan_incremental",
        )
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
    fn deepen_incremental_scan_preserves_baseline_and_adds_deeper_nodes() {
        let root = temp_path("deepen-root");
        let db_path = temp_path("deepen-db.sqlite");
        fs::create_dir_all(&root).expect("create root");
        create_scan_fixture(&root);
        persist::init_db(&db_path).expect("init db");

        let baseline_task_id = format!("scan_{}", Uuid::new_v4().simple());
        let root_string = root.to_string_lossy().to_string();
        persist::init_scan_task(
            &db_path,
            &baseline_task_id,
            &root_string,
            0,
            Some(1),
            false,
            None,
            "full_rescan_incremental",
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

        let deepen_task_id = format!("scan_{}", Uuid::new_v4().simple());
        persist::init_scan_task(
            &db_path,
            &deepen_task_id,
            &root_string,
            0,
            Some(2),
            false,
            Some(&baseline_task_id),
            "deepen_incremental",
        )
        .expect("init deepen task");
        persist::clone_scan_task_data(&db_path, &baseline_task_id, &deepen_task_id)
            .expect("clone baseline task");
        persist::delete_scan_data_for_paths(
            &db_path,
            &deepen_task_id,
            &boundary_nodes
                .iter()
                .map(|node| node.path.clone())
                .collect::<Vec<_>>(),
        )
        .expect("delete boundary nodes");

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
            deepen_task_id.clone(),
            "--roots-json".to_string(),
            roots_json.to_string_lossy().to_string(),
            "--max-depth".to_string(),
            "2".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &deepen_task_id,
            &root,
            "deepen_incremental",
            Some(baseline_task_id.clone()),
            Some(2),
            &deepen_lines,
        );

        let deepen_snapshot = persist::load_scan_snapshot(&db_path, &deepen_task_id)
            .expect("load deepen snapshot")
            .expect("deepen snapshot exists");
        assert_eq!(deepen_snapshot.status, "done");
        assert_eq!(deepen_snapshot.configured_max_depth, Some(2));
        assert_eq!(deepen_snapshot.max_scanned_depth, 2);

        let node_map =
            persist::load_scan_node_map(&db_path, &deepen_task_id).expect("load node map");
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
            persist::load_scan_children(&db_path, &deepen_task_id, &dir_a_id, false)
                .expect("load A children");
        assert!(dir_a_children.iter().any(|node| node.name == "nested"));
        assert!(dir_a_children.iter().any(|node| node.name == "a.txt"));

        let latest = persist::find_latest_visible_scan_task_id_for_path(&db_path, &root_string)
            .expect("find latest task")
            .expect("latest task exists");
        assert_eq!(latest, deepen_task_id);

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
        persist::init_scan_task(
            &db_path,
            &baseline_task_id,
            &root_string,
            0,
            Some(1),
            false,
            None,
            "full_rescan_incremental",
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

        let stopped_task_id = format!("scan_{}", Uuid::new_v4().simple());
        persist::init_scan_task(
            &db_path,
            &stopped_task_id,
            &root_string,
            0,
            Some(3),
            false,
            Some(&baseline_task_id),
            "deepen_incremental",
        )
        .expect("init stopped deepen task");
        persist::clone_scan_task_data(&db_path, &baseline_task_id, &stopped_task_id)
            .expect("clone baseline task");

        let stopped_snapshot = persist::load_scan_snapshot(&db_path, &stopped_task_id)
            .expect("load stopped snapshot")
            .expect("stopped snapshot exists");
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
