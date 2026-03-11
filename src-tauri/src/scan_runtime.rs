use crate::backend::{AppState, ScanResultItem, ScanSnapshot, ScanStartInput, TokenUsage};
use crate::persist;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use parking_lot::Mutex;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use uuid::Uuid;

#[derive(Clone)]
struct ScanAiConfig {
    endpoint: String,
    api_key: String,
    model: String,
    response_language: String,
}

pub struct ScanTaskRuntime {
    pub stop: AtomicBool,
    pub child_pid: Mutex<Option<u32>>,
    pub snapshot: Mutex<ScanSnapshot>,
    pub max_depth: u32,
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
    content: String,
    token_usage: TokenUsage,
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

fn is_zh_language(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized == "zh" || normalized.starts_with("zh-") || normalized.starts_with("zh_")
}

fn prompt_language_name(value: &str) -> &'static str {
    if is_zh_language(value) {
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
) -> Result<ChatResponse, String> {
    let url = format!("{}/chat/completions", ai.endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
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
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("chat completion failed")
            .to_string());
    }
    let content = body
        .pointer("/choices/0/message/content")
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
        .unwrap_or_default();
    Ok(ChatResponse {
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

async fn analyze_scan_node(
    ai: &ScanAiConfig,
    node: &persist::ScanNode,
    child_dirs: &[persist::ScanNode],
) -> ScanReview {
    let response_language = prompt_language_name(&ai.response_language);
    let system_prompt = if node.node_type == "directory" {
        [
            "You are a disk cleanup safety assistant.",
            "Return JSON only.",
            "Schema: {\"classification\":\"safe_to_delete|suspicious|keep\",\"reason\":\"...\",\"risk\":\"low|medium|high\",\"hasPotentialDeletableSubfolders\":true}",
            "Be conservative. If unsure, use suspicious.",
            &format!(
                "The \"reason\" field must be written in {} only.",
                response_language
            ),
        ]
        .join("\n")
    } else {
        [
            "You are a disk cleanup safety assistant.",
            "Return JSON only.",
            "Schema: {\"classification\":\"safe_to_delete|suspicious|keep\",\"reason\":\"...\",\"risk\":\"low|medium|high\"}",
            "Be conservative. If unsure, use suspicious.",
            &format!(
                "The \"reason\" field must be written in {} only.",
                response_language
            ),
        ]
        .join("\n")
    };
    let child_summary = if child_dirs.is_empty() {
        "(none)".to_string()
    } else {
        child_dirs
            .iter()
            .take(24)
            .map(|child| format!("- {} ({})", child.name, format_size(child.size)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let user_prompt = if node.node_type == "directory" {
        format!(
            "Type: directory\nPath: {}\nName: {}\nSize: {}\nDirect child directories:\n{}\nJudge whether the whole directory can be deleted safely, and whether it may contain deletable subfolders.",
            node.path,
            node.name,
            format_size(node.size),
            child_summary
        )
    } else {
        format!(
            "Type: file\nPath: {}\nName: {}\nSize: {}\nJudge whether the file can be deleted safely.",
            node.path,
            node.name,
            format_size(node.size)
        )
    };
    match chat_completion(ai, &system_prompt, &user_prompt).await {
        Ok(resp) => {
            let parsed: Value = serde_json::from_str(&extract_json_text(&resp.content))
                .unwrap_or_else(|_| json!({}));
            ScanReview {
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
                token_usage: resp.token_usage,
                trace: json!({
                    "model": ai.model,
                    "responseLanguage": ai.response_language,
                    "systemPrompt": system_prompt,
                    "userPrompt": user_prompt,
                    "rawContent": resp.content,
                }),
            }
        }
        Err(err) => ScanReview {
            classification: "suspicious".to_string(),
            reason: default_analysis_failed_reason(&ai.response_language, &err),
            risk: "high".to_string(),
            has_potential_deletable_subfolders: node.node_type == "directory",
            token_usage: TokenUsage::default(),
            trace: json!({
                "model": ai.model,
                "responseLanguage": ai.response_language,
                "systemPrompt": system_prompt,
                "userPrompt": user_prompt,
                "error": err,
            }),
        },
    }
}

fn sort_queue(queue: &mut Vec<persist::ScanNode>) {
    queue.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
}

async fn emit_progress<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
) -> Result<(), String> {
    let snap = task.snapshot.lock().clone();
    persist::save_scan_snapshot(&state.db_path, &snap)?;
    app.emit(
        "scan_progress",
        serde_json::to_value(&snap).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

async fn run_auto_analyze<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
) -> Result<(), String> {
    let root = task.snapshot.lock().root_node_id.clone();
    let task_id = task.snapshot.lock().id.clone();
    let mut queue = persist::load_scan_children(&state.db_path, &task_id, &root, false)?;
    let mut queued: HashSet<String> = queue.iter().map(|x| x.path.to_lowercase()).collect();
    let mut analyzed = HashSet::new();
    sort_queue(&mut queue);

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
                "error": review.trace.get("error").cloned().unwrap_or(Value::Null),
            }),
        )
        .map_err(|e| e.to_string())?;

        let item = ScanResultItem {
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
        };
        persist::upsert_scan_finding(&state.db_path, &task_id, &item)?;

        let (should_enqueue_children, reached_target) = {
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
            if review.classification == "safe_to_delete"
                && !snap
                    .deletable
                    .iter()
                    .any(|x| x.path.eq_ignore_ascii_case(&node.path))
            {
                snap.total_cleanable = snap.total_cleanable.saturating_add(node.size);
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
            let should_enqueue_children = node.node_type == "directory"
                && review.classification != "safe_to_delete"
                && review.has_potential_deletable_subfolders;
            let reached_target = snap.target_size > 0 && snap.total_cleanable >= snap.target_size;
            (should_enqueue_children, reached_target)
        };

        emit_progress(app, state, task).await?;

        if should_enqueue_children {
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
        "task_started" | "scan_progress" | "scan_completed" => {
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
                snap.scanned_count = payload
                    .get("scanned_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(snap.scanned_count);
                snap.total_entries = payload
                    .get("total_entries")
                    .and_then(Value::as_u64)
                    .unwrap_or(snap.total_entries);
            }
            emit_progress(app, state, task).await?;
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
            emit_progress(app, state, task).await?;
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
    let mut child = Command::new(bin)
        .arg("scan")
        .arg("--db")
        .arg(&state.db_path)
        .arg("--task-id")
        .arg(&snap.id)
        .arg("--root")
        .arg(&snap.target_path)
        .arg("--max-depth")
        .arg(task.max_depth.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

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
    run_sidecar_scan(app, state, task).await?;
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
    let snapshot = ScanSnapshot {
        id: task_id.clone(),
        status: "idle".to_string(),
        target_path: target_path.clone(),
        auto_analyze: input.auto_analyze.unwrap_or(true),
        root_node_id: persist::create_node_id(&target_path),
        current_path: target_path.clone(),
        current_depth: 0,
        scanned_count: 0,
        total_entries: 0,
        processed_entries: 0,
        deletable_count: 0,
        total_cleanable: 0,
        target_size: ((input.target_size_gb.unwrap_or(1.0).max(0.0)) * 1024.0 * 1024.0 * 1024.0)
            as u64,
        token_usage: TokenUsage::default(),
        deletable: Vec::new(),
        permission_denied_count: 0,
        permission_denied_paths: Vec::new(),
        error_message: String::new(),
    };
    let max_depth = input.max_depth.unwrap_or(5).clamp(1, 16);
    persist::init_scan_task(
        &state.db_path,
        &task_id,
        &target_path,
        snapshot.target_size,
        max_depth,
        snapshot.auto_analyze,
    )?;
    persist::save_scan_snapshot(&state.db_path, &snapshot)?;
    let task = Arc::new(ScanTaskRuntime {
        stop: AtomicBool::new(false),
        child_pid: Mutex::new(None),
        snapshot: Mutex::new(snapshot),
        max_depth,
        ai: ScanAiConfig {
            endpoint: input
                .api_endpoint
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            api_key: input.api_key.unwrap_or_default(),
            model: input.model.unwrap_or_else(|| "gpt-4o-mini".to_string()),
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
    let handle = tauri::async_runtime::spawn(async move {
        let result = run_scan_task(&app_clone, &state_clone, &runtime).await;
        if let Err(err) = result {
            let mut snap = runtime.snapshot.lock();
            snap.status = "error".to_string();
            snap.error_message = err.clone();
            let _ = persist::save_scan_snapshot(&state_clone.db_path, &snap);
            let payload = json!({ "taskId": task_id_clone, "message": err, "snapshot": &*snap });
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
        state_clone
            .scan_tasks
            .lock()
            .remove(&task_id_clone);
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
                "targetPath": snap.target_path,
                "autoAnalyze": snap.auto_analyze,
                "rootNodeId": snap.root_node_id,
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
