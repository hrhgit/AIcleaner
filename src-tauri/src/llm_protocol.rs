use crate::backend::TokenUsage;
use reqwest::{Client, RequestBuilder, StatusCode};
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::time::Duration;
use uuid::Uuid;

const ANTHROPIC_VERSION: &str = "2023-06-01";
const LLM_ACCEPT_ENCODING: &str = "identity";
pub const DEFAULT_MAX_TOKENS: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiFormat {
    OpenAi,
    Anthropic,
}

impl ApiFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai" => Some(Self::OpenAi),
            "anthropic" => Some(Self::Anthropic),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ThinkingConfig<'a> {
    pub enabled: bool,
    pub level: &'a str,
}

#[derive(Clone, Debug)]
pub struct ParsedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

const TOOL_ARGUMENTS_RAW_FIELD: &str = "__tool_arguments_raw";
const TOOL_ARGUMENTS_PARSE_ERROR_FIELD: &str = "__tool_arguments_parse_error";

#[derive(Clone, Debug)]
pub struct ParsedCompletion {
    pub assistant_text: String,
    pub raw_message: Value,
    pub tool_calls: Vec<ParsedToolCall>,
    pub finish_reason: Option<String>,
    pub usage: TokenUsage,
}

pub fn detect_api_format(endpoint: &str) -> ApiFormat {
    let normalized = endpoint.trim().trim_end_matches('/').to_ascii_lowercase();
    if normalized.contains("/anthropic") {
        ApiFormat::Anthropic
    } else {
        ApiFormat::OpenAi
    }
}

pub fn build_messages_url(endpoint: &str, format: ApiFormat) -> String {
    let trimmed = endpoint.trim().trim_end_matches('/');
    match format {
        ApiFormat::OpenAi => {
            if trimmed.ends_with("/chat/completions") {
                trimmed.to_string()
            } else {
                format!("{trimmed}/chat/completions")
            }
        }
        ApiFormat::Anthropic => {
            if trimmed.ends_with("/messages") {
                trimmed.to_string()
            } else if trimmed.ends_with("/v1") {
                format!("{trimmed}/messages")
            } else {
                format!("{trimmed}/v1/messages")
            }
        }
    }
}

pub fn apply_auth_headers(req: RequestBuilder, format: ApiFormat, api_key: &str) -> RequestBuilder {
    if api_key.trim().is_empty() {
        return req;
    }
    match format {
        ApiFormat::OpenAi => req
            .header("Authorization", format!("Bearer {}", api_key))
            .header("x-api-key", api_key)
            .header("api-key", api_key),
        ApiFormat::Anthropic => req
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_VERSION),
    }
}

pub fn build_llm_http_client(timeout_secs: u64) -> Result<Client, reqwest::Error> {
    Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .http1_only()
        .build()
}

pub fn apply_llm_transport_headers(req: RequestBuilder) -> RequestBuilder {
    req.header(reqwest::header::ACCEPT_ENCODING, LLM_ACCEPT_ENCODING)
}

pub fn build_completion_payload(
    format: ApiFormat,
    model: &str,
    messages: &[Value],
    tools: Option<&[Value]>,
    temperature: f64,
    max_tokens: u64,
    thinking: ThinkingConfig<'_>,
) -> Result<Value, String> {
    match format {
        ApiFormat::OpenAi => Ok(build_openai_payload(
            model,
            messages,
            tools,
            temperature,
            thinking,
        )),
        ApiFormat::Anthropic => build_anthropic_payload(
            model,
            messages,
            tools,
            temperature,
            max_tokens.max(1),
            thinking,
        ),
    }
}

pub fn parse_completion_response(
    format: ApiFormat,
    status: StatusCode,
    raw_body: &str,
) -> Result<ParsedCompletion, String> {
    match format {
        ApiFormat::OpenAi => parse_openai_completion(status, raw_body),
        ApiFormat::Anthropic => parse_anthropic_completion(status, raw_body),
    }
}

pub fn content_value_to_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(content_part_to_text)
            .collect::<Vec<_>>()
            .join(""),
        other => other.to_string(),
    }
}

fn content_part_to_text(part: &Value) -> Option<String> {
    if let Some(text) = part.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    match part.get("type").and_then(Value::as_str) {
        Some("text") => part.get("text").and_then(Value::as_str).map(str::to_string),
        Some("output_text") => part
            .get("text")
            .or_else(|| part.get("content"))
            .and_then(Value::as_str)
            .map(str::to_string),
        Some("thinking") => part
            .get("thinking")
            .or_else(|| part.get("text"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

fn build_openai_payload(
    model: &str,
    messages: &[Value],
    tools: Option<&[Value]>,
    temperature: f64,
    thinking: ThinkingConfig<'_>,
) -> Value {
    let mut payload = json!({
        "model": model,
        "messages": messages.iter().map(normalize_message_for_openai).collect::<Vec<_>>(),
        "temperature": temperature
    });
    if let Some(tools) = tools.filter(|rows| !rows.is_empty()) {
        payload["tools"] = Value::Array(tools.to_vec());
        payload["tool_choice"] = Value::String("auto".to_string());
    }
    if thinking.enabled {
        payload["reasoning_effort"] =
            Value::String(normalize_reasoning_level(thinking.level).to_string());
        payload["thinking"] = json!({ "type": "enabled" });
    }
    payload
}

fn normalize_message_for_openai(message: &Value) -> Value {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user")
        .trim();
    let mut out = json!({
        "role": role,
        "content": content_value_to_text(message.get("content").unwrap_or(&Value::Null)),
    });
    if role == "assistant" {
        let tool_calls = extract_tool_calls_from_message(message)
            .into_iter()
            .map(|call| {
                json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": call.arguments.to_string(),
                    }
                })
            })
            .collect::<Vec<_>>();
        if !tool_calls.is_empty() {
            out["tool_calls"] = Value::Array(tool_calls);
        }
        // DeepSeek V4: preserve reasoning_content for passback
        if let Some(rc) = message.get("reasoning_content").and_then(Value::as_str) {
            if !rc.trim().is_empty() {
                out["reasoning_content"] = Value::String(rc.to_string());
            }
        }
    } else if role == "tool" {
        out["tool_call_id"] = message
            .get("tool_call_id")
            .cloned()
            .unwrap_or(Value::String(String::new()));
    }
    out
}

fn build_anthropic_payload(
    model: &str,
    messages: &[Value],
    tools: Option<&[Value]>,
    temperature: f64,
    max_tokens: u64,
    thinking: ThinkingConfig<'_>,
) -> Result<Value, String> {
    let (system, anthropic_messages) = convert_messages_to_anthropic(messages)?;
    let mut payload = json!({
        "model": model,
        "messages": anthropic_messages,
        "temperature": temperature,
        "max_tokens": max_tokens,
    });
    if !system.trim().is_empty() {
        payload["system"] = Value::String(system);
    }
    if let Some(tools) = tools.filter(|rows| !rows.is_empty()) {
        payload["tools"] = Value::Array(
            tools
                .iter()
                .filter_map(openai_tool_to_anthropic)
                .collect::<Vec<_>>(),
        );
        payload["tool_choice"] = json!({ "type": "auto" });
    }
    if thinking.enabled {
        payload["thinking"] = json!({
            "type": "enabled",
            "budget_tokens": anthropic_thinking_budget(thinking.level, max_tokens),
        });
    }
    Ok(payload)
}

fn normalize_reasoning_level(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => "low",
        "high" => "high",
        _ => "medium",
    }
}

fn anthropic_thinking_budget(level: &str, max_tokens: u64) -> u64 {
    let budget = match normalize_reasoning_level(level) {
        "low" => 1024,
        "high" => 3072,
        _ => 2048,
    };
    budget.min(max_tokens.max(1))
}

fn convert_messages_to_anthropic(messages: &[Value]) -> Result<(String, Vec<Value>), String> {
    let mut system_parts = Vec::new();
    let mut converted = Vec::new();
    let mut pending_tool_results = Vec::new();

    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        match role {
            "system" => {
                let text = content_value_to_text(message.get("content").unwrap_or(&Value::Null));
                if !text.trim().is_empty() {
                    system_parts.push(text);
                }
            }
            "tool" => {
                let tool_use_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "anthropic tool_result is missing tool_call_id".to_string())?;
                let content = content_value_to_text(message.get("content").unwrap_or(&Value::Null));
                pending_tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content,
                }));
            }
            "user" | "assistant" => {
                if !pending_tool_results.is_empty() {
                    converted.push(json!({
                        "role": "user",
                        "content": Value::Array(std::mem::take(&mut pending_tool_results)),
                    }));
                }
                let content = anthropic_blocks_for_message(role, message);
                if !content.is_empty() {
                    converted.push(json!({
                        "role": role,
                        "content": content,
                    }));
                }
            }
            _ => {}
        }
    }

    if !pending_tool_results.is_empty() {
        converted.push(json!({
            "role": "user",
            "content": Value::Array(pending_tool_results),
        }));
    }

    Ok((system_parts.join("\n\n"), converted))
}

fn anthropic_blocks_for_message(role: &str, message: &Value) -> Vec<Value> {
    if role == "assistant" {
        let existing = normalize_existing_anthropic_blocks(message.get("content"));
        if existing.iter().any(|block| {
            matches!(
                block.get("type").and_then(Value::as_str),
                Some("tool_use" | "thinking")
            )
        }) {
            return existing;
        }

        let mut blocks = Vec::new();
        let text = content_value_to_text(message.get("content").unwrap_or(&Value::Null));
        if !text.trim().is_empty() {
            blocks.push(json!({ "type": "text", "text": text }));
        }
        blocks.extend(
            extract_tool_calls_from_message(message)
                .into_iter()
                .map(|call| {
                    json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments,
                    })
                }),
        );
        return blocks;
    }

    let existing = normalize_existing_anthropic_blocks(message.get("content"));
    if !existing.is_empty() {
        return existing;
    }

    let text = content_value_to_text(message.get("content").unwrap_or(&Value::Null));
    if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![json!({ "type": "text", "text": text })]
    }
}

fn normalize_existing_anthropic_blocks(value: Option<&Value>) -> Vec<Value> {
    value
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| {
                    if part.get("type").is_some() {
                        Some(part.clone())
                    } else if let Some(text) =
                        part.as_str().map(str::trim).filter(|v| !v.is_empty())
                    {
                        Some(json!({ "type": "text", "text": text }))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn openai_tool_to_anthropic(tool: &Value) -> Option<Value> {
    let function = tool.get("function")?;
    let name = function.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    Some(json!({
        "name": name,
        "description": function
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or(""),
        "input_schema": function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
    }))
}

fn parse_openai_completion(status: StatusCode, raw_body: &str) -> Result<ParsedCompletion, String> {
    let body: Value =
        serde_json::from_str(raw_body).map_err(|e| format!("error decoding response body: {e}"))?;
    if !status.is_success() {
        let api_message = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("request failed");
        return Err(format!("{api_message} (HTTP {})", status.as_u16()));
    }

    let message = body
        .pointer("/choices/0/message")
        .cloned()
        .ok_or_else(|| "response missing choices[0].message".to_string())?;
    let assistant_text = content_value_to_text(message.get("content").unwrap_or(&Value::Null))
        .trim()
        .to_string();
    let tool_calls = extract_tool_calls_from_message(&message);
    if assistant_text.is_empty() && tool_calls.is_empty() {
        return Err("response missing assistant text and tool calls".to_string());
    }

    Ok(ParsedCompletion {
        assistant_text,
        raw_message: message,
        tool_calls,
        finish_reason: body
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
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

fn parse_anthropic_completion(
    status: StatusCode,
    raw_body: &str,
) -> Result<ParsedCompletion, String> {
    let body: Value =
        serde_json::from_str(raw_body).map_err(|e| format!("error decoding response body: {e}"))?;
    if !status.is_success() {
        let api_message = body
            .pointer("/error/message")
            .or_else(|| body.pointer("/message"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("request failed");
        return Err(format!("{api_message} (HTTP {})", status.as_u16()));
    }

    let content = body
        .get("content")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let assistant_text = content_value_to_text(&content).trim().to_string();
    let tool_calls = extract_tool_calls_from_anthropic_content(&content);
    if assistant_text.is_empty() && tool_calls.is_empty() {
        return Err("response missing assistant text and tool calls".to_string());
    }
    let input_tokens = body
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = body
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Ok(ParsedCompletion {
        assistant_text,
        raw_message: json!({
            "role": "assistant",
            "content": content,
            "tool_calls": tool_calls.iter().map(|call| {
                json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": call.arguments.to_string(),
                    }
                })
            }).collect::<Vec<_>>(),
        }),
        tool_calls,
        finish_reason: body
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        usage: TokenUsage {
            prompt: input_tokens,
            completion: output_tokens,
            total: input_tokens + output_tokens,
        },
    })
}

fn extract_tool_calls_from_message(message: &Value) -> Vec<ParsedToolCall> {
    if let Some(rows) = message.get("tool_calls").and_then(Value::as_array) {
        let mut tool_calls = Vec::new();
        for row in rows {
            let function = row.get("function").cloned().unwrap_or(Value::Null);
            let Some(name) = function
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let Ok(arguments) = parse_arguments_value(function.get("arguments")) else {
                continue;
            };
            tool_calls.push(ParsedToolCall {
                id: row
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("call_{}", Uuid::new_v4())),
                name: name.to_string(),
                arguments,
            });
        }
        return tool_calls;
    }

    if let Some(function_call) = message.get("function_call") {
        let Some(name) = function_call
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Vec::new();
        };
        let Ok(arguments) = parse_arguments_value(function_call.get("arguments")) else {
            return Vec::new();
        };
        return vec![ParsedToolCall {
            id: format!("call_{}", Uuid::new_v4()),
            name: name.to_string(),
            arguments,
        }];
    }

    Vec::new()
}

fn extract_tool_calls_from_anthropic_content(content: &Value) -> Vec<ParsedToolCall> {
    content
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| {
                    if part.get("type").and_then(Value::as_str) != Some("tool_use") {
                        return None;
                    }
                    let name = part
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())?;
                    Some(ParsedToolCall {
                        id: part
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("call_{}", Uuid::new_v4())),
                        name: name.to_string(),
                        arguments: part.get("input").cloned().unwrap_or_else(|| json!({})),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn parse_arguments_value(value: Option<&Value>) -> Result<Value, String> {
    match value {
        None | Some(Value::Null) => Ok(json!({})),
        Some(Value::Object(map)) => Ok(Value::Object(map.clone())),
        Some(Value::String(raw)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(json!({}))
            } else {
                parse_tool_arguments_string(trimmed)
            }
        }
        Some(other) => Ok(other.clone()),
    }
}

fn parse_tool_arguments_string(raw: &str) -> Result<Value, String> {
    if let Some(value) = try_parse_tool_arguments_string(raw) {
        return Ok(value);
    }
    let original_err = serde_json::from_str::<Value>(raw)
        .err()
        .map(|err| err.to_string())
        .unwrap_or_else(|| "unknown parse failure".to_string());
    Ok(tool_argument_parse_fallback(
        raw,
        &format!("tool call arguments are not valid JSON: {original_err}"),
    ))
}

fn tool_argument_parse_fallback(raw: &str, parse_error: &str) -> Value {
    json!({
        TOOL_ARGUMENTS_RAW_FIELD: raw,
        TOOL_ARGUMENTS_PARSE_ERROR_FIELD: parse_error,
    })
}

pub(crate) fn tool_arguments_parse_error(value: &Value) -> Option<&str> {
    value
        .get(TOOL_ARGUMENTS_PARSE_ERROR_FIELD)
        .and_then(Value::as_str)
}

fn try_parse_tool_arguments_string(raw: &str) -> Option<Value> {
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();

    queue.push_back(raw.trim().to_string());
    while let Some(candidate) = queue.pop_front() {
        let trimmed = candidate.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }
        if let Some(value) = try_parse_json_value(trimmed) {
            return Some(value);
        }
        enqueue_repaired_variants(trimmed, &mut queue);
    }

    None
}

fn try_parse_json_value(raw: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    if let Value::String(inner) = &value {
        let trimmed = inner.trim();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            return serde_json::from_str::<Value>(trimmed).ok().or(Some(value));
        }
    }
    Some(value)
}

fn enqueue_repaired_variants(raw: &str, queue: &mut VecDeque<String>) {
    if let Some(normalized) = normalize_jsonish_quotes(raw) {
        queue.push_back(normalized);
    }
    if let Some(unwrapped) = strip_markdown_code_fence(raw) {
        queue.push_back(unwrapped);
    }
    if let Some(repaired) = escape_likely_unescaped_string_quotes(raw) {
        queue.push_back(repaired);
    }
    if let Some(repaired) = remove_premature_root_closes(raw) {
        queue.push_back(repaired);
    }
    if let Some(repaired) = remove_trailing_commas(raw) {
        queue.push_back(repaired);
    }
    if let Some(repaired) = close_unbalanced_json_delimiters(raw) {
        queue.push_back(repaired);
    }
    if let Some(extracted) = extract_first_json_payload(raw) {
        queue.push_back(extracted);
    }
}

fn normalize_jsonish_quotes(raw: &str) -> Option<String> {
    let normalized = raw
        .replace(['“', '”', '„', '‟', '〝', '〞', '＂'], "\"")
        .replace(['‘', '’'], "'");
    (normalized != raw).then_some(normalized)
}

fn strip_markdown_code_fence(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let start = trimmed.find("```")?;
    let rest = &trimmed[start + 3..];
    let newline_idx = rest.find('\n')?;
    let body = &rest[newline_idx + 1..];
    let end = body.find("```")?;
    let extracted = body[..end].trim();
    (!extracted.is_empty()).then(|| extracted.to_string())
}

fn extract_first_json_payload(raw: &str) -> Option<String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut start = None;
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in chars.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if *ch == '\\' && in_string {
            escaped = true;
            continue;
        }
        if *ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        if start.is_none() {
            match *ch {
                '{' => {
                    start = Some(idx);
                    stack.push('}');
                }
                '[' => {
                    start = Some(idx);
                    stack.push(']');
                }
                _ => {}
            }
            continue;
        }
        match *ch {
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                let expected = stack.pop()?;
                if *ch != expected {
                    return None;
                }
                if stack.is_empty() {
                    let begin = start?;
                    let extracted: String = chars[begin..=idx].iter().collect();
                    let extracted = extracted.trim().to_string();
                    return (!extracted.is_empty() && extracted != raw.trim()).then_some(extracted);
                }
            }
            _ => {}
        }
    }
    None
}

/// Some models produce malformed JSON like:
///   {"draftTree": {...}, "notes": "..."}, "proposalMappings": [...]}
/// where an extra `}` closes the root object prematurely, pushing later
/// fields outside. This function detects that pattern by tracking brace
/// depth and removing `}` characters that would close the root object
/// while non-whitespace content still follows.
fn remove_premature_root_closes(raw: &str) -> Option<String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut skip_positions = Vec::new();

    for (i, ch) in chars.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if *ch == '\\' && in_string {
            escaped = true;
            continue;
        }
        if *ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match *ch {
            '{' | '[' => depth += 1,
            '}' | ']' => {
                if depth == 1 {
                    // This brace would close the root object.
                    // Check if non-whitespace content follows.
                    let rest: String = chars[i + 1..].iter().collect();
                    if rest.trim_start().starts_with(',') {
                        // Premature close: remove this brace
                        skip_positions.push(i);
                        // Don't decrement depth — stay at 1
                    } else {
                        depth -= 1;
                    }
                } else if depth > 1 {
                    depth -= 1;
                }
            }
            _ => {}
        }
    }

    if skip_positions.is_empty() {
        return None;
    }
    let skip_set: HashSet<usize> = skip_positions.into_iter().collect();
    let fixed: String = chars
        .iter()
        .enumerate()
        .filter(|(i, _)| !skip_set.contains(i))
        .map(|(_, ch)| *ch)
        .collect();
    Some(fixed)
}

fn remove_trailing_commas(raw: &str) -> Option<String> {
    let chars = raw.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(raw.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut changed = false;

    for (idx, ch) in chars.iter().enumerate() {
        if escaped {
            out.push(*ch);
            escaped = false;
            continue;
        }
        if *ch == '\\' && in_string {
            out.push(*ch);
            escaped = true;
            continue;
        }
        if *ch == '"' {
            out.push(*ch);
            in_string = !in_string;
            continue;
        }
        if !in_string && *ch == ',' {
            if chars[idx + 1..]
                .iter()
                .find(|ch| !ch.is_whitespace())
                .is_some_and(|ch| matches!(ch, '}' | ']'))
            {
                changed = true;
                continue;
            }
        }
        out.push(*ch);
    }

    changed.then_some(out)
}

fn close_unbalanced_json_delimiters(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }

    let mut expected_closers = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for ch in trimmed.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && in_string {
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '{' => expected_closers.push('}'),
            '[' => expected_closers.push(']'),
            '}' | ']' => {
                let expected = expected_closers.pop()?;
                if ch != expected {
                    return None;
                }
            }
            _ => {}
        }
    }

    if in_string || expected_closers.is_empty() {
        return None;
    }

    let mut repaired = trimmed.to_string();
    for ch in expected_closers.iter().rev() {
        repaired.push(*ch);
    }
    Some(repaired)
}

fn escape_likely_unescaped_string_quotes(raw: &str) -> Option<String> {
    let chars = raw.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(raw.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut changed = false;

    for (idx, ch) in chars.iter().enumerate() {
        if escaped {
            out.push(*ch);
            escaped = false;
            continue;
        }

        if !in_string {
            out.push(*ch);
            if *ch == '"' {
                in_string = true;
            }
            continue;
        }

        match *ch {
            '\\' => {
                out.push('\\');
                escaped = true;
            }
            '"' => {
                if looks_like_json_string_end(&chars, idx) {
                    out.push('"');
                    in_string = false;
                } else {
                    out.push('\\');
                    out.push('"');
                    changed = true;
                }
            }
            _ => out.push(*ch),
        }
    }

    changed.then_some(out)
}

fn looks_like_json_string_end(chars: &[char], quote_idx: usize) -> bool {
    chars[quote_idx + 1..]
        .iter()
        .find(|ch| !ch.is_whitespace())
        .map(|ch| matches!(ch, ':' | ',' | '}' | ']'))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_anthropic_endpoint() {
        assert_eq!(
            detect_api_format("https://api.minimax.io/anthropic/v1"),
            ApiFormat::Anthropic
        );
        assert_eq!(
            detect_api_format("https://api.openai.com/v1"),
            ApiFormat::OpenAi
        );
    }

    #[test]
    fn builds_anthropic_payload_from_tool_history() {
        let payload = build_completion_payload(
            ApiFormat::Anthropic,
            "MiniMax-M2.7",
            &[
                json!({ "role": "system", "content": "system prompt" }),
                json!({ "role": "user", "content": "find temp files" }),
                json!({
                    "role": "assistant",
                    "content": [
                        { "type": "thinking", "thinking": "Analyzing" },
                        { "type": "tool_use", "id": "call_1", "name": "find_files", "input": { "nameQuery": "temp" } }
                    ],
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "find_files",
                            "arguments": "{\"nameQuery\":\"temp\"}"
                        }
                    }]
                }),
                json!({
                    "role": "tool",
                    "tool_call_id": "call_1",
                    "content": "{\"items\":[]}"
                }),
            ],
            Some(&[json!({
                "type": "function",
                "function": {
                    "name": "find_files",
                    "description": "Find files",
                    "parameters": { "type": "object", "properties": {} }
                }
            })]),
            0.0,
            1024,
            ThinkingConfig {
                enabled: true,
                level: "medium",
            },
        )
        .expect("payload");

        assert_eq!(
            payload["system"],
            Value::String("system prompt".to_string())
        );
        assert_eq!(
            payload["messages"][1]["role"],
            Value::String("assistant".to_string())
        );
        assert_eq!(
            payload["messages"][1]["content"][0]["type"],
            Value::String("thinking".to_string())
        );
        assert_eq!(
            payload["messages"][2]["content"][0]["type"],
            Value::String("tool_result".to_string())
        );
        assert_eq!(
            payload["tools"][0]["name"],
            Value::String("find_files".to_string())
        );
    }

    #[test]
    fn parses_anthropic_tool_use_response() {
        let raw = json!({
            "role": "assistant",
            "content": [
                { "type": "thinking", "thinking": "Need to inspect files" },
                { "type": "text", "text": "I need more context." },
                { "type": "tool_use", "id": "toolu_1", "name": "find_files", "input": { "nameQuery": "cache" } }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        })
        .to_string();

        let parsed =
            parse_completion_response(ApiFormat::Anthropic, StatusCode::OK, &raw).expect("parsed");
        assert_eq!(
            parsed.assistant_text,
            "Need to inspect filesI need more context."
        );
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "find_files");
        assert_eq!(parsed.usage.total, 15);
    }

    #[test]
    fn parses_openai_tool_call_with_unescaped_quotes_in_arguments_string() {
        let raw = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "submit_classification_batch",
                            "arguments": "{\"baseTreeVersion\":1,\"assignments\":[{\"itemId\":\"batch12_1\",\"leafNodeId\":\"node_1\",\"reason\":\".exe文件名含\"Installer\"，表明是安装包\"}]}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        })
        .to_string();

        let parsed =
            parse_completion_response(ApiFormat::OpenAi, StatusCode::OK, &raw).expect("parsed");
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "submit_classification_batch");
        assert_eq!(
            parsed.tool_calls[0].arguments["assignments"][0]["reason"],
            ".exe文件名含\"Installer\"，表明是安装包"
        );
        assert_eq!(parsed.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn repairs_premature_root_close_in_tool_arguments() {
        // Model produced: {"draftTree": {...}, "notes": "..."}, "proposalMappings": [...]}
        // where extra } after notes closes the root prematurely.
        let raw = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "revise_tree_draft",
                            "arguments": "{\"draftTree\":{\"nodeId\":\"root\",\"name\":\"\",\"children\":[]},\"notes\":\"test note\"},\"proposalMappings\":[{\"proposalId\":\"p1\",\"status\":\"accepted\"}],\"rejectedProposalIds\":[\"p2\"]}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        })
        .to_string();

        let parsed =
            parse_completion_response(ApiFormat::OpenAi, StatusCode::OK, &raw).expect("parsed");
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "revise_tree_draft");
        assert_eq!(parsed.tool_calls[0].arguments["notes"], "test note");
        assert_eq!(
            parsed.tool_calls[0].arguments["proposalMappings"][0]["proposalId"],
            "p1"
        );
        assert_eq!(
            parsed.tool_calls[0].arguments["rejectedProposalIds"][0],
            "p2"
        );
    }

    #[test]
    fn preserves_tool_call_with_parse_fallback_when_arguments_stay_invalid() {
        let raw = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "submit_classification_batch",
                            "arguments": "{\"assignments\":[}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        })
        .to_string();

        let parsed =
            parse_completion_response(ApiFormat::OpenAi, StatusCode::OK, &raw).expect("parsed");
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "submit_classification_batch");
        assert_eq!(
            parsed.tool_calls[0].arguments[TOOL_ARGUMENTS_RAW_FIELD],
            json!("{\"assignments\":[}")
        );
        assert!(
            parsed.tool_calls[0].arguments[TOOL_ARGUMENTS_PARSE_ERROR_FIELD]
                .as_str()
                .unwrap_or_default()
                .contains("tool call arguments are not valid JSON")
        );
    }

    #[test]
    fn parses_tool_call_arguments_inside_markdown_code_fence() {
        let raw = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "submit_classification_batch",
                            "arguments": "```json\n{\"baseTreeVersion\":1,\"assignments\":[]}\n```"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        })
        .to_string();

        let parsed =
            parse_completion_response(ApiFormat::OpenAi, StatusCode::OK, &raw).expect("parsed");
        assert_eq!(parsed.tool_calls[0].arguments["baseTreeVersion"], 1);
    }

    #[test]
    fn parses_tool_call_arguments_embedded_in_natural_language() {
        let raw = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "submit_classification_batch",
                            "arguments": "Here is the JSON payload: {\"baseTreeVersion\":1,\"assignments\":[]} Please apply it."
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        })
        .to_string();

        let parsed =
            parse_completion_response(ApiFormat::OpenAi, StatusCode::OK, &raw).expect("parsed");
        assert_eq!(parsed.tool_calls[0].arguments["assignments"], json!([]));
    }

    #[test]
    fn repairs_trailing_commas_in_tool_arguments() {
        let raw = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "submit_classification_batch",
                            "arguments": "{\"baseTreeVersion\":1,\"assignments\":[{\"itemId\":\"batch1_1\",\"reason\":\"doc\",}],}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        })
        .to_string();

        let parsed =
            parse_completion_response(ApiFormat::OpenAi, StatusCode::OK, &raw).expect("parsed");
        assert_eq!(
            parsed.tool_calls[0].arguments["assignments"][0]["itemId"],
            "batch1_1"
        );
    }

    #[test]
    fn repairs_missing_closing_delimiters_in_tool_arguments() {
        let raw = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "submit_classification_batch",
                            "arguments": "{\"baseTreeVersion\":1,\"assignments\":[{\"itemId\":\"batch1_1\",\"reason\":\"doc\"}]"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        })
        .to_string();

        let parsed =
            parse_completion_response(ApiFormat::OpenAi, StatusCode::OK, &raw).expect("parsed");
        assert_eq!(parsed.tool_calls[0].arguments["baseTreeVersion"], 1);
    }

    #[test]
    fn parses_double_encoded_tool_arguments_string() {
        let raw = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "submit_classification_batch",
                            "arguments": "\"{\\\"baseTreeVersion\\\":1,\\\"assignments\\\":[]}\""
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        })
        .to_string();

        let parsed =
            parse_completion_response(ApiFormat::OpenAi, StatusCode::OK, &raw).expect("parsed");
        assert_eq!(parsed.tool_calls[0].arguments["baseTreeVersion"], 1);
    }
}
