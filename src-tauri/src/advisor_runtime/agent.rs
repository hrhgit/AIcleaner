use super::llm::AdvisorLlm;
use super::tools::ToolService;
use super::types::{local_text, now_iso, CARD_EXECUTION, WORKFLOW_UNDERSTAND};
use crate::backend::AppState;
use crate::llm_protocol::ParsedToolCall;
use crate::llm_tools::{
    serialize_tool_result_content, ToolContext, ToolExecutionContext, ToolRegistry, ToolWorkflow,
};
use crate::persist;
use serde_json::{json, Value};
use std::time::Instant;
use uuid::Uuid;

const MAX_AGENT_STEPS: usize = 8;

pub(super) struct AgentTurnResult {
    pub reply: String,
    pub cards: Vec<Value>,
    pub trace: Value,
}

pub(super) struct AdvisorAgentRunner<'a> {
    state: &'a AppState,
    tools: ToolService<'a>,
    llm: AdvisorLlm<'a>,
    tool_registry: ToolRegistry,
}

impl<'a> AdvisorAgentRunner<'a> {
    pub(super) fn new(state: &'a AppState) -> Self {
        Self {
            state,
            tools: ToolService::new(state),
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
        let context_payload = self.build_context_payload(session)?;
        let mut messages = vec![
            json!({
                "role": "system",
                "content": self.build_system_prompt(
                    &lang,
                    bootstrap_turn,
                    session
                        .get("webSearchEnabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                ),
            }),
            json!({
                "role": "user",
                "content": self.build_user_prompt(&lang, user_message, &context_payload),
            }),
        ];
        let mut cards = Vec::<Value>::new();
        let mut trace_steps = Vec::<Value>::new();

        for step in 0..MAX_AGENT_STEPS {
            let stage_owned = session
                .get("workflowStage")
                .and_then(Value::as_str)
                .unwrap_or(WORKFLOW_UNDERSTAND)
                .to_string();
            let stage = stage_owned.as_str();
            let web_search_allowed = session
                .get("webSearchEnabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let tool_defs = {
                let tool_ctx = ToolContext {
                    workflow: ToolWorkflow::Advisor,
                    stage,
                    session: Some(session),
                    bootstrap_turn,
                    response_language: &lang,
                    web_search_allowed,
                    search_remaining: 0,
                };
                self.tool_registry.definitions(&tool_ctx)
            };
            let session_id = session
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let completion = self
                .llm
                .complete_with_tools(&messages, &tool_defs, Some(&session_id))
                .await?;
            trace_steps.push(json!({
                "step": step,
                "route": {
                    "endpoint": completion.route.endpoint,
                    "model": completion.route.model,
                },
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
            messages.push(normalize_assistant_message(&completion.raw_message));

            if completion.tool_calls.is_empty() {
                let reply = if bootstrap_turn {
                    normalize_bootstrap_reply(&completion.assistant_text)
                } else {
                    completion.assistant_text.trim().to_string()
                };
                if reply.is_empty() {
                    return Err(local_text(
                        &lang,
                        "顾问返回了空回复，请重试。",
                        "The advisor returned an empty reply. Please try again.",
                    )
                    .to_string());
                }
                return Ok(AgentTurnResult {
                    reply,
                    cards,
                    trace: json!({ "steps": trace_steps }),
                });
            }

            for tool_call in completion.tool_calls {
                let operation_id = crate::diagnostics::new_operation_id();
                let tool_started_at = Instant::now();
                crate::diagnostics::record_state_event(
                    self.state,
                    "info",
                    "advisor",
                    "advisor_tool_call",
                    Some(&operation_id),
                    "advisor tool call started",
                    json!({
                        "sessionId": session_id.clone(),
                        "step": step,
                        "tool": tool_call.name.clone(),
                        "arguments": tool_call.arguments.clone(),
                    }),
                    None,
                    None,
                );
                let dispatch_stage_owned = session
                    .get("workflowStage")
                    .and_then(Value::as_str)
                    .unwrap_or(WORKFLOW_UNDERSTAND)
                    .to_string();
                let dispatch_stage = dispatch_stage_owned.as_str();
                let dispatch_web_search_allowed = session
                    .get("webSearchEnabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let dispatch_result = {
                    let mut tool_ctx = ToolExecutionContext {
                        workflow: ToolWorkflow::Advisor,
                        stage: dispatch_stage,
                        session: Some(session),
                        bootstrap_turn,
                        response_language: &lang,
                        web_search_allowed: dispatch_web_search_allowed,
                        search_remaining: 0,
                        state: Some(self.state),
                        search_api_key: None,
                        diagnostics: None,
                    };
                    let parsed_call = ParsedToolCall {
                        id: tool_call.id.clone(),
                        name: tool_call.name.clone(),
                        arguments: tool_call.arguments.clone(),
                    };
                    self.tool_registry
                        .dispatch(&mut tool_ctx, &parsed_call)
                        .await
                };
                match dispatch_result {
                    Ok(result) => {
                        let envelope = result.envelope();
                        crate::diagnostics::record_state_event(
                            self.state,
                            "info",
                            "advisor",
                            "advisor_tool_result",
                            Some(&operation_id),
                            "advisor tool call succeeded",
                            json!({
                                "sessionId": session_id.clone(),
                                "step": step,
                                "tool": tool_call.name.clone(),
                                "result": envelope.clone(),
                            }),
                            None,
                            Some(tool_started_at.elapsed()),
                        );
                        if let Some(card) = result.card.clone().filter(|value| !value.is_null()) {
                            cards.push(card);
                        }
                        if !result.cards.is_empty() {
                            cards.extend(result.cards.clone());
                        }
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_call.id,
                            "content": serialize_tool_result_content(&result.result),
                        }));
                    }
                    Err(message) => {
                        crate::diagnostics::record_state_event(
                            self.state,
                            "error",
                            "advisor",
                            "advisor_tool_result",
                            Some(&operation_id),
                            "advisor tool call failed",
                            json!({
                                "sessionId": session_id.clone(),
                                "step": step,
                                "tool": tool_call.name.clone(),
                            }),
                            Some(json!({ "message": message.clone() })),
                            Some(tool_started_at.elapsed()),
                        );
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_call.id,
                            "content": message,
                        }));
                    }
                }
            }
        }

        Err(local_text(
            &lang,
            "顾问在当前轮次内没有收敛，请重试。",
            "The advisor did not converge within this turn. Please try again.",
        )
        .to_string())
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

    fn build_user_prompt(
        &self,
        lang: &str,
        user_message: Option<&str>,
        context_payload: &Value,
    ) -> String {
        let current_message = user_message
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if lang.eq_ignore_ascii_case("en") {
                    "Please give the first-turn advice for this directory.".to_string()
                } else {
                    "请基于当前目录给出首轮建议。".to_string()
                }
            });
        format!(
            "context payload:\n{}\n\n当前轮用户消息:\n{}\n\n请按需要调用工具；若信息已经足够，再给最终自然语言回复。",
            serde_json::to_string_pretty(context_payload).unwrap_or_else(|_| "{}".to_string()),
            current_message
        )
    }

    fn build_context_payload(&self, session: &Value) -> Result<Value, String> {
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let memories = self.tools.list_preferences_tool(Some(session_id))?;
        let overview =
            self.tools
                .get_directory_overview_tool(session, Some("summaryTree"), None, None)?;
        let active_selection_card = session
            .get("activeSelectionId")
            .and_then(Value::as_str)
            .map(|selection_id| self.selection_card_summary(selection_id))
            .transpose()?
            .flatten();
        let active_preview_card = session
            .get("activePreviewId")
            .and_then(Value::as_str)
            .map(|preview_id| self.preview_card_summary(preview_id))
            .transpose()?
            .flatten();
        let latest_execution_card = self.latest_execution_summary(session_id)?;
        Ok(json!({
            "sessionId": session.get("sessionId").cloned().unwrap_or(Value::Null),
            "workflowStage": session.get("workflowStage").cloned().unwrap_or(Value::String(WORKFLOW_UNDERSTAND.to_string())),
            "rollbackAvailable": session.get("rollbackAvailable").cloned().unwrap_or(Value::Bool(false)),
            "rootPath": session.get("rootPath").cloned().unwrap_or(Value::Null),
            "webSearch": {
                "useWebSearch": session.get("useWebSearch").cloned().unwrap_or(Value::Bool(false)),
                "webSearchEnabled": session.get("webSearchEnabled").cloned().unwrap_or(Value::Bool(false)),
            },
            "memory": {
                "session": memories.get("sessionPreferences").cloned().unwrap_or_else(|| json!([])),
                "global": memories.get("globalPreferences").cloned().unwrap_or_else(|| json!([])),
            },
            "overview": {
                "viewType": overview.get("viewType").cloned().unwrap_or(Value::String("summaryTree".to_string())),
                "treeText": overview.get("treeText").cloned().unwrap_or(Value::String(String::new())),
            },
            "activeSelectionCard": active_selection_card,
            "activePreviewCard": active_preview_card,
            "latestExecutionCard": latest_execution_card,
        }))
    }

    fn selection_card_summary(&self, selection_id: &str) -> Result<Option<Value>, String> {
        let Some(selection) = persist::load_advisor_selection(&self.state.db_path(), selection_id)?
        else {
            return Ok(None);
        };
        Ok(Some(json!({
            "selectionId": selection.get("selectionId").cloned().unwrap_or(Value::Null),
            "querySummary": selection.get("querySummary").cloned().unwrap_or(Value::String(String::new())),
            "total": selection.get("total").cloned().unwrap_or(Value::from(0)),
        })))
    }

    fn preview_card_summary(&self, preview_id: &str) -> Result<Option<Value>, String> {
        let Some(job) = persist::load_advisor_plan_job(&self.state.db_path(), preview_id)? else {
            return Ok(None);
        };
        Ok(Some(json!({
            "previewId": preview_id,
            "planId": preview_id,
            "intentSummary": job.pointer("/preview/intentSummary").cloned().unwrap_or(Value::String(String::new())),
            "topActions": job.pointer("/preview/entries").and_then(Value::as_array).map(|rows| {
                rows.iter().take(3).map(|row| format!(
                    "{} -> {}",
                    row.get("name").and_then(Value::as_str).or_else(|| row.get("sourcePath").and_then(Value::as_str)).unwrap_or("-"),
                    row.get("action").and_then(Value::as_str).unwrap_or("-")
                )).collect::<Vec<_>>()
            }).unwrap_or_default(),
            "summary": job.pointer("/preview/summary").cloned().unwrap_or_else(|| json!({})),
            "topBlockedReasons": job.pointer("/preview/entries").and_then(Value::as_array).map(|rows| {
                rows.iter()
                    .flat_map(|row| row.get("warnings").and_then(Value::as_array).cloned().unwrap_or_default())
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .take(3)
                    .collect::<Vec<_>>()
            }).unwrap_or_default(),
        })))
    }

    fn latest_execution_summary(&self, session_id: &str) -> Result<Option<Value>, String> {
        let cards = persist::load_advisor_cards(&self.state.db_path(), session_id)?;
        let latest = cards
            .into_iter()
            .rev()
            .find(|card| card.get("cardType").and_then(Value::as_str) == Some(CARD_EXECUTION));
        Ok(latest.map(|card| {
            json!({
                "jobId": card.pointer("/body/jobId").cloned().unwrap_or(Value::Null),
                "intentSummary": card.pointer("/body/intentSummary").cloned().unwrap_or(Value::String(String::new())),
                "summary": card.pointer("/body/result/summary").cloned().unwrap_or_else(|| json!({})),
            })
        }))
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

fn normalize_assistant_message(message: &Value) -> Value {
    let mut obj = message.as_object().cloned().unwrap_or_default();
    obj.insert("role".to_string(), Value::String("assistant".to_string()));
    Value::Object(obj)
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
