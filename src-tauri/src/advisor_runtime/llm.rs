use crate::backend::{
    resolve_provider_api_key, resolve_provider_endpoint_and_model, AppState, TokenUsage,
};
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::time::Duration;
use uuid::Uuid;

const CHAT_COMPLETION_TIMEOUT_SECS: u64 = 180;
const RESPONSE_ERROR_SNIPPET_CHARS: usize = 400;

#[derive(Clone, Debug)]
pub(super) struct AdvisorModelRoute {
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
}

#[derive(Clone, Debug)]
pub(super) struct AdvisorToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug)]
pub(super) struct AdvisorCompletionOutput {
    pub raw_body: String,
    pub assistant_text: String,
    pub tool_calls: Vec<AdvisorToolCall>,
    pub raw_message: Value,
    pub finish_reason: Option<String>,
    pub usage: TokenUsage,
    pub route: AdvisorModelRoute,
}

#[derive(Clone)]
pub(super) struct AdvisorLlm<'a> {
    state: &'a AppState,
}

#[derive(Debug)]
struct ChatCompletionError {
    message: String,
    raw_body: String,
}

impl<'a> AdvisorLlm<'a> {
    pub(super) fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    pub(super) fn resolve_route(
        &self,
        endpoint_hint: Option<&str>,
        model_hint: Option<&str>,
    ) -> Result<AdvisorModelRoute, String> {
        let (endpoint, model) =
            resolve_provider_endpoint_and_model(self.state, endpoint_hint, model_hint);
        let api_key = resolve_provider_api_key(self.state, &endpoint)?;
        Ok(AdvisorModelRoute {
            endpoint,
            model,
            api_key,
        })
    }

    pub(super) async fn complete_with_tools(
        &self,
        messages: &[Value],
        tools: &[Value],
    ) -> Result<AdvisorCompletionOutput, String> {
        let route = self.resolve_route(None, None)?;
        chat_completion(&route, messages, Some(tools))
            .await
            .map_err(|err| render_chat_error(&err))
    }

    pub(super) async fn complete_text(
        &self,
        messages: &[Value],
    ) -> Result<AdvisorCompletionOutput, String> {
        let route = self.resolve_route(None, None)?;
        chat_completion(&route, messages, None)
            .await
            .map_err(|err| render_chat_error(&err))
    }
}

fn render_chat_error(err: &ChatCompletionError) -> String {
    if err.raw_body.trim().is_empty() {
        err.message.clone()
    } else {
        format!("{} | body: {}", err.message, summarize_response_body(&err.raw_body))
    }
}

fn summarize_response_body(raw_body: &str) -> String {
    let snippet = raw_body.trim();
    if snippet.chars().count() <= RESPONSE_ERROR_SNIPPET_CHARS {
        snippet.to_string()
    } else {
        snippet.chars().take(RESPONSE_ERROR_SNIPPET_CHARS).collect()
    }
}

fn parse_chat_completion_http_body(
    route: &AdvisorModelRoute,
    status: StatusCode,
    raw_body: &str,
) -> Result<AdvisorCompletionOutput, ChatCompletionError> {
    let body: Value = serde_json::from_str(raw_body).map_err(|e| ChatCompletionError {
        message: format!("error decoding response body: {e}"),
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
            message: format!("{api_message} (HTTP {})", status.as_u16()),
            raw_body: raw_body.to_string(),
        });
    }

    let choice = body
        .pointer("/choices/0")
        .cloned()
        .ok_or_else(|| ChatCompletionError {
            message: "advisor response missing choices[0]".to_string(),
            raw_body: raw_body.to_string(),
        })?;
    let message = choice
        .get("message")
        .cloned()
        .ok_or_else(|| ChatCompletionError {
            message: "advisor response missing choices[0].message".to_string(),
            raw_body: raw_body.to_string(),
        })?;
    let assistant_text = message
        .get("content")
        .map(content_value_to_text)
        .unwrap_or_default()
        .trim()
        .to_string();
    let tool_calls = parse_tool_calls(&message).map_err(|message| ChatCompletionError {
        message,
        raw_body: raw_body.to_string(),
    })?;

    if assistant_text.is_empty() && tool_calls.is_empty() {
        return Err(ChatCompletionError {
            message: "advisor response missing assistant text and tool calls".to_string(),
            raw_body: raw_body.to_string(),
        });
    }

    let usage = TokenUsage {
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
    };

    Ok(AdvisorCompletionOutput {
        raw_body: raw_body.to_string(),
        assistant_text,
        tool_calls,
        raw_message: message,
        finish_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        usage,
        route: route.clone(),
    })
}

fn parse_tool_calls(message: &Value) -> Result<Vec<AdvisorToolCall>, String> {
    if let Some(rows) = message.get("tool_calls").and_then(Value::as_array) {
        let mut tool_calls = Vec::new();
        for row in rows {
            let function = row.get("function").cloned().unwrap_or(Value::Null);
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "advisor tool call missing function.name".to_string())?;
            let arguments = parse_arguments_value(function.get("arguments"))?;
            tool_calls.push(AdvisorToolCall {
                id: row
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("call_{}", Uuid::new_v4())),
                name: name.to_string(),
                arguments,
            });
        }
        return Ok(tool_calls);
    }

    if let Some(function_call) = message.get("function_call") {
        let name = function_call
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "advisor function_call missing name".to_string())?;
        return Ok(vec![AdvisorToolCall {
            id: format!("call_{}", Uuid::new_v4()),
            name: name.to_string(),
            arguments: parse_arguments_value(function_call.get("arguments"))?,
        }]);
    }

    Ok(Vec::new())
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
                serde_json::from_str::<Value>(trimmed)
                    .map_err(|e| format!("advisor tool call arguments are not valid JSON: {e}"))
            }
        }
        Some(other) => Ok(other.clone()),
    }
}

fn content_value_to_text(value: &Value) -> String {
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
        Some("text") => part
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string),
        Some("output_text") => part
            .get("text")
            .or_else(|| part.get("content"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

async fn chat_completion(
    route: &AdvisorModelRoute,
    messages: &[Value],
    tools: Option<&[Value]>,
) -> Result<AdvisorCompletionOutput, ChatCompletionError> {
    let url = format!("{}/chat/completions", route.endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(CHAT_COMPLETION_TIMEOUT_SECS))
        .build()
        .map_err(|e| ChatCompletionError {
            message: e.to_string(),
            raw_body: String::new(),
        })?;

    let mut payload = json!({
        "model": route.model,
        "messages": messages,
        "temperature": 0
    });
    if let Some(tools) = tools.filter(|rows| !rows.is_empty()) {
        payload["tools"] = Value::Array(tools.to_vec());
        payload["tool_choice"] = Value::String("auto".to_string());
    }

    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&payload);
    if !route.api_key.is_empty() {
        req = req
            .header("Authorization", format!("Bearer {}", route.api_key))
            .header("x-api-key", route.api_key.clone())
            .header("api-key", route.api_key.clone());
    }
    let resp = req.send().await.map_err(|e| ChatCompletionError {
        message: e.to_string(),
        raw_body: String::new(),
    })?;
    let status = resp.status();
    let raw_body = resp.text().await.map_err(|e| ChatCompletionError {
        message: format!("error reading response body: {e}"),
        raw_body: String::new(),
    })?;
    parse_chat_completion_http_body(route, status, &raw_body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_route() -> AdvisorModelRoute {
        AdvisorModelRoute {
            endpoint: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            api_key: "test".to_string(),
        }
    }

    #[test]
    fn parses_tool_call_arguments_from_string() {
        let raw = json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "find_files",
                            "arguments": "{\"nameQuery\":\"shot\"}"
                        }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })
        .to_string();
        let output = parse_chat_completion_http_body(&make_route(), StatusCode::OK, &raw)
            .expect("parse output");
        assert_eq!(output.tool_calls.len(), 1);
        assert_eq!(output.tool_calls[0].name, "find_files");
        assert_eq!(output.tool_calls[0].arguments["nameQuery"], "shot");
    }

    #[test]
    fn parses_content_array_and_multiple_tool_calls() {
        let raw = json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": [{ "type": "text", "text": "Need more context." }],
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "get_directory_overview",
                                "arguments": {}
                            }
                        },
                        {
                            "id": "call_2",
                            "type": "function",
                            "function": {
                                "name": "list_preferences",
                                "arguments": "{}"
                            }
                        }
                    ]
                }
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })
        .to_string();
        let output = parse_chat_completion_http_body(&make_route(), StatusCode::OK, &raw)
            .expect("parse output");
        assert_eq!(output.assistant_text, "Need more context.");
        assert_eq!(output.tool_calls.len(), 2);
    }

    #[test]
    fn parses_plain_final_reply() {
        let raw = json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": "先看截图和安装包。"
                }
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })
        .to_string();
        let output = parse_chat_completion_http_body(&make_route(), StatusCode::OK, &raw)
            .expect("parse output");
        assert_eq!(output.assistant_text, "先看截图和安装包。");
        assert!(output.tool_calls.is_empty());
    }
}
