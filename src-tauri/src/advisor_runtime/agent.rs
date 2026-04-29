use super::llm::AdvisorLlm;
use super::types::{local_text, now_iso, WORKFLOW_UNDERSTAND};
use crate::agent_runtime::{
    AgentCompletion, AgentLoopTrace, AgentToolPolicy, AgentTurnLoop, AgentTurnSpec,
    ToolCallErrorOutcome, ToolCallOutcome,
};
use crate::backend::AppState;
use crate::llm_protocol::ParsedToolCall;
use crate::llm_tools::{ToolExecutionContext, ToolId, ToolRegistry, ToolResult, ToolWorkflow};
use crate::persist;
use serde_json::{json, Value};
use uuid::Uuid;

const MAX_AGENT_STEPS: usize = 8;

pub(super) struct AgentTurnResult {
    pub reply: String,
    pub cards: Vec<Value>,
    pub trace: Value,
}

pub(super) struct AdvisorAgentRunner<'a> {
    state: &'a AppState,
    llm: AdvisorLlm<'a>,
    tool_registry: ToolRegistry,
}

impl<'a> AdvisorAgentRunner<'a> {
    pub(super) fn new(state: &'a AppState) -> Self {
        Self {
            state,
            llm: AdvisorLlm::new(state),
            tool_registry: ToolRegistry::new(),
        }
    }

    pub(super) async fn run_user_turn(
        &self,
        session: &mut Value,
        message: &str,
    ) -> Result<AgentTurnResult, String> {
        self.run_turn(session, Some(message)).await
    }

    async fn run_turn(
        &self,
        session: &mut Value,
        user_message: Option<&str>,
    ) -> Result<AgentTurnResult, String> {
        let lang = session
            .get("responseLanguage")
            .and_then(Value::as_str)
            .unwrap_or("zh")
            .to_string();
        let bootstrap_turn = user_message.is_none();
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let messages = self.build_transcript_messages(
            session,
            &session_id,
            &lang,
            bootstrap_turn,
            user_message,
        )?;
        let mut spec = AdvisorTurnSpec {
            state: self.state,
            session,
            session_id,
            lang,
            bootstrap_turn,
            initial_messages: messages,
            cards: Vec::new(),
            dispatch_stage: WORKFLOW_UNDERSTAND.to_string(),
        };
        AgentTurnLoop::new(&self.llm, &self.tool_registry)
            .run(&mut spec)
            .await
    }

    fn build_system_prompt(
        &self,
        lang: &str,
        bootstrap_turn: bool,
        web_search_enabled: bool,
    ) -> String {
        let language_name = if lang.eq_ignore_ascii_case("en") {
            "English"
        } else {
            "Chinese"
        };
        let mut lines = vec![
            "你是文件整理顾问。".to_string(),
            "你必须通过原生 tool calling 调用工具，不能手写 JSON 协议。".to_string(),
            "不要重新生成扫描结果，也不要重新生成完整归类结果。".to_string(),
            "不要在自然语言回复里输出执行 schema、JSON 或代码块。".to_string(),
            "你会收到本会话完整 transcript，包含历史用户消息、顾问回复、工具调用和工具结果；必须把它作为连续对话上下文。".to_string(),
            "不要假设上一轮自然语言方案会被压缩成额外状态；确认、继续、执行等短指令应优先回看完整 transcript。".to_string(),
            "需要当前目录、筛选、预览或执行证据时，继续通过工具读取或操作。".to_string(),
            "当 session 记忆和 global 记忆冲突时，以 session 为准。".to_string(),
            "高风险动作不确定时先澄清，不要直接执行。".to_string(),
            format!("最终自然语言回复必须使用 {language_name}。"),
        ];
        if bootstrap_turn {
            lines.push("这是首轮回复。先看树结果，再给 2 到 4 条简短建议。".to_string());
            lines.push("首轮禁止调用 execute_plan。".to_string());
        } else {
            lines.push("如果工具已经返回结构化结果卡，最终回复只解释结论和下一步。".to_string());
        }
        if web_search_enabled {
            lines.push("当本地证据不足且确实需要外部背景时，可以调用 web_search。".to_string());
        } else {
            lines.push("当前轮次不可联网搜索，请基于已有本地证据判断。".to_string());
        }
        lines.join("\n")
    }

    fn build_transcript_messages(
        &self,
        session: &Value,
        session_id: &str,
        lang: &str,
        bootstrap_turn: bool,
        user_message: Option<&str>,
    ) -> Result<Vec<Value>, String> {
        let mut messages = vec![json!({
            "role": "system",
            "content": self.build_system_prompt(
                lang,
                bootstrap_turn,
                session
                    .get("webSearchEnabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ),
        })];

        let turns = persist::load_advisor_turns(&self.state.db_path(), session_id)?;
        for turn in turns {
            append_turn_messages(&mut messages, &turn);
        }

        if messages.len() == 1 {
            messages.push(json!({
                "role": "user",
                "content": user_message
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        if lang.eq_ignore_ascii_case("en") {
                            "Please give the first-turn advice for this directory.".to_string()
                        } else {
                            "请基于当前目录给出首轮建议。".to_string()
                        }
                    }),
            }));
        } else if let Some(message) = user_message
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let last_user_matches = messages.last().is_some_and(|row| {
                row.get("role").and_then(Value::as_str) == Some("user")
                    && row.get("content").and_then(Value::as_str) == Some(message)
            });
            if !last_user_matches {
                messages.push(json!({
                    "role": "user",
                    "content": message,
                }));
            }
        }

        Ok(messages)
    }
}

fn append_turn_messages(messages: &mut Vec<Value>, turn: &Value) {
    let role = turn
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("assistant");
    let text = turn.get("text").and_then(Value::as_str).unwrap_or_default();

    if role == "assistant" {
        if append_agent_trace_messages(messages, turn) {
            return;
        }
    }

    messages.push(json!({
        "role": role,
        "content": text,
    }));
}

fn append_agent_trace_messages(messages: &mut Vec<Value>, turn: &Value) -> bool {
    let Some(steps) = turn.pointer("/agentTrace/steps").and_then(Value::as_array) else {
        return false;
    };
    if steps.is_empty() {
        return false;
    }

    let mut appended = false;
    for step in steps {
        let tool_calls = step
            .get("toolCalls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let assistant_text = step
            .get("assistantText")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();

        if !tool_calls.is_empty() {
            let mut assistant = json!({
                "role": "assistant",
                "content": assistant_text,
                "tool_calls": tool_calls.iter().map(trace_tool_call_to_message).collect::<Vec<_>>(),
            });
            if let Some(raw_reasoning) = step
                .pointer("/rawMessage/reasoning_content")
                .or_else(|| step.pointer("/rawBody/choices/0/message/reasoning_content"))
            {
                assistant["reasoning_content"] = raw_reasoning.clone();
            }
            messages.push(assistant);
            appended = true;

            let results = step
                .get("toolResults")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for result in results {
                let tool_call_id = result.get("id").and_then(Value::as_str).unwrap_or_default();
                let payload = result.get("payload").cloned().unwrap_or(Value::Null);
                let model_payload = payload
                    .get("result")
                    .cloned()
                    .filter(|_| payload.get("card").is_some() || payload.get("cards").is_some())
                    .unwrap_or(payload);
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": serde_json::to_string_pretty(
                        &model_payload
                    )
                    .unwrap_or_else(|_| String::new()),
                }));
            }
        } else if !assistant_text.is_empty() {
            messages.push(json!({
                "role": "assistant",
                "content": assistant_text,
            }));
            appended = true;
        }
    }

    if !appended {
        let text = turn.get("text").and_then(Value::as_str).unwrap_or_default();
        if !text.trim().is_empty() {
            messages.push(json!({
                "role": "assistant",
                "content": text,
            }));
            return true;
        }
    }
    appended
}

fn trace_tool_call_to_message(call: &Value) -> Value {
    json!({
        "id": call.get("id").cloned().unwrap_or(Value::String(String::new())),
        "type": "function",
        "function": {
            "name": call.get("name").cloned().unwrap_or(Value::String(String::new())),
            "arguments": call.get("arguments").cloned().unwrap_or_else(|| json!({})).to_string(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_trace_expands_tool_messages_and_final_reply() {
        let turn = json!({
            "role": "assistant",
            "text": "最终方案",
            "agentTrace": {
                "steps": [
                    {
                        "assistantText": "我先查一下。",
                        "toolCalls": [
                            {
                                "id": "call_1",
                                "name": "find_files",
                                "arguments": { "categoryIds": ["安装包与可执行文件"] }
                            }
                        ],
                        "toolResults": [
                            {
                                "id": "call_1",
                                "name": "find_files",
                                "status": "ok",
                                "payload": {
                                    "result": { "total": 99 },
                                    "card": {
                                        "body": {
                                            "tree": {
                                                "categoryId": "real-category-id"
                                            }
                                        }
                                    }
                                }
                            }
                        ]
                    },
                    {
                        "assistantText": "最终方案",
                        "toolCalls": []
                    }
                ]
            }
        });

        let mut messages = Vec::new();
        append_turn_messages(&mut messages, &turn);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(
            messages[0]["tool_calls"][0]["function"]["name"],
            "find_files"
        );
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "call_1");
        let tool_content = messages[1]["content"].as_str().unwrap_or_default();
        assert!(tool_content.contains("\"total\": 99"));
        assert!(!tool_content.contains("real-category-id"));
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "最终方案");
    }
}

struct AdvisorTurnSpec<'a, 's> {
    state: &'a AppState,
    session: &'s mut Value,
    session_id: String,
    lang: String,
    bootstrap_turn: bool,
    initial_messages: Vec<Value>,
    cards: Vec<Value>,
    dispatch_stage: String,
}

impl<'a, 's> AdvisorTurnSpec<'a, 's> {
    fn refresh_dispatch_stage(&mut self) {
        self.dispatch_stage = self
            .session
            .get("workflowStage")
            .and_then(Value::as_str)
            .unwrap_or(WORKFLOW_UNDERSTAND)
            .to_string();
    }
}

impl<'a, 's> AgentTurnSpec for AdvisorTurnSpec<'a, 's> {
    type Output = AgentTurnResult;

    fn max_steps(&self) -> usize {
        MAX_AGENT_STEPS
    }

    fn tool_policy<'ctx>(&'ctx self) -> AgentToolPolicy<'ctx> {
        let stage = self
            .session
            .get("workflowStage")
            .and_then(Value::as_str)
            .unwrap_or(WORKFLOW_UNDERSTAND);
        let web_search_allowed = self
            .session
            .get("webSearchEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        AgentToolPolicy {
            workflow: ToolWorkflow::Advisor,
            stage,
            session: Some(&*self.session),
            bootstrap_turn: self.bootstrap_turn,
            response_language: &self.lang,
            web_search_allowed,
            search_remaining: 0,
        }
    }

    fn trace_key(&self) -> Option<&str> {
        Some(&self.session_id)
    }

    fn build_initial_messages(&mut self) -> Result<Vec<Value>, String> {
        Ok(std::mem::take(&mut self.initial_messages))
    }

    fn tool_execution_context<'ctx>(&'ctx mut self) -> ToolExecutionContext<'ctx> {
        self.refresh_dispatch_stage();
        let web_search_allowed = self
            .session
            .get("webSearchEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        ToolExecutionContext {
            workflow: ToolWorkflow::Advisor,
            stage: self.dispatch_stage.as_str(),
            session: Some(&mut *self.session),
            bootstrap_turn: self.bootstrap_turn,
            response_language: &self.lang,
            web_search_allowed,
            search_remaining: 0,
            state: Some(self.state),
            search_api_key: None,
            diagnostics: None,
            organizer_search_counter: None,
            organizer_search_gate: None,
        }
    }

    fn on_no_tool_calls(
        &mut self,
        completion: AgentCompletion,
        trace: &AgentLoopTrace,
    ) -> Result<AgentTurnResult, String> {
        let reply = if self.bootstrap_turn {
            normalize_bootstrap_reply(&completion.assistant_text)
        } else {
            completion.assistant_text.trim().to_string()
        };
        if reply.is_empty() {
            return Err(local_text(
                &self.lang,
                "顾问返回了空回复，请重试。",
                "The advisor returned an empty reply. Please try again.",
            )
            .to_string());
        }
        Ok(AgentTurnResult {
            reply,
            cards: std::mem::take(&mut self.cards),
            trace: trace.as_json(),
        })
    }

    fn on_tool_success(
        &mut self,
        step: usize,
        _tool_id: Option<ToolId>,
        call: &ParsedToolCall,
        result: ToolResult,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallOutcome<AgentTurnResult>, String> {
        let envelope = result.envelope();
        crate::diagnostics::record_state_event(
            self.state,
            "info",
            "advisor",
            "advisor_tool_result",
            None,
            "advisor tool call succeeded",
            json!({
                "sessionId": self.session_id,
                "step": step,
                "tool": call.name,
                "result": envelope,
            }),
            None,
            None,
        );
        if let Some(card) = result.card.clone().filter(|value| !value.is_null()) {
            self.cards.push(card);
        }
        if !result.cards.is_empty() {
            self.cards.extend(result.cards.clone());
        }
        Ok(ToolCallOutcome::Continue {
            result: result.result,
        })
    }

    fn on_tool_error(
        &mut self,
        step: usize,
        call: &ParsedToolCall,
        message: String,
        _trace: &AgentLoopTrace,
    ) -> Result<ToolCallErrorOutcome<AgentTurnResult>, String> {
        crate::diagnostics::record_state_event(
            self.state,
            "error",
            "advisor",
            "advisor_tool_result",
            None,
            "advisor tool call failed",
            json!({
                "sessionId": self.session_id,
                "step": step,
                "tool": call.name,
            }),
            Some(json!({ "message": message.clone() })),
            None,
        );
        Ok(ToolCallErrorOutcome::Continue { message })
    }

    fn on_loop_exhausted(&mut self, _trace: &AgentLoopTrace) -> Result<AgentTurnResult, String> {
        Err(local_text(
            &self.lang,
            "顾问在当前轮次内没有收敛，请重试。",
            "The advisor did not converge within this turn. Please try again.",
        )
        .to_string())
    }
}

fn normalize_bootstrap_reply(reply: &str) -> String {
    let lines = reply
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(4)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        reply.trim().to_string()
    } else {
        lines.join("\n")
    }
}

pub(super) fn materialize_card(session_id: &str, turn_id: &str, card: &Value) -> Value {
    json!({
        "cardId": card.get("cardId").cloned().unwrap_or(Value::String(Uuid::new_v4().to_string())),
        "sessionId": session_id,
        "turnId": turn_id,
        "cardType": card.get("cardType").cloned().unwrap_or(Value::String(String::new())),
        "status": card.get("status").cloned().unwrap_or(Value::String("ready".to_string())),
        "title": card.get("title").cloned().unwrap_or(Value::String(String::new())),
        "body": card.get("body").cloned().unwrap_or(Value::Null),
        "actions": card.get("actions").cloned().unwrap_or_else(|| json!([])),
        "createdAt": card.get("createdAt").cloned().unwrap_or(Value::String(now_iso())),
        "updatedAt": card.get("updatedAt").cloned().unwrap_or(Value::String(now_iso())),
    })
}
