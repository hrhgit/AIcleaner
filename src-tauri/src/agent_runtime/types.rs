use crate::backend::TokenUsage;
use crate::llm_protocol::ParsedToolCall;
use crate::llm_tools::{ToolContext, ToolWorkflow};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone, Debug)]
pub(crate) struct AgentCompletion {
    pub raw_body: String,
    pub assistant_text: String,
    pub tool_calls: Vec<ParsedToolCall>,
    pub raw_message: Value,
    pub finish_reason: Option<String>,
    pub usage: TokenUsage,
    pub route: Option<AgentRoute>,
}

#[derive(Clone, Debug)]
pub(crate) struct AgentRoute {
    pub endpoint: String,
    pub model: String,
}

#[derive(Clone, Debug)]
pub(crate) struct AgentLlmError {
    pub message: String,
    pub raw_body: String,
}

impl AgentLlmError {
    pub(crate) fn new(message: impl Into<String>, raw_body: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            raw_body: raw_body.into(),
        }
    }
}

pub(crate) struct AgentToolPolicy<'a> {
    pub workflow: ToolWorkflow,
    pub stage: &'a str,
    pub session: Option<&'a Value>,
    pub bootstrap_turn: bool,
    pub response_language: &'a str,
    pub web_search_allowed: bool,
    pub search_remaining: usize,
}

impl<'a> AgentToolPolicy<'a> {
    pub(crate) fn as_tool_context(&self) -> ToolContext<'_> {
        ToolContext {
            workflow: self.workflow,
            stage: self.stage,
            session: self.session,
            bootstrap_turn: self.bootstrap_turn,
            response_language: self.response_language,
            web_search_allowed: self.web_search_allowed,
            search_remaining: self.search_remaining,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AgentLoopTrace {
    steps: Vec<Value>,
    duration_ms: Option<u64>,
}

impl AgentLoopTrace {
    pub(crate) fn set_duration(&mut self, duration: Duration) {
        self.duration_ms = Some(duration.as_millis() as u64);
    }

    pub(crate) fn record_model_step(&mut self, step: usize, completion: &AgentCompletion) {
        self.steps.push(json!({
            "step": step,
            "route": completion.route.as_ref().map(|route| {
                json!({
                    "endpoint": route.endpoint,
                    "model": route.model,
                })
            }).unwrap_or(Value::Null),
            "usage": {
                "prompt": completion.usage.prompt,
                "completion": completion.usage.completion,
                "total": completion.usage.total,
            },
            "finishReason": completion.finish_reason,
            "assistantText": completion.assistant_text,
            "toolCalls": completion.tool_calls.iter().map(|call| {
                json!({
                    "id": call.id,
                    "name": call.name,
                    "arguments": call.arguments,
                })
            }).collect::<Vec<_>>(),
            "rawBody": completion.raw_body,
        }));
    }

    pub(crate) fn record_tool_result(
        &mut self,
        step: usize,
        call: &ParsedToolCall,
        status: &str,
        payload: Value,
        duration_ms: u64,
    ) {
        if let Some(row) = self
            .steps
            .iter_mut()
            .rev()
            .find(|row| row.get("step").and_then(Value::as_u64) == Some(step as u64))
        {
            if !row.get("toolResults").is_some_and(Value::is_array) {
                row["toolResults"] = json!([]);
            }
            if let Some(results) = row.get_mut("toolResults").and_then(Value::as_array_mut) {
                results.push(json!({
                    "id": call.id,
                    "name": call.name,
                    "status": status,
                    "durationMs": duration_ms,
                    "payload": payload,
                }));
            }
        }
    }

    pub(crate) fn as_json(&self) -> Value {
        json!({
            "summary": self.summary_json(),
            "steps": self.steps,
        })
    }

    fn summary_json(&self) -> Value {
        let mut usage = TokenUsage::default();
        let mut model_step_count = 0_u64;
        let mut tool_call_count = 0_u64;
        let mut tool_error_count = 0_u64;
        let mut tool_request_count = 0_u64;
        let mut tool_reported_errors = 0_u64;

        for step in &self.steps {
            if let Some(step_usage) = step.get("usage") {
                add_usage_from_value(&mut usage, step_usage);
            }
            model_step_count = model_step_count.saturating_add(1);
            tool_call_count = tool_call_count.saturating_add(
                step.get("toolCalls")
                    .and_then(Value::as_array)
                    .map(|items| items.len() as u64)
                    .unwrap_or(0),
            );
            if let Some(results) = step.get("toolResults").and_then(Value::as_array) {
                for result in results {
                    if result.get("status").and_then(Value::as_str) == Some("error") {
                        tool_error_count = tool_error_count.saturating_add(1);
                    }
                    if let Some(payload) = result.get("payload") {
                        if let Some(tool_usage) = payload.get("tokenUsage") {
                            add_usage_from_value(&mut usage, tool_usage);
                        }
                        tool_request_count = tool_request_count.saturating_add(
                            payload
                                .get("requestCount")
                                .and_then(Value::as_u64)
                                .unwrap_or(0),
                        );
                        tool_reported_errors = tool_reported_errors.saturating_add(
                            payload
                                .get("errorCount")
                                .and_then(Value::as_u64)
                                .unwrap_or(0),
                        );
                    }
                }
            }
        }

        json!({
            "durationMs": self.duration_ms,
            "tokenUsage": {
                "prompt": usage.prompt,
                "completion": usage.completion,
                "total": usage.total,
            },
            "modelStepCount": model_step_count,
            "toolCallCount": tool_call_count,
            "requestCount": model_step_count.saturating_add(tool_request_count),
            "errorCount": tool_error_count.saturating_add(tool_reported_errors),
        })
    }
}

fn add_usage_from_value(total: &mut TokenUsage, value: &Value) {
    total.prompt = total
        .prompt
        .saturating_add(value.get("prompt").and_then(Value::as_u64).unwrap_or(0));
    total.completion = total
        .completion
        .saturating_add(value.get("completion").and_then(Value::as_u64).unwrap_or(0));
    total.total = total
        .total
        .saturating_add(value.get("total").and_then(Value::as_u64).unwrap_or(0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_trace_carries_duration_and_summary_usage() {
        let mut trace = AgentLoopTrace::default();
        let completion = AgentCompletion {
            raw_body: "{}".to_string(),
            assistant_text: String::new(),
            tool_calls: vec![ParsedToolCall {
                id: "call_1".to_string(),
                name: "summarize_files".to_string(),
                arguments: json!({}),
            }],
            raw_message: json!({ "role": "assistant" }),
            finish_reason: None,
            usage: TokenUsage {
                prompt: 3,
                completion: 2,
                total: 5,
            },
            route: None,
        };
        trace.record_model_step(0, &completion);
        trace.record_tool_result(
            0,
            &completion.tool_calls[0],
            "ok",
            json!({
                "tokenUsage": { "prompt": 7, "completion": 4, "total": 11 },
                "requestCount": 2,
            }),
            42,
        );
        trace.set_duration(Duration::from_millis(99));

        let out = trace.as_json();
        assert_eq!(out["steps"][0]["toolResults"][0]["durationMs"], 42);
        assert_eq!(out["summary"]["durationMs"], 99);
        assert_eq!(out["summary"]["tokenUsage"]["total"], 16);
        assert_eq!(out["summary"]["requestCount"], 3);
    }
}
