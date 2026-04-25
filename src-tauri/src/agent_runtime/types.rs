use crate::backend::TokenUsage;
use crate::llm_protocol::ParsedToolCall;
use crate::llm_tools::{ToolContext, ToolWorkflow};
use serde_json::{json, Value};

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
}

impl AgentLoopTrace {
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
                    "payload": payload,
                }));
            }
        }
    }

    pub(crate) fn as_json(&self) -> Value {
        json!({ "steps": self.steps })
    }
}
