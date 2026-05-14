use super::*;
use crate::advisor_runtime::types::local_text;
use crate::agent_runtime::{
    AgentCompletion, AgentLlm, AgentLlmError, AgentLoopTrace, AgentToolPolicy, AgentTurnLoop,
    AgentTurnSpec, NoToolCallOutcome, ToolCallErrorOutcome, ToolCallOutcome,
};
use crate::llm_tools::{ToolExecutionContext, ToolId, ToolRegistry, ToolResult, ToolWorkflow};
use crate::reasoning_policy;
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

pub(crate) fn extract_plain_text_summary(unit: &OrganizeUnit) -> SummaryExtraction {
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

pub(crate) fn extract_unit_content_for_summary(
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

pub(crate) fn supports_tika_extraction(unit: &OrganizeUnit) -> bool {
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

pub(crate) async fn tika_extract_text(
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

pub(crate) async fn extract_unit_content_for_summary_with_tools(
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
    route: &RouteConfig,
    status: StatusCode,
    raw_body: &str,
) -> Result<ChatCompletionOutput, ChatCompletionError> {
    let parsed =
        parse_completion_response(route.api_format, status, raw_body).map_err(|message| {
            ChatCompletionError {
                message: format!(
                    "{} | body: {}",
                    message,
                    summarize_response_body_for_error(raw_body)
                ),
                raw_body: raw_body.to_string(),
            }
        })?;
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
    let api_format = route.api_format;
    let url = build_messages_url(&route.endpoint, api_format);
    let client =
        build_llm_http_client(CHAT_COMPLETION_TIMEOUT_SECS).map_err(|e| ChatCompletionError {
            message: e.to_string(),
            raw_body: String::new(),
        })?;
    let effective_thinking = reasoning_policy::organizer_stage(stage, &route.thinking_level);
    let payload = build_completion_payload(
        api_format,
        &route.model,
        messages,
        tools,
        0.0,
        DEFAULT_MAX_TOKENS,
        ThinkingConfig {
            enabled: effective_thinking.enabled,
            level: effective_thinking.level,
        },
    )
    .map_err(|message| ChatCompletionError {
        message,
        raw_body: String::new(),
    })?;
    let started_at = Instant::now();
    if let Some(diagnostics) = diagnostics {
        diagnostics.model_request(
            stage,
            route,
            effective_thinking.enabled,
            effective_thinking.level,
            &url,
            messages,
            tools.unwrap_or(&[]),
            &payload,
        );
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
    let route_for_request = route.clone();
    let stage_for_request = stage.to_string();
    let request_future = async move {
        let resp = match req.send().await {
            Ok(resp) => resp,
            Err(e) => {
                if let Some(diagnostics) = diagnostics_for_request.as_ref() {
                    diagnostics.model_error(
                        &stage_for_request,
                        &route_for_request,
                        effective_thinking.enabled,
                        effective_thinking.level,
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
                        &route_for_request,
                        effective_thinking.enabled,
                        effective_thinking.level,
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
                &route_for_request,
                effective_thinking.enabled,
                effective_thinking.level,
                status.as_u16(),
                &raw_body,
                started_at.elapsed(),
            );
        }
        parse_chat_completion_http_body(&route_for_request, status, &raw_body)
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

输出语言使用中文。

你必须使用原生 tool calling。
不要输出普通文本结果，不要手写 JSON。
当你准备好时，调用 submit_file_summaries。
每个输入 itemId 都必须且只能回传一次。"#
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

Write summaries in English only.

You must use native tool calling.
Do not return plain text results and do not hand-write JSON.
When ready, call submit_file_summaries.
Each input itemId must be returned exactly once."#.to_string()
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
    let tool_registry = ToolRegistry::new();
    let llm = SummaryBatchLlm {
        route: text_route,
        stop,
        diagnostics,
        stage,
    };
    let mut spec = SummaryBatchSpec {
        route: text_route,
        response_language,
        diagnostics,
        stage: "summary_batch",
        initial_messages: vec![
            json!({ "role": "system", "content": system_prompt }),
            json!({ "role": "user", "content": payload.to_string() }),
        ],
        total_usage: TokenUsage::default(),
        round_trace: Vec::new(),
        available_tool_names: Vec::new(),
    };
    match AgentTurnLoop::new(&llm, &tool_registry)
        .run(&mut spec)
        .await
    {
        Ok(output) => output,
        Err(message) => SummaryAgentBatchOutput {
            items: HashMap::new(),
            usage: TokenUsage::default(),
            error: Some(message),
        },
    }
}

struct SummaryBatchLlm<'a> {
    route: &'a RouteConfig,
    stop: &'a AtomicBool,
    diagnostics: Option<&'a OrganizerDiagnostics>,
    stage: &'a str,
}

#[async_trait::async_trait]
impl<'a> AgentLlm for SummaryBatchLlm<'a> {
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

struct SummaryBatchSpec<'a> {
    route: &'a RouteConfig,
    response_language: &'a str,
    diagnostics: Option<&'a OrganizerDiagnostics>,
    stage: &'a str,
    initial_messages: Vec<Value>,
    total_usage: TokenUsage,
    round_trace: Vec<String>,
    available_tool_names: Vec<&'static str>,
}

impl SummaryBatchSpec<'_> {
    fn parse_tool_result(&self, value: Value) -> Result<HashMap<String, SummaryAgentItem>, String> {
        parse_summary_agent_output(&value.to_string())
    }
}

impl AgentTurnSpec for SummaryBatchSpec<'_> {
    type Output = SummaryAgentBatchOutput;

    fn max_steps(&self) -> usize {
        2
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

    fn build_initial_messages(&mut self) -> Result<Vec<Value>, String> {
        Ok(std::mem::take(&mut self.initial_messages))
    }

    fn before_step(&mut self, _step: usize, _messages: &mut Vec<Value>) -> Result<(), String> {
        self.available_tool_names = vec!["submit_file_summaries"];
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
            diagnostics: self.diagnostics,
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
    ) -> Result<Option<SummaryAgentBatchOutput>, String> {
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
        Ok(Some(SummaryAgentBatchOutput {
            items: HashMap::new(),
            usage: self.total_usage.clone(),
            error: Some(err.message),
        }))
    }

    fn on_no_tool_calls(
        &mut self,
        _completion: &AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<NoToolCallOutcome<SummaryAgentBatchOutput>, String> {
        Ok(NoToolCallOutcome::Finish(SummaryAgentBatchOutput {
            items: HashMap::new(),
            usage: self.total_usage.clone(),
            error: Some("summary response did not call submit_file_summaries".to_string()),
        }))
    }

    fn on_tool_success(
        &mut self,
        _step: usize,
        tool_id: Option<ToolId>,
        call: &crate::llm_protocol::ParsedToolCall,
        result: ToolResult,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallOutcome<SummaryAgentBatchOutput>, String> {
        match tool_id {
            Some(ToolId::SubmitFileSummaries) => {
                let items = self.parse_tool_result(result.result)?;
                append_batch_trace_note(
                    &mut self.round_trace,
                    "submit_file_summaries",
                    vec![format!("Item Count: {}", items.len())],
                );
                Ok(ToolCallOutcome::Finish(SummaryAgentBatchOutput {
                    items,
                    usage: self.total_usage.clone(),
                    error: None,
                }))
            }
            _ => Ok(ToolCallOutcome::Finish(SummaryAgentBatchOutput {
                items: HashMap::new(),
                usage: self.total_usage.clone(),
                error: Some(format!("unsupported summary tool: {}", call.name)),
            })),
        }
    }

    fn on_tool_error(
        &mut self,
        _step: usize,
        _call: &crate::llm_protocol::ParsedToolCall,
        message: String,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallErrorOutcome<SummaryAgentBatchOutput>, String> {
        Ok(ToolCallErrorOutcome::Finish(SummaryAgentBatchOutput {
            items: HashMap::new(),
            usage: self.total_usage.clone(),
            error: Some(message),
        }))
    }

    fn on_loop_exhausted(
        &mut self,
        _trace: &AgentLoopTrace,
    ) -> Result<SummaryAgentBatchOutput, String> {
        Ok(SummaryAgentBatchOutput {
            items: HashMap::new(),
            usage: self.total_usage.clone(),
            error: Some("summary tool loop exhausted without submit_file_summaries".to_string()),
        })
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

所有分类名称、reason 和其他文本字段必须使用中文纯文本。禁止使用 emoji 或装饰性 Unicode 符号（如 📁、✅、🎯 等）。

你必须使用原生 tool calling。
不要在 assistant 文本中手写 JSON 协议。
不要用普通自然语言返回最终分类树。
当你准备好时，调用 submit_classification_batch。
每次回复最多调用一个工具。

已有节点在本轮输入中使用 prompt-local 短 nodeId，例如 n1、n2。
当你复用、重命名或移动已有节点时，必须原样保留这些短 nodeId。
不要生成 UUID 或后端真实 ID；新增分类请使用 treeProposals 的 suggestedPath 表达。

assignment 中的 reason 字段必须放在最前面，格式为 reason、itemId、leafNodeId、categoryPath。
reason 保持简短，只写足以解释分类的证据或不确定性。

分类目标：
构建一个实用的文件整理层级分类树。

fileIndex 字段：
fileIndex 只是当前批次的快速文件名索引，用于先扫一眼文件列表。
每项只包含 itemId、name，必要时包含 relativePath 用于重名消歧。
不要把 fileIndex 当作完整证据；分类判断应以 items 中的 evidence 和结构化字段为准。

categoryInventory 字段：
categoryInventory 是已有分类节点下的历史文件轻量清单，用于理解类别边界。
每一项包含 nodeId、path、count、files、truncated。
nodeId 是本轮短 ID，复用该类别时应保留这个 nodeId。
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
1. 如果存在 evidence，优先使用 evidence。
2. 其次使用 itemType 和 modality。
3. 再参考文件扩展名和 MIME 类型。
4. 再参考文件名关键词和路径模式。
5. 最后参考大小、时间和其他元数据。

当 evidence 存在时，优先依据 evidence 判断。
当 evidence 较少时，使用 name、relativePath、itemType、modality 判断。
不要因为 evidence 很短就假设文件内容未知或无法分类。

类型优先规则：
如果文件的扩展名、文件名模式、MIME 类型或 itemType 能明确指向某个基础类别，即使具体用途不明，也应按该基础类型分类。
不要仅仅因为不知道文件的业务用途、来源应用或具体内容，就归入"其他待定"。

只有当无法根据 name、extension、MIME type、relativePath、size、time metadata、itemType、modality 或 evidence 判断文件基础类型时，才使用"其他待定"。

冲突处理：
如果语义推断结果与强文件类型证据冲突，优先相信文件类型证据，除非 evidence 明确证明该文件应归入其他类别。
如果置信度较低，但文件基础类型仍然可以判断，应归入最接近的类型类别，并在 reason 中简要说明不确定性。

目录整体归类规则：
当 item evidence 中包含 resultKind=whole 时，将其视为目录整体候选。
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

All category names, reasons, and other text fields must use plain text. Do not use emoji or decorative Unicode symbols (e.g. 📁, ✅, 🎯, etc.).

You must use native tool calling.
Do not hand-write JSON protocol in assistant text.
Do not return the final tree as plain assistant text.
When you are ready, call submit_classification_batch.
Call at most one tool per reply.

Existing nodes use prompt-local short nodeId values in this request, such as n1 and n2.
Keep those short nodeId values exactly when you reuse, rename, or move existing nodes.
Do not generate UUIDs or backend real IDs; use treeProposals suggestedPath for new categories.

The assignment "reason" field must come first, in the order: reason, itemId, leafNodeId, categoryPath.
Keep reason concise; include only the evidence or uncertainty needed to explain the assignment.

Classification goal:
Build a practical hierarchical category tree for file organization.

fileIndex field:
fileIndex is only a quick filename index for the current batch.
Each entry contains itemId and name, and may include relativePath only to disambiguate duplicate names.
Do not treat fileIndex as full evidence; classify from each item's evidence and structured fields.

categoryInventory field:
categoryInventory is a lightweight list of historical files under existing category nodes. Use it to understand category boundaries.
Each entry contains nodeId, path, count, files, and truncated.
nodeId is a prompt-local short ID. Keep this nodeId when reusing the category.
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
1. Prefer evidence when it exists.
2. Then use itemType and modality.
3. Then use file extension and MIME type.
4. Then use filename keywords and path patterns.
5. Finally use size, time, and other metadata.

When evidence exists, prefer it.
When evidence is sparse, classify using name, relativePath, itemType, and modality.
Do not assume short evidence means the content is unknown or unclassifiable.

Type-first rule:
If a file's extension, filename pattern, MIME type, or itemType clearly indicates a fundamental category, classify it by that type even if its specific purpose is unclear.
Do not use "其他待定" merely because the file's business purpose, source application, or exact content is unknown.

Only use "其他待定" when the file's fundamental type cannot be determined from name, extension, MIME type, relativePath, size, time metadata, itemType, modality, or evidence.

Conflict rule:
If semantic inference conflicts with strong file-type evidence, prefer the file-type evidence unless evidence clearly proves the file belongs elsewhere.
If confidence is low but the fundamental type is still identifiable, assign the file to the closest type-based category and briefly explain the uncertainty in the reason.

Bundle rule:
When item evidence includes resultKind=whole, treat it as a whole-directory bundle candidate.
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

pub(super) fn build_classification_file_index(batch_rows: &[Value]) -> Vec<Value> {
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    for row in batch_rows {
        let name = row
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        *name_counts.entry(name).or_insert(0) += 1;
    }

    batch_rows
        .iter()
        .map(|row| {
            let name = row.get("name").and_then(Value::as_str).unwrap_or("");
            let name_key = name.trim().to_string();
            let mut item = json!({
                "itemId": row.get("itemId").and_then(Value::as_str).unwrap_or(""),
                "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
            });
            if name_counts.get(&name_key).copied().unwrap_or(0) > 1 {
                item["relativePath"] = Value::String(
                    row.get("relativePath")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                );
            }
            item
        })
        .collect()
}

pub(super) fn classification_evidence(row: &Value, representation: &FileRepresentation) -> String {
    let source = representation.source.trim();
    let best_text = representation.best_text();
    if !best_text.is_empty() && source != SUMMARY_SOURCE_FILENAME_ONLY {
        return best_text;
    }

    let relative_path = row
        .get("relativePath")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !relative_path.is_empty() {
        return relative_path.to_string();
    }
    row.get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

pub(super) fn build_classification_batch_items(batch_rows: &[Value]) -> Vec<Value> {
    batch_rows
        .iter()
        .map(|row| {
            let created_age = compute_relative_age(row.get("createdAt").and_then(Value::as_str));
            let modified_age = compute_relative_age(row.get("modifiedAt").and_then(Value::as_str));
            let representation =
                FileRepresentation::from_value(row.get("representation").unwrap_or(&Value::Null));
            let mut item = json!({
                "itemId": row.get("itemId").and_then(Value::as_str).unwrap_or(""),
                "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
                "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
                "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
                "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
                "createdAge": created_age,
                "modifiedAge": modified_age,
                "evidence": classification_evidence(row, &representation),
            });
            if !representation.keywords.is_empty()
                && representation.source != SUMMARY_SOURCE_FILENAME_ONLY
            {
                item["keywords"] = Value::Array(
                    representation
                        .keywords
                        .iter()
                        .map(|keyword| Value::String(keyword.clone()))
                        .collect(),
                );
            }
            item
        })
        .collect()
}

#[derive(Default)]
#[allow(dead_code)]
struct CategoryInventoryEntry {
    node_id: String,
    path: Vec<String>,
    count: usize,
    files: Vec<String>,
    seen_files: HashSet<String>,
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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
        "你负责为文件整理任务生成初始分类树。只基于文件名、后缀、itemType/modality 和统计信息建树；不要分配文件；顶层优先按基础文件类型分类；准备好后调用 submit_initial_tree。所有分类名称必须使用中文纯文本，禁止使用 emoji 或装饰性 Unicode 符号。".to_string()
    } else {
        "Generate the initial category tree for a file organization task. Use only filenames, extensions, itemType/modality, and stats; do not assign files; keep top-level nodes primarily type-based; call submit_initial_tree when ready. All category names must be plain text; do not use emoji or decorative Unicode symbols.".to_string()
    }
}

fn organizer_submit_tool_retry_message(
    response_language: &str,
    tool_name: &str,
    message: &str,
) -> Option<String> {
    let guidance = match tool_name {
        "submit_initial_tree" => Some((
            "重新调用 submit_initial_tree，只提交合法参数。tree 必须是 JSON 对象，形如 {\"nodeId\":\"root\",\"name\":\"\",\"children\":[]}；不要把自然语言说明放进 tree，说明只能放 notes。",
            "Re-call submit_initial_tree with valid arguments only. tree must be a JSON object shaped like {\"nodeId\":\"root\",\"name\":\"\",\"children\":[]}; do not put natural-language explanation in tree, use notes for explanation.",
        )),
        "submit_classification_batch" => Some((
            "重新调用 submit_classification_batch，只提交合法参数。assignments 必须是数组；只能引用已有 leafNodeId，新增分类建议放 treeProposals。",
            "Re-call submit_classification_batch with valid arguments only. assignments must be an array; reference existing leafNodeId values only, and put new category suggestions in treeProposals.",
        )),
        "submit_local_reclassification" => Some((
            "重新调用 submit_local_reclassification，只提交合法参数。每个 assignment 必须包含 itemId 和 reason，并至少提供 leafNodeId 或非空 categoryPath；categoryPath 必须是当前子树内的相对后代路径。",
            "Re-call submit_local_reclassification with valid arguments only. Each assignment must include itemId and reason, plus either leafNodeId or a non-empty categoryPath. categoryPath must be a relative descendant path within the current subtree.",
        )),
        "revise_tree_draft" => Some((
            "重新调用 revise_tree_draft，只提交合法参数。draftTree 必须是 JSON 对象，节点必须包含 nodeId、name、children。",
            "Re-call revise_tree_draft with valid arguments only. draftTree must be a JSON object, and every node must include nodeId, name, and children.",
        )),
        "review_organize_draft" => Some((
            "重新调用 review_organize_draft，只提交合法参数。issues 必须是数组，needsRevision 必须是布尔值。",
            "Re-call review_organize_draft with valid arguments only. issues must be an array and needsRevision must be a boolean.",
        )),
        "submit_reconciled_tree" => Some((
            "重新调用 submit_reconciled_tree，只提交合法参数。finalTree 必须是 JSON 对象；proposalMappings 和 finalAssignments 必须是数组。",
            "Re-call submit_reconciled_tree with valid arguments only. finalTree must be a JSON object; proposalMappings and finalAssignments must be arrays.",
        )),
        _ => None,
    }?;
    let guidance = if response_language
        .trim()
        .to_ascii_lowercase()
        .starts_with("zh")
    {
        guidance.0
    } else {
        guidance.1
    };
    Some(format!(
        "{tool_name} arguments failed validation: {message}. {guidance}"
    ))
}

#[cfg(test)]
fn build_reconcile_system_prompt(response_language: &str, stage: &str) -> String {
    let zh = response_language
        .trim()
        .to_ascii_lowercase()
        .starts_with("zh");
    match (zh, stage) {
        (true, "reconcile_tree") => "你负责统一处理并行分类阶段提交的 treeProposals。你同时拥有 revise_tree_draft 和 review_organize_draft。所有 assignments（含原 deferredAssignments）已由 runtime 合并，不会提供，也不要重新输出。nodeId 使用本轮短别名，必须原样保留这些别名。提交一版 draftTree、proposalMappings 和 rejectedProposalIds；不要输出自然语言结果，只调用 revise_tree_draft。执行 merge_nodes 时，targetNodeId 必须填写为要保留并重命名的现有节点，sourceNodeIds 是要并入并删除的其他节点。所有节点名称必须使用纯文本，禁止 emoji 或装饰性符号。".to_string(),
        (true, "submit_reconciled_tree") => "你负责提交通过审查后的最终分类树。所有 assignments 已由 runtime 持有，不要重新输出。只调用 submit_reconciled_tree。所有节点名称必须使用纯文本，禁止 emoji 或装饰性符号。".to_string(),
        (false, "reconcile_tree") => "Reconcile only treeProposals from parallel classification. You have both revise_tree_draft and review_organize_draft tools. All assignments (including former deferredAssignments) are already merged by the runtime and are not provided; do not restate them. nodeId values use prompt-local short aliases and must be preserved exactly. Submit one draftTree with proposalMappings and rejectedProposalIds; call revise_tree_draft only. For merge_nodes, targetNodeId is required and must be the existing node you keep and rename; sourceNodeIds are the nodes you merge into it and remove. All node names must be plain text; no emoji or decorative symbols.".to_string(),
        _ => "Submit the reviewed final tree. All assignments are already held by the runtime; do not restate any files. call submit_reconciled_tree only. All node names must be plain text; no emoji or decorative symbols.".to_string(),
    }
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
        _completion: &AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<NoToolCallOutcome<Self::Output>, String> {
        Ok(NoToolCallOutcome::Finish(self.output(
            None,
            Some("initial tree response did not call submit_initial_tree".to_string()),
        )))
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
        call: &ParsedToolCall,
        message: String,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallErrorOutcome<Self::Output>, String> {
        if let Some(message) =
            organizer_submit_tool_retry_message(self.response_language, &call.name, &message)
        {
            return Ok(ToolCallErrorOutcome::Continue { message });
        }
        Ok(ToolCallErrorOutcome::Finish(
            self.output(None, Some(message)),
        ))
    }

    fn on_loop_exhausted(&mut self, _trace: &AgentLoopTrace) -> Result<Self::Output, String> {
        Ok(self.output(None, Some("initial tree tool loop exhausted".to_string())))
    }
}

#[cfg(test)]
pub(super) async fn reconcile_organize_batches(
    text_route: &RouteConfig,
    response_language: &str,
    stop: &AtomicBool,
    initial_tree: &Value,
    classification_results: &[Value],
    diagnostics: Option<&OrganizerDiagnostics>,
) -> Result<ReconcileOrganizeOutput, String> {
    let aliases = ModelIdMap::from_values(&[initial_tree]);
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

#[cfg(test)]
struct ReconcileSpec<'a> {
    route: &'a RouteConfig,
    response_language: &'a str,
    stage: &'static str,
    initial_tree: Value,
    classification_results: Vec<Value>,
    aliases: ModelIdMap,
    total_usage: TokenUsage,
    round_trace: Vec<String>,
    available_tool_names: Vec<&'static str>,
    latest_draft: Value,
    latest_review: Value,
}

#[cfg(test)]
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
            "submit_reconciled_tree" => json!({
                "draft": self.latest_draft.clone(),
                "review": self.latest_review.clone(),
                "instruction": "Submit finalTree and proposalMappings. All assignments are already merged by runtime."
            }),
            _ if !self.latest_draft.is_null() => {
                json!({
                    "draft": self.latest_draft.clone(),
                    "review": self.latest_review.clone(),
                    "instruction": "Review the draft and make further revisions with revise_tree_draft. Optionally call review_organize_draft. When satisfied, call submit_tree_shape."
                })
            }
            _ => {
                let mut payload = json!({
                    "initialTree": self.initial_tree.clone(),
                    "classificationResults": self.classification_results.clone(),
                    "inputContract": {
                        "pendingOnly": true,
                        "nodeIds": "prompt-local aliases; preserve aliases exactly in tool calls",
                        "omitted": ["direct assignments", "reasons", "raw model output", "file extraction context"]
                    },
                    "instruction": "Use only initialTree and classificationResults (treeProposals only). First call revise_tree_draft. When clean, call submit_tree_shape."
                });
                if !self.latest_review.is_null() {
                    payload["reviewFeedback"] = self.latest_review.clone();
                }
                payload
            }
        }
    }
}

#[cfg(test)]
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

    fn build_initial_messages(&mut self) -> Result<Vec<Value>, String> {
        Ok(Vec::new())
    }

    fn before_step(&mut self, step: usize, messages: &mut Vec<Value>) -> Result<(), String> {
        if step == 0 {
            *messages = self.stage_messages();
        } else {
            if let Some(system_message) = messages.get_mut(0) {
                system_message["content"] = Value::String(build_reconcile_system_prompt(
                    self.response_language,
                    self.stage,
                ));
            }
            if messages.len() >= 2 {
                messages[1] =
                    json!({"role": "user", "content": self.stage_user_payload().to_string()});
            }
        }
        self.available_tool_names = match self.stage {
            "reconcile_tree" => vec![
                "revise_tree_draft",
                "review_organize_draft",
                "submit_tree_shape",
            ],
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
        _completion: &AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<NoToolCallOutcome<Self::Output>, String> {
        Ok(NoToolCallOutcome::Finish(self.output(
            None,
            Some(format!(
                "reconcile stage {} did not call a required tool",
                self.stage
            )),
        )))
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
                append_batch_trace_note(&mut self.round_trace, "revise_tree_draft", vec![]);
                Ok(ToolCallOutcome::Continue {
                    result: json!({
                        "ok": true,
                        "draftTree": self.latest_draft["draftTree"],
                        "instruction": "Draft updated. Self-review via review_organize_draft when ready, then call submit_reconciled_tree."
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
                append_batch_trace_note(
                    &mut self.round_trace,
                    "review_organize_draft",
                    vec![format!("Needs Revision: {needs_revision}")],
                );
                Ok(ToolCallOutcome::Continue {
                    result: json!({
                        "ok": true,
                        "needsRevision": needs_revision,
                        "draft": self.latest_draft,
                        "review": self.latest_review,
                        "instruction": if needs_revision { "Fix issues with revise_tree_draft, then submit_reconciled_tree." } else { "Review passed. Call submit_reconciled_tree." }
                    }),
                })
            }
            Some(ToolId::SubmitTreeShape) => {
                append_batch_trace_note(&mut self.round_trace, "submit_tree_shape", vec![]);
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
        call: &ParsedToolCall,
        message: String,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallErrorOutcome<Self::Output>, String> {
        if let Some(message) =
            organizer_submit_tool_retry_message(self.response_language, &call.name, &message)
        {
            return Ok(ToolCallErrorOutcome::Continue { message });
        }
        Ok(ToolCallErrorOutcome::Finish(
            self.output(None, Some(message)),
        ))
    }

    fn on_loop_exhausted(&mut self, _trace: &AgentLoopTrace) -> Result<Self::Output, String> {
        Ok(self.output(None, Some("reconcile tool loop exhausted".to_string())))
    }
}

// ---------------------------------------------------------------------------
// Phase 1: Build Tree Shape (LLM)
// ---------------------------------------------------------------------------

fn build_tree_shape_system_prompt(response_language: &str) -> String {
    let zh = response_language
        .trim()
        .to_ascii_lowercase()
        .starts_with("zh");
    if zh {
        "你负责审查并提交最终分类树。你同时拥有 revise_tree_draft（修订草稿）、review_organize_draft（局部审查）和 submit_tree_shape（提交最终树）。处理流程：1) 首轮输出完整 draftTree 合并分类提案；2) 主动扫描整棵树的所有叶子节点，寻找语义重叠的兄弟节点（如 音频+视频→音视频、其他文件+其他待定→其他文件），通过 operations 提出合并；3) 执行 merge_nodes 时，targetNodeId 必须填写为要保留并重命名的现有节点，sourceNodeIds 是要并入并删除的其他节点；4) 后续修改优先使用 operations 字段只提交变更的节点，避免重复输出整棵树；5) review 时可指定 reviewNodeIds 聚焦变更区域；6) 在 reasoning 中自审命名一致性、语义重叠、粒度均衡；7) 确认无误后调用 submit_tree_shape 提交。不要反复纠结格式细节，只关注真正影响分类质量的问题。所有节点名称必须使用纯文本，禁止 emoji 或装饰性 Unicode 符号。".to_string()
    } else {
        "Review and submit the final category tree. You have three tools: revise_tree_draft (revise the draft), review_organize_draft (local review with reviewNodeIds), and submit_tree_shape (submit final tree). Workflow: 1) First turn outputs full draftTree merging classification proposals; 2) Proactively scan ALL leaf nodes for semantic overlap (e.g. audio+video→media, other files+other pending→other files) and propose merges via operations; 3) For merge_nodes, targetNodeId is required and must be the existing node you keep and rename, while sourceNodeIds are the nodes you merge into it and remove; 4) Subsequent revisions should use operations to submit only changed nodes — do not repeat unchanged nodes; 5) When reviewing, specify reviewNodeIds to focus on changed areas; 6) Self-review in reasoning for naming consistency, semantic overlap, and balanced granularity; 7) Call submit_tree_shape when satisfied. Do not obsess over formatting trivia; focus on issues that affect classification quality. All node names must be plain text; no emoji or decorative Unicode symbols.".to_string()
    }
}

pub(super) async fn reconcile_tree_shape(
    text_route: &RouteConfig,
    response_language: &str,
    stop: &AtomicBool,
    initial_tree: &Value,
    classification_results: &[Value],
    diagnostics: Option<&OrganizerDiagnostics>,
) -> Result<ReconcileOrganizeOutput, String> {
    let aliases = ModelIdMap::from_values(&[initial_tree]);
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
    let mut spec = TreeShapeSpec {
        route: text_route,
        response_language,
        initial_tree: compact_initial_tree,
        classification_results: compact_classification_results,
        aliases,
        total_usage: TokenUsage::default(),
        round_trace: Vec::new(),
        available_tool_names: Vec::new(),
        latest_draft: Value::Null,
        step_count: 0,
    };
    AgentTurnLoop::new(&llm, &registry).run(&mut spec).await
}

struct TreeShapeSpec<'a> {
    route: &'a RouteConfig,
    response_language: &'a str,
    initial_tree: Value,
    classification_results: Vec<Value>,
    aliases: ModelIdMap,
    total_usage: TokenUsage,
    round_trace: Vec<String>,
    available_tool_names: Vec<&'static str>,
    latest_draft: Value,
    step_count: usize,
}

impl TreeShapeSpec<'_> {
    fn output(&self, parsed: Option<Value>, error: Option<String>) -> ReconcileOrganizeOutput {
        ReconcileOrganizeOutput {
            parsed: parsed.map(|value| self.aliases.expand_value(&value)),
            usage: self.total_usage.clone(),
            raw_output: self.round_trace.join("\n\n====================\n\n"),
            error,
        }
    }

    fn initial_user_payload(&self) -> Value {
        let mut payload = json!({
            "initialTree": self.initial_tree.clone(),
            "classificationResults": self.classification_results.clone(),
            "inputContract": {
                "pendingOnly": true,
                "nodeIds": "prompt-local aliases; preserve aliases exactly in tool calls",
                "omitted": ["direct assignments", "reasons", "raw model output", "file extraction context"]
            },
            "instruction": "Call revise_tree_draft to merge classification proposals. Proactively scan the full tree for semantically overlapping sibling leaves (e.g. 音频+视频→音视频, 其他文件+其他待定→其他文件) and propose merges — do not limit yourself to nodes referenced by proposals. For merge_nodes, targetNodeId must be the existing node you keep and rename, and sourceNodeIds must list the nodes you merge into it and remove. Then self-review (optionally call review_organize_draft with reviewNodeIds). When satisfied, call submit_tree_shape."
        });
        if !self.latest_draft.is_null() {
            payload["previousDraft"] = self.latest_draft.clone();
        }
        payload
    }
}

impl AgentTurnSpec for TreeShapeSpec<'_> {
    type Output = ReconcileOrganizeOutput;

    fn max_steps(&self) -> usize {
        20
    }

    fn tool_policy<'ctx>(&'ctx self) -> AgentToolPolicy<'ctx> {
        AgentToolPolicy {
            workflow: ToolWorkflow::Organizer,
            stage: "reconcile_tree",
            session: None,
            bootstrap_turn: false,
            response_language: self.response_language,
            web_search_allowed: false,
            search_remaining: 0,
        }
    }

    fn build_initial_messages(&mut self) -> Result<Vec<Value>, String> {
        Ok(Vec::new())
    }

    fn before_step(&mut self, step: usize, messages: &mut Vec<Value>) -> Result<(), String> {
        if step == 0 {
            *messages = vec![
                json!({
                    "role": "system",
                    "content": build_tree_shape_system_prompt(self.response_language),
                }),
                json!({
                    "role": "user",
                    "content": self.initial_user_payload().to_string(),
                }),
            ];
        } else if let Some(system_message) = messages.get_mut(0) {
            system_message["content"] =
                Value::String(build_tree_shape_system_prompt(self.response_language));
        }
        self.available_tool_names = vec![
            "revise_tree_draft",
            "review_organize_draft",
            "submit_tree_shape",
        ];
        self.step_count = step;
        Ok(())
    }

    fn tool_execution_context<'ctx>(&'ctx mut self) -> ToolExecutionContext<'ctx> {
        ToolExecutionContext {
            workflow: ToolWorkflow::Organizer,
            stage: "reconcile_tree",
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
        _completion: &AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<NoToolCallOutcome<Self::Output>, String> {
        Ok(NoToolCallOutcome::Finish(self.output(
            None,
            Some("tree_shape stage did not call a required tool".to_string()),
        )))
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
                let notes = result
                    .result
                    .get("notes")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let has_operations = result
                    .result
                    .get("operations")
                    .and_then(Value::as_array)
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);
                if has_operations {
                    let operations = result.result.get("operations").unwrap();
                    let current_draft_tree = self
                        .latest_draft
                        .get("draftTree")
                        .filter(|v| v.is_object())
                        .unwrap_or(&self.initial_tree);
                    let updated_tree = apply_tree_patches(current_draft_tree, operations)?;
                    let proposal_mappings = result
                        .result
                        .get("proposalMappings")
                        .cloned()
                        .filter(Value::is_array)
                        .unwrap_or_else(|| json!([]));
                    let rejected_ids = result
                        .result
                        .get("rejectedProposalIds")
                        .cloned()
                        .filter(Value::is_array)
                        .unwrap_or_else(|| json!([]));
                    self.latest_draft = json!({
                        "draftTree": updated_tree,
                        "proposalMappings": proposal_mappings,
                        "rejectedProposalIds": rejected_ids,
                    });
                } else {
                    self.latest_draft = result.result.clone();
                }
                let applied = if has_operations {
                    result
                        .result
                        .get("operations")
                        .and_then(Value::as_array)
                        .map(|a| a.len())
                        .unwrap_or(0)
                } else {
                    0
                };
                append_batch_trace_note(
                    &mut self.round_trace,
                    "revise_tree_draft",
                    vec![
                        format!("Notes: {notes}"),
                        format!("Applied operations: {applied}"),
                    ],
                );
                Ok(ToolCallOutcome::Continue {
                    result: json!({
                        "ok": true,
                        "applied": applied,
                        "draftTree": self.latest_draft["draftTree"],
                        "instruction": "Draft updated. Self-review the changes in reasoning. Optionally call review_organize_draft with reviewNodeIds for changed nodes. When satisfied, call submit_tree_shape."
                    }),
                })
            }
            Some(ToolId::ReviewOrganizeDraft) => {
                let needs_revision = result
                    .result
                    .get("needsRevision")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let issue_count = result
                    .result
                    .get("issues")
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                let review_node_ids = result
                    .result
                    .get("reviewNodeIds")
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                append_batch_trace_note(
                    &mut self.round_trace,
                    "review_organize_draft",
                    vec![
                        format!("Issues: {issue_count}"),
                        format!("Review node IDs: {review_node_ids}"),
                        format!("Needs Revision: {needs_revision}"),
                    ],
                );
                Ok(ToolCallOutcome::Continue {
                    result: json!({
                        "ok": true,
                        "issueCount": issue_count,
                        "needsRevision": needs_revision,
                        "draftTree": self.latest_draft["draftTree"],
                        "instruction": if needs_revision {
                            "Issues found. Call revise_tree_draft with operations to fix them, then review again or submit."
                        } else {
                            "No issues requiring revision. Call submit_tree_shape with finalTree."
                        }
                    }),
                })
            }
            Some(ToolId::SubmitTreeShape) => {
                append_batch_trace_note(&mut self.round_trace, "submit_tree_shape", vec![]);
                Ok(ToolCallOutcome::Finish(
                    self.output(Some(result.result), None),
                ))
            }
            _ => Ok(ToolCallOutcome::Finish(self.output(
                None,
                Some(format!("unsupported tree_shape tool: {}", call.name)),
            ))),
        }
    }

    fn on_tool_error(
        &mut self,
        _step: usize,
        call: &ParsedToolCall,
        message: String,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallErrorOutcome<Self::Output>, String> {
        if let Some(message) =
            organizer_submit_tool_retry_message(self.response_language, &call.name, &message)
        {
            return Ok(ToolCallErrorOutcome::Continue { message });
        }
        Ok(ToolCallErrorOutcome::Finish(
            self.output(None, Some(message)),
        ))
    }

    fn on_loop_exhausted(&mut self, _trace: &AgentLoopTrace) -> Result<Self::Output, String> {
        Ok(self.output(None, Some("tree_shape tool loop exhausted".to_string())))
    }
}

fn apply_tree_patches(tree: &Value, operations: &Value) -> Result<Value, String> {
    let ops = operations
        .as_array()
        .ok_or_else(|| "operations must be an array".to_string())?;
    if ops.is_empty() {
        return Ok(tree.clone());
    }
    let mut updated = tree.clone();
    for op in ops {
        let operation = op
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| "operation type missing".to_string())?;
        match operation {
            "rename_node" => {
                let target = op
                    .get("targetNodeId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "rename_node missing targetNodeId".to_string())?;
                let new_name = op
                    .get("suggestedName")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "rename_node missing suggestedName".to_string())?;
                rename_node_in_tree(&mut updated, target, new_name);
            }
            "add_node" => {
                let parent_id = op
                    .get("targetNodeId")
                    .and_then(Value::as_str)
                    .unwrap_or("root");
                let name = op
                    .get("suggestedName")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "add_node missing suggestedName".to_string())?;
                let new_id = format!(
                    "n_{}",
                    Uuid::new_v4()
                        .to_string()
                        .replace('-', "")
                        .chars()
                        .take(8)
                        .collect::<String>()
                );
                add_node_to_tree(&mut updated, parent_id, &new_id, name);
            }
            "merge_nodes" => {
                let target = op
                    .get("targetNodeId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        "merge_nodes missing targetNodeId; set it to the existing node that should be kept and renamed".to_string()
                    })?;
                let sources: Vec<String> = op
                    .get("sourceNodeIds")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                if sources.is_empty() {
                    return Err(
                        "merge_nodes missing sourceNodeIds; list the other nodes that should be merged into targetNodeId"
                            .to_string(),
                    );
                }
                if let Some(new_name) = op.get("suggestedName").and_then(Value::as_str) {
                    rename_node_in_tree(&mut updated, target, new_name);
                }
                for src_id in &sources {
                    if src_id != target {
                        remove_node_from_tree(&mut updated, src_id);
                    }
                }
            }
            "split_node" | "remove_node" => {}
            _ => {}
        }
    }
    Ok(updated)
}

fn find_node_mut<'v>(tree: &'v mut Value, node_id: &str) -> Option<&'v mut Value> {
    if tree
        .get("nodeId")
        .and_then(Value::as_str)
        .map(|id| id == node_id)
        .unwrap_or(false)
    {
        return Some(tree);
    }
    if let Some(children) = tree.get_mut("children").and_then(Value::as_array_mut) {
        for child in children {
            if let Some(found) = find_node_mut(child, node_id) {
                return Some(found);
            }
        }
    }
    None
}

fn rename_node_in_tree(tree: &mut Value, node_id: &str, new_name: &str) {
    if let Some(node) = find_node_mut(tree, node_id) {
        if let Some(obj) = node.as_object_mut() {
            obj.insert("name".to_string(), Value::String(new_name.to_string()));
        }
    }
}

fn add_node_to_tree(tree: &mut Value, parent_id: &str, new_id: &str, name: &str) {
    if let Some(parent) = find_node_mut(tree, parent_id) {
        if let Some(obj) = parent.as_object_mut() {
            let children = obj
                .entry("children".to_string())
                .or_insert_with(|| json!([]));
            if let Some(arr) = children.as_array_mut() {
                arr.push(json!({
                    "nodeId": new_id,
                    "name": name,
                    "children": []
                }));
            }
        }
    }
}

fn remove_node_from_tree(tree: &mut Value, node_id: &str) {
    if let Some(children) = tree.get_mut("children").and_then(Value::as_array_mut) {
        children.retain(|child| {
            child
                .get("nodeId")
                .and_then(Value::as_str)
                .map(|id| id != node_id)
                .unwrap_or(true)
        });
        for child in children {
            remove_node_from_tree(child, node_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 3: Post-placement Adjustment (LLM)
// ---------------------------------------------------------------------------

fn build_adjust_system_prompt(response_language: &str) -> String {
    let zh = response_language
        .trim()
        .to_ascii_lowercase()
        .starts_with("zh");
    if zh {
        "你负责审查已填充 item 的分类树。你同时拥有 revise_tree_draft（修订草稿）、review_organize_draft（局部审查）和 submit_tree_shape（提交最终树）。处理流程：1) 首轮调用 revise_tree_draft 输出完整 draftTree，主动扫描所有兄弟叶子节点，寻找语义重叠（如 音频+视频→音视频、其他文件+其他待定→其他文件）提出合并，对文件类型混杂的节点提出拆分；2) 执行 merge_nodes 时，targetNodeId 必须填写为要保留并重命名的现有节点，sourceNodeIds 是要并入并删除的其他节点；3) 后续修改优先使用 operations 字段只提交变更的节点；4) review 时可指定 reviewNodeIds 聚焦变更区域；5) 在 reasoning 中自审语义重叠、粒度均衡；6) 确认无误后调用 submit_tree_shape 提交。不要仅凭数量做决定；2 个文件的节点如果语义独立且合理就可以保留。所有节点名称必须使用纯文本，禁止 emoji 或装饰性 Unicode 符号。".to_string()
    } else {
        "Review the filled category tree for semantic coherence. Check whether files under each node truly belong together based on names and types. If sibling nodes have overlapping semantics or sample files clearly belong to the same subcategory, propose merging. If a leaf node contains mixed file types or widely divergent names, propose splitting. Do NOT decide solely based on count — a node with 2 files is fine if they are semantically distinct and correctly placed. Call submit_tree_shape with the finalTree and proposalMappings when satisfied. nodeId values use prompt-local short aliases. All node names and newName values must be plain text; no emoji or decorative Unicode symbols.".to_string()
    }
}

fn build_local_refine_system_prompt(response_language: &str) -> String {
    let zh = response_language
        .trim()
        .to_ascii_lowercase()
        .starts_with("zh");
    if zh {
        "你负责对单个超大分类子树做一次局部细分重分类。你只能处理当前子树里的 items，不能把任何 item 移到当前子树之外，也不能修改父级或兄弟分类。可复用当前子树里已有的叶子节点；如果需要新增分类，只能在当前子树下新增后代路径。每个 item 都必须且只能返回一次最终落位。不要输出普通文本结果，只调用 submit_local_reclassification。所有节点名称必须使用纯文本，禁止 emoji 或装饰性 Unicode 符号。".to_string()
    } else {
        "You are refining one oversized category subtree exactly once. You may only reclassify items inside the current subtree. Do not move any item outside the subtree and do not modify parent or sibling categories. You may reuse existing leaf nodes inside the subtree, or create descendant categories under the subtree. Every item must be returned exactly once with a final placement. Do not return plain text; call submit_local_reclassification only. All node names must be plain text with no emoji or decorative Unicode symbols.".to_string()
    }
}

pub(super) async fn refine_local_subtree_once(
    text_route: &RouteConfig,
    response_language: &str,
    stop: &AtomicBool,
    subtree: &Value,
    items: &[Value],
    diagnostics: Option<&OrganizerDiagnostics>,
) -> Result<LocalReclassificationOutput, String> {
    let aliases = ModelIdMap::from_values(&[subtree]);
    let compact_subtree = aliases.compact_value(subtree);
    let payload = json!({
        "subtree": compact_subtree,
        "items": items,
        "rules": {
            "scope": "subtree_only",
            "allowExistingLeafNodeIds": true,
            "allowNewDescendantPaths": true,
            "allowMoveOutsideSubtree": false,
            "requireAssignmentForEveryItem": true,
        }
    });
    let messages = vec![
        json!({
            "role": "system",
            "content": build_local_refine_system_prompt(response_language),
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
        stage: "local_refine_subtree",
    };
    let mut spec = LocalRefineSpec {
        route: text_route,
        response_language,
        initial_messages: messages,
        aliases,
        total_usage: TokenUsage::default(),
        round_trace: Vec::new(),
        available_tool_names: Vec::new(),
    };
    AgentTurnLoop::new(&llm, &tool_registry)
        .run(&mut spec)
        .await
}

struct LocalRefineSpec<'a> {
    route: &'a RouteConfig,
    response_language: &'a str,
    initial_messages: Vec<Value>,
    aliases: ModelIdMap,
    total_usage: TokenUsage,
    round_trace: Vec<String>,
    available_tool_names: Vec<&'static str>,
}

impl LocalRefineSpec<'_> {
    fn output(&self, parsed: Option<Value>, error: Option<String>) -> LocalReclassificationOutput {
        LocalReclassificationOutput {
            parsed: parsed.map(|value| self.aliases.expand_value(&value)),
            usage: self.total_usage.clone(),
            raw_output: self.round_trace.join("\n\n====================\n\n"),
            error,
        }
    }
}

impl AgentTurnSpec for LocalRefineSpec<'_> {
    type Output = LocalReclassificationOutput;

    fn max_steps(&self) -> usize {
        2
    }

    fn tool_policy<'ctx>(&'ctx self) -> AgentToolPolicy<'ctx> {
        AgentToolPolicy {
            workflow: ToolWorkflow::Organizer,
            stage: "local_refine_subtree",
            session: None,
            bootstrap_turn: false,
            response_language: self.response_language,
            web_search_allowed: false,
            search_remaining: 0,
        }
    }

    fn build_initial_messages(&mut self) -> Result<Vec<Value>, String> {
        Ok(std::mem::take(&mut self.initial_messages))
    }

    fn before_step(&mut self, _step: usize, messages: &mut Vec<Value>) -> Result<(), String> {
        self.available_tool_names = vec!["submit_local_reclassification"];
        if let Some(system_message) = messages.get_mut(0) {
            system_message["content"] =
                Value::String(build_local_refine_system_prompt(self.response_language));
        }
        Ok(())
    }

    fn tool_execution_context<'ctx>(&'ctx mut self) -> ToolExecutionContext<'ctx> {
        ToolExecutionContext {
            workflow: ToolWorkflow::Organizer,
            stage: "local_refine_subtree",
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
        _completion: &AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<NoToolCallOutcome<Self::Output>, String> {
        Ok(NoToolCallOutcome::Finish(self.output(
            None,
            Some("local refine response did not call submit_local_reclassification".to_string()),
        )))
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
            Some(ToolId::SubmitLocalReclassification) => {
                append_batch_trace_note(
                    &mut self.round_trace,
                    "submit_local_reclassification",
                    vec![format!(
                        "Assignment Count: {}",
                        result
                            .result
                            .get("assignments")
                            .and_then(Value::as_array)
                            .map(|rows| rows.len())
                            .unwrap_or(0)
                    )],
                );
                Ok(ToolCallOutcome::Finish(
                    self.output(Some(result.result), None),
                ))
            }
            _ => Ok(ToolCallOutcome::Finish(self.output(
                None,
                Some(format!("unsupported local refine tool: {}", call.name)),
            ))),
        }
    }

    fn on_tool_error(
        &mut self,
        _step: usize,
        call: &ParsedToolCall,
        message: String,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallErrorOutcome<Self::Output>, String> {
        if let Some(message) =
            organizer_submit_tool_retry_message(self.response_language, &call.name, &message)
        {
            return Ok(ToolCallErrorOutcome::Continue { message });
        }
        Ok(ToolCallErrorOutcome::Finish(
            self.output(None, Some(message)),
        ))
    }

    fn on_loop_exhausted(&mut self, _trace: &AgentLoopTrace) -> Result<Self::Output, String> {
        Ok(self.output(
            None,
            Some(
                "local refine tool loop exhausted without submit_local_reclassification"
                    .to_string(),
            ),
        ))
    }
}

pub(super) fn build_tree_with_counts(
    tree: &CategoryTreeNode,
    assignments: &HashMap<String, (String, Vec<String>, String)>,
    rows_by_id: &HashMap<String, (usize, Value)>,
) -> Value {
    fn count_items_for_node(
        node: &CategoryTreeNode,
        assignments: &HashMap<String, (String, Vec<String>, String)>,
        rows_by_id: &HashMap<String, (usize, Value)>,
    ) -> Value {
        let mut item_count = 0u64;
        let mut item_summaries: Vec<Value> = Vec::new();
        let leaf_ids = collect_leaf_ids(node);
        for (item_id, (leaf_node_id, _path, reason)) in assignments.iter() {
            if leaf_ids.contains(leaf_node_id.as_str()) {
                item_count += 1;
                if item_summaries.len() < 5 {
                    let name = rows_by_id
                        .get(item_id.as_str())
                        .and_then(|(_, row)| row.get("name").and_then(Value::as_str))
                        .unwrap_or("");
                    item_summaries.push(json!({
                        "itemId": item_id,
                        "name": name,
                        "reason": reason,
                    }));
                }
            }
        }
        json!({
            "nodeId": node.node_id,
            "name": node.name,
            "itemCount": item_count,
            "itemSummaries": item_summaries,
            "children": node.children.iter().map(|child| count_items_for_node(child, assignments, rows_by_id)).collect::<Vec<_>>(),
        })
    }

    fn collect_leaf_ids(node: &CategoryTreeNode) -> HashSet<&str> {
        let mut ids = HashSet::new();
        if node.children.is_empty() {
            ids.insert(node.node_id.as_str());
        }
        for child in &node.children {
            ids.extend(collect_leaf_ids(child));
        }
        ids
    }

    count_items_for_node(tree, assignments, rows_by_id)
}

pub(super) async fn reconcile_tree_adjustment(
    text_route: &RouteConfig,
    response_language: &str,
    stop: &AtomicBool,
    tree_with_counts: &Value,
    diagnostics: Option<&OrganizerDiagnostics>,
) -> Result<ReconcileOrganizeOutput, String> {
    let aliases = ModelIdMap::from_values(&[tree_with_counts]);
    let compact_tree = aliases.compact_value(tree_with_counts);
    let llm = OrganizerBatchLlm {
        route: text_route,
        stop,
        diagnostics,
        stage: "adjust_tree",
    };
    let registry = ToolRegistry::new();
    let mut spec = TreeAdjustmentSpec {
        route: text_route,
        response_language,
        tree_with_counts: compact_tree,
        aliases,
        total_usage: TokenUsage::default(),
        round_trace: Vec::new(),
        available_tool_names: Vec::new(),
        latest_draft: Value::Null,
        step_count: 0,
        submit_reminder_sent: false,
    };
    AgentTurnLoop::new(&llm, &registry).run(&mut spec).await
}

struct TreeAdjustmentSpec<'a> {
    route: &'a RouteConfig,
    response_language: &'a str,
    tree_with_counts: Value,
    aliases: ModelIdMap,
    total_usage: TokenUsage,
    round_trace: Vec<String>,
    available_tool_names: Vec<&'static str>,
    latest_draft: Value,
    step_count: usize,
    submit_reminder_sent: bool,
}

impl TreeAdjustmentSpec<'_> {
    fn output(&self, parsed: Option<Value>, error: Option<String>) -> ReconcileOrganizeOutput {
        ReconcileOrganizeOutput {
            parsed: parsed.map(|value| self.aliases.expand_value(&value)),
            usage: self.total_usage.clone(),
            raw_output: self.round_trace.join("\n\n====================\n\n"),
            error,
        }
    }
}

impl AgentTurnSpec for TreeAdjustmentSpec<'_> {
    type Output = ReconcileOrganizeOutput;

    fn max_steps(&self) -> usize {
        20
    }

    fn tool_policy<'ctx>(&'ctx self) -> AgentToolPolicy<'ctx> {
        AgentToolPolicy {
            workflow: ToolWorkflow::Organizer,
            stage: "reconcile_tree",
            session: None,
            bootstrap_turn: false,
            response_language: self.response_language,
            web_search_allowed: false,
            search_remaining: 0,
        }
    }

    fn build_initial_messages(&mut self) -> Result<Vec<Value>, String> {
        Ok(Vec::new())
    }

    fn before_step(&mut self, step: usize, messages: &mut Vec<Value>) -> Result<(), String> {
        if step == 0 {
            *messages = vec![
                json!({
                    "role": "system",
                    "content": build_adjust_system_prompt(self.response_language),
                }),
                json!({
                    "role": "user",
                    "content": json!({
                        "treeWithCounts": self.tree_with_counts.clone(),
                        "instruction": "First call revise_tree_draft to propose merges for semantically overlapping sibling leaves (scan ALL sibling groups proactively) and splits for mixed nodes. For merge_nodes, targetNodeId must be the existing node you keep and rename, and sourceNodeIds must list the nodes you merge into it and remove. Then self-review (optionally call review_organize_draft with reviewNodeIds). When satisfied, call submit_tree_shape."
                    }).to_string(),
                }),
            ];
        } else if let Some(system_message) = messages.get_mut(0) {
            system_message["content"] =
                Value::String(build_adjust_system_prompt(self.response_language));
        }
        self.available_tool_names = vec![
            "revise_tree_draft",
            "review_organize_draft",
            "submit_tree_shape",
        ];
        self.step_count = step;
        Ok(())
    }

    fn tool_execution_context<'ctx>(&'ctx mut self) -> ToolExecutionContext<'ctx> {
        ToolExecutionContext {
            workflow: ToolWorkflow::Organizer,
            stage: "reconcile_tree",
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
        _completion: &AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<NoToolCallOutcome<Self::Output>, String> {
        let has_retry_budget = self.step_count + 1 < self.max_steps();
        let has_draft_tree = self
            .latest_draft
            .get("draftTree")
            .filter(|value| value.is_object())
            .is_some();
        if !self.submit_reminder_sent && has_retry_budget && has_draft_tree {
            self.submit_reminder_sent = true;
            let reminder = local_text(
                self.response_language,
                "你已经完成修订，但还没有调用最终提交工具。不要输出普通文本；现在直接调用 submit_tree_shape，并提交当前 draftTree 作为 finalTree，附带 proposalMappings。",
                "You finished the revision but did not call the final submit tool. Do not return plain text. Call submit_tree_shape now, submit the current draftTree as finalTree, and include proposalMappings.",
            )
            .to_string();
            append_batch_trace_note(
                &mut self.round_trace,
                "adjust_tree_missing_submit_retry",
                vec![format!("Step: {}", self.step_count + 1)],
            );
            return Ok(NoToolCallOutcome::Continue { message: reminder });
        }
        Ok(NoToolCallOutcome::Finish(self.output(
            None,
            Some("adjust_tree stage did not call a required tool".to_string()),
        )))
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
                let notes = result
                    .result
                    .get("notes")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let has_operations = result
                    .result
                    .get("operations")
                    .and_then(Value::as_array)
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);
                if has_operations {
                    let current_draft_tree = self
                        .latest_draft
                        .get("draftTree")
                        .filter(|v| v.is_object())
                        .unwrap_or(&self.tree_with_counts);
                    let updated_tree = apply_tree_patches(
                        current_draft_tree,
                        result.result.get("operations").unwrap(),
                    )?;
                    let proposal_mappings = result
                        .result
                        .get("proposalMappings")
                        .cloned()
                        .filter(Value::is_array)
                        .unwrap_or_else(|| json!([]));
                    self.latest_draft = json!({
                        "draftTree": updated_tree,
                        "proposalMappings": proposal_mappings,
                    });
                } else {
                    self.latest_draft = result.result.clone();
                }
                let applied = if has_operations {
                    result
                        .result
                        .get("operations")
                        .and_then(Value::as_array)
                        .map(|a| a.len())
                        .unwrap_or(0)
                } else {
                    0
                };
                append_batch_trace_note(
                    &mut self.round_trace,
                    "revise_tree_draft",
                    vec![
                        format!("Notes: {notes}"),
                        format!("Applied operations: {applied}"),
                    ],
                );
                Ok(ToolCallOutcome::Continue {
                    result: json!({
                        "ok": true,
                        "applied": applied,
                        "draftTree": self.latest_draft["draftTree"],
                        "instruction": "Draft updated. Self-review the changes in reasoning. Optionally call review_organize_draft with reviewNodeIds for changed nodes. When satisfied, call submit_tree_shape."
                    }),
                })
            }
            Some(ToolId::ReviewOrganizeDraft) => {
                let needs_revision = result
                    .result
                    .get("needsRevision")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let issue_count = result
                    .result
                    .get("issues")
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                let notes = result
                    .result
                    .get("notes")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                append_batch_trace_note(
                    &mut self.round_trace,
                    "review_organize_draft",
                    vec![
                        format!("Needs revision: {needs_revision}"),
                        format!("Issues: {issue_count}"),
                        format!("Notes: {notes}"),
                    ],
                );
                if needs_revision {
                    Ok(ToolCallOutcome::Continue {
                        result: json!({
                            "ok": true,
                            "needsRevision": true,
                            "issues": result.result.get("issues"),
                            "recommendedOperations": result.result.get("recommendedOperations"),
                            "instruction": "Review found issues. Use revise_tree_draft to fix them before re-submitting."
                        }),
                    })
                } else {
                    Ok(ToolCallOutcome::Continue {
                        result: json!({
                            "ok": true,
                            "needsRevision": false,
                            "notes": notes,
                            "instruction": "Review passed. When ready, call submit_tree_shape to finalize."
                        }),
                    })
                }
            }
            Some(ToolId::SubmitTreeShape) => {
                append_batch_trace_note(&mut self.round_trace, "submit_tree_shape", vec![]);
                Ok(ToolCallOutcome::Finish(
                    self.output(Some(result.result), None),
                ))
            }
            _ => Ok(ToolCallOutcome::Finish(self.output(
                None,
                Some(format!("unsupported adjust tool: {}", call.name)),
            ))),
        }
    }

    fn on_tool_error(
        &mut self,
        _step: usize,
        call: &ParsedToolCall,
        message: String,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallErrorOutcome<Self::Output>, String> {
        if let Some(message) =
            organizer_submit_tool_retry_message(self.response_language, &call.name, &message)
        {
            return Ok(ToolCallErrorOutcome::Continue { message });
        }
        Ok(ToolCallErrorOutcome::Finish(
            self.output(None, Some(message)),
        ))
    }

    fn on_loop_exhausted(&mut self, _trace: &AgentLoopTrace) -> Result<Self::Output, String> {
        Ok(self.output(None, Some("adjust_tree tool loop exhausted".to_string())))
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
    let file_index = build_classification_file_index(batch_rows);
    let existing_tree_value = category_tree_to_value(existing_tree);
    let category_inventory_value = Value::Array(category_inventory.to_vec());
    let aliases = ModelIdMap::from_values(&[&existing_tree_value, &category_inventory_value]);
    let mut payload = json!({
        "existingTree": aliases.compact_value(&existing_tree_value),
        "baseTreeVersion": base_tree_version,
        "categoryInventory": aliases.compact_value(&category_inventory_value),
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
        aliases,
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
    aliases: ModelIdMap,
}

impl OrganizerBatchSpec<'_> {
    fn search_remaining(&self) -> usize {
        ORGANIZER_WEB_SEARCH_BUDGET.saturating_sub(self.shared_search_calls.load(Ordering::Relaxed))
    }

    fn output(&self, parsed: Option<Value>, error: Option<String>) -> ClassifyOrganizeBatchOutput {
        ClassifyOrganizeBatchOutput {
            parsed: parsed.map(|value| self.aliases.expand_value(&value)),
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
        _completion: &AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<NoToolCallOutcome<ClassifyOrganizeBatchOutput>, String> {
        Ok(NoToolCallOutcome::Finish(self.output(
            None,
            Some("classification response did not call a required organizer tool".to_string()),
        )))
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
        call: &ParsedToolCall,
        message: String,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallErrorOutcome<ClassifyOrganizeBatchOutput>, String> {
        if let Some(message) =
            organizer_submit_tool_retry_message(self.response_language, &call.name, &message)
        {
            return Ok(ToolCallErrorOutcome::Continue { message });
        }
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

    fn on_loop_started(&mut self, step_count: usize, messages_count: usize) {
        if let Some(diagnostics) = self.diagnostics {
            diagnostics.record(
                "info",
                "organizer_turn_loop_started",
                "organizer turn loop started",
                json!({
                    "stage": self.stage,
                    "maxSteps": step_count,
                    "initialMessages": messages_count,
                }),
                None,
                None,
            );
        }
    }

    fn on_step_started(&mut self, step: usize, message_count: usize, tool_count: usize) {
        if let Some(diagnostics) = self.diagnostics {
            diagnostics.record(
                "info",
                "organizer_turn_step_started",
                "organizer turn step started",
                json!({
                    "stage": self.stage,
                    "step": step,
                    "messageCount": message_count,
                    "toolCount": tool_count,
                }),
                None,
                None,
            );
        }
    }

    fn on_loop_completed(
        &mut self,
        total_duration: Duration,
        steps: usize,
        _trace: &AgentLoopTrace,
    ) {
        if let Some(diagnostics) = self.diagnostics {
            diagnostics.record(
                "info",
                "organizer_turn_loop_completed",
                "organizer turn loop completed",
                json!({
                    "stage": self.stage,
                    "totalDurationMs": total_duration.as_millis() as u64,
                    "steps": steps,
                }),
                None,
                Some(total_duration),
            );
        }
    }

    fn on_loop_exhausted_early_exit(&mut self, _trace: &AgentLoopTrace) {
        if let Some(diagnostics) = self.diagnostics {
            diagnostics.record(
                "warn",
                "organizer_turn_loop_exhausted",
                "organizer turn loop exhausted without converge",
                json!({ "stage": self.stage }),
                None,
                None,
            );
        }
    }
}

#[cfg(test)]
mod tool_policy_tests {
    use super::*;

    fn tool_completion(id: &str, name: &str, arguments: Value) -> AgentCompletion {
        AgentCompletion {
            raw_body: String::new(),
            assistant_text: String::new(),
            tool_calls: vec![ParsedToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: arguments.clone(),
            }],
            raw_message: json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments.to_string(),
                    }
                }]
            }),
            finish_reason: Some("tool_calls".to_string()),
            usage: TokenUsage::default(),
            route: None,
        }
    }

    struct InitialTreeRepairLlm {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl AgentLlm for InitialTreeRepairLlm {
        async fn complete(
            &self,
            messages: &[Value],
            _tools: &[Value],
            _trace_key: Option<&str>,
        ) -> Result<AgentCompletion, AgentLlmError> {
            let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
            if call_index == 0 {
                return Ok(tool_completion(
                    "call_bad",
                    "submit_initial_tree",
                    json!({ "tree": "bad natural-language tree" }),
                ));
            }

            let saw_validation_feedback = messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("tool")
                    && message
                        .get("content")
                        .and_then(Value::as_str)
                        .map(|content| {
                            content.contains("tree must be an object")
                                && content.contains("submit_initial_tree")
                        })
                        .unwrap_or(false)
            });
            assert!(
                saw_validation_feedback,
                "model should receive tool validation feedback before retrying"
            );

            Ok(tool_completion(
                "call_good",
                "submit_initial_tree",
                json!({
                    "tree": {
                        "nodeId": "root",
                        "name": "",
                        "children": []
                    }
                }),
            ))
        }
    }

    #[tokio::test]
    async fn initial_tree_tool_validation_error_is_returned_for_model_repair() {
        let route = RouteConfig {
            endpoint: "https://example.invalid/v1/chat/completions".to_string(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            api_format: ApiFormat::OpenAi,
            thinking_enabled: false,
            thinking_level: "medium".to_string(),
        };
        let llm = InitialTreeRepairLlm {
            calls: AtomicUsize::new(0),
        };
        let registry = ToolRegistry::new();
        let mut spec = InitialTreeSpec {
            route: &route,
            response_language: "zh",
            initial_messages: vec![
                json!({ "role": "system", "content": build_initial_tree_system_prompt("zh") }),
                json!({ "role": "user", "content": "{\"fileIndex\":[]}" }),
            ],
            total_usage: TokenUsage::default(),
            round_trace: Vec::new(),
            available_tool_names: Vec::new(),
        };

        let output = AgentTurnLoop::new(&llm, &registry)
            .run(&mut spec)
            .await
            .expect("initial tree output");

        assert!(output.error.is_none());
        assert_eq!(
            output
                .parsed
                .as_ref()
                .and_then(|value| { value.pointer("/tree/nodeId").and_then(Value::as_str) }),
            Some("root")
        );
        assert_eq!(llm.calls.load(Ordering::SeqCst), 2);
    }

    struct ClassificationBatchRepairLlm {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl AgentLlm for ClassificationBatchRepairLlm {
        async fn complete(
            &self,
            messages: &[Value],
            _tools: &[Value],
            _trace_key: Option<&str>,
        ) -> Result<AgentCompletion, AgentLlmError> {
            let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
            if call_index == 0 {
                return Ok(tool_completion(
                    "call_bad_batch",
                    "submit_classification_batch",
                    json!({ "baseTreeVersion": 0, "assignments": "bad assignments" }),
                ));
            }

            let saw_validation_feedback = messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("tool")
                    && message
                        .get("content")
                        .and_then(Value::as_str)
                        .map(|content| {
                            content.contains("assignments must be an array")
                                && content.contains("submit_classification_batch")
                        })
                        .unwrap_or(false)
            });
            assert!(
                saw_validation_feedback,
                "classification model should receive tool validation feedback before retrying"
            );

            Ok(tool_completion(
                "call_good_batch",
                "submit_classification_batch",
                json!({ "baseTreeVersion": 0, "assignments": [] }),
            ))
        }
    }

    #[tokio::test]
    async fn classification_tool_validation_error_is_returned_for_model_repair() {
        let route = RouteConfig {
            endpoint: "https://example.invalid/v1/chat/completions".to_string(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            api_format: ApiFormat::OpenAi,
            thinking_enabled: false,
            thinking_level: "medium".to_string(),
        };
        let llm = ClassificationBatchRepairLlm {
            calls: AtomicUsize::new(0),
        };
        let registry = ToolRegistry::new();
        let mut spec = OrganizerBatchSpec {
            route: &route,
            response_language: "zh",
            search_enabled: false,
            search_api_key: "",
            shared_search_calls: Arc::new(AtomicUsize::new(0)),
            shared_search_gate: Arc::new(tokio::sync::Semaphore::new(ORGANIZER_SEARCH_CONCURRENCY)),
            diagnostics: None,
            stage: "classification_batch",
            initial_messages: vec![
                json!({ "role": "system", "content": build_organize_system_prompt("zh", false) }),
                json!({ "role": "user", "content": "{}" }),
            ],
            search_calls: 0,
            budget_exhausted_prompt_sent: false,
            total_usage: TokenUsage::default(),
            round_trace: Vec::new(),
            available_tool_names: Vec::new(),
            aliases: ModelIdMap::default(),
        };

        let output = AgentTurnLoop::new(&llm, &registry)
            .run(&mut spec)
            .await
            .expect("classification output");

        assert!(output.error.is_none());
        assert_eq!(
            output
                .parsed
                .as_ref()
                .and_then(|value| value.get("assignments"))
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
        assert_eq!(llm.calls.load(Ordering::SeqCst), 2);
    }

    struct ReconcileRepairLlm {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl AgentLlm for ReconcileRepairLlm {
        async fn complete(
            &self,
            messages: &[Value],
            _tools: &[Value],
            _trace_key: Option<&str>,
        ) -> Result<AgentCompletion, AgentLlmError> {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(tool_completion(
                    "call_bad_reconcile",
                    "revise_tree_draft",
                    json!({ "draftTree": "bad draft tree" }),
                )),
                1 => {
                    let saw_validation_feedback = messages.iter().any(|message| {
                        message.get("role").and_then(Value::as_str) == Some("tool")
                            && message
                                .get("content")
                                .and_then(Value::as_str)
                                .map(|content| {
                                    content.contains("draftTree must be an object")
                                        && content.contains("revise_tree_draft")
                                })
                                .unwrap_or(false)
                    });
                    assert!(
                        saw_validation_feedback,
                        "reconcile model should receive tool validation feedback before retrying"
                    );
                    Ok(tool_completion(
                        "call_good_reconcile",
                        "revise_tree_draft",
                        json!({
                            "draftTree": {
                                "nodeId": "root",
                                "name": "",
                                "children": []
                            }
                        }),
                    ))
                }
                2 => Ok(tool_completion(
                    "call_review",
                    "review_organize_draft",
                    json!({ "issues": [], "needsRevision": false }),
                )),
                _ => Ok(tool_completion(
                    "call_submit_final",
                    "submit_tree_shape",
                    json!({
                        "finalTree": {
                            "nodeId": "root",
                            "name": "",
                            "children": []
                        },
                        "proposalMappings": [],
                        "finalAssignments": []
                    }),
                )),
            }
        }
    }

    #[tokio::test]
    async fn reconcile_tool_validation_error_is_returned_for_model_repair() {
        let route = RouteConfig {
            endpoint: "https://example.invalid/v1/chat/completions".to_string(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            api_format: ApiFormat::OpenAi,
            thinking_enabled: false,
            thinking_level: "medium".to_string(),
        };
        let llm = ReconcileRepairLlm {
            calls: AtomicUsize::new(0),
        };
        let registry = ToolRegistry::new();
        let mut spec = ReconcileSpec {
            route: &route,
            response_language: "zh",
            stage: "reconcile_tree",
            initial_tree: json!({"nodeId": "root", "name": "", "children": []}),
            classification_results: Vec::new(),
            aliases: ModelIdMap::default(),
            total_usage: TokenUsage::default(),
            round_trace: Vec::new(),
            available_tool_names: Vec::new(),
            latest_draft: Value::Null,
            latest_review: Value::Null,
        };

        let output = AgentTurnLoop::new(&llm, &registry)
            .run(&mut spec)
            .await
            .expect("reconcile output");

        assert!(output.error.is_none());
        assert_eq!(
            output
                .parsed
                .as_ref()
                .and_then(|value| { value.pointer("/finalTree/nodeId").and_then(Value::as_str) }),
            Some("root")
        );
        assert_eq!(llm.calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn organizer_specs_allow_multiple_tool_calls() {
        let route = RouteConfig {
            endpoint: "https://example.invalid/v1/chat/completions".to_string(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            api_format: ApiFormat::OpenAi,
            thinking_enabled: false,
            thinking_level: "medium".to_string(),
        };
        let initial_spec = InitialTreeSpec {
            route: &route,
            response_language: "zh",
            initial_messages: Vec::new(),
            total_usage: TokenUsage::default(),
            round_trace: Vec::new(),
            available_tool_names: Vec::new(),
        };
        let reconcile_spec = ReconcileSpec {
            route: &route,
            response_language: "zh",
            stage: "reconcile_tree",
            initial_tree: json!({"nodeId": "root", "name": "", "children": []}),
            classification_results: Vec::new(),
            aliases: ModelIdMap::default(),
            total_usage: TokenUsage::default(),
            round_trace: Vec::new(),
            available_tool_names: Vec::new(),
            latest_draft: Value::Null,
            latest_review: Value::Null,
        };
        let batch_spec = OrganizerBatchSpec {
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
            aliases: ModelIdMap::default(),
        };

        assert!(initial_spec.allow_multiple_tool_calls());
        assert!(reconcile_spec.allow_multiple_tool_calls());
        assert!(batch_spec.allow_multiple_tool_calls());
    }
}

pub(super) fn emit_organize_summary_ready<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    batch_index: u64,
    row: &Value,
) {
    let payload = build_organize_summary_ready_payload(task_id, batch_index, row);
    if let Err(err) = app.emit("organize_summary_ready", payload) {
        log::warn!("failed to emit organize_summary_ready for task {task_id}: {err}");
    }
}

pub(super) fn build_organize_summary_ready_payload(
    task_id: &str,
    batch_index: u64,
    row: &Value,
) -> Value {
    json!({
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_budget_exhausted_message_is_localized() {
        assert!(search_budget_exhausted_message("zh-CN").contains("联网搜索额度已用完"));
        assert!(search_budget_exhausted_message("en").contains("Web search budget is exhausted"));
    }

    #[test]
    fn adjust_prompt_uses_submit_tree_shape_in_english() {
        let prompt = build_adjust_system_prompt("en-US");
        assert!(prompt.contains("submit_tree_shape"));
        assert!(!prompt.contains("submit_tree_adjustment"));
    }

    #[test]
    fn merge_nodes_requires_target_node_id() {
        let tree = json!({
            "nodeId": "root",
            "name": "root",
            "children": [
                { "nodeId": "n1", "name": "音频", "children": [] },
                { "nodeId": "n2", "name": "视频", "children": [] }
            ]
        });
        let operations = json!([
            {
                "operation": "merge_nodes",
                "sourceNodeIds": ["n1", "n2"],
                "suggestedName": "音视频",
                "reason": "合并语义相近节点"
            }
        ]);

        let err = apply_tree_patches(&tree, &operations).unwrap_err();
        assert!(err.contains("merge_nodes missing targetNodeId"));
        assert!(err.contains("existing node"));
    }
}
