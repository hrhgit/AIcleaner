use crate::backend::{
    resolve_provider_api_key, resolve_provider_endpoint_and_model, AppState, TokenUsage,
};
use crate::llm_protocol::{
    apply_auth_headers, build_completion_payload, build_messages_url, detect_api_format,
    parse_completion_response, DEFAULT_MAX_TOKENS,
};
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

#[cfg(test)]
use serde_json::json as test_json;

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
        session_id: Option<&str>,
    ) -> Result<AdvisorCompletionOutput, String> {
        let route = self.resolve_route(None, None)?;
        let operation_id = crate::diagnostics::new_operation_id();
        chat_completion(
            self.state,
            &route,
            messages,
            Some(tools),
            &operation_id,
            session_id,
        )
        .await
        .map_err(|err| render_chat_error(&err))
    }
}

fn render_chat_error(err: &ChatCompletionError) -> String {
    if err.raw_body.trim().is_empty() {
        err.message.clone()
    } else {
        format!(
            "{} | body: {}",
            err.message,
            summarize_response_body(&err.raw_body)
        )
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
    let parsed = parse_completion_response(detect_api_format(&route.endpoint), status, raw_body)
        .map_err(|message| ChatCompletionError {
            message,
            raw_body: raw_body.to_string(),
        })?;

    Ok(AdvisorCompletionOutput {
        raw_body: raw_body.to_string(),
        assistant_text: parsed.assistant_text,
        tool_calls: parsed
            .tool_calls
            .into_iter()
            .map(|call| AdvisorToolCall {
                id: call.id,
                name: call.name,
                arguments: call.arguments,
            })
            .collect(),
        raw_message: parsed.raw_message,
        finish_reason: parsed.finish_reason,
        usage: parsed.usage,
        route: route.clone(),
    })
}

async fn chat_completion(
    state: &AppState,
    route: &AdvisorModelRoute,
    messages: &[Value],
    tools: Option<&[Value]>,
    operation_id: &str,
    session_id: Option<&str>,
) -> Result<AdvisorCompletionOutput, ChatCompletionError> {
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
    let started_at = Instant::now();
    crate::diagnostics::record_state_event(
        state,
        "info",
        "advisor",
        "advisor_model_request",
        Some(operation_id),
        "advisor model request started",
        json!({
            "sessionId": session_id.unwrap_or(""),
            "endpoint": route.endpoint.clone(),
            "model": route.model.clone(),
            "url": url.clone(),
            "messages": messages,
            "tools": tools.unwrap_or(&[]),
            "payload": payload.clone(),
        }),
        None,
        None,
    );

    let req = client
        .post(url.clone())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&payload);
    let req = apply_auth_headers(req, api_format, &route.api_key);
    let resp = match req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            crate::diagnostics::record_state_event(
                state,
                "error",
                "advisor",
                "advisor_model_error",
                Some(operation_id),
                "advisor model request failed",
                json!({
                    "sessionId": session_id.unwrap_or(""),
                    "endpoint": route.endpoint.clone(),
                    "model": route.model.clone(),
                    "url": url.clone(),
                }),
                Some(json!({ "message": e.to_string() })),
                Some(started_at.elapsed()),
            );
            return Err(ChatCompletionError {
                message: e.to_string(),
                raw_body: String::new(),
            });
        }
    };
    let status = resp.status();
    let raw_body = match resp.text().await {
        Ok(raw_body) => raw_body,
        Err(e) => {
            crate::diagnostics::record_state_event(
                state,
                "error",
                "advisor",
                "advisor_model_error",
                Some(operation_id),
                "advisor model response body read failed",
                json!({
                    "sessionId": session_id.unwrap_or(""),
                    "endpoint": route.endpoint.clone(),
                    "model": route.model.clone(),
                    "status": status.as_u16(),
                }),
                Some(json!({ "message": e.to_string() })),
                Some(started_at.elapsed()),
            );
            return Err(ChatCompletionError {
                message: format!("error reading response body: {e}"),
                raw_body: String::new(),
            });
        }
    };
    crate::diagnostics::record_state_event(
        state,
        if status.is_success() { "info" } else { "error" },
        "advisor",
        "advisor_model_response",
        Some(operation_id),
        "advisor model response received",
        json!({
            "sessionId": session_id.unwrap_or(""),
            "endpoint": route.endpoint.clone(),
            "model": route.model.clone(),
            "status": status.as_u16(),
            "rawBody": raw_body.clone(),
        }),
        None,
        Some(started_at.elapsed()),
    );
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
        let raw = test_json!({
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
        let raw = test_json!({
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
        let raw = test_json!({
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
