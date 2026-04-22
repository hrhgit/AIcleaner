use super::*;

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
        | ".ini" | ".cfg" | ".conf" | ".js" | ".ts" | ".jsx" | ".tsx" | ".rs" | ".py"
        | ".java" | ".go" | ".c" | ".cpp" | ".h" | ".hpp" | ".css" | ".html" | ".sql"
        | ".sh" | ".bat" | ".ps1" => extract_plain_text_summary(unit),
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
            summary: String::new(),
            source: SUMMARY_SOURCE_FILENAME_ONLY.to_string(),
            degraded: true,
            warnings: extracted.warnings.clone(),
            confidence: None,
        };
    }

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
    if !extracted.excerpt.trim().is_empty() {
        lines.push(format!(
            "excerpt={}",
            trim_to_chars(
                &normalize_multiline_text(&extracted.excerpt, LOCAL_TEXT_EXCERPT_CHARS),
                LOCAL_SUMMARY_EXCERPT_CHARS
            )
        ));
    }

    SummaryBuildResult {
        summary: lines.join("\n"),
        source: SUMMARY_SOURCE_LOCAL_SUMMARY.to_string(),
        degraded: false,
        warnings: extracted.warnings.clone(),
        confidence: None,
    }
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
    let parsed = parse_completion_response(detect_api_format(endpoint), status, raw_body)
        .map_err(|message| ChatCompletionError {
            message: format!(
                "{} | body: {}",
                message,
                summarize_response_body_for_error(raw_body)
            ),
            raw_body: raw_body.to_string(),
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
) -> Result<ChatCompletionOutput, ChatCompletionError> {
    let api_format = detect_api_format(&route.endpoint);
    let url = build_messages_url(&route.endpoint, api_format);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(CHAT_COMPLETION_TIMEOUT_SECS))
        .build()
        .map_err(|e| ChatCompletionError {
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
    let req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&payload);
    let req = apply_auth_headers(req, api_format, &route.api_key);
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
        parse_chat_completion_http_body(&route.endpoint, status, &raw_body)
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
) -> Result<ChatCompletionOutput, ChatCompletionError> {
    chat_completion_with_messages(
        route,
        &[
            json!({ "role": "system", "content": system_prompt }),
            json!({ "role": "user", "content": user_prompt }),
        ],
        None,
        stop,
    )
    .await
}

fn build_summary_agent_system_prompt(response_language: &str) -> String {
    let output_language = localized_language_name(response_language, response_language);
    [
        "You prepare short standardized summaries for a later file classification step."
            .to_string(),
        "Return JSON only.".to_string(),
        "Schema: {\"items\":[{\"itemId\":\"...\",\"summary\":\"...\",\"keywords\":[\"...\"],\"confidence\":\"high|medium|low\",\"warnings\":[\"...\"]}]}".to_string(),
        "Cover every input item exactly once and preserve itemId verbatim.".to_string(),
        "Do not classify, rename, or omit items.".to_string(),
        format!(
            "Write summary and warnings in {output_language} only. Keep each summary under about {SUMMARY_AGENT_SUMMARY_CHARS} characters."
        ),
        "Use the provided local extraction material first. If content is sparse, say so briefly instead of inventing details.".to_string(),
    ]
    .join("\n")
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
        let summary = item
            .get("summary")
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
                summary,
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
            "summary": row.get("summary").and_then(Value::as_str).unwrap_or(""),
            "summarySource": row.get("summarySource").and_then(Value::as_str).unwrap_or(""),
            "summaryKeywords": row.get("summaryKeywords").cloned().unwrap_or(Value::Array(Vec::new())),
            "summaryWarnings": row.get("summaryWarnings").cloned().unwrap_or(Value::Array(Vec::new())),
            "localExtraction": row.get("localExtraction").cloned().unwrap_or(Value::Null),
        })).collect::<Vec<_>>(),
    });

    match chat_completion(text_route, &system_prompt, &payload.to_string(), stop).await {
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
    let output_language = localized_language_name(response_language, response_language);
    let mut lines = vec![
        "You cluster file summaries into a hierarchical category tree.".to_string(),
        "You must use native tool calling. Do not hand-write JSON protocol in assistant text."
            .to_string(),
        "Do not return the final tree as plain assistant text. When you are ready, call submit_organize_result."
            .to_string(),
        "Call at most one tool per reply.".to_string(),
        "Existing nodes already have stable nodeId values; keep nodeId when you reuse, rename, or move existing nodes.".to_string(),
        "When an item summary includes resultKind=whole, treat it as a bundle candidate and prefer assigning the directory as one whole unit unless the summary clearly shows unrelated mixed content.".to_string(),
        "Some items may have an empty summary and only provide file name, path, type, or modality. In that case, classify using those fields instead of assuming missing content.".to_string(),
        "The classification payload only includes normalized summaries and lightweight metadata, not raw extraction text.".to_string(),
        "Prefer using summary first when it exists; otherwise fall back to name, relativePath, itemType, and modality.".to_string(),
        format!("Use {output_language} names and keep labels short."),
        format!("The assignment \"reason\" field must be written in {output_language} only."),
    ];
    if allow_web_search {
        lines.push(
            "If local metadata is insufficient and external context is necessary, call web_search with one concise query."
                .to_string(),
        );
    } else {
        lines.push(
            "web_search is unavailable for the current step. Base your answer on the evidence already collected and call submit_organize_result."
                .to_string(),
        );
    }
    lines.join("\n")
}

pub(super) fn build_classification_batch_items(batch_rows: &[Value]) -> Vec<Value> {
    batch_rows
        .iter()
        .map(|row| {
            let created_age = compute_relative_age(row.get("createdAt").and_then(Value::as_str));
            let modified_age = compute_relative_age(row.get("modifiedAt").and_then(Value::as_str));
            json!({
                "itemId": row.get("itemId").and_then(Value::as_str).unwrap_or(""),
                "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
                "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
                "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
                "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
                "createdAge": created_age,
                "modifiedAge": modified_age,
                "summary": row.get("summary").and_then(Value::as_str).unwrap_or(""),
                "summarySource": row.get("summarySource").and_then(Value::as_str).unwrap_or(""),
                "summaryConfidence": row.get("summaryConfidence").cloned().unwrap_or(Value::Null),
                "summaryKeywords": row
                    .get("summaryKeywords")
                    .cloned()
                    .unwrap_or(Value::Array(Vec::new())),
                "summaryWarnings": row
                    .get("summaryWarnings")
                    .cloned()
                    .unwrap_or(Value::Array(Vec::new())),
            })
        })
        .collect()
}

fn build_organize_tools(allow_web_search: bool) -> Vec<Value> {
    let mut tools = Vec::new();
    if allow_web_search {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web for external context that is missing from the local file summaries.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "reason": { "type": "string" }
                    },
                    "required": ["query"]
                }
            }
        }));
    }
    tools.push(json!({
        "type": "function",
        "function": {
            "name": "submit_organize_result",
            "description": "Submit the final category tree and assignments for this batch.",
            "parameters": {
                "type": "object",
                "properties": {
                    "tree": { "type": "object" },
                    "assignments": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "itemId": { "type": "string" },
                                "leafNodeId": { "type": "string" },
                                "categoryPath": {
                                    "type": "array",
                                    "items": { "type": "string" }
                                },
                                "reason": { "type": "string" }
                            },
                            "required": ["itemId"]
                        }
                    }
                },
                "required": ["tree", "assignments"]
            }
        }
    }));
    tools
}

fn normalize_web_search_query(value: &str) -> String {
    let compact = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    if compact.chars().count() <= 240 {
        compact
    } else {
        compact.chars().take(240).collect()
    }
}

fn parse_web_search_tool_arguments(value: &Value) -> Result<WebSearchRequest, String> {
    let query = value
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| "web_search requires a non-empty query".to_string())?;
    let reason = value
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    Ok(WebSearchRequest {
        query: normalize_web_search_query(query),
        reason,
    })
}

fn parse_submit_organize_result_arguments(value: &Value) -> Result<Value, String> {
    let tree = value
        .get("tree")
        .cloned()
        .ok_or_else(|| "submit_organize_result is missing tree".to_string())?;
    if !tree.is_object() {
        return Err("submit_organize_result tree must be an object".to_string());
    }
    let assignments = value
        .get("assignments")
        .cloned()
        .ok_or_else(|| "submit_organize_result is missing assignments".to_string())?;
    if !assignments.is_array() {
        return Err("submit_organize_result assignments must be an array".to_string());
    }
    Ok(json!({
        "tree": tree,
        "assignments": assignments,
    }))
}

fn serialize_tool_result_content(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn search_budget_exhausted_message(response_language: &str) -> &'static str {
    if response_language
        .trim()
        .to_ascii_lowercase()
        .starts_with("zh")
    {
        "联网搜索额度已用完，不能再调用 web_search。请基于当前已有证据立即调用 submit_organize_result 提交最终聚类结果。"
    } else {
        "Web search budget is exhausted. You can no longer call web_search. Based on the evidence already collected, call submit_organize_result now."
    }
}

pub(super) async fn classify_organize_batch(
    text_route: &RouteConfig,
    response_language: &str,
    stop: &AtomicBool,
    existing_tree: &CategoryTreeNode,
    batch_rows: &[Value],
    max_cluster_depth: Option<u32>,
    reference_structure: Option<&String>,
    use_web_search: bool,
    search_api_key: &str,
) -> Result<ClassifyOrganizeBatchOutput, String> {
    let search_enabled = use_web_search && !search_api_key.trim().is_empty();
    let mut total_usage = TokenUsage::default();
    let mut round_trace = Vec::new();
    let max_steps = if search_enabled {
        ORGANIZER_WEB_SEARCH_BUDGET + 1
    } else {
        1
    };
    let classification_items = build_classification_batch_items(batch_rows);
    let file_index = batch_rows
        .iter()
        .map(|row| {
            let created_age = compute_relative_age(row.get("createdAt").and_then(Value::as_str));
            let modified_age = compute_relative_age(row.get("modifiedAt").and_then(Value::as_str));
            json!({
                "itemId": row.get("itemId").and_then(Value::as_str).unwrap_or(""),
                "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
                "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
                "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
                "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
                "createdAge": created_age,
                "modifiedAge": modified_age,
                "summarySource": row.get("summarySource").and_then(Value::as_str).unwrap_or(""),
            })
        })
        .collect::<Vec<_>>();
    let mut payload = json!({
        "maxClusterDepth": max_cluster_depth,
        "existingTree": category_tree_to_value(existing_tree),
        "fileIndex": file_index,
        "items": classification_items,
        "useWebSearch": use_web_search,
    });
    if let Some(structure) = reference_structure {
        payload["referenceStructure"] = Value::String(structure.clone());
    }

    let mut messages = vec![
        json!({
            "role": "system",
            "content": build_organize_system_prompt(response_language, search_enabled),
        }),
        json!({
            "role": "user",
            "content": payload.to_string(),
        }),
    ];
    let mut search_calls = 0usize;
    let mut budget_exhausted_prompt_sent = false;

    for step_idx in 0..max_steps {
        let allow_web_search = search_enabled && search_calls < ORGANIZER_WEB_SEARCH_BUDGET;
        messages[0]["content"] =
            Value::String(build_organize_system_prompt(response_language, allow_web_search));

        if search_enabled && !allow_web_search && !budget_exhausted_prompt_sent {
            let prompt = search_budget_exhausted_message(response_language).to_string();
            messages.push(json!({
                "role": "user",
                "content": prompt.clone(),
            }));
            append_batch_trace_note(
                &mut round_trace,
                "budget_exhausted",
                vec![
                    format!("Step: {}", step_idx + 1),
                    format!("Prompt: {}", prompt),
                ],
            );
            budget_exhausted_prompt_sent = true;
        }

        let tools = build_organize_tools(allow_web_search);
        let available_tool_names = tools
            .iter()
            .filter_map(|tool| {
                tool.get("function")
                    .and_then(|value| value.get("name"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>();

        let completion =
            match chat_completion_with_messages(text_route, &messages, Some(&tools), stop).await {
                Ok(output) => output,
                Err(err) => {
                    append_batch_trace(
                        &mut round_trace,
                        step_idx + 1,
                        text_route,
                        "http_error",
                        &err.raw_body,
                        None,
                        Some(&err.message),
                        &available_tool_names,
                        ORGANIZER_WEB_SEARCH_BUDGET.saturating_sub(search_calls),
                    );
                    return Ok(ClassifyOrganizeBatchOutput {
                        parsed: None,
                        usage: total_usage,
                        raw_output: round_trace.join("\n\n====================\n\n"),
                        error: Some(err.message),
                    });
                }
            };
        total_usage.prompt = total_usage.prompt.saturating_add(completion.usage.prompt);
        total_usage.completion = total_usage
            .completion
            .saturating_add(completion.usage.completion);
        total_usage.total = total_usage.total.saturating_add(completion.usage.total);
        append_batch_trace(
            &mut round_trace,
            step_idx + 1,
            text_route,
            "http_ok",
            &completion.raw_body,
            Some(&completion.content),
            None,
            &available_tool_names,
            ORGANIZER_WEB_SEARCH_BUDGET.saturating_sub(search_calls),
        );

        if completion.tool_calls.is_empty() {
            return Ok(ClassifyOrganizeBatchOutput {
                parsed: None,
                usage: total_usage,
                raw_output: round_trace.join("\n\n====================\n\n"),
                error: Some(
                    "classification response did not call a required organizer tool".to_string(),
                ),
            });
        }
        if completion.tool_calls.len() > 1 {
            return Ok(ClassifyOrganizeBatchOutput {
                parsed: None,
                usage: total_usage,
                raw_output: round_trace.join("\n\n====================\n\n"),
                error: Some(
                    "classification response used multiple tool calls in one step".to_string(),
                ),
            });
        }

        messages.push(completion.raw_message.clone());
        let Some(tool_call) = completion.tool_calls.into_iter().next() else {
            return Ok(ClassifyOrganizeBatchOutput {
                parsed: None,
                usage: total_usage,
                raw_output: round_trace.join("\n\n====================\n\n"),
                error: Some("classification response lost its organizer tool call".to_string()),
            });
        };

        match tool_call.name.as_str() {
            "web_search" => {
                if !allow_web_search {
                    return Ok(ClassifyOrganizeBatchOutput {
                        parsed: None,
                        usage: total_usage,
                        raw_output: round_trace.join("\n\n====================\n\n"),
                        error: Some(
                            "classification response attempted web_search after budget exhaustion"
                                .to_string(),
                        ),
                    });
                }
                search_calls = search_calls.saturating_add(1);
                let tool_result = match parse_web_search_tool_arguments(&tool_call.arguments) {
                    Ok(request) => match tavily_search(search_api_key, &request).await {
                        Ok(trace) => {
                            append_batch_trace_note(
                                &mut round_trace,
                                "web_search",
                                vec![
                                    format!("Query: {}", trace.query),
                                    format!("Reason: {}", trace.reason),
                                    format!("Result Count: {}", trace.results.len()),
                                    format!(
                                        "Search Budget Remaining: {}",
                                        ORGANIZER_WEB_SEARCH_BUDGET.saturating_sub(search_calls)
                                    ),
                                ],
                            );
                            json!({
                                "ok": true,
                                "query": trace.query,
                                "reason": trace.reason,
                                "answer": trace.answer,
                                "results": trace.results,
                                "formattedContext": format_web_search_context(&trace, response_language),
                            })
                        }
                        Err(err) => {
                            append_batch_trace_note(
                                &mut round_trace,
                                "web_search",
                                vec![
                                    format!("Error: {}", err),
                                    format!(
                                        "Search Budget Remaining: {}",
                                        ORGANIZER_WEB_SEARCH_BUDGET.saturating_sub(search_calls)
                                    ),
                                ],
                            );
                            json!({
                                "ok": false,
                                "error": err,
                            })
                        }
                    },
                    Err(err) => {
                        append_batch_trace_note(
                            &mut round_trace,
                            "web_search",
                            vec![
                                format!("Error: {}", err),
                                format!(
                                    "Search Budget Remaining: {}",
                                    ORGANIZER_WEB_SEARCH_BUDGET.saturating_sub(search_calls)
                                ),
                            ],
                        );
                        json!({
                            "ok": false,
                            "error": err,
                        })
                    }
                };
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_call.id,
                    "content": serialize_tool_result_content(&tool_result),
                }));
            }
            "submit_organize_result" => {
                let parsed = match parse_submit_organize_result_arguments(&tool_call.arguments) {
                    Ok(value) => value,
                    Err(err) => {
                        return Ok(ClassifyOrganizeBatchOutput {
                            parsed: None,
                            usage: total_usage,
                            raw_output: round_trace.join("\n\n====================\n\n"),
                            error: Some(err),
                        });
                    }
                };
                append_batch_trace_note(
                    &mut round_trace,
                    "submit_organize_result",
                    vec![
                        format!(
                            "Assignment Count: {}",
                            parsed
                                .get("assignments")
                                .and_then(Value::as_array)
                                .map(|rows| rows.len())
                                .unwrap_or(0)
                        ),
                        format!("Search Calls Used: {}", search_calls),
                    ],
                );
                return Ok(ClassifyOrganizeBatchOutput {
                    parsed: Some(parsed),
                    usage: total_usage,
                    raw_output: round_trace.join("\n\n====================\n\n"),
                    error: None,
                });
            }
            other => {
                return Ok(ClassifyOrganizeBatchOutput {
                    parsed: None,
                    usage: total_usage,
                    raw_output: round_trace.join("\n\n====================\n\n"),
                    error: Some(format!("unsupported organizer tool: {other}")),
                });
            }
        }
    }

    Ok(ClassifyOrganizeBatchOutput {
        parsed: None,
        usage: total_usage,
        raw_output: round_trace.join("\n\n====================\n\n"),
        error: Some("classification tool loop exhausted without submit_organize_result".to_string()),
    })
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
        "summaryMode": row
            .get("summaryMode")
            .cloned()
            .unwrap_or(Value::String(SUMMARY_MODE_FILENAME_ONLY.to_string())),
        "summary": row.get("summary").and_then(Value::as_str).unwrap_or(""),
        "summarySource": row
            .get("summarySource")
            .and_then(Value::as_str)
            .unwrap_or(SUMMARY_SOURCE_FILENAME_ONLY),
        "summaryConfidence": row.get("summaryConfidence").cloned().unwrap_or(Value::Null),
        "summaryKeywords": row
            .get("summaryKeywords")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
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
    fn organize_tools_only_include_submit_when_search_disabled() {
        let tools = build_organize_tools(false);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "submit_organize_result");
    }

    #[test]
    fn parse_web_search_tool_arguments_requires_query() {
        let err = parse_web_search_tool_arguments(&json!({}))
            .expect_err("missing query should fail");
        assert!(err.contains("non-empty query"));

        let parsed = parse_web_search_tool_arguments(&json!({
            "query": "  unity   cache   folder  ",
            "reason": "Need vendor context"
        }))
        .expect("query");
        assert_eq!(parsed.query, "unity cache folder");
        assert_eq!(parsed.reason, "Need vendor context");
    }

    #[test]
    fn parse_submit_organize_result_arguments_validates_shape() {
        let parsed = parse_submit_organize_result_arguments(&json!({
            "tree": { "nodeId": "root", "name": "", "children": [] },
            "assignments": []
        }))
        .expect("valid submit payload");
        assert!(parsed["tree"].is_object());
        assert!(parsed["assignments"].is_array());

        let err = parse_submit_organize_result_arguments(&json!({
            "tree": [],
            "assignments": {}
        }))
        .expect_err("invalid shape");
        assert!(err.contains("tree must be an object"));
    }

    #[test]
    fn search_budget_exhausted_message_is_localized() {
        assert!(search_budget_exhausted_message("zh-CN").contains("联网搜索额度已用完"));
        assert!(search_budget_exhausted_message("en").contains("Web search budget is exhausted"));
    }
}
