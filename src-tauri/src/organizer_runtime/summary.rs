use super::*;
use crate::agent_runtime::{
    AgentCompletion, AgentLlm, AgentLlmError, AgentLoopTrace, AgentToolPolicy, AgentTurnLoop,
    AgentTurnSpec, ToolCallErrorOutcome, ToolCallOutcome,
};
use crate::llm_tools::{ToolExecutionContext, ToolId, ToolRegistry, ToolResult, ToolWorkflow};
use serde_json::Map;

fn collect_name_keywords(unit: &OrganizeUnit, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let stem = Path::new(&unit.name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(&unit.name);
    for token in stem
        .split(|ch: char| !ch.is_alphanumeric() && !('\u{4e00}'..='\u{9fff}').contains(&ch))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let normalized = token.to_ascii_lowercase();
        if seen.insert(normalized) {
            out.push(token.to_string());
        }
        if out.len() >= limit {
            break;
        }
    }
    if out.is_empty() {
        out.push(unit.name.clone());
    }
    out
}

fn build_empty_extraction(unit: &OrganizeUnit, warning: &str) -> SummaryExtraction {
    build_empty_extraction_with_warnings(unit, vec![warning.to_string()])
}

fn build_empty_extraction_with_warnings(
    unit: &OrganizeUnit,
    warnings: Vec<String>,
) -> SummaryExtraction {
    SummaryExtraction {
        parser: "unavailable".to_string(),
        title: None,
        excerpt: String::new(),
        keywords: collect_name_keywords(unit, 6),
        metadata_lines: Vec::new(),
        warnings,
    }
}

pub(super) fn extract_plain_text_summary(unit: &OrganizeUnit) -> SummaryExtraction {
    if unit.size > LOCAL_SUMMARY_MAX_PLAIN_TEXT_BYTES {
        return build_empty_extraction_with_warnings(
            unit,
            vec![format!(
                "summary_input_too_large:{}>{}",
                unit.size, LOCAL_SUMMARY_MAX_PLAIN_TEXT_BYTES
            )],
        );
    }
    match fs::read(&unit.path) {
        Ok(bytes) => {
            let text = normalize_multiline_text(
                &String::from_utf8_lossy(&bytes),
                LOCAL_TEXT_EXCERPT_CHARS,
            );
            if text.trim().is_empty() {
                return build_empty_extraction(unit, "text_summary_fallback");
            }
            SummaryExtraction {
                parser: "plain_text".to_string(),
                title: Some(unit.name.clone()),
                excerpt: text,
                keywords: collect_name_keywords(unit, 6),
                metadata_lines: vec![
                    format!("name={}", unit.name),
                    format!("relativePath={}", unit.relative_path),
                    format!("size={}", unit.size),
                ],
                warnings: Vec::new(),
            }
        }
        Err(_) => build_empty_extraction(unit, "text_summary_fallback"),
    }
}

pub(super) fn extract_unit_content_for_summary(
    unit: &OrganizeUnit,
    response_language: &str,
    stop: &AtomicBool,
) -> SummaryExtraction {
    if stop.load(Ordering::Relaxed) {
        return build_empty_extraction(unit, "stop_requested");
    }
    if unit.item_type == "directory" {
        return SummaryExtraction {
            parser: "directory_assessment".to_string(),
            title: Some(unit.name.clone()),
            excerpt: summarize_directory_for_prompt(unit, response_language),
            keywords: collect_name_keywords(unit, 6),
            metadata_lines: vec![
                format!("name={}", unit.name),
                format!("relativePath={}", unit.relative_path),
                "itemType=directory".to_string(),
            ],
            warnings: unit
                .directory_assessment
                .as_ref()
                .map(|assessment| assessment.fragmentation_warnings.clone())
                .unwrap_or_default(),
        };
    }

    let ext = extension_key(Path::new(&unit.path));
    match ext.as_str() {
        ".txt" | ".md" | ".csv" | ".json" | ".yaml" | ".yml" | ".toml" | ".xml" | ".log"
        | ".ini" | ".cfg" | ".conf" | ".js" | ".ts" | ".jsx" | ".tsx" | ".rs" | ".py" | ".java"
        | ".go" | ".c" | ".cpp" | ".h" | ".hpp" | ".css" | ".html" | ".sql" | ".sh" | ".bat"
        | ".ps1" => extract_plain_text_summary(unit),
        _ if unit.modality == "text" && !supports_tika_extraction(unit) => {
            extract_plain_text_summary(unit)
        }
        _ => build_empty_extraction(unit, "filename_only_fallback"),
    }
}

fn supports_tika_extraction(unit: &OrganizeUnit) -> bool {
    if unit.item_type != "file" {
        return false;
    }
    let ext = extension_key(Path::new(&unit.path));
    matches!(
        ext.as_str(),
        ".pdf"
            | ".doc"
            | ".docx"
            | ".xls"
            | ".xlsx"
            | ".ppt"
            | ".pptx"
            | ".rtf"
            | ".odt"
            | ".ods"
            | ".odp"
            | ".epub"
    )
}

async fn tika_extract_text(
    config: &ExtractionToolConfig,
    unit: &OrganizeUnit,
    stop: &AtomicBool,
) -> Result<String, String> {
    if !config.tika_enabled || !supports_tika_extraction(unit) {
        return Err("tika_not_enabled".to_string());
    }
    if unit.size > TIKA_MAX_UPLOAD_BYTES {
        return Err(format!(
            "tika_input_too_large:{}>{}",
            unit.size, TIKA_MAX_UPLOAD_BYTES
        ));
    }
    if stop.load(Ordering::Relaxed) {
        return Err("stop_requested".to_string());
    }

    let body = fs::read(&unit.path).map_err(|e| format!("tika_read_failed:{e}"))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(TIKA_EXTRACT_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("tika_client_failed:{e}"))?;
    let request = client
        .put(format!("{}/tika", config.tika_url))
        .header("Accept", "text/plain")
        .header("Content-Type", "application/octet-stream")
        .body(body);
    let request_future = async move {
        let response = request
            .send()
            .await
            .map_err(|e| format!("tika_request_failed:{e}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| format!("tika_body_failed:{e}"))?;
        if !status.is_success() {
            return Err(format!("tika_http_{}:{}", status.as_u16(), text));
        }
        Ok(text)
    };
    tokio::pin!(request_future);

    loop {
        if stop.load(Ordering::Relaxed) {
            return Err("stop_requested".to_string());
        }
        tokio::select! {
            result = &mut request_future => return result,
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
    }
}

pub(super) async fn extract_unit_content_for_summary_with_tools(
    unit: &OrganizeUnit,
    response_language: &str,
    stop: &AtomicBool,
    extraction_tool: &ExtractionToolConfig,
) -> SummaryExtraction {
    if extraction_tool.tika_ready && supports_tika_extraction(unit) {
        match tika_extract_text(extraction_tool, unit, stop).await {
            Ok(text) => {
                let excerpt = normalize_multiline_text(&text, LOCAL_TEXT_EXCERPT_CHARS);
                if !excerpt.trim().is_empty() {
                    return SummaryExtraction {
                        parser: "tika".to_string(),
                        title: Some(unit.name.clone()),
                        excerpt,
                        keywords: collect_name_keywords(unit, 6),
                        metadata_lines: vec![
                            format!("name={}", unit.name),
                            format!("relativePath={}", unit.relative_path),
                            format!("itemType={}", unit.item_type),
                            format!("modality={}", unit.modality),
                            "externalExtractor=tika".to_string(),
                        ],
                        warnings: Vec::new(),
                    };
                }
                return build_empty_extraction_with_warnings(
                    unit,
                    vec![
                        "tika_empty_text".to_string(),
                        "filename_only_fallback".to_string(),
                    ],
                );
            }
            Err(err) if err == "stop_requested" => {
                return build_empty_extraction(unit, "stop_requested");
            }
            Err(err) => {
                let mut fallback = extract_unit_content_for_summary(unit, response_language, stop);
                fallback.warnings.push(err);
                return fallback;
            }
        }
    }
    extract_unit_content_for_summary(unit, response_language, stop)
}

pub(super) fn build_local_summary(
    unit: &OrganizeUnit,
    extracted: &SummaryExtraction,
) -> SummaryBuildResult {
    if unit.item_type != "directory" && extracted.excerpt.trim().is_empty() {
        return SummaryBuildResult {
            representation: FileRepresentation {
                metadata: Some(unit.name.clone()),
                short: None,
                long: None,
                source: SUMMARY_SOURCE_FILENAME_ONLY.to_string(),
                degraded: true,
                confidence: None,
                keywords: extracted.keywords.clone(),
            },
            warnings: extracted.warnings.clone(),
        };
    }

    let metadata = Some(build_representation_metadata(unit, extracted));
    SummaryBuildResult {
        representation: FileRepresentation {
            metadata,
            short: Some(build_representation_short(unit, extracted)),
            long: Some(build_representation_long(unit, extracted)),
            source: SUMMARY_SOURCE_LOCAL_SUMMARY.to_string(),
            degraded: false,
            confidence: None,
            keywords: extracted.keywords.clone(),
        },
        warnings: extracted.warnings.clone(),
    }
}

pub(super) fn build_representation_metadata(
    unit: &OrganizeUnit,
    extracted: &SummaryExtraction,
) -> String {
    let mut lines = vec![
        format!("name={}", unit.name),
        format!("relativePath={}", unit.relative_path),
        format!("itemType={}", unit.item_type),
        format!("modality={}", unit.modality),
        format!("parser={}", extracted.parser),
    ];
    if let Some(title) = extracted
        .title
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!(
            "title={}",
            trim_to_chars(title, LOCAL_SUMMARY_EXCERPT_CHARS)
        ));
    }
    if !extracted.keywords.is_empty() {
        lines.push(format!(
            "keywords={}",
            trim_to_chars(&extracted.keywords.join(", "), LOCAL_SUMMARY_EXCERPT_CHARS)
        ));
    }
    lines.extend(extracted.metadata_lines.iter().cloned());
    lines.join("\n")
}

fn build_representation_short(unit: &OrganizeUnit, extracted: &SummaryExtraction) -> String {
    let signal = extracted
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| trim_to_chars(value, 120))
        .or_else(|| {
            (!extracted.excerpt.trim().is_empty()).then(|| {
                trim_to_chars(
                    &normalize_multiline_text(&extracted.excerpt, 200),
                    SUMMARY_AGENT_SUMMARY_CHARS,
                )
            })
        })
        .or_else(|| {
            (!extracted.metadata_lines.is_empty()).then(|| {
                trim_to_chars(
                    &normalize_multiline_text(&extracted.metadata_lines.join("；"), 200),
                    SUMMARY_AGENT_SUMMARY_CHARS,
                )
            })
        })
        .unwrap_or_else(|| format!("{} | {}", unit.item_type, unit.modality));
    format!("{} | {}", unit.name, signal)
}

fn build_representation_long(unit: &OrganizeUnit, extracted: &SummaryExtraction) -> String {
    let mut lines = vec![build_representation_metadata(unit, extracted)];
    if !extracted.excerpt.trim().is_empty() {
        lines.push(format!(
            "excerpt={}",
            trim_to_chars(
                &normalize_multiline_text(&extracted.excerpt, LOCAL_TEXT_EXCERPT_CHARS),
                LOCAL_SUMMARY_EXCERPT_CHARS
            )
        ));
    }
    lines.join("\n")
}

fn sanitize_json_block(content: &str) -> String {
    content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_string()
}

fn append_batch_trace(
    trace: &mut Vec<String>,
    step: usize,
    route: &RouteConfig,
    outcome: &str,
    raw_body: &str,
    message_content: Option<&str>,
    error_message: Option<&str>,
    available_tools: &[&str],
    search_budget_remaining: usize,
) {
    let mut sections = vec![
        format!("Step: {}", step),
        format!("Model: {}/{}", route.endpoint, route.model),
        format!("Outcome: {}", outcome),
        format!(
            "Available Tools: {}",
            if available_tools.is_empty() {
                "(none)".to_string()
            } else {
                available_tools.join(", ")
            }
        ),
        format!("Search Budget Remaining: {}", search_budget_remaining),
    ];
    if let Some(error_message) = error_message.filter(|value| !value.trim().is_empty()) {
        sections.push(format!("Error: {}", error_message));
    }
    if let Some(message_content) = message_content.filter(|value| !value.trim().is_empty()) {
        sections.push("Message Content:".to_string());
        sections.push(message_content.to_string());
    }
    sections.push("HTTP Raw Response Body:".to_string());
    sections.push(if raw_body.trim().is_empty() {
        "(unavailable)".to_string()
    } else {
        raw_body.to_string()
    });
    trace.push(sections.join("\n"));
}

fn append_batch_trace_note(trace: &mut Vec<String>, title: &str, details: Vec<String>) {
    let mut sections = vec![format!("Event: {title}")];
    sections.extend(details);
    trace.push(sections.join("\n"));
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

pub(super) fn parse_chat_completion_http_body(
    endpoint: &str,
    status: StatusCode,
    raw_body: &str,
) -> Result<ChatCompletionOutput, ChatCompletionError> {
    let parsed = parse_completion_response(detect_api_format(endpoint), status, raw_body).map_err(
        |message| ChatCompletionError {
            message: format!(
                "{} | body: {}",
                message,
                summarize_response_body_for_error(raw_body)
            ),
            raw_body: raw_body.to_string(),
        },
    )?;
    if parsed.assistant_text.trim().is_empty() && parsed.tool_calls.is_empty() {
        return Err(ChatCompletionError {
            message: format!(
                "classification response missing assistant content and tool calls | body: {}",
                summarize_response_body_for_error(raw_body)
            ),
            raw_body: raw_body.to_string(),
        });
    }
    Ok(ChatCompletionOutput {
        raw_body: raw_body.to_string(),
        content: parsed.assistant_text,
        raw_message: parsed.raw_message,
        tool_calls: parsed.tool_calls,
        usage: parsed.usage,
    })
}

async fn chat_completion_with_messages(
    route: &RouteConfig,
    messages: &[Value],
    tools: Option<&[Value]>,
    stop: &AtomicBool,
    diagnostics: Option<&OrganizerDiagnostics>,
    stage: &str,
) -> Result<ChatCompletionOutput, ChatCompletionError> {
    let api_format = detect_api_format(&route.endpoint);
    let url = build_messages_url(&route.endpoint, api_format);
    let client =
        build_llm_http_client(CHAT_COMPLETION_TIMEOUT_SECS).map_err(|e| ChatCompletionError {
            message: e.to_string(),
            raw_body: String::new(),
        })?;
    let payload = build_completion_payload(
        api_format,
        &route.model,
        messages,
        tools,
        0.0,
        DEFAULT_MAX_TOKENS,
    )
    .map_err(|message| ChatCompletionError {
        message,
        raw_body: String::new(),
    })?;
    let started_at = Instant::now();
    if let Some(diagnostics) = diagnostics {
        diagnostics.model_request(stage, route, &url, messages, tools.unwrap_or(&[]), &payload);
    }
    let req = apply_llm_transport_headers(
        client
            .post(url.clone())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&payload),
    );
    let req = apply_auth_headers(req, api_format, &route.api_key);
    let diagnostics_for_request = diagnostics.cloned();
    let endpoint_for_request = route.endpoint.clone();
    let model_for_request = route.model.clone();
    let stage_for_request = stage.to_string();
    let request_future = async move {
        let resp = match req.send().await {
            Ok(resp) => resp,
            Err(e) => {
                if let Some(diagnostics) = diagnostics_for_request.as_ref() {
                    diagnostics.model_error(
                        &stage_for_request,
                        &endpoint_for_request,
                        &model_for_request,
                        "organizer model request failed",
                        json!({
                            "url": url.clone(),
                            "error": { "message": e.to_string() },
                        }),
                        started_at.elapsed(),
                    );
                }
                return Err(ChatCompletionError {
                    message: e.to_string(),
                    raw_body: String::new(),
                });
            }
        };
        let status = resp.status();
        let raw_body = match resp.bytes().await {
            Ok(raw_body) => String::from_utf8_lossy(&raw_body).into_owned(),
            Err(e) => {
                if let Some(diagnostics) = diagnostics_for_request.as_ref() {
                    diagnostics.model_error(
                        &stage_for_request,
                        &endpoint_for_request,
                        &model_for_request,
                        "organizer model response body read failed",
                        json!({
                            "status": status.as_u16(),
                            "error": { "message": e.to_string() },
                        }),
                        started_at.elapsed(),
                    );
                }
                return Err(ChatCompletionError {
                    message: format!("error reading response body: {}", e),
                    raw_body: String::new(),
                });
            }
        };
        if let Some(diagnostics) = diagnostics_for_request.as_ref() {
            diagnostics.model_response(
                &stage_for_request,
                &endpoint_for_request,
                &model_for_request,
                status.as_u16(),
                &raw_body,
                started_at.elapsed(),
            );
        }
        parse_chat_completion_http_body(&endpoint_for_request, status, &raw_body)
    };
    tokio::pin!(request_future);

    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(ChatCompletionError {
                message: "stop_requested".to_string(),
                raw_body: String::new(),
            });
        }
        tokio::select! {
            result = &mut request_future => return result,
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
    }
}

async fn chat_completion(
    route: &RouteConfig,
    system_prompt: &str,
    user_prompt: &str,
    stop: &AtomicBool,
    diagnostics: Option<&OrganizerDiagnostics>,
    stage: &str,
) -> Result<ChatCompletionOutput, ChatCompletionError> {
    chat_completion_with_messages(
        route,
        &[
            json!({ "role": "system", "content": system_prompt }),
            json!({ "role": "user", "content": user_prompt }),
        ],
        None,
        stop,
        diagnostics,
        stage,
    )
    .await
}

fn build_summary_agent_system_prompt(response_language: &str) -> String {
    let is_zh = response_language
        .trim()
        .to_ascii_lowercase()
        .starts_with("zh");
    if is_zh {
        r#"你负责为文件整理系统生成 summaryText。

summaryText 用于为后续文件归类提供内容证据。
你的任务是概括当前输入中可读或可解析的信息，不是最终归类，也不是生成分类树。

请简洁总结：
1. 文件或目录的主要内容、主题、用途、文档类型、数据类型或主要对象。
2. 对分类有帮助的类型、命名或路径线索。
3. 如果信息不足，请说明具体不确定点。

不要编造未提供的内容。

输出语言使用中文。"#
            .to_string()
    } else {
        r#"You are responsible for generating summaryText for a file organization system.

summaryText provides content evidence for subsequent file classification.
Your task is to summarize readable or parseable information from the current input, not to perform final classification or generate a category tree.

Please concisely summarize:
1. The main content, topic, purpose, document type, data type, or primary object of the file or directory.
2. Type, naming, or path clues that help with classification.
3. If information is insufficient, specify the exact uncertainty.

Do not fabricate content not provided.

Write summaries in English only."#.to_string()
    }
}

pub(super) fn parse_summary_agent_output(
    content: &str,
) -> Result<HashMap<String, SummaryAgentItem>, String> {
    let parsed: Value =
        serde_json::from_str(&sanitize_json_block(content)).map_err(|e| e.to_string())?;
    let items = parsed
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "summary agent response missing items".to_string())?;
    let mut out = HashMap::new();
    for item in items {
        let Some(item_id) = item.get("itemId").and_then(Value::as_str) else {
            continue;
        };
        let summary_short = item
            .get("summaryShort")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        let summary_long = item
            .get("summaryLong")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        let keywords = item
            .get("keywords")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(str::trim).map(str::to_string))
            .filter(|value| !value.is_empty())
            .take(8)
            .collect::<Vec<_>>();
        let warnings = item
            .get("warnings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(str::trim).map(str::to_string))
            .filter(|value| !value.is_empty())
            .take(8)
            .collect::<Vec<_>>();
        out.insert(
            item_id.to_string(),
            SummaryAgentItem {
                summary_short,
                summary_long,
                keywords,
                confidence: sanitize_summary_confidence(
                    item.get("confidence").and_then(Value::as_str),
                ),
                warnings,
            },
        );
    }
    Ok(out)
}

pub(super) async fn summarize_batch_with_agent(
    text_route: &RouteConfig,
    response_language: &str,
    stop: &AtomicBool,
    batch_rows: &[Value],
    diagnostics: Option<&OrganizerDiagnostics>,
    stage: &str,
) -> SummaryAgentBatchOutput {
    if text_route.api_key.trim().is_empty() {
        return SummaryAgentBatchOutput {
            items: HashMap::new(),
            usage: TokenUsage::default(),
            error: Some("summary_agent_missing_api_key".to_string()),
        };
    }

    let system_prompt = build_summary_agent_system_prompt(response_language);
    let payload = json!({
        "mode": SUMMARY_MODE_AGENT_SUMMARY,
        "outputLanguage": localized_language_name(response_language, response_language),
        "fileIndex": batch_rows.iter().map(|row| json!({
            "itemId": row.get("itemId").and_then(Value::as_str).unwrap_or(""),
            "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
            "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
            "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
            "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
        })).collect::<Vec<_>>(),
        "items": batch_rows.iter().map(|row| json!({
            "itemId": row.get("itemId").and_then(Value::as_str).unwrap_or(""),
            "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
            "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
            "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
            "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
            "representation": row.get("representation").cloned().unwrap_or_else(|| FileRepresentation::default().to_value()),
            "summaryWarnings": row.get("summaryWarnings").cloned().unwrap_or(Value::Array(Vec::new())),
            "localExtraction": row.get("localExtraction").cloned().unwrap_or(Value::Null),
        })).collect::<Vec<_>>(),
    });

    match chat_completion(
        text_route,
        &system_prompt,
        &payload.to_string(),
        stop,
        diagnostics,
        stage,
    )
    .await
    {
        Ok(output) => {
            let usage = output.usage.clone();
            SummaryAgentBatchOutput {
                usage,
                items: match parse_summary_agent_output(&output.content) {
                    Ok(items) => items,
                    Err(err) => {
                        return SummaryAgentBatchOutput {
                            items: HashMap::new(),
                            usage: output.usage,
                            error: Some(format!("summary_agent_parse_failed:{err}")),
                        };
                    }
                },
                error: None,
            }
        }
        Err(err) => SummaryAgentBatchOutput {
            items: HashMap::new(),
            usage: TokenUsage::default(),
            error: Some(err.message),
        },
    }
}

fn build_organize_system_prompt(response_language: &str, allow_web_search: bool) -> String {
    let is_zh = response_language
        .trim()
        .to_ascii_lowercase()
        .starts_with("zh");

    let web_search_line = if allow_web_search {
        if is_zh {
            "如果本地元数据不足，并且确实需要外部上下文，调用 web_search，且只使用一个简短查询。"
        } else {
            "If local metadata is insufficient and external context is truly necessary, call web_search with one concise query."
        }
    } else {
        if is_zh {
            "web_search 当前不可用。请基于已收集的证据完成判断，并调用 submit_classification_batch。"
        } else {
            "web_search is unavailable for the current step. Base your answer on the evidence already collected and call submit_classification_batch."
        }
    };

    if is_zh {
        format!(
            r#"你负责将文件摘要聚类为一个层级分类树。

你必须使用原生 tool calling。
不要在 assistant 文本中手写 JSON 协议。
不要用普通自然语言返回最终分类树。
当你准备好时，调用 submit_classification_batch。
每次回复最多调用一个工具。

已有节点已经拥有稳定的 nodeId。
当你复用、重命名或移动已有节点时，必须保留原 nodeId。

assignment 中的 reason 字段必须放在最前面，格式为 reason、itemId、leafNodeId、categoryPath。

分类目标：
构建一个实用的文件整理层级分类树。

categoryInventory 字段：
categoryInventory 是已有分类节点下的历史文件轻量清单，用于理解类别边界。
每一项包含 nodeId、path、count、files、truncated。
nodeId 是已有分类节点 ID，复用该类别时应保留这个 nodeId。
path 是该类别在分类树中的路径。
count 是该类别下已归类文件总数。
files 是该类别下的部分或全部历史文件名/短路径，只是文件名和路径线索，不是文件内容或 summary。
truncated=true 表示 files 只列出了一部分历史文件；truncated=false 表示 files 已完整列出。
categoryInventory 只能作为历史参考，当前 items 的文件证据优先。
如果当前文件与历史类别不匹配，可以新建类别或调整分类树。
不要仅因为某个类别 count 很大，就强行把新文件归入该类别。
不要把 files 当作文件内容证据。

顶层分类原则：
顶层分类应优先基于文件的基础类型，例如安装包、文档、压缩包、媒体、代码、数据、应用程序、其他待定等。
不要在顶层优先按业务用途分类，除非文件的基础类型已经清楚。

证据优先级：
1. 如果存在 summaryText，优先使用 summaryText。
2. 其次使用 itemType 和 modality。
3. 再参考文件扩展名和 MIME 类型。
4. 再参考文件名关键词和路径模式。
5. 最后参考大小、时间和其他元数据。

当 summaryText 存在时，优先依据 summaryText 判断。
当 summaryText 不存在时，使用 name、relativePath、itemType、modality 和 representation metadata 判断。
不要因为缺少 summaryText 就假设文件内容未知或无法分类。

类型优先规则：
如果文件的扩展名、文件名模式、MIME 类型或 itemType 能明确指向某个基础类别，即使具体用途不明，也应按该基础类型分类。
不要仅仅因为不知道文件的业务用途、来源应用或具体内容，就归入"其他待定"。

只有当无法根据 name、extension、MIME type、relativePath、size、time metadata、itemType、modality 或 representation metadata 判断文件基础类型时，才使用"其他待定"。

冲突处理：
如果语义推断结果与强文件类型证据冲突，优先相信文件类型证据，除非 summaryText 明确证明该文件应归入其他类别。
如果置信度较低，但文件基础类型仍然可以判断，应归入最接近的类型类别，并在 reason 中简要说明不确定性。

目录整体归类规则：
当 item representation 或 summaryText 中包含 resultKind=whole 时，将其视为目录整体候选。
如果该目录内容看起来具有一致的类型或用途，优先将目录作为一个整体分配到分类树中。
只有当证据明确显示该目录包含无关的混合内容时，才拆分目录中的内容分别归类。

细分规则：
当某个类别包含 5 个或更多项目，并且这些项目存在明显不同的子类型时，可以考虑拆分为简短、实用的子类别。
不要为了过度精细而创建过深或过碎的分类层级。

"其他待定"使用规则：
"其他待定"是最后选择，而不是默认选择。
如果文件类型可以判断，但具体用途不清楚，应归入对应类型类别，而不是"其他待定"。

{web_search_line}"#
        )
    } else {
        format!(
            r#"You cluster file summaries into a hierarchical category tree.

You must use native tool calling.
Do not hand-write JSON protocol in assistant text.
Do not return the final tree as plain assistant text.
When you are ready, call submit_classification_batch.
Call at most one tool per reply.

Existing nodes already have stable nodeId values.
Keep nodeId when you reuse, rename, or move existing nodes.

The assignment "reason" field must come first, in the order: reason, itemId, leafNodeId, categoryPath.

Classification goal:
Build a practical hierarchical category tree for file organization.

categoryInventory field:
categoryInventory is a lightweight list of historical files under existing category nodes. Use it to understand category boundaries.
Each entry contains nodeId, path, count, files, and truncated.
nodeId is the existing category node ID. Keep this nodeId when reusing the category.
path is the category path in the tree.
count is the total number of already-classified files under that category.
files contains some or all historical filenames or short paths for that category. They are filename/path clues only, not file content or summaries.
truncated=true means files contains only part of the historical files; truncated=false means files is complete.
categoryInventory is only historical reference. Current items evidence has priority.
If a current file does not match historical categories, you may create a new category or adjust the tree.
Do not force a new file into a category only because count is large.
Do not treat files as content evidence.

Top-level classification rule:
Top-level categories should be based primarily on the file's fundamental type, such as installer, document, archive, media, code, data, application, or other pending.
Do not classify primarily by business purpose at the top level unless the fundamental file type is already clear.

Evidence priority:
1. Prefer summaryText when it exists.
2. Then use itemType and modality.
3. Then use file extension and MIME type.
4. Then use filename keywords and path patterns.
5. Finally use size, time, and other metadata.

When summaryText exists, prefer it.
When summaryText is missing, classify using name, relativePath, itemType, modality, and representation metadata.
Do not assume missing summaryText means the content is unknown or unclassifiable.

Type-first rule:
If a file's extension, filename pattern, MIME type, or itemType clearly indicates a fundamental category, classify it by that type even if its specific purpose is unclear.
Do not use "其他待定" merely because the file's business purpose, source application, or exact content is unknown.

Only use "其他待定" when the file's fundamental type cannot be determined from name, extension, MIME type, relativePath, size, time metadata, itemType, modality, or representation metadata.

Conflict rule:
If semantic inference conflicts with strong file-type evidence, prefer the file-type evidence unless summaryText clearly proves the file belongs elsewhere.
If confidence is low but the fundamental type is still identifiable, assign the file to the closest type-based category and briefly explain the uncertainty in the reason.

Bundle rule:
When an item representation or summaryText includes resultKind=whole, treat it as a whole-directory bundle candidate.
If the directory appears coherent in type or purpose, prefer assigning the directory as one whole unit.
Only split the directory when the evidence clearly shows unrelated mixed content.

Subdivision rule:
When a category contains 5 or more items with clearly different subtypes, consider splitting it into short, practical subcategories.
Avoid creating overly deep or overly fragmented category hierarchies.

"Other pending" rule:
"其他待定" is a last resort, not a default category.
If the file type is identifiable but the specific purpose is unclear, assign it to the corresponding type-based category instead of "其他待定".

{web_search_line}"#
        )
    }
}

pub(super) fn build_classification_batch_items(batch_rows: &[Value]) -> Vec<Value> {
    batch_rows
        .iter()
        .map(|row| {
            let created_age = compute_relative_age(row.get("createdAt").and_then(Value::as_str));
            let modified_age = compute_relative_age(row.get("modifiedAt").and_then(Value::as_str));
            let representation =
                FileRepresentation::from_value(row.get("representation").unwrap_or(&Value::Null));
            json!({
                "itemId": row.get("itemId").and_then(Value::as_str).unwrap_or(""),
                "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
                "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
                "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
                "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
                "createdAge": created_age,
                "modifiedAge": modified_age,
                "summaryText": representation.best_text(),
                "representation": representation.to_value(),
                "summaryWarnings": row
                    .get("summaryWarnings")
                    .cloned()
                    .unwrap_or(Value::Array(Vec::new())),
            })
        })
        .collect()
}

#[derive(Default)]
struct CategoryInventoryEntry {
    node_id: String,
    path: Vec<String>,
    count: usize,
    files: Vec<String>,
    seen_files: HashSet<String>,
}

fn compact_history_file_label(row: &Value) -> Option<String> {
    let relative_path = row
        .get("relativePath")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let name = row
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    relative_path
        .or(name)
        .map(|value| trim_to_chars(value, 180))
        .filter(|value| !value.trim().is_empty())
}

pub(super) fn build_category_inventory(
    existing_tree: &CategoryTreeNode,
    previous_results: &[Value],
    max_files_per_category: usize,
) -> Vec<Value> {
    let mut entries: Vec<CategoryInventoryEntry> = Vec::new();
    let mut entry_index: HashMap<String, usize> = HashMap::new();

    for row in previous_results {
        if row_has_classification_error(row) {
            continue;
        }
        let node_id = row
            .get("leafNodeId")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if node_id.is_empty() {
            continue;
        }
        let Some(path) = category_path_for_id(existing_tree, node_id) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }

        let idx = if let Some(idx) = entry_index.get(node_id).copied() {
            idx
        } else {
            entries.push(CategoryInventoryEntry {
                node_id: node_id.to_string(),
                path,
                ..CategoryInventoryEntry::default()
            });
            let idx = entries.len() - 1;
            entry_index.insert(node_id.to_string(), idx);
            idx
        };

        let entry = &mut entries[idx];
        entry.count = entry.count.saturating_add(1);
        if entry.files.len() < max_files_per_category {
            if let Some(label) = compact_history_file_label(row) {
                if entry.seen_files.insert(label.clone()) {
                    entry.files.push(label);
                }
            }
        }
    }

    entries
        .into_iter()
        .map(|entry| {
            json!({
                "nodeId": entry.node_id,
                "path": entry.path,
                "count": entry.count,
                "files": entry.files,
                "truncated": entry.count > entry.files.len(),
            })
        })
        .collect()
}

fn search_budget_exhausted_message(response_language: &str) -> &'static str {
    if response_language
        .trim()
        .to_ascii_lowercase()
        .starts_with("zh")
    {
        "联网搜索额度已用完，不能再调用 web_search。请基于当前已有证据立即调用 submit_classification_batch 提交最终聚类结果。"
    } else {
        "Web search budget is exhausted. You can no longer call web_search. Based on the evidence already collected, call submit_classification_batch now."
    }
}

fn build_initial_tree_system_prompt(response_language: &str) -> String {
    if response_language
        .trim()
        .to_ascii_lowercase()
        .starts_with("zh")
    {
        "你负责为文件整理任务生成初始分类树。只基于文件名、后缀、itemType/modality 和统计信息建树；不要分配文件；顶层优先按基础文件类型分类；准备好后调用 submit_initial_tree。".to_string()
    } else {
        "Generate the initial category tree for a file organization task. Use only filenames, extensions, itemType/modality, and stats; do not assign files; keep top-level nodes primarily type-based; call submit_initial_tree when ready.".to_string()
    }
}

fn build_reconcile_system_prompt(response_language: &str, stage: &str) -> String {
    let zh = response_language
        .trim()
        .to_ascii_lowercase()
        .starts_with("zh");
    match (zh, stage) {
        (true, "reconcile_tree") => "你负责统一处理并行分类阶段提交的待处理结果，只处理 treeProposals 和 deferredAssignments。直接 assignments 已由 runtime 合并，不会提供，也不要重新输出。nodeId 使用本轮短别名，必须原样保留这些别名。提交一版 draftTree、proposalMappings 和 rejectedProposalIds；不要输出自然语言结果，只调用 revise_tree_draft。".to_string(),
        (true, "review_tree") => "你负责对 runtime 指定的局部范围做审查。只提交 issues、recommendedOperations 和 needsRevision；不要直接改树，只调用 review_organize_draft。".to_string(),
        (true, "submit_reconciled_tree") => "你负责提交通过审查后的最终分类树，以及仅待处理 item 的最终分配。直接 assignments 已由 runtime 持有，不要重新输出所有文件。所有 leafNodeId 必须存在于 finalTree；只调用 submit_reconciled_tree。".to_string(),
        (false, "reconcile_tree") => "Reconcile only pending structured classification results: treeProposals and deferredAssignments. Direct assignments are already merged by the runtime and are not provided; do not restate them. nodeId values use prompt-local short aliases and must be preserved exactly. Submit one draftTree with proposalMappings and rejectedProposalIds; call revise_tree_draft only.".to_string(),
        (false, "review_tree") => "Review only the runtime-provided local scopes. Submit issues, recommendedOperations, and needsRevision; do not modify the tree; call review_organize_draft only.".to_string(),
        _ => "Submit the reviewed final tree and only the pending item assignments. Direct assignments are already held by the runtime; do not restate every file. Ensure all leafNodeId values exist in finalTree; call submit_reconciled_tree only.".to_string(),
    }
}

#[derive(Clone, Debug, Default)]
struct ReconcileNodeAliases {
    to_alias: HashMap<String, String>,
    to_real: HashMap<String, String>,
}

impl ReconcileNodeAliases {
    fn from_tree(value: &Value) -> Self {
        fn visit(value: &Value, out: &mut Vec<String>) {
            let Some(obj) = value.as_object() else {
                return;
            };
            if let Some(node_id) = obj.get("nodeId").and_then(Value::as_str) {
                let node_id = node_id.trim();
                if !node_id.is_empty() && node_id != "root" && !out.iter().any(|id| id == node_id) {
                    out.push(node_id.to_string());
                }
            }
            if let Some(children) = obj.get("children").and_then(Value::as_array) {
                for child in children {
                    visit(child, out);
                }
            }
        }

        let mut ids = Vec::new();
        visit(value, &mut ids);
        let mut aliases = Self::default();
        for (idx, real) in ids.into_iter().enumerate() {
            let alias = format!("n{}", idx + 1);
            aliases.to_alias.insert(real.clone(), alias.clone());
            aliases.to_real.insert(alias, real);
        }
        aliases
    }

    fn compact_value(&self, value: &Value) -> Value {
        self.rewrite_value(value, true)
    }

    fn expand_value(&self, value: &Value) -> Value {
        self.rewrite_value(value, false)
    }

    fn rewrite_value(&self, value: &Value, compact: bool) -> Value {
        match value {
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| self.rewrite_value(item, compact))
                    .collect(),
            ),
            Value::Object(obj) => {
                let mut out = Map::new();
                for (key, field) in obj {
                    out.insert(key.clone(), self.rewrite_field(key, field, compact));
                }
                Value::Object(out)
            }
            _ => value.clone(),
        }
    }

    fn rewrite_field(&self, key: &str, value: &Value, compact: bool) -> Value {
        match key {
            "nodeId" | "leafNodeId" | "targetNodeId" => value
                .as_str()
                .map(|id| Value::String(self.rewrite_id(id, compact)))
                .unwrap_or_else(|| self.rewrite_value(value, compact)),
            "sourceNodeIds" | "nodeIds" => Value::Array(
                value
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map(|id| Value::String(self.rewrite_id(id, compact)))
                            .unwrap_or_else(|| self.rewrite_value(item, compact))
                    })
                    .collect(),
            ),
            _ => self.rewrite_value(value, compact),
        }
    }

    fn rewrite_id(&self, id: &str, compact: bool) -> String {
        let trimmed = id.trim();
        if compact {
            self.to_alias
                .get(trimmed)
                .cloned()
                .unwrap_or_else(|| trimmed.to_string())
        } else {
            self.to_real
                .get(trimmed)
                .cloned()
                .unwrap_or_else(|| trimmed.to_string())
        }
    }
}

fn pending_deferred_assignments(classification_results: &[Value]) -> Vec<Value> {
    classification_results
        .iter()
        .flat_map(|result| {
            result
                .get("deferredAssignments")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

fn compact_classification_result_for_reconcile(value: &Value) -> Value {
    let source = value
        .get("output")
        .filter(|field| field.is_object())
        .unwrap_or(value);
    json!({
        "batchIndex": value.get("batchIndex").and_then(Value::as_u64).unwrap_or(0),
        "baseTreeVersion": value
            .get("baseTreeVersion")
            .or_else(|| source.get("baseTreeVersion"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        "treeProposals": compact_tree_proposals_for_reconcile(
            source
                .get("treeProposals")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        ),
        "deferredAssignments": compact_deferred_assignments_for_reconcile(
            source
                .get("deferredAssignments")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        ),
        "error": value.get("error").and_then(Value::as_str).unwrap_or(""),
    })
}

fn compact_tree_proposals_for_reconcile(values: &[Value]) -> Vec<Value> {
    values
        .iter()
        .filter_map(|value| {
            let proposal_id = value.get("proposalId").and_then(Value::as_str)?.trim();
            if proposal_id.is_empty() {
                return None;
            }
            let mut out = json!({ "proposalId": proposal_id });
            for key in [
                "operation",
                "targetNodeId",
                "sourceNodeIds",
                "suggestedName",
                "suggestedPath",
                "evidenceItemIds",
            ] {
                if let Some(field) = value.get(key) {
                    if !field.is_null() {
                        out[key] = field.clone();
                    }
                }
            }
            Some(out)
        })
        .collect()
}

fn compact_deferred_assignments_for_reconcile(values: &[Value]) -> Vec<Value> {
    values
        .iter()
        .filter_map(|value| {
            let item_id = value.get("itemId").and_then(Value::as_str)?.trim();
            if item_id.is_empty() {
                return None;
            }
            let mut out = json!({ "itemId": item_id });
            for key in ["proposalId", "suggestedPath", "confidence"] {
                if let Some(field) = value.get(key) {
                    if !field.is_null() {
                        out[key] = field.clone();
                    }
                }
            }
            Some(out)
        })
        .collect()
}

pub(super) async fn generate_initial_tree(
    text_route: &RouteConfig,
    response_language: &str,
    stop: &AtomicBool,
    batch_rows: &[Value],
    diagnostics: Option<&OrganizerDiagnostics>,
) -> Result<InitialTreeOutput, String> {
    let file_index = batch_rows
        .iter()
        .map(|row| {
            json!({
                "itemId": row.get("itemId").and_then(Value::as_str).unwrap_or(""),
                "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
                "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
                "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
                "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
            })
        })
        .collect::<Vec<_>>();
    let messages = vec![
        json!({ "role": "system", "content": build_initial_tree_system_prompt(response_language) }),
        json!({ "role": "user", "content": json!({ "fileIndex": file_index }).to_string() }),
    ];
    let llm = OrganizerBatchLlm {
        route: text_route,
        stop,
        diagnostics,
        stage: "initial_tree",
    };
    let registry = ToolRegistry::new();
    let mut spec = InitialTreeSpec {
        route: text_route,
        response_language,
        initial_messages: messages,
        total_usage: TokenUsage::default(),
        round_trace: Vec::new(),
        available_tool_names: Vec::new(),
    };
    AgentTurnLoop::new(&llm, &registry).run(&mut spec).await
}

struct InitialTreeSpec<'a> {
    route: &'a RouteConfig,
    response_language: &'a str,
    initial_messages: Vec<Value>,
    total_usage: TokenUsage,
    round_trace: Vec<String>,
    available_tool_names: Vec<&'static str>,
}

impl InitialTreeSpec<'_> {
    fn output(&self, parsed: Option<Value>, error: Option<String>) -> InitialTreeOutput {
        InitialTreeOutput {
            parsed,
            usage: self.total_usage.clone(),
            raw_output: self.round_trace.join("\n\n====================\n\n"),
            error,
        }
    }
}

impl AgentTurnSpec for InitialTreeSpec<'_> {
    type Output = InitialTreeOutput;

    fn max_steps(&self) -> usize {
        2
    }

    fn tool_policy<'ctx>(&'ctx self) -> AgentToolPolicy<'ctx> {
        AgentToolPolicy {
            workflow: ToolWorkflow::Organizer,
            stage: "initial_tree",
            session: None,
            bootstrap_turn: false,
            response_language: self.response_language,
            web_search_allowed: false,
            search_remaining: 0,
        }
    }

    fn allow_multiple_tool_calls(&self) -> bool {
        false
    }

    fn build_initial_messages(&mut self) -> Result<Vec<Value>, String> {
        Ok(std::mem::take(&mut self.initial_messages))
    }

    fn before_step(&mut self, _step: usize, messages: &mut Vec<Value>) -> Result<(), String> {
        self.available_tool_names = vec!["submit_initial_tree"];
        if let Some(system_message) = messages.get_mut(0) {
            system_message["content"] =
                Value::String(build_initial_tree_system_prompt(self.response_language));
        }
        Ok(())
    }

    fn tool_execution_context<'ctx>(&'ctx mut self) -> ToolExecutionContext<'ctx> {
        ToolExecutionContext {
            workflow: ToolWorkflow::Organizer,
            stage: "initial_tree",
            session: None,
            bootstrap_turn: false,
            response_language: self.response_language,
            web_search_allowed: false,
            search_remaining: 0,
            state: None,
            search_api_key: None,
            diagnostics: None,
            organizer_search_counter: None,
            organizer_search_gate: None,
        }
    }

    fn on_model_success(
        &mut self,
        step: usize,
        completion: &AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<(), String> {
        self.total_usage.prompt = self
            .total_usage
            .prompt
            .saturating_add(completion.usage.prompt);
        self.total_usage.completion = self
            .total_usage
            .completion
            .saturating_add(completion.usage.completion);
        self.total_usage.total = self
            .total_usage
            .total
            .saturating_add(completion.usage.total);
        append_batch_trace(
            &mut self.round_trace,
            step + 1,
            self.route,
            "http_ok",
            &completion.raw_body,
            Some(&completion.assistant_text),
            None,
            &self.available_tool_names,
            0,
        );
        Ok(())
    }

    fn on_model_error(
        &mut self,
        step: usize,
        err: AgentLlmError,
        _trace: &AgentLoopTrace,
    ) -> Result<Option<Self::Output>, String> {
        append_batch_trace(
            &mut self.round_trace,
            step + 1,
            self.route,
            "http_error",
            &err.raw_body,
            None,
            Some(&err.message),
            &self.available_tool_names,
            0,
        );
        Ok(Some(self.output(None, Some(err.message))))
    }

    fn on_no_tool_calls(
        &mut self,
        _completion: AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<Self::Output, String> {
        Ok(self.output(
            None,
            Some("initial tree response did not call submit_initial_tree".to_string()),
        ))
    }

    fn on_multiple_tool_calls(
        &mut self,
        _completion: AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<Self::Output, String> {
        Ok(self.output(
            None,
            Some("initial tree response used multiple tool calls".to_string()),
        ))
    }

    fn on_tool_success(
        &mut self,
        _step: usize,
        tool_id: Option<ToolId>,
        call: &ParsedToolCall,
        result: ToolResult,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallOutcome<Self::Output>, String> {
        match tool_id {
            Some(ToolId::SubmitInitialTree) => {
                append_batch_trace_note(&mut self.round_trace, "submit_initial_tree", vec![]);
                Ok(ToolCallOutcome::Finish(
                    self.output(Some(result.result), None),
                ))
            }
            _ => Ok(ToolCallOutcome::Finish(self.output(
                None,
                Some(format!("unsupported initial tree tool: {}", call.name)),
            ))),
        }
    }

    fn on_tool_error(
        &mut self,
        _step: usize,
        _call: &ParsedToolCall,
        message: String,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallErrorOutcome<Self::Output>, String> {
        Ok(ToolCallErrorOutcome::Finish(
            self.output(None, Some(message)),
        ))
    }

    fn on_loop_exhausted(&mut self, _trace: &AgentLoopTrace) -> Result<Self::Output, String> {
        Ok(self.output(None, Some("initial tree tool loop exhausted".to_string())))
    }
}

pub(super) async fn reconcile_organize_batches(
    text_route: &RouteConfig,
    response_language: &str,
    stop: &AtomicBool,
    initial_tree: &Value,
    classification_results: &[Value],
    diagnostics: Option<&OrganizerDiagnostics>,
) -> Result<ReconcileOrganizeOutput, String> {
    let aliases = ReconcileNodeAliases::from_tree(initial_tree);
    let compact_initial_tree = aliases.compact_value(initial_tree);
    let compact_classification_results = classification_results
        .iter()
        .map(compact_classification_result_for_reconcile)
        .map(|value| aliases.compact_value(&value))
        .collect::<Vec<_>>();
    let llm = OrganizerBatchLlm {
        route: text_route,
        stop,
        diagnostics,
        stage: "reconcile_tree",
    };
    let registry = ToolRegistry::new();
    let mut spec = ReconcileSpec {
        route: text_route,
        response_language,
        stage: "reconcile_tree",
        initial_tree: compact_initial_tree,
        classification_results: compact_classification_results,
        aliases,
        total_usage: TokenUsage::default(),
        round_trace: Vec::new(),
        available_tool_names: Vec::new(),
        latest_draft: Value::Null,
        latest_review: Value::Null,
    };
    AgentTurnLoop::new(&llm, &registry).run(&mut spec).await
}

struct ReconcileSpec<'a> {
    route: &'a RouteConfig,
    response_language: &'a str,
    stage: &'static str,
    initial_tree: Value,
    classification_results: Vec<Value>,
    aliases: ReconcileNodeAliases,
    total_usage: TokenUsage,
    round_trace: Vec<String>,
    available_tool_names: Vec<&'static str>,
    latest_draft: Value,
    latest_review: Value,
}

impl ReconcileSpec<'_> {
    fn output(&self, parsed: Option<Value>, error: Option<String>) -> ReconcileOrganizeOutput {
        ReconcileOrganizeOutput {
            parsed: parsed.map(|value| self.aliases.expand_value(&value)),
            usage: self.total_usage.clone(),
            raw_output: self.round_trace.join("\n\n====================\n\n"),
            error,
        }
    }

    fn stage_messages(&self) -> Vec<Value> {
        vec![
            json!({
                "role": "system",
                "content": build_reconcile_system_prompt(self.response_language, self.stage),
            }),
            json!({
                "role": "user",
                "content": self.stage_user_payload().to_string(),
            }),
        ]
    }

    fn stage_user_payload(&self) -> Value {
        match self.stage {
            "review_tree" => json!({
                "draft": self.latest_draft.clone(),
                "reviewScope": { "type": "changed_nodes" },
                "instruction": "Review only this draft and proposal mappings. Return issues and needsRevision; do not restate classification input."
            }),
            "submit_reconciled_tree" => json!({
                "draft": self.latest_draft.clone(),
                "review": self.latest_review.clone(),
                "pendingAssignments": pending_deferred_assignments(&self.classification_results),
                "instruction": "Submit finalTree plus proposalMappings. finalAssignments must include only pendingAssignments that still need placement; direct assignments are already merged by runtime."
            }),
            _ => {
                let mut payload = json!({
                    "initialTree": self.initial_tree.clone(),
                    "classificationResults": self.classification_results.clone(),
                    "inputContract": {
                        "pendingOnly": true,
                        "nodeIds": "prompt-local aliases; preserve aliases exactly in tool calls",
                        "omitted": ["direct assignments", "reasons", "raw model output", "file extraction context"]
                    },
                    "instruction": "Use only initialTree and pending classificationResults. First call revise_tree_draft. Runtime will review the draft; when clean, submit only pending finalAssignments."
                });
                if !self.latest_draft.is_null() {
                    payload["previousDraft"] = self.latest_draft.clone();
                }
                if !self.latest_review.is_null() {
                    payload["reviewFeedback"] = self.latest_review.clone();
                }
                payload
            }
        }
    }
}

impl AgentTurnSpec for ReconcileSpec<'_> {
    type Output = ReconcileOrganizeOutput;

    fn max_steps(&self) -> usize {
        8
    }

    fn tool_policy<'ctx>(&'ctx self) -> AgentToolPolicy<'ctx> {
        AgentToolPolicy {
            workflow: ToolWorkflow::Organizer,
            stage: self.stage,
            session: None,
            bootstrap_turn: false,
            response_language: self.response_language,
            web_search_allowed: false,
            search_remaining: 0,
        }
    }

    fn allow_multiple_tool_calls(&self) -> bool {
        false
    }

    fn build_initial_messages(&mut self) -> Result<Vec<Value>, String> {
        Ok(Vec::new())
    }

    fn before_step(&mut self, _step: usize, messages: &mut Vec<Value>) -> Result<(), String> {
        *messages = self.stage_messages();
        self.available_tool_names = match self.stage {
            "reconcile_tree" => vec!["revise_tree_draft"],
            "review_tree" => vec!["review_organize_draft"],
            _ => vec!["submit_reconciled_tree"],
        };
        Ok(())
    }

    fn tool_execution_context<'ctx>(&'ctx mut self) -> ToolExecutionContext<'ctx> {
        ToolExecutionContext {
            workflow: ToolWorkflow::Organizer,
            stage: self.stage,
            session: None,
            bootstrap_turn: false,
            response_language: self.response_language,
            web_search_allowed: false,
            search_remaining: 0,
            state: None,
            search_api_key: None,
            diagnostics: None,
            organizer_search_counter: None,
            organizer_search_gate: None,
        }
    }

    fn on_model_success(
        &mut self,
        step: usize,
        completion: &AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<(), String> {
        self.total_usage.prompt = self
            .total_usage
            .prompt
            .saturating_add(completion.usage.prompt);
        self.total_usage.completion = self
            .total_usage
            .completion
            .saturating_add(completion.usage.completion);
        self.total_usage.total = self
            .total_usage
            .total
            .saturating_add(completion.usage.total);
        append_batch_trace(
            &mut self.round_trace,
            step + 1,
            self.route,
            "http_ok",
            &completion.raw_body,
            Some(&completion.assistant_text),
            None,
            &self.available_tool_names,
            0,
        );
        Ok(())
    }

    fn on_model_error(
        &mut self,
        step: usize,
        err: AgentLlmError,
        _trace: &AgentLoopTrace,
    ) -> Result<Option<Self::Output>, String> {
        append_batch_trace(
            &mut self.round_trace,
            step + 1,
            self.route,
            "http_error",
            &err.raw_body,
            None,
            Some(&err.message),
            &self.available_tool_names,
            0,
        );
        Ok(Some(self.output(None, Some(err.message))))
    }

    fn on_no_tool_calls(
        &mut self,
        _completion: AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<Self::Output, String> {
        Ok(self.output(
            None,
            Some(format!(
                "reconcile stage {} did not call a required tool",
                self.stage
            )),
        ))
    }

    fn on_multiple_tool_calls(
        &mut self,
        _completion: AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<Self::Output, String> {
        Ok(self.output(
            None,
            Some("reconcile response used multiple tool calls".to_string()),
        ))
    }

    fn on_tool_success(
        &mut self,
        _step: usize,
        tool_id: Option<ToolId>,
        call: &ParsedToolCall,
        result: ToolResult,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallOutcome<Self::Output>, String> {
        match tool_id {
            Some(ToolId::ReviseTreeDraft) => {
                self.latest_draft = result.result.clone();
                self.stage = "review_tree";
                append_batch_trace_note(&mut self.round_trace, "revise_tree_draft", vec![]);
                Ok(ToolCallOutcome::Continue {
                    result: json!({
                        "ok": true,
                        "nextStage": "review_tree",
                        "reviewScope": { "type": "changed_nodes" },
                        "validation": []
                    }),
                })
            }
            Some(ToolId::ReviewOrganizeDraft) => {
                self.latest_review = result.result.clone();
                let needs_revision = result
                    .result
                    .get("needsRevision")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.stage = if needs_revision {
                    "reconcile_tree"
                } else {
                    "submit_reconciled_tree"
                };
                append_batch_trace_note(
                    &mut self.round_trace,
                    "review_organize_draft",
                    vec![format!("Needs Revision: {needs_revision}")],
                );
                Ok(ToolCallOutcome::Continue {
                    result: json!({
                        "ok": true,
                        "nextStage": self.stage,
                        "draft": self.latest_draft,
                        "review": self.latest_review,
                    }),
                })
            }
            Some(ToolId::SubmitReconciledTree) => {
                append_batch_trace_note(&mut self.round_trace, "submit_reconciled_tree", vec![]);
                Ok(ToolCallOutcome::Finish(
                    self.output(Some(result.result), None),
                ))
            }
            _ => Ok(ToolCallOutcome::Finish(self.output(
                None,
                Some(format!("unsupported reconcile tool: {}", call.name)),
            ))),
        }
    }

    fn on_tool_error(
        &mut self,
        _step: usize,
        _call: &ParsedToolCall,
        message: String,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallErrorOutcome<Self::Output>, String> {
        Ok(ToolCallErrorOutcome::Finish(
            self.output(None, Some(message)),
        ))
    }

    fn on_loop_exhausted(&mut self, _trace: &AgentLoopTrace) -> Result<Self::Output, String> {
        Ok(self.output(None, Some("reconcile tool loop exhausted".to_string())))
    }
}

pub(super) async fn classify_organize_batch(
    text_route: &RouteConfig,
    response_language: &str,
    stop: &AtomicBool,
    existing_tree: &CategoryTreeNode,
    base_tree_version: u64,
    batch_rows: &[Value],
    category_inventory: &[Value],
    reference_structure: Option<&String>,
    use_web_search: bool,
    search_api_key: &str,
    shared_search_calls: Arc<AtomicUsize>,
    shared_search_gate: Arc<tokio::sync::Semaphore>,
    diagnostics: Option<&OrganizerDiagnostics>,
    stage: &str,
) -> Result<ClassifyOrganizeBatchOutput, String> {
    let search_enabled = use_web_search && !search_api_key.trim().is_empty();
    let classification_items = build_classification_batch_items(batch_rows);
    let file_index = batch_rows
        .iter()
        .map(|row| {
            let created_age = compute_relative_age(row.get("createdAt").and_then(Value::as_str));
            let modified_age = compute_relative_age(row.get("modifiedAt").and_then(Value::as_str));
            let representation =
                FileRepresentation::from_value(row.get("representation").unwrap_or(&Value::Null));
            json!({
                "itemId": row.get("itemId").and_then(Value::as_str).unwrap_or(""),
                "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
                "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
                "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
                "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
                "createdAge": created_age,
                "modifiedAge": modified_age,
                "summaryText": representation.best_text(),
                "representationSource": representation.source,
            })
        })
        .collect::<Vec<_>>();
    let mut payload = json!({
        "existingTree": category_tree_to_value(existing_tree),
        "baseTreeVersion": base_tree_version,
        "categoryInventory": category_inventory,
        "fileIndex": file_index,
        "items": classification_items,
        "useWebSearch": use_web_search,
    });
    if let Some(structure) = reference_structure {
        payload["referenceStructure"] = Value::String(structure.clone());
    }

    let messages = vec![
        json!({
            "role": "system",
            "content": build_organize_system_prompt(response_language, search_enabled),
        }),
        json!({
            "role": "user",
            "content": payload.to_string(),
        }),
    ];
    let tool_registry = ToolRegistry::new();
    let llm = OrganizerBatchLlm {
        route: text_route,
        stop,
        diagnostics,
        stage,
    };
    let mut spec = OrganizerBatchSpec {
        route: text_route,
        response_language,
        search_enabled,
        search_api_key,
        shared_search_calls,
        shared_search_gate,
        diagnostics,
        stage: "classification_batch",
        initial_messages: messages,
        search_calls: 0,
        budget_exhausted_prompt_sent: false,
        total_usage: TokenUsage::default(),
        round_trace: Vec::new(),
        available_tool_names: Vec::new(),
    };

    AgentTurnLoop::new(&llm, &tool_registry)
        .run(&mut spec)
        .await
}

struct OrganizerBatchLlm<'a> {
    route: &'a RouteConfig,
    stop: &'a AtomicBool,
    diagnostics: Option<&'a OrganizerDiagnostics>,
    stage: &'a str,
}

#[async_trait::async_trait]
impl<'a> AgentLlm for OrganizerBatchLlm<'a> {
    async fn complete(
        &self,
        messages: &[Value],
        tools: &[Value],
        _trace_key: Option<&str>,
    ) -> Result<AgentCompletion, AgentLlmError> {
        chat_completion_with_messages(
            self.route,
            messages,
            Some(tools),
            self.stop,
            self.diagnostics,
            self.stage,
        )
        .await
        .map(|output| AgentCompletion {
            raw_body: output.raw_body,
            assistant_text: output.content,
            tool_calls: output.tool_calls,
            raw_message: output.raw_message,
            finish_reason: None,
            usage: output.usage,
            route: Some(crate::agent_runtime::types::AgentRoute {
                endpoint: self.route.endpoint.clone(),
                model: self.route.model.clone(),
            }),
        })
        .map_err(|err| AgentLlmError::new(err.message, err.raw_body))
    }
}

struct OrganizerBatchSpec<'a> {
    route: &'a RouteConfig,
    response_language: &'a str,
    search_enabled: bool,
    search_api_key: &'a str,
    shared_search_calls: Arc<AtomicUsize>,
    shared_search_gate: Arc<tokio::sync::Semaphore>,
    diagnostics: Option<&'a OrganizerDiagnostics>,
    stage: &'a str,
    initial_messages: Vec<Value>,
    search_calls: usize,
    budget_exhausted_prompt_sent: bool,
    total_usage: TokenUsage,
    round_trace: Vec<String>,
    available_tool_names: Vec<&'static str>,
}

impl OrganizerBatchSpec<'_> {
    fn search_remaining(&self) -> usize {
        ORGANIZER_WEB_SEARCH_BUDGET.saturating_sub(self.shared_search_calls.load(Ordering::Relaxed))
    }

    fn output(&self, parsed: Option<Value>, error: Option<String>) -> ClassifyOrganizeBatchOutput {
        ClassifyOrganizeBatchOutput {
            parsed,
            usage: self.total_usage.clone(),
            raw_output: self.round_trace.join("\n\n====================\n\n"),
            error,
            search_calls: self.search_calls,
        }
    }
}

impl AgentTurnSpec for OrganizerBatchSpec<'_> {
    type Output = ClassifyOrganizeBatchOutput;

    fn max_steps(&self) -> usize {
        if self.search_enabled {
            ORGANIZER_WEB_SEARCH_BUDGET + 2
        } else {
            2
        }
    }

    fn tool_policy<'ctx>(&'ctx self) -> AgentToolPolicy<'ctx> {
        let allow_web_search = self.search_enabled && self.search_remaining() > 0;
        AgentToolPolicy {
            workflow: ToolWorkflow::Organizer,
            stage: self.stage,
            session: None,
            bootstrap_turn: false,
            response_language: self.response_language,
            web_search_allowed: allow_web_search,
            search_remaining: self.search_remaining(),
        }
    }

    fn allow_multiple_tool_calls(&self) -> bool {
        false
    }

    fn build_initial_messages(&mut self) -> Result<Vec<Value>, String> {
        Ok(std::mem::take(&mut self.initial_messages))
    }

    fn before_step(&mut self, step: usize, messages: &mut Vec<Value>) -> Result<(), String> {
        let allow_web_search = self.search_enabled && self.search_remaining() > 0;
        if let Some(system_message) = messages.get_mut(0) {
            system_message["content"] = Value::String(build_organize_system_prompt(
                self.response_language,
                allow_web_search,
            ));
        }
        self.available_tool_names = if allow_web_search {
            vec!["web_search", "submit_classification_batch"]
        } else {
            vec!["submit_classification_batch"]
        };

        if self.search_enabled && self.search_remaining() == 0 && !self.budget_exhausted_prompt_sent
        {
            let prompt = search_budget_exhausted_message(self.response_language).to_string();
            messages.push(json!({
                "role": "user",
                "content": prompt.clone(),
            }));
            append_batch_trace_note(
                &mut self.round_trace,
                "budget_exhausted",
                vec![format!("Step: {}", step + 1), format!("Prompt: {}", prompt)],
            );
            self.budget_exhausted_prompt_sent = true;
        }
        Ok(())
    }

    fn tool_execution_context<'ctx>(&'ctx mut self) -> ToolExecutionContext<'ctx> {
        let allow_web_search = self.search_enabled && self.search_remaining() > 0;
        let search_remaining = self.search_remaining();
        ToolExecutionContext {
            workflow: ToolWorkflow::Organizer,
            stage: self.stage,
            session: None,
            bootstrap_turn: false,
            response_language: self.response_language,
            web_search_allowed: allow_web_search,
            search_remaining,
            state: None,
            search_api_key: Some(self.search_api_key),
            diagnostics: self.diagnostics,
            organizer_search_counter: Some(&self.shared_search_calls),
            organizer_search_gate: Some(&self.shared_search_gate),
        }
    }

    fn on_model_success(
        &mut self,
        step: usize,
        completion: &AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<(), String> {
        self.total_usage.prompt = self
            .total_usage
            .prompt
            .saturating_add(completion.usage.prompt);
        self.total_usage.completion = self
            .total_usage
            .completion
            .saturating_add(completion.usage.completion);
        self.total_usage.total = self
            .total_usage
            .total
            .saturating_add(completion.usage.total);
        let search_remaining = self.search_remaining();
        append_batch_trace(
            &mut self.round_trace,
            step + 1,
            self.route,
            "http_ok",
            &completion.raw_body,
            Some(&completion.assistant_text),
            None,
            &self.available_tool_names,
            search_remaining,
        );
        Ok(())
    }

    fn on_model_error(
        &mut self,
        step: usize,
        err: AgentLlmError,
        _trace: &AgentLoopTrace,
    ) -> Result<Option<ClassifyOrganizeBatchOutput>, String> {
        let search_remaining = self.search_remaining();
        append_batch_trace(
            &mut self.round_trace,
            step + 1,
            self.route,
            "http_error",
            &err.raw_body,
            None,
            Some(&err.message),
            &self.available_tool_names,
            search_remaining,
        );
        Ok(Some(self.output(None, Some(err.message))))
    }

    fn on_no_tool_calls(
        &mut self,
        _completion: AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<ClassifyOrganizeBatchOutput, String> {
        Ok(self.output(
            None,
            Some("classification response did not call a required organizer tool".to_string()),
        ))
    }

    fn on_multiple_tool_calls(
        &mut self,
        _completion: AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<ClassifyOrganizeBatchOutput, String> {
        Ok(self.output(
            None,
            Some("classification response used multiple tool calls in one step".to_string()),
        ))
    }

    fn on_tool_success(
        &mut self,
        _step: usize,
        tool_id: Option<ToolId>,
        call: &ParsedToolCall,
        result: ToolResult,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallOutcome<ClassifyOrganizeBatchOutput>, String> {
        match tool_id {
            Some(ToolId::WebSearch) => {
                if result
                    .diagnostics
                    .as_ref()
                    .and_then(|value| value.get("searchConsumed"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    self.search_calls = self.search_calls.saturating_add(1);
                }
                let mut note = Vec::new();
                if let Some(query) = result.result.get("query").and_then(Value::as_str) {
                    note.push(format!("Query: {}", query));
                }
                if let Some(reason) = result.result.get("reason").and_then(Value::as_str) {
                    note.push(format!("Reason: {}", reason));
                }
                if let Some(results) = result.result.get("results").and_then(Value::as_array) {
                    note.push(format!("Result Count: {}", results.len()));
                }
                if let Some(error) = result.result.get("error").and_then(Value::as_str) {
                    note.push(format!("Error: {}", error));
                }
                if let Some(message) = result.result.get("message").and_then(Value::as_str) {
                    note.push(format!("Message: {}", message));
                }
                note.push(format!(
                    "Search Budget Remaining: {}",
                    self.search_remaining()
                ));
                append_batch_trace_note(&mut self.round_trace, "web_search", note);
                Ok(ToolCallOutcome::Continue {
                    result: result.result,
                })
            }
            Some(ToolId::SubmitClassificationBatch) => {
                let parsed = result.result;
                append_batch_trace_note(
                    &mut self.round_trace,
                    "submit_classification_batch",
                    vec![
                        format!(
                            "Assignment Count: {}",
                            parsed
                                .get("assignments")
                                .and_then(Value::as_array)
                                .map(|rows| rows.len())
                                .unwrap_or(0)
                        ),
                        format!("Search Calls Used: {}", self.search_calls),
                    ],
                );
                Ok(ToolCallOutcome::Finish(self.output(Some(parsed), None)))
            }
            _ => Ok(ToolCallOutcome::Finish(self.output(
                None,
                Some(format!("unsupported organizer tool: {}", call.name)),
            ))),
        }
    }

    fn on_tool_error(
        &mut self,
        _step: usize,
        _call: &ParsedToolCall,
        message: String,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallErrorOutcome<ClassifyOrganizeBatchOutput>, String> {
        Ok(ToolCallErrorOutcome::Finish(
            self.output(None, Some(message)),
        ))
    }

    fn on_loop_exhausted(
        &mut self,
        _trace: &AgentLoopTrace,
    ) -> Result<ClassifyOrganizeBatchOutput, String> {
        Ok(self.output(
            None,
            Some(
                "classification tool loop exhausted without submit_classification_batch"
                    .to_string(),
            ),
        ))
    }
}

#[cfg(test)]
mod tool_policy_tests {
    use super::*;

    #[test]
    fn organizer_batch_disallows_multiple_tool_calls() {
        let route = RouteConfig {
            endpoint: "https://example.invalid/v1/chat/completions".to_string(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
        };
        let spec = OrganizerBatchSpec {
            route: &route,
            response_language: "zh",
            search_enabled: true,
            search_api_key: "search-key",
            shared_search_calls: Arc::new(AtomicUsize::new(0)),
            shared_search_gate: Arc::new(tokio::sync::Semaphore::new(ORGANIZER_SEARCH_CONCURRENCY)),
            diagnostics: None,
            stage: "classification_batch",
            initial_messages: Vec::new(),
            search_calls: 0,
            budget_exhausted_prompt_sent: false,
            total_usage: TokenUsage::default(),
            round_trace: Vec::new(),
            available_tool_names: Vec::new(),
        };

        assert!(!spec.allow_multiple_tool_calls());
    }
}

pub(super) fn emit_organize_summary_ready<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    batch_index: u64,
    row: &Value,
) {
    let payload = json!({
        "taskId": task_id,
        "batchIndex": batch_index,
        "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
        "path": row.get("path").and_then(Value::as_str).unwrap_or(""),
        "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
        "size": row.get("size").and_then(Value::as_u64).unwrap_or(0),
        "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
        "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
        "summaryStrategy": row
            .get("summaryStrategy")
            .cloned()
            .unwrap_or(Value::String(SUMMARY_MODE_FILENAME_ONLY.to_string())),
        "representation": row.get("representation").cloned().unwrap_or_else(|| FileRepresentation::default().to_value()),
        "summaryWarnings": row
            .get("summaryWarnings")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
        "warnings": row
            .get("summaryWarnings")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
        "localExtraction": row.get("localExtraction").cloned().unwrap_or(Value::Null),
        "provider": row.get("provider").and_then(Value::as_str).unwrap_or(""),
        "model": row.get("model").and_then(Value::as_str).unwrap_or(""),
    });
    if let Err(err) = app.emit("organize_summary_ready", payload) {
        log::warn!("failed to emit organize_summary_ready for task {task_id}: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_budget_exhausted_message_is_localized() {
        assert!(search_budget_exhausted_message("zh-CN").contains("联网搜索额度已用完"));
        assert!(search_budget_exhausted_message("en").contains("Web search budget is exhausted"));
    }
}
