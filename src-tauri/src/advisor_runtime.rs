use crate::backend::{
    resolve_provider_api_key, resolve_provider_endpoint_and_model, AppState, TokenUsage,
};
use crate::persist;
use crate::system_ops;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use tauri::State;
use uuid::Uuid;

const CHAT_COMPLETION_TIMEOUT_SECS: u64 = 180;
const RESPONSE_ERROR_SNIPPET_CHARS: usize = 400;
const MAX_INITIAL_DELETE_SUGGESTIONS: usize = 12;
const MAX_INITIAL_MOVE_SUGGESTIONS: usize = 18;
const PROJECT_MARKER_NAMES: [&str; 8] = [
    ".git",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "requirements.txt",
    "go.mod",
    ".sln",
    "pom.xml",
];

#[derive(Clone)]
struct RouteConfig {
    endpoint: String,
    api_key: String,
    model: String,
}

#[derive(Debug)]
struct ChatCompletionOutput {
    content: String,
    usage: TokenUsage,
}

#[derive(Debug)]
struct ChatCompletionError {
    message: String,
    raw_body: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AdvisorSessionStartInput {
    pub root_path: String,
    pub scan_task_id: Option<String>,
    pub mode: Option<String>,
    pub response_language: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AdvisorMessageSendInput {
    pub session_id: String,
    pub message: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AdvisorPreferenceApplyInput {
    pub session_id: String,
    pub draft_id: Option<String>,
    pub scope: Option<String>,
    pub rule: Option<Value>,
    pub kind: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AdvisorSuggestionUpdateInput {
    pub session_id: String,
    pub suggestion_id: String,
    pub status: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AdvisorExecutePreviewInput {
    pub session_id: String,
    pub suggestion_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AdvisorExecuteConfirmInput {
    pub session_id: String,
    pub preview_id: String,
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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

fn normalize_mode(value: Option<&str>) -> String {
    match value.unwrap_or("").trim() {
        "cleanup_first" => "cleanup_first".to_string(),
        "balanced" => "balanced".to_string(),
        _ => "organize_first".to_string(),
    }
}

fn normalize_path_key(path: &str) -> String {
    path.trim().replace('/', "\\").to_lowercase()
}

fn sanitize_json_block(content: &str) -> String {
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

fn parse_chat_completion_http_body(
    status: StatusCode,
    raw_body: &str,
) -> Result<ChatCompletionOutput, ChatCompletionError> {
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
            .unwrap_or("advisor request failed");
        return Err(ChatCompletionError {
            message: format!("{} (HTTP {})", api_message, status.as_u16()),
            raw_body: raw_body.to_string(),
        });
    }
    let content = body
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if content.trim().is_empty() {
        return Err(ChatCompletionError {
            message: format!(
                "advisor response missing choices[0].message.content | body: {}",
                summarize_response_body_for_error(raw_body)
            ),
            raw_body: raw_body.to_string(),
        });
    }
    Ok(ChatCompletionOutput {
        content,
        usage: TokenUsage {
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

async fn chat_completion(
    route: &RouteConfig,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<ChatCompletionOutput, ChatCompletionError> {
    let stop = AtomicBool::new(false);
    let url = format!("{}/chat/completions", route.endpoint.trim_end_matches('/'));
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
            "model": route.model,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": user_prompt }
            ],
            "temperature": 0
        }));
    if !route.api_key.is_empty() {
        req = req
            .header("Authorization", format!("Bearer {}", route.api_key))
            .header("x-api-key", route.api_key.clone())
            .header("api-key", route.api_key.clone());
    }
    let request_future = async move {
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
    };
    tokio::pin!(request_future);

    loop {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(ChatCompletionError {
                message: "stop_requested".to_string(),
                raw_body: String::new(),
            });
        }
        tokio::select! {
            result = &mut request_future => return result,
            _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
        }
    }
}

fn stable_suggestion_id(session_id: &str, path: &str) -> String {
    persist::create_node_id(&format!("advisor:{session_id}:{}", normalize_path_key(path)))
}

fn build_move_suggestion(root_path: &str, session_id: &str, row: &Value) -> Option<Value> {
    let path = row.get("path").and_then(Value::as_str)?.trim().to_string();
    if path.is_empty() {
        return None;
    }
    let file_name = Path::new(&path)
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("item");
    let category_path = row
        .get("categoryPath")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(str::trim).map(str::to_string))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if category_path.is_empty() {
        return None;
    }
    let target_path = category_path
        .iter()
        .fold(PathBuf::from(root_path), |acc, segment| acc.join(segment))
        .join(file_name);
    Some(json!({
        "suggestionId": stable_suggestion_id(session_id, &path),
        "kind": "move",
        "title": format!("归类到 {}", category_path.join(" / ")),
        "summary": row.get("reason").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("根据最近一次整理结果，建议移动到 {}", category_path.join(" / "))),
        "path": path,
        "targetPath": target_path.to_string_lossy().to_string(),
        "risk": "low",
        "confidence": "medium",
        "why": [format!("最近一次整理结果归入 {}", category_path.join(" / "))],
        "triggeredPreferences": [],
        "requiresConfirmation": true,
        "executable": true,
        "status": "new"
    }))
}

fn build_delete_suggestion(session_id: &str, row: &Value) -> Option<Value> {
    let path = row.get("path").and_then(Value::as_str)?.trim().to_string();
    if path.is_empty() {
        return None;
    }
    let name = row.get("name").and_then(Value::as_str).unwrap_or("项目");
    let reason = row
        .get("reason")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("最近扫描将其标记为低风险可清理项");
    let risk = row
        .get("risk")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("low");
    Some(json!({
        "suggestionId": stable_suggestion_id(session_id, &path),
        "kind": "delete",
        "title": format!("清理 {}", name),
        "summary": reason,
        "path": path,
        "targetPath": Value::Null,
        "risk": risk,
        "confidence": "medium",
        "why": [reason],
        "triggeredPreferences": [],
        "requiresConfirmation": true,
        "executable": risk == "low",
        "status": "new"
    }))
}

fn merge_initial_suggestions(
    mode: &str,
    organize_suggestions: Vec<Value>,
    delete_suggestions: Vec<Value>,
) -> Vec<Value> {
    let mut by_path = HashMap::<String, Value>::new();
    let mut push_rows = |rows: Vec<Value>| {
        for row in rows {
            if let Some(path) = row.get("path").and_then(Value::as_str) {
                by_path.entry(normalize_path_key(path)).or_insert(row);
            }
        }
    };
    match mode {
        "cleanup_first" => {
            push_rows(delete_suggestions);
            push_rows(organize_suggestions);
        }
        "balanced" => {
            let mut zipped = Vec::new();
            let max_len = organize_suggestions.len().max(delete_suggestions.len());
            for idx in 0..max_len {
                if let Some(item) = organize_suggestions.get(idx).cloned() {
                    zipped.push(item);
                }
                if let Some(item) = delete_suggestions.get(idx).cloned() {
                    zipped.push(item);
                }
            }
            push_rows(zipped);
        }
        _ => {
            push_rows(organize_suggestions);
            push_rows(delete_suggestions);
        }
    }
    by_path.into_values().collect()
}

fn build_context_summary(
    root_path: &str,
    scan_task_id: Option<&str>,
    scan_snapshot: Option<&crate::backend::ScanSnapshot>,
    organize_snapshot: Option<&crate::backend::OrganizeSnapshot>,
    existing_tree: Option<&Value>,
    mode: &str,
    response_language: &str,
) -> Value {
    json!({
        "rootPath": root_path,
        "scanTaskId": scan_task_id,
        "scanSummary": scan_snapshot.map(|snapshot| json!({
            "status": snapshot.status,
            "scannedCount": snapshot.scanned_count,
            "totalEntries": snapshot.total_entries,
            "deletableCount": snapshot.deletable_count,
            "totalCleanable": snapshot.total_cleanable,
            "maxScannedDepth": snapshot.max_scanned_depth,
        })).unwrap_or(Value::Null),
        "organizeSummary": organize_snapshot.map(|snapshot| json!({
            "status": snapshot.status,
            "totalFiles": snapshot.total_files,
            "processedFiles": snapshot.processed_files,
            "treeVersion": snapshot.tree_version,
        })).unwrap_or(Value::Null),
        "latestTree": existing_tree.cloned().unwrap_or(Value::Null),
        "mode": mode,
        "responseLanguage": response_language
    })
}

fn build_default_session(
    root_path: &str,
    scan_task_id: Option<&str>,
    context_summary: Value,
    mode: &str,
    response_language: &str,
) -> Value {
    let created_at = now_iso();
    json!({
        "sessionId": format!("advisor_{}", Uuid::new_v4().simple()),
        "rootPath": root_path,
        "rootPathKey": persist::create_root_path_key(root_path),
        "scanTaskId": scan_task_id,
        "mode": mode,
        "responseLanguage": response_language,
        "status": "active",
        "contextSummary": context_summary,
        "pendingPreferenceDrafts": [],
        "createdAt": created_at,
        "updatedAt": created_at,
        "lastExecutionJobId": Value::Null
    })
}

fn current_route(state: &AppState) -> Result<RouteConfig, String> {
    let (endpoint, model) = resolve_provider_endpoint_and_model(state, None, None);
    let api_key = resolve_provider_api_key(state, &endpoint).unwrap_or_default();
    Ok(RouteConfig {
        endpoint,
        api_key,
        model,
    })
}

fn build_preference_extraction_system_prompt(response_language: &str) -> String {
    let output_language = localized_language_name(response_language, response_language);
    [
        "You extract cleanup and organization preferences from a user's message.".to_string(),
        "Return JSON only.".to_string(),
        "Schema: {\"preferenceDrafts\":[{\"kind\":\"keep|archive_preferred|delete_allowed|avoid_category|project_protect\",\"scope\":\"session|global_suggested\",\"match\":{\"pathContains\":[\"...\"],\"extensions\":[\".zip\"],\"nameContains\":[\"...\"],\"ageRule\":\"older_than_90d\",\"sizeRule\":\"larger_than_1gb\"},\"reason\":\"...\"}],\"reply\":\"...\"}".to_string(),
        "Extract only explicit or strongly implied preferences.".to_string(),
        "Do not invent thresholds unless the user stated them.".to_string(),
        "If the message contains no actionable preference, return an empty preferenceDrafts array.".to_string(),
        "Be conservative. When uncertain, do not create a preference draft.".to_string(),
        format!("The reply and reason fields must be written in {output_language} only."),
    ]
    .join("\n")
}

fn build_suggestion_generation_system_prompt(response_language: &str, mode: &str) -> String {
    let output_language = localized_language_name(response_language, response_language);
    [
        "You are an AI file cleanup and organization advisor.".to_string(),
        "Return JSON only.".to_string(),
        "Schema: {\"suggestions\":[{\"suggestionId\":\"...\",\"kind\":\"move|archive|delete|keep|review\",\"path\":\"...\",\"targetPath\":\"... optional\",\"title\":\"...\",\"summary\":\"...\",\"risk\":\"low|medium|high\",\"confidence\":\"high|medium|low\",\"why\":[\"...\"],\"triggeredPreferences\":[\"...\"],\"requiresConfirmation\":true,\"executable\":true}],\"reply\":\"...\"}".to_string(),
        format!("Follow the current advisor mode exactly: {mode}."),
        "Prefer using structured local context first.".to_string(),
        "Never recommend direct deletion for uncertain items.".to_string(),
        "Use kind=review for ambiguous or risky items.".to_string(),
        "Use kind=keep when the item is likely important or protected by user preference.".to_string(),
        "Use kind=delete only for low-risk items.".to_string(),
        "Use targetPath only for move or archive suggestions.".to_string(),
        "Keep suggestions practical and non-overlapping.".to_string(),
        format!("The reply, title, summary, why, and triggeredPreferences fields must be written in {output_language} only."),
    ]
    .join("\n")
}

fn build_suggestion_revision_system_prompt(response_language: &str) -> String {
    let output_language = localized_language_name(response_language, response_language);
    [
        "You revise an existing set of file suggestions based on user feedback.".to_string(),
        "Return JSON only.".to_string(),
        "Schema: {\"operations\":[{\"type\":\"replace|remove|add\",\"suggestionId\":\"... optional\",\"newSuggestion\":{\"path\":\"...\",\"kind\":\"move|archive|delete|keep|review\",\"targetPath\":\"... optional\",\"title\":\"...\",\"summary\":\"...\",\"risk\":\"low|medium|high\",\"confidence\":\"high|medium|low\",\"why\":[\"...\"],\"triggeredPreferences\":[\"...\"],\"requiresConfirmation\":true,\"executable\":true}}],\"reply\":\"...\"}".to_string(),
        "Preserve unaffected suggestions.".to_string(),
        "Modify only what is necessary to honor the new user feedback.".to_string(),
        "If the feedback implies a reusable preference, reflect that in the reply but do not create preference drafts here.".to_string(),
        format!("The reply and any newSuggestion text fields must be written in {output_language} only."),
    ]
    .join("\n")
}

async fn generate_suggestions_from_context(
    db_path: &Path,
    route: &RouteConfig,
    session: &Value,
    messages: &[Value],
    preferences: &[Value],
) -> Result<(String, Vec<Value>), String> {
    let response_language = session
        .get("responseLanguage")
        .and_then(Value::as_str)
        .unwrap_or("zh")
        .to_string();
    let mode = session
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("organize_first")
        .to_string();
    let scan_task_id = session.get("scanTaskId").and_then(Value::as_str);
    let scan_snapshot = scan_task_id
        .and_then(|task_id| persist::load_scan_snapshot(db_path, task_id).ok().flatten());
    let organize_snapshot = persist::find_latest_organize_task_id_for_root(
        db_path,
        session.get("rootPath").and_then(Value::as_str).unwrap_or(""),
    )?
    .and_then(|task_id| persist::load_organize_snapshot(db_path, &task_id).ok().flatten());
    let payload = json!({
        "mode": mode,
        "rootPath": session.get("rootPath").and_then(Value::as_str).unwrap_or(""),
        "scanSummary": session.get("contextSummary").and_then(|value| value.get("scanSummary")).cloned().unwrap_or(Value::Null),
        "fileCandidates": compose_file_candidates(organize_snapshot.as_ref(), scan_snapshot.as_ref()),
        "existingTree": session.get("contextSummary").and_then(|value| value.get("latestTree")).cloned().unwrap_or(Value::Null),
        "preferences": preferences,
        "recentConversationSummary": build_recent_conversation(messages),
    });
    let output = chat_completion(
        route,
        &build_suggestion_generation_system_prompt(&response_language, &mode),
        &payload.to_string(),
    )
    .await
    .map_err(|err| format!("suggestion generation failed: {}", err.message))?;
    let parsed = parse_prompt_json(&output.content)?;
    let suggestions = parsed
        .get("suggestions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| {
            normalize_suggestion(
                session.get("sessionId").and_then(Value::as_str).unwrap_or(""),
                &row,
                None,
            )
        })
        .collect::<Vec<_>>();
    let reply = parsed
        .get("reply")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    Ok((reply, suggestions))
}

fn normalize_preference_drafts(
    session_id: &str,
    source_message_id: i64,
    drafts: &[Value],
) -> Vec<Value> {
    drafts
        .iter()
        .enumerate()
        .filter_map(|(idx, row)| {
            let kind = row.get("kind").and_then(Value::as_str)?.trim().to_string();
            let scope = row
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or("session")
                .trim()
                .to_string();
            let reason = row
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let rule = row.get("match").cloned().unwrap_or_else(|| json!({}));
            Some(json!({
                "draftId": persist::create_node_id(&format!("draft:{session_id}:{source_message_id}:{idx}:{kind}")),
                "kind": kind,
                "scope": scope,
                "rule": rule,
                "reason": reason,
                "sourceMessageId": source_message_id
            }))
        })
        .collect()
}

fn normalize_suggestion(
    session_id: &str,
    row: &Value,
    existing_status: Option<&str>,
) -> Option<Value> {
    let path = row.get("path").and_then(Value::as_str)?.trim().to_string();
    if path.is_empty() {
        return None;
    }
    let kind = row
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("review")
        .trim();
    let normalized_kind = match kind {
        "move" | "archive" | "delete" | "keep" | "review" => kind,
        _ => "review",
    };
    let risk = match row.get("risk").and_then(Value::as_str).unwrap_or("medium") {
        "low" | "medium" | "high" => row.get("risk").and_then(Value::as_str).unwrap_or("medium"),
        _ => "medium",
    };
    let confidence = match row
        .get("confidence")
        .and_then(Value::as_str)
        .unwrap_or("medium")
    {
        "low" | "medium" | "high" => row
            .get("confidence")
            .and_then(Value::as_str)
            .unwrap_or("medium"),
        _ => "medium",
    };
    let target_path = row
        .get("targetPath")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let why = row
        .get("why")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(str::trim).map(str::to_string))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let triggered_preferences = row
        .get("triggeredPreferences")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(str::trim).map(str::to_string))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let executable = if normalized_kind == "review" || normalized_kind == "keep" {
        false
    } else if normalized_kind == "delete" {
        risk == "low"
    } else {
        row.get("executable").and_then(Value::as_bool).unwrap_or(true)
    };
    Some(json!({
        "suggestionId": row.get("suggestionId").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| stable_suggestion_id(session_id, &path)),
        "kind": normalized_kind,
        "title": row.get("title").and_then(Value::as_str).unwrap_or("").trim(),
        "summary": row.get("summary").and_then(Value::as_str).unwrap_or("").trim(),
        "path": path,
        "targetPath": target_path,
        "risk": risk,
        "confidence": confidence,
        "why": why,
        "triggeredPreferences": triggered_preferences,
        "requiresConfirmation": row.get("requiresConfirmation").and_then(Value::as_bool).unwrap_or(true),
        "executable": executable,
        "status": existing_status.unwrap_or("new"),
    }))
}

fn apply_suggestion_operations(
    session_id: &str,
    existing: &[Value],
    operations: &[Value],
) -> Vec<Value> {
    let mut by_id = existing
        .iter()
        .filter_map(|row| {
            row.get("suggestionId")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), row.clone()))
        })
        .collect::<HashMap<_, _>>();
    for op in operations {
        match op.get("type").and_then(Value::as_str).unwrap_or("") {
            "remove" => {
                if let Some(id) = op.get("suggestionId").and_then(Value::as_str) {
                    by_id.remove(id);
                }
            }
            "replace" => {
                let target_id = op.get("suggestionId").and_then(Value::as_str);
                if let Some(new_suggestion) = op.get("newSuggestion") {
                    let existing_status = target_id
                        .and_then(|id| by_id.get(id))
                        .and_then(|row| row.get("status").and_then(Value::as_str));
                    if let Some(normalized) =
                        normalize_suggestion(session_id, new_suggestion, existing_status)
                    {
                        let normalized_id = normalized
                            .get("suggestionId")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if let Some(old_id) = target_id {
                            by_id.remove(old_id);
                        }
                        by_id.insert(normalized_id, normalized);
                    }
                }
            }
            "add" => {
                if let Some(new_suggestion) = op.get("newSuggestion") {
                    if let Some(normalized) = normalize_suggestion(session_id, new_suggestion, None)
                    {
                        let normalized_id = normalized
                            .get("suggestionId")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        by_id.insert(normalized_id, normalized);
                    }
                }
            }
            _ => {}
        }
    }
    by_id.into_values().collect()
}

fn parse_prompt_json(content: &str) -> Result<Value, String> {
    serde_json::from_str::<Value>(&sanitize_json_block(content)).map_err(|e| e.to_string())
}

fn build_recent_conversation(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|row| {
            json!({
                "role": row.get("role").and_then(Value::as_str).unwrap_or("assistant"),
                "content": row.pointer("/content/text").and_then(Value::as_str).unwrap_or(""),
            })
        })
        .collect()
}

fn compose_file_candidates(
    organize_snapshot: Option<&crate::backend::OrganizeSnapshot>,
    scan_snapshot: Option<&crate::backend::ScanSnapshot>,
) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(snapshot) = organize_snapshot {
        for row in snapshot.results.iter().take(48) {
            out.push(json!({
                "path": row.get("path").and_then(Value::as_str).unwrap_or(""),
                "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
                "categoryPath": row.get("categoryPath").cloned().unwrap_or(Value::Array(Vec::new())),
                "summary": row.get("summary").cloned().unwrap_or(Value::Null),
                "risk": "low",
                "kindHint": "move",
            }));
        }
    }
    if let Some(snapshot) = scan_snapshot {
        for row in snapshot.deletable.iter().take(48) {
            out.push(json!({
                "path": row.path,
                "name": row.name,
                "reason": row.reason,
                "risk": row.risk,
                "kindHint": "delete",
            }));
        }
    }
    out
}

fn is_protected_system_path(path: &Path) -> bool {
    let normalized = normalize_path_key(&path.to_string_lossy());
    normalized.starts_with("c:\\windows")
        || normalized.starts_with("c:\\program files")
        || normalized.starts_with("c:\\program files (x86)")
        || normalized.starts_with("c:\\programdata")
}

fn is_project_root_or_descendant(path: &Path) -> bool {
    let mut current = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(path).to_path_buf()
    };
    for _ in 0..5 {
        for marker in PROJECT_MARKER_NAMES {
            if current.join(marker).exists() {
                return true;
            }
        }
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent.to_path_buf();
    }
    false
}

fn preference_matches_path(rule: &Value, path: &Path) -> bool {
    let normalized = normalize_path_key(&path.to_string_lossy());
    let file_name = path
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|x| x.to_str())
        .map(|value| format!(".{}", value.to_ascii_lowercase()))
        .unwrap_or_default();
    let path_contains = rule
        .get("pathContains")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(|x| x.to_ascii_lowercase()))
        .any(|segment| normalized.contains(&segment));
    let name_contains = rule
        .get("nameContains")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(|x| x.to_ascii_lowercase()))
        .any(|segment| file_name.contains(&segment));
    let extension_match = rule
        .get("extensions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(|x| x.to_ascii_lowercase()))
        .any(|candidate| candidate == extension);
    path_contains || name_contains || extension_match
}

fn load_session_bundle(
    db_path: &Path,
    session_id: &str,
) -> Result<(Value, Vec<Value>, Vec<Value>, Vec<Value>), String> {
    let session = persist::load_advisor_session(db_path, session_id)?
        .ok_or_else(|| "advisor session not found".to_string())?;
    let messages = persist::load_advisor_messages(db_path, session_id)?;
    let preferences = persist::load_advisor_preferences(db_path, Some(session_id))?;
    let suggestions = persist::load_advisor_suggestions(db_path, session_id)?;
    Ok((session, messages, preferences, suggestions))
}

fn save_session_bundle(db_path: &Path, session: &Value, suggestions: &[Value]) -> Result<(), String> {
    persist::save_advisor_session(db_path, session)?;
    persist::save_advisor_suggestions(
        db_path,
        session.get("sessionId").and_then(Value::as_str).unwrap_or(""),
        suggestions,
    )?;
    Ok(())
}

pub async fn advisor_session_start(
    state: State<'_, AppState>,
    input: AdvisorSessionStartInput,
) -> Result<Value, String> {
    let root_path = input.root_path.trim().to_string();
    if root_path.is_empty() {
        return Err("rootPath is required".to_string());
    }
    let mode = normalize_mode(input.mode.as_deref());
    let response_language = input
        .response_language
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("zh")
        .to_string();
    let scan_task_id = input.scan_task_id.clone().or_else(|| {
        persist::find_latest_visible_scan_task_id_for_path(&state.db_path(), &root_path)
            .ok()
            .flatten()
    });
    let scan_snapshot = scan_task_id
        .as_ref()
        .and_then(|task_id| persist::load_scan_snapshot(&state.db_path(), task_id).ok().flatten());
    let organize_snapshot = persist::find_latest_organize_task_id_for_root(
        &state.db_path(),
        &root_path,
    )?
    .and_then(|task_id| persist::load_organize_snapshot(&state.db_path(), &task_id).ok().flatten());
    let latest_tree =
        persist::load_latest_organize_tree(&state.db_path(), &root_path)?.map(|(tree, _)| tree);
    let session = build_default_session(
        &root_path,
        scan_task_id.as_deref(),
        build_context_summary(
            &root_path,
            scan_task_id.as_deref(),
            scan_snapshot.as_ref(),
            organize_snapshot.as_ref(),
            latest_tree.as_ref(),
            &mode,
            &response_language,
        ),
        &mode,
        &response_language,
    );
    let session_id = session
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let organize_suggestions = organize_snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .results
                .iter()
                .take(MAX_INITIAL_MOVE_SUGGESTIONS)
                .filter_map(|row| build_move_suggestion(&root_path, &session_id, row))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let delete_suggestions = scan_snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .deletable
                .iter()
                .take(MAX_INITIAL_DELETE_SUGGESTIONS)
                .filter_map(|row| build_delete_suggestion(&session_id, &serde_json::to_value(row).ok()?))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut suggestions = merge_initial_suggestions(&mode, organize_suggestions, delete_suggestions);
    if suggestions.is_empty() {
        if let Ok(route) = current_route(&state) {
            if !route.api_key.trim().is_empty() {
                let db_path = state.db_path();
                match generate_suggestions_from_context(
                    db_path.as_path(),
                    &route,
                    &session,
                    &[],
                    &[],
                )
                .await
                {
                    Ok((_reply, generated)) if !generated.is_empty() => {
                        suggestions = generated;
                    }
                    Ok(_) => {}
                    Err(err) => {
                        eprintln!("[Advisor] Failed to auto-generate initial suggestions: {err}");
                    }
                }
            }
        }
    }
    save_session_bundle(&state.db_path(), &session, &suggestions)?;
    advisor_session_get(state, session_id).await
}

pub async fn advisor_session_get(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Value, String> {
    let (session, messages, preferences, suggestions) =
        load_session_bundle(&state.db_path(), &session_id)?;
    let pending_actions = suggestions
        .iter()
        .filter(|row| {
            row.get("status").and_then(Value::as_str) == Some("accepted")
                && row.get("executable").and_then(Value::as_bool).unwrap_or(false)
        })
        .cloned()
        .collect::<Vec<_>>();
    Ok(json!({
        "session": session,
        "contextSummary": session.get("contextSummary").cloned().unwrap_or(Value::Null),
        "messages": messages,
        "preferences": preferences,
        "pendingPreferenceDrafts": session.get("pendingPreferenceDrafts").cloned().unwrap_or(Value::Array(Vec::new())),
        "suggestions": suggestions,
        "pendingActions": pending_actions,
        "lastExecution": persist::load_latest_advisor_execution_for_session(&state.db_path(), &session_id)?,
    }))
}

pub async fn advisor_message_send(
    state: State<'_, AppState>,
    input: AdvisorMessageSendInput,
) -> Result<Value, String> {
    let (mut session, messages, preferences, suggestions) =
        load_session_bundle(&state.db_path(), &input.session_id)?;
    let user_message = persist::save_advisor_message(
        &state.db_path(),
        &input.session_id,
        "user",
        &json!({ "text": input.message }),
    )?;
    let user_message_idx = user_message.get("idx").and_then(Value::as_i64).unwrap_or(0);
    let route = current_route(&state)?;
    let response_language = session
        .get("responseLanguage")
        .and_then(Value::as_str)
        .unwrap_or("zh")
        .to_string();
    let mode = session
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("organize_first")
        .to_string();

    if route.api_key.trim().is_empty() {
        let reply = if is_zh_language(&response_language) {
            "尚未配置可用的 AI API Key，当前无法根据新偏好刷新建议。".to_string()
        } else {
            "No AI API key is configured, so suggestions cannot be refreshed from the new preference yet.".to_string()
        };
        let assistant_content = json!({
            "text": reply,
            "preferenceDrafts": [],
            "suggestionPatch": { "operations": [] }
        });
        persist::save_advisor_message(
            &state.db_path(),
            &input.session_id,
            "assistant",
            &assistant_content,
        )?;
        return Ok(json!({
            "reply": reply,
            "preferenceDrafts": [],
            "suggestionPatch": { "operations": [] },
            "suggestions": suggestions
        }));
    }

    let preference_prompt = json!({
        "message": input.message,
        "mode": mode,
        "preferences": preferences,
        "outputLanguage": localized_language_name(&response_language, &response_language)
    });
    let preference_output = chat_completion(
        &route,
        &build_preference_extraction_system_prompt(&response_language),
        &preference_prompt.to_string(),
    )
    .await
    .map_err(|err| format!("preference extraction failed: {}", err.message))?;
    let preference_json = parse_prompt_json(&preference_output.content)?;
    let preference_drafts = normalize_preference_drafts(
        &input.session_id,
        user_message_idx,
        preference_json
            .get("preferenceDrafts")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
    );

    let route_reply = if suggestions.is_empty() {
        let db_path = state.db_path();
        let (reply, new_suggestions) =
            generate_suggestions_from_context(db_path.as_path(), &route, &session, &messages, &preferences)
                .await?;
        (
            reply,
            json!({ "operations": new_suggestions.iter().map(|row| json!({"type": "add", "newSuggestion": row})).collect::<Vec<_>>() }),
            new_suggestions,
        )
    } else {
        let payload = json!({
            "message": input.message,
            "suggestions": suggestions,
            "preferences": preferences,
            "mode": mode,
        });
        let output = chat_completion(
            &route,
            &build_suggestion_revision_system_prompt(&response_language),
            &payload.to_string(),
        )
        .await
        .map_err(|err| format!("suggestion revision failed: {}", err.message))?;
        let parsed = parse_prompt_json(&output.content)?;
        let operations = parsed
            .get("operations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let next_suggestions = apply_suggestion_operations(&input.session_id, &suggestions, &operations);
        let reply = parsed
            .get("reply")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default();
        (reply, json!({ "operations": operations }), next_suggestions)
    };

    let (reply, suggestion_patch, next_suggestions) = route_reply;
    session["pendingPreferenceDrafts"] = Value::Array(preference_drafts.clone());
    session["updatedAt"] = Value::String(now_iso());
    save_session_bundle(&state.db_path(), &session, &next_suggestions)?;
    let assistant_content = json!({
        "text": reply,
        "preferenceDrafts": preference_drafts,
        "suggestionPatch": suggestion_patch,
    });
    persist::save_advisor_message(
        &state.db_path(),
        &input.session_id,
        "assistant",
        &assistant_content,
    )?;
    Ok(json!({
        "reply": assistant_content.get("text").cloned().unwrap_or(Value::String(String::new())),
        "preferenceDrafts": assistant_content.get("preferenceDrafts").cloned().unwrap_or(Value::Array(Vec::new())),
        "suggestionPatch": assistant_content.get("suggestionPatch").cloned().unwrap_or(json!({ "operations": [] })),
        "suggestions": next_suggestions
    }))
}

pub async fn advisor_preference_apply(
    state: State<'_, AppState>,
    input: AdvisorPreferenceApplyInput,
) -> Result<Value, String> {
    let (mut session, _messages, _preferences, suggestions) =
        load_session_bundle(&state.db_path(), &input.session_id)?;
    let mut pending = session
        .get("pendingPreferenceDrafts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let selected = if let Some(draft_id) = input.draft_id.as_deref() {
        let idx = pending
            .iter()
            .position(|row| row.get("draftId").and_then(Value::as_str) == Some(draft_id))
            .ok_or_else(|| "preference draft not found".to_string())?;
        pending.remove(idx)
    } else {
        json!({
            "draftId": Value::Null,
            "kind": input.kind.clone().unwrap_or_else(|| "keep".to_string()),
            "scope": input.scope.clone().unwrap_or_else(|| "session".to_string()),
            "rule": input.rule.clone().unwrap_or_else(|| json!({})),
            "reason": Value::Null
        })
    };
    let raw_scope = input
        .scope
        .clone()
        .unwrap_or_else(|| selected.get("scope").and_then(Value::as_str).unwrap_or("session").to_string());
    let persisted_scope = if raw_scope == "global_suggested" {
        "global"
    } else {
        raw_scope.as_str()
    };
    let preference = json!({
        "preferenceId": persist::create_node_id(&format!("pref:{}:{}:{}", input.session_id, selected.get("kind").and_then(Value::as_str).unwrap_or("keep"), now_iso())),
        "sessionId": if persisted_scope == "session" { Value::String(input.session_id.clone()) } else { Value::Null },
        "scope": persisted_scope,
        "kind": selected.get("kind").and_then(Value::as_str).unwrap_or("keep"),
        "enabled": input.enabled.unwrap_or(true),
        "source": "advisor",
        "reason": selected.get("reason").cloned().unwrap_or(Value::Null),
        "rule": input.rule.clone().unwrap_or_else(|| selected.get("rule").cloned().unwrap_or_else(|| json!({}))),
        "createdAt": now_iso(),
        "updatedAt": now_iso(),
    });
    persist::save_advisor_preference(&state.db_path(), &preference)?;
    session["pendingPreferenceDrafts"] = Value::Array(pending);
    session["updatedAt"] = Value::String(now_iso());
    save_session_bundle(&state.db_path(), &session, &suggestions)?;
    Ok(json!({
        "success": true,
        "applied": preference,
        "preferences": persist::load_advisor_preferences(&state.db_path(), Some(&input.session_id))?
    }))
}

pub async fn advisor_suggestion_update(
    state: State<'_, AppState>,
    input: AdvisorSuggestionUpdateInput,
) -> Result<Value, String> {
    let (session, _messages, _preferences, mut suggestions) =
        load_session_bundle(&state.db_path(), &input.session_id)?;
    let allowed = ["new", "accepted", "ignored", "snoozed", "restored"];
    if !allowed.contains(&input.status.as_str()) {
        return Err("invalid suggestion status".to_string());
    }
    let normalized = if input.status == "restored" {
        "new"
    } else {
        input.status.as_str()
    };
    let target = suggestions
        .iter_mut()
        .find(|row| row.get("suggestionId").and_then(Value::as_str) == Some(input.suggestion_id.as_str()))
        .ok_or_else(|| "suggestion not found".to_string())?;
    target["status"] = Value::String(normalized.to_string());
    save_session_bundle(&state.db_path(), &session, &suggestions)?;
    Ok(json!({ "success": true, "suggestions": suggestions }))
}

fn build_preview_entry(root_path: &str, preferences: &[Value], suggestion: &Value) -> Value {
    let kind = suggestion.get("kind").and_then(Value::as_str).unwrap_or("review");
    let source_path = suggestion
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let source = PathBuf::from(&source_path);
    let mut warnings = Vec::<String>::new();
    let mut can_execute = suggestion
        .get("executable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut target_path = suggestion
        .get("targetPath")
        .and_then(Value::as_str)
        .map(str::to_string);
    let action = match kind {
        "move" => "move",
        "archive" => "archive",
        "delete" => "recycle",
        _ => "review",
    };

    if source_path.trim().is_empty() || !source.exists() {
        can_execute = false;
        warnings.push("source_not_found".to_string());
    }
    let risk = suggestion.get("risk").and_then(Value::as_str).unwrap_or("medium");
    if risk == "high" {
        can_execute = false;
        warnings.push("high_risk_requires_review".to_string());
    }
    if action == "recycle" && risk != "low" {
        can_execute = false;
        warnings.push("delete_requires_low_risk".to_string());
    }
    if is_protected_system_path(&source) {
        can_execute = false;
        warnings.push("protected_system_path".to_string());
    }
    if action == "recycle" && is_project_root_or_descendant(&source) {
        can_execute = false;
        warnings.push("project_path_protected".to_string());
    }
    for preference in preferences {
        let preference_kind = preference.get("kind").and_then(Value::as_str).unwrap_or("");
        if !preference
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            continue;
        }
        let rule = preference.get("rule").unwrap_or(&Value::Null);
        if !preference_matches_path(rule, &source) {
            continue;
        }
        if (preference_kind == "project_protect" || preference_kind == "keep")
            && action == "recycle"
        {
            can_execute = false;
            warnings.push(format!("blocked_by_preference:{preference_kind}"));
        }
    }
    if action == "move" && target_path.is_none() {
        can_execute = false;
        warnings.push("target_path_missing".to_string());
    }
    if action == "archive" && target_path.is_none() {
        let file_name = source
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("item")
            .to_string();
        target_path = Some(
            PathBuf::from(root_path)
                .join("Archive")
                .join(file_name)
                .to_string_lossy()
                .to_string(),
        );
    }
    if let Some(target_raw) = target_path.as_ref() {
        let target = PathBuf::from(target_raw);
        if normalize_path_key(&source.to_string_lossy())
            == normalize_path_key(&target.to_string_lossy())
        {
            can_execute = false;
            warnings.push("source_equals_target".to_string());
        } else if target.exists() {
            can_execute = false;
            warnings.push("target_conflict".to_string());
        }
    }
    if action == "review" {
        can_execute = false;
        warnings.push("review_only".to_string());
    }
    json!({
        "suggestionId": suggestion.get("suggestionId").cloned().unwrap_or(Value::Null),
        "action": action,
        "sourcePath": source_path,
        "targetPath": target_path,
        "canExecute": can_execute,
        "warning": warnings.join("|"),
        "warnings": warnings,
        "rollbackable": action == "move" || action == "archive",
        "risk": risk,
        "title": suggestion.get("title").cloned().unwrap_or(Value::Null),
        "summary": suggestion.get("summary").cloned().unwrap_or(Value::Null),
    })
}

pub async fn advisor_execute_preview(
    state: State<'_, AppState>,
    input: AdvisorExecutePreviewInput,
) -> Result<Value, String> {
    let (session, _messages, preferences, suggestions) =
        load_session_bundle(&state.db_path(), &input.session_id)?;
    let selected = input
        .suggestion_ids
        .unwrap_or_default()
        .into_iter()
        .collect::<HashSet<_>>();
    let selected_rows = if selected.is_empty() {
        suggestions
            .into_iter()
            .filter(|row| row.get("status").and_then(Value::as_str) == Some("accepted"))
            .collect::<Vec<_>>()
    } else {
        suggestions
            .into_iter()
            .filter(|row| {
                row.get("suggestionId")
                    .and_then(Value::as_str)
                    .map(|id| selected.contains(id))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>()
    };
    let root_path = session.get("rootPath").and_then(Value::as_str).unwrap_or("");
    let entries = selected_rows
        .iter()
        .map(|row| build_preview_entry(root_path, &preferences, row))
        .collect::<Vec<_>>();
    let warnings = entries
        .iter()
        .flat_map(|row| {
            row.get("warnings")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect::<Vec<_>>();
    let blocked_entries = entries
        .iter()
        .filter(|row| !row.get("canExecute").and_then(Value::as_bool).unwrap_or(false))
        .cloned()
        .collect::<Vec<_>>();
    let summary = json!({
        "total": entries.len(),
        "canExecute": entries.iter().filter(|row| row.get("canExecute").and_then(Value::as_bool).unwrap_or(false)).count(),
        "blocked": blocked_entries.len(),
        "rollbackable": entries.iter().filter(|row| row.get("rollbackable").and_then(Value::as_bool).unwrap_or(false)).count(),
    });
    let preview_id = format!("ajob_{}", Uuid::new_v4().simple());
    let preview = json!({
        "previewId": preview_id,
        "sessionId": input.session_id,
        "entries": entries,
        "warnings": warnings,
        "blockedEntries": blocked_entries,
        "summary": summary,
    });
    persist::save_advisor_execution_job(
        &state.db_path(),
        preview_id.as_str(),
        preview.get("sessionId").and_then(Value::as_str).unwrap_or(""),
        &preview,
        None,
        None,
    )?;
    Ok(preview)
}

fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn execute_preview_entries(entries: &[Value]) -> Vec<Value> {
    entries
        .iter()
        .map(|entry| {
            let source_path = entry
                .get("sourcePath")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let action = entry
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("review")
                .to_string();
            let source = PathBuf::from(&source_path);
            if !entry.get("canExecute").and_then(Value::as_bool).unwrap_or(false) {
                return json!({
                    "sourcePath": source_path,
                    "targetPath": entry.get("targetPath").cloned().unwrap_or(Value::Null),
                    "action": action,
                    "status": "skipped",
                    "error": "preview_blocked",
                    "rollbackable": false,
                });
            }
            match action.as_str() {
                "move" | "archive" => {
                    let target_path = entry
                        .get("targetPath")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let target = PathBuf::from(&target_path);
                    match ensure_parent_dir(&target)
                        .and_then(|_| fs::rename(&source, &target).map_err(|e| e.to_string()))
                    {
                        Ok(()) => json!({
                            "sourcePath": source_path,
                            "targetPath": target_path,
                            "action": action,
                            "status": "moved",
                            "error": Value::Null,
                            "rollbackable": true,
                        }),
                        Err(err) => json!({
                            "sourcePath": source_path,
                            "targetPath": target_path,
                            "action": action,
                            "status": "failed",
                            "error": err,
                            "rollbackable": true,
                        }),
                    }
                }
                "recycle" => match system_ops::move_to_recycle_bin(&source) {
                    Ok(()) => json!({
                        "sourcePath": source_path,
                        "targetPath": Value::Null,
                        "action": action,
                        "status": "recycled",
                        "error": Value::Null,
                        "rollbackable": false,
                    }),
                    Err(err) => json!({
                        "sourcePath": source_path,
                        "targetPath": Value::Null,
                        "action": action,
                        "status": "failed",
                        "error": err,
                        "rollbackable": false,
                    }),
                },
                _ => json!({
                    "sourcePath": source_path,
                    "targetPath": entry.get("targetPath").cloned().unwrap_or(Value::Null),
                    "action": action,
                    "status": "skipped",
                    "error": "unsupported_action",
                    "rollbackable": false,
                }),
            }
        })
        .collect()
}

pub async fn advisor_execute_confirm(
    state: State<'_, AppState>,
    input: AdvisorExecuteConfirmInput,
) -> Result<Value, String> {
    let preview_job = persist::load_advisor_execution_job(&state.db_path(), &input.preview_id)?
        .ok_or_else(|| "preview not found".to_string())?;
    if preview_job.get("sessionId").and_then(Value::as_str) != Some(input.session_id.as_str()) {
        return Err("preview does not belong to session".to_string());
    }
    let preview = preview_job
        .get("preview")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let entries = preview
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let results = execute_preview_entries(&entries);
    let result = json!({
        "at": now_iso(),
        "entries": results,
        "summary": {
            "moved": results.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("moved")).count(),
            "recycled": results.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("recycled")).count(),
            "failed": results.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("failed")).count(),
            "skipped": results.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("skipped")).count(),
            "total": results.len(),
        }
    });
    persist::save_advisor_execution_job(
        &state.db_path(),
        &input.preview_id,
        &input.session_id,
        &preview,
        Some(&result),
        None,
    )?;
    let (mut session, _messages, _preferences, suggestions) =
        load_session_bundle(&state.db_path(), &input.session_id)?;
    session["lastExecutionJobId"] = Value::String(input.preview_id.clone());
    session["updatedAt"] = Value::String(now_iso());
    save_session_bundle(&state.db_path(), &session, &suggestions)?;
    Ok(json!({
        "success": true,
        "jobId": input.preview_id,
        "preview": preview,
        "result": result
    }))
}

pub async fn advisor_execution_get(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<Value, String> {
    persist::load_advisor_execution_job(&state.db_path(), &job_id)?
        .ok_or_else(|| "execution job not found".to_string())
}

pub async fn advisor_execution_rollback(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<Value, String> {
    let job = persist::load_advisor_execution_job(&state.db_path(), &job_id)?
        .ok_or_else(|| "execution job not found".to_string())?;
    let session_id = job
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let result_entries = job
        .pointer("/result/entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut rollback_entries = Vec::new();
    for entry in result_entries.iter().rev() {
        let status = entry.get("status").and_then(Value::as_str).unwrap_or("");
        let action = entry.get("action").and_then(Value::as_str).unwrap_or("");
        let source = PathBuf::from(entry.get("sourcePath").and_then(Value::as_str).unwrap_or(""));
        let target = PathBuf::from(entry.get("targetPath").and_then(Value::as_str).unwrap_or(""));
        if action == "recycle" {
            rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": entry.get("targetPath").cloned().unwrap_or(Value::Null),
                "action": action,
                "status": "not_rollbackable",
                "error": "recycle_requires_manual_restore"
            }));
            continue;
        }
        if status != "moved" {
            rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "action": action,
                "status": "skipped",
                "error": "not_moved_in_confirm"
            }));
            continue;
        }
        if !target.exists() {
            rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "action": action,
                "status": "failed",
                "error": "target_not_found"
            }));
            continue;
        }
        if source.exists() {
            rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "action": action,
                "status": "failed",
                "error": "source_already_exists"
            }));
            continue;
        }
        match ensure_parent_dir(&source)
            .and_then(|_| fs::rename(&target, &source).map_err(|e| e.to_string()))
        {
            Ok(()) => rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "action": action,
                "status": "rolled_back",
                "error": Value::Null
            })),
            Err(err) => rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "action": action,
                "status": "failed",
                "error": err
            })),
        }
    }
    let rollback = json!({
        "at": now_iso(),
        "entries": rollback_entries,
        "summary": {
            "rolledBack": rollback_entries.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("rolled_back")).count(),
            "notRollbackable": rollback_entries.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("not_rollbackable")).count(),
            "failed": rollback_entries.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("failed")).count(),
            "skipped": rollback_entries.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("skipped")).count(),
            "total": rollback_entries.len(),
        }
    });
    let preview_owned = job
        .get("preview")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let result_owned = job.get("result").cloned().unwrap_or(Value::Null);
    persist::save_advisor_execution_job(
        &state.db_path(),
        &job_id,
        &session_id,
        &preview_owned,
        Some(&result_owned),
        Some(&rollback),
    )?;
    Ok(json!({
        "success": true,
        "jobId": job_id,
        "rollback": rollback
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_mode_defaults_to_organize_first() {
        assert_eq!(normalize_mode(None), "organize_first");
        assert_eq!(normalize_mode(Some("cleanup_first")), "cleanup_first");
        assert_eq!(normalize_mode(Some("balanced")), "balanced");
        assert_eq!(normalize_mode(Some("weird")), "organize_first");
    }

    #[test]
    fn apply_suggestion_operations_preserves_unaffected_rows() {
        let existing = vec![
            json!({"suggestionId": "a", "path": r"C:\a.txt", "kind": "move", "status": "accepted", "risk": "low", "confidence": "medium", "title": "A", "summary": "A", "why": [], "triggeredPreferences": [], "requiresConfirmation": true, "executable": true}),
            json!({"suggestionId": "b", "path": r"C:\b.txt", "kind": "delete", "status": "ignored", "risk": "low", "confidence": "medium", "title": "B", "summary": "B", "why": [], "triggeredPreferences": [], "requiresConfirmation": true, "executable": true}),
        ];
        let operations = vec![json!({
            "type": "replace",
            "suggestionId": "a",
            "newSuggestion": {
                "path": r"C:\a.txt",
                "kind": "archive",
                "targetPath": r"C:\Archive\a.txt",
                "title": "Archive A",
                "summary": "Archive it",
                "risk": "low",
                "confidence": "high",
                "why": ["updated"],
                "triggeredPreferences": [],
                "requiresConfirmation": true,
                "executable": true
            }
        })];
        let updated = apply_suggestion_operations("session_1", &existing, &operations);
        assert_eq!(updated.len(), 2);
        assert!(updated
            .iter()
            .any(|row| row.get("status").and_then(Value::as_str) == Some("ignored")));
    }

    #[test]
    fn build_preview_blocks_high_risk_delete() {
        let entry = build_preview_entry(
            r"C:\root",
            &[],
            &json!({
                "suggestionId": "x",
                "kind": "delete",
                "path": r"C:\Windows\Temp\foo.tmp",
                "risk": "high",
                "executable": true
            }),
        );
        assert_eq!(entry.get("canExecute").and_then(Value::as_bool), Some(false));
    }
}
