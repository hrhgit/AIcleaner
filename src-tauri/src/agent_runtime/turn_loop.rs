use super::llm::AgentLlm;
use super::types::{AgentCompletion, AgentLlmError, AgentLoopTrace, AgentToolPolicy};
use crate::llm_protocol::ParsedToolCall;
use crate::llm_tools::{
    serialize_tool_result_content, ToolExecutionContext, ToolId, ToolRegistry, ToolResult,
};
use serde_json::{json, Value};

pub(crate) enum ToolCallOutcome<O> {
    Continue { result: Value },
    Finish(O),
}

pub(crate) enum ToolCallErrorOutcome<O> {
    Continue { message: String },
    Finish(O),
}

pub(crate) trait AgentTurnSpec {
    type Output;

    fn max_steps(&self) -> usize;
    fn tool_policy<'ctx>(&'ctx self) -> AgentToolPolicy<'ctx>;
    fn trace_key(&self) -> Option<&str> {
        None
    }
    fn allow_multiple_tool_calls(&self) -> bool {
        true
    }
    fn build_initial_messages(&mut self) -> Result<Vec<Value>, String>;
    fn before_step(&mut self, _step: usize, _messages: &mut Vec<Value>) -> Result<(), String> {
        Ok(())
    }
    fn tool_execution_context<'ctx>(&'ctx mut self) -> ToolExecutionContext<'ctx>;
    fn on_model_success(
        &mut self,
        _step: usize,
        _completion: &AgentCompletion,
        _trace: &AgentLoopTrace,
    ) -> Result<(), String> {
        Ok(())
    }
    fn on_model_error(
        &mut self,
        _step: usize,
        err: AgentLlmError,
        _trace: &AgentLoopTrace,
    ) -> Result<Option<Self::Output>, String> {
        Err(err.message)
    }
    fn on_no_tool_calls(
        &mut self,
        completion: AgentCompletion,
        trace: &AgentLoopTrace,
    ) -> Result<Self::Output, String>;
    fn on_multiple_tool_calls(
        &mut self,
        completion: AgentCompletion,
        trace: &AgentLoopTrace,
    ) -> Result<Self::Output, String> {
        self.on_no_tool_calls(completion, trace)
    }
    fn on_tool_success(
        &mut self,
        step: usize,
        tool_id: Option<ToolId>,
        call: &ParsedToolCall,
        result: ToolResult,
        trace: &AgentLoopTrace,
    ) -> Result<ToolCallOutcome<Self::Output>, String>;
    fn on_tool_error(
        &mut self,
        _step: usize,
        _call: &ParsedToolCall,
        message: String,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallErrorOutcome<Self::Output>, String> {
        Ok(ToolCallErrorOutcome::Continue { message })
    }
    fn on_loop_exhausted(&mut self, trace: &AgentLoopTrace) -> Result<Self::Output, String>;
}

pub(crate) struct AgentTurnLoop<'a, L> {
    llm: &'a L,
    tool_registry: &'a ToolRegistry,
}

impl<'a, L> AgentTurnLoop<'a, L>
where
    L: AgentLlm,
{
    pub(crate) fn new(llm: &'a L, tool_registry: &'a ToolRegistry) -> Self {
        Self { llm, tool_registry }
    }

    pub(crate) async fn run<S>(&self, spec: &mut S) -> Result<S::Output, String>
    where
        S: AgentTurnSpec,
    {
        let mut messages = spec.build_initial_messages()?;
        let mut trace = AgentLoopTrace::default();

        for step in 0..spec.max_steps() {
            spec.before_step(step, &mut messages)?;
            let tools = {
                let policy = spec.tool_policy();
                let ctx = policy.as_tool_context();
                self.tool_registry.definitions(&ctx)
            };
            let completion = match self.llm.complete(&messages, &tools, spec.trace_key()).await {
                Ok(completion) => completion,
                Err(err) => {
                    if let Some(output) = spec.on_model_error(step, err, &trace)? {
                        return Ok(output);
                    }
                    continue;
                }
            };
            trace.record_model_step(step, &completion);
            spec.on_model_success(step, &completion, &trace)?;

            if completion.tool_calls.is_empty() {
                return spec.on_no_tool_calls(completion, &trace);
            }
            if !spec.allow_multiple_tool_calls() && completion.tool_calls.len() > 1 {
                return spec.on_multiple_tool_calls(completion, &trace);
            }

            messages.push(normalize_assistant_message(&completion.raw_message));
            for call in completion.tool_calls {
                let tool_id = self.tool_registry.id_for_name(&call.name);
                let dispatch_result = {
                    let mut tool_ctx = spec.tool_execution_context();
                    self.tool_registry.dispatch(&mut tool_ctx, &call).await
                };
                match dispatch_result {
                    Ok(result) => {
                        trace.record_tool_result(step, &call, "ok", result.envelope());
                        match spec.on_tool_success(step, tool_id, &call, result, &trace)? {
                            ToolCallOutcome::Continue { result } => {
                                messages.push(tool_result_message(&call.id, result));
                            }
                            ToolCallOutcome::Finish(output) => return Ok(output),
                        }
                    }
                    Err(message) => match spec.on_tool_error(step, &call, message, &trace)? {
                        ToolCallErrorOutcome::Continue { message } => {
                            trace.record_tool_result(
                                step,
                                &call,
                                "error",
                                json!({ "message": message.clone() }),
                            );
                            messages.push(tool_error_message(&call.id, message));
                        }
                        ToolCallErrorOutcome::Finish(output) => return Ok(output),
                    },
                }
            }
        }

        spec.on_loop_exhausted(&trace)
    }
}

fn normalize_assistant_message(message: &Value) -> Value {
    let mut obj = message.as_object().cloned().unwrap_or_default();
    obj.insert("role".to_string(), Value::String("assistant".to_string()));
    Value::Object(obj)
}

fn tool_result_message(tool_call_id: &str, result: Value) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "content": serialize_tool_result_content(&result),
    })
}

fn tool_error_message(tool_call_id: &str, message: String) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "content": message,
    })
}
