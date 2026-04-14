use super::llm::AdvisorLlm;
use super::tools::ToolService;
use super::types::{
    local_text, now_iso, CARD_EXECUTION, CARD_PLAN_PREVIEW, CARD_PREFERENCE, CARD_RECLASS,
    CARD_TREE, WORKFLOW_EXECUTE_READY, WORKFLOW_PREVIEW_READY, WORKFLOW_UNDERSTAND,
};
use crate::backend::AppState;
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
    tools: ToolService<'a>,
    llm: AdvisorLlm<'a>,
}

impl<'a> AdvisorAgentRunner<'a> {
    pub(super) fn new(state: &'a AppState) -> Self {
        Self {
            state,
            tools: ToolService::new(state),
            llm: AdvisorLlm::new(state),
        }
    }

    pub(super) async fn run_bootstrap_turn(
        &self,
        session: &mut Value,
    ) -> Result<AgentTurnResult, String> {
        self.run_turn(session, None).await
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
                "content": self.build_system_prompt(&lang, bootstrap_turn),
            }),
            json!({
                "role": "user",
                "content": self.build_user_prompt(&lang, user_message, &context_payload),
            }),
        ];
        let mut cards = Vec::<Value>::new();
        let mut trace_steps = Vec::<Value>::new();

        for step in 0..MAX_AGENT_STEPS {
            let available_tools = available_tools_for_stage(
                session
                    .get("workflowStage")
                    .and_then(Value::as_str)
                    .unwrap_or(WORKFLOW_UNDERSTAND),
                session,
                bootstrap_turn,
            );
            let tool_defs = build_tool_definitions(&available_tools);
            let completion = self.llm.complete_with_tools(&messages, &tool_defs).await?;
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
                match self
                    .dispatch_tool(session, &tool_call.name, &tool_call.arguments)
                    .await
                {
                    Ok(result) => {
                        if let Some(card) = result.get("card").cloned().filter(|value| !value.is_null()) {
                            cards.push(card);
                        }
                        if let Some(extra_cards) = result.get("cards").and_then(Value::as_array).cloned() {
                            cards.extend(extra_cards);
                        }
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_call.id,
                            "content": serde_json::to_string_pretty(result.get("result").unwrap_or(&result))
                                .unwrap_or_else(|_| "{}".to_string()),
                        }));
                    }
                    Err(message) => {
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

    fn build_system_prompt(&self, lang: &str, bootstrap_turn: bool) -> String {
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
        let overview = self
            .tools
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
        let latest = cards.into_iter().rev().find(|card| {
            card.get("cardType").and_then(Value::as_str) == Some(CARD_EXECUTION)
        });
        Ok(latest.map(|card| {
            json!({
                "jobId": card.pointer("/body/jobId").cloned().unwrap_or(Value::Null),
                "intentSummary": card.pointer("/body/intentSummary").cloned().unwrap_or(Value::String(String::new())),
                "summary": card.pointer("/body/result/summary").cloned().unwrap_or_else(|| json!({})),
            })
        }))
    }

    async fn dispatch_tool(
        &self,
        session: &mut Value,
        tool: &str,
        arguments: &Value,
    ) -> Result<Value, String> {
        let lang = session
            .get("responseLanguage")
            .and_then(Value::as_str)
            .unwrap_or("zh")
            .to_string();
        let stage = session
            .get("workflowStage")
            .and_then(Value::as_str)
            .unwrap_or(WORKFLOW_UNDERSTAND);

        match tool {
            "get_directory_overview" => {
                let result = self.tools.get_directory_overview_tool(
                    session,
                    arguments.get("viewType").and_then(Value::as_str),
                    arguments.get("rootCategoryId").and_then(Value::as_str),
                    arguments.get("maxDepth").and_then(Value::as_u64),
                )?;
                Ok(json!({
                    "result": result,
                    "card": {
                        "cardType": CARD_TREE,
                        "status": "ready",
                        "title": local_text(&lang, "当前分类树", "Current Tree"),
                        "body": {
                            "tree": result.get("tree").cloned().unwrap_or(Value::Null),
                            "stats": {
                                "itemCount": session.pointer("/contextBar/inventorySummary/itemCount").cloned().unwrap_or(Value::from(0))
                            }
                        },
                        "actions": []
                    }
                }))
            }
            "find_files" => {
                if !matches!(stage, WORKFLOW_UNDERSTAND | WORKFLOW_PREVIEW_READY) {
                    return Err(
                        "当前阶段不适合重新筛选，请先处理已有预览或回到理解阶段。"
                            .to_string(),
                    );
                }
                let result = self.tools.find_files_by_args(session, arguments)?;
                if let Some(obj) = session.as_object_mut() {
                    obj.insert(
                        "activeSelectionId".to_string(),
                        result.get("selectionId").cloned().unwrap_or(Value::Null),
                    );
                    obj.insert("activePreviewId".to_string(), Value::Null);
                    obj.insert(
                        "workflowStage".to_string(),
                        Value::String(WORKFLOW_PREVIEW_READY.to_string()),
                    );
                }
                Ok(json!({ "result": result }))
            }
            "summarize_files" => Ok(json!({
                "result": self.tools.summarize_files_tool(session, arguments).await?
            })),
            "read_only_file_summaries" => Ok(json!({
                "result": self.tools.read_only_file_summaries_tool(session, arguments)?
            })),
            "capture_preference" => {
                let result = self.tools.capture_preference(
                    session,
                    arguments.get("scope").and_then(Value::as_str).unwrap_or("session"),
                    arguments.get("text").and_then(Value::as_str).unwrap_or_default(),
                    arguments
                        .get("sourceMessage")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )?;
                Ok(json!({
                    "result": result,
                    "card": {
                        "cardType": CARD_PREFERENCE,
                        "status": "pending",
                        "title": local_text(&lang, "偏好提炼", "Preference Draft"),
                        "body": {
                            "preferenceId": result.get("preferenceId").cloned().unwrap_or(Value::Null),
                            "kind": result.get("kind").cloned().unwrap_or(Value::Null),
                            "suggestedScope": result.get("suggestedScope").cloned().unwrap_or(Value::String("session".to_string())),
                            "summary": result.get("summary").cloned().unwrap_or(Value::String(String::new())),
                            "message": result.get("sourceMessage").cloned().unwrap_or(Value::String(String::new())),
                        },
                        "actions": [
                            { "action": "apply_preference", "label": local_text(&lang, "应用 / 保存", "Apply / Save"), "variant": "primary" },
                            { "action": "dismiss_preference", "label": local_text(&lang, "撤销", "Dismiss"), "variant": "secondary" }
                        ]
                    }
                }))
            }
            "list_preferences" => Ok(json!({
                "result": self.tools.list_preferences_tool(session.get("sessionId").and_then(Value::as_str))?
            })),
            "preview_plan" => self.dispatch_preview_plan(session, arguments, &lang),
            "execute_plan" => self.dispatch_execute_plan(session, arguments, &lang),
            "rollback_plan" => self.dispatch_rollback_plan(session, arguments, &lang),
            "apply_reclassification" => {
                self.dispatch_apply_reclassification(session, arguments, &lang)
            }
            "rollback_reclassification" => {
                self.dispatch_rollback_reclassification(session, arguments, &lang)
            }
            other => Err(format!("unsupported advisor tool: {other}")),
        }
    }

    fn dispatch_preview_plan(
        &self,
        session: &mut Value,
        arguments: &Value,
        lang: &str,
    ) -> Result<Value, String> {
        let stage = session
            .get("workflowStage")
            .and_then(Value::as_str)
            .unwrap_or(WORKFLOW_UNDERSTAND);
        if !matches!(stage, WORKFLOW_PREVIEW_READY | WORKFLOW_EXECUTE_READY) {
            return Err("当前阶段缺少有效筛选结果，请先调用 find_files。".to_string());
        }
        let job = self
            .tools
            .preview_plan_from_value(session, arguments.get("plan").unwrap_or(arguments))?;
        let preview = job.get("preview").cloned().unwrap_or_else(|| json!({}));
        if let Some(obj) = session.as_object_mut() {
            obj.insert(
                "workflowStage".to_string(),
                Value::String(WORKFLOW_EXECUTE_READY.to_string()),
            );
            obj.insert(
                "activeSelectionId".to_string(),
                job.get("selectionId").cloned().unwrap_or(Value::Null),
            );
            obj.insert(
                "activePreviewId".to_string(),
                Value::String(
                    preview
                        .get("previewId")
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| {
                            job.get("jobId").and_then(Value::as_str).unwrap_or_default()
                        })
                        .to_string(),
                ),
            );
        }
        Ok(json!({
            "result": preview,
            "card": {
                "cardType": CARD_PLAN_PREVIEW,
                "status": "ready",
                "title": local_text(lang, "计划预览", "Plan Preview"),
                "body": {
                    "previewId": preview.get("previewId").cloned().unwrap_or(job.get("jobId").cloned().unwrap_or(Value::Null)),
                    "jobId": job.get("jobId").cloned().unwrap_or(Value::Null),
                    "selectionId": job.get("selectionId").cloned().unwrap_or(Value::Null),
                    "intentSummary": preview.get("intentSummary").cloned().unwrap_or(Value::String(String::new())),
                    "summary": preview.get("summary").cloned().unwrap_or_else(|| json!({})),
                    "entries": preview.get("entries").cloned().unwrap_or_else(|| json!([])),
                },
                "actions": if preview.pointer("/summary/canExecute").and_then(Value::as_u64).unwrap_or(0) > 0 {
                    vec![json!({ "action": "execute_plan", "label": local_text(lang, "执行", "Execute"), "variant": "primary" })]
                } else {
                    Vec::<Value>::new()
                }
            }
        }))
    }

    fn dispatch_execute_plan(
        &self,
        session: &mut Value,
        arguments: &Value,
        lang: &str,
    ) -> Result<Value, String> {
        let preview_id = arguments
            .get("previewId")
            .and_then(Value::as_str)
            .ok_or_else(|| "当前预览不存在或已过期，请先重新生成 preview，再执行。".to_string())?;
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (_job, result, rollback) =
            self.tools.execute_plan_by_preview_id(session_id, preview_id)?;
        if let Some(obj) = session.as_object_mut() {
            obj.insert(
                "workflowStage".to_string(),
                Value::String(WORKFLOW_UNDERSTAND.to_string()),
            );
            obj.insert("activeSelectionId".to_string(), Value::Null);
            obj.insert("activePreviewId".to_string(), Value::Null);
            obj.insert(
                "rollbackAvailable".to_string(),
                Value::Bool(rollback.get("available").and_then(Value::as_bool).unwrap_or(false)),
            );
        }
        Ok(json!({
            "result": {
                "jobId": preview_id,
                "result": result,
                "rollbackAvailable": rollback.get("available").cloned().unwrap_or(Value::Bool(false)),
            },
            "card": {
                "cardType": CARD_EXECUTION,
                "status": "done",
                "title": local_text(lang, "执行结果", "Execution Result"),
                "body": {
                    "jobId": preview_id,
                    "intentSummary": arguments.get("intentSummary").cloned().unwrap_or(Value::String(String::new())),
                    "result": result,
                    "rollbackAvailable": rollback.get("available").cloned().unwrap_or(Value::Bool(false)),
                },
                "actions": if rollback.get("available").and_then(Value::as_bool).unwrap_or(false) {
                    vec![json!({ "action": "rollback_plan", "label": local_text(lang, "撤销", "Rollback"), "variant": "secondary" })]
                } else {
                    Vec::<Value>::new()
                }
            }
        }))
    }

    fn dispatch_rollback_plan(
        &self,
        session: &mut Value,
        arguments: &Value,
        lang: &str,
    ) -> Result<Value, String> {
        let job_id = arguments
            .get("jobId")
            .and_then(Value::as_str)
            .ok_or_else(|| "当前执行记录不可回滚，请不要继续尝试回滚这个任务。".to_string())?;
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (_job, result) = self.tools.rollback_plan(session_id, job_id)?;
        if let Some(obj) = session.as_object_mut() {
            obj.insert("rollbackAvailable".to_string(), Value::Bool(false));
        }
        Ok(json!({
            "result": {
                "jobId": job_id,
                "result": result,
                "rollbackAvailable": false,
            },
            "card": {
                "cardType": CARD_EXECUTION,
                "status": "rolled_back",
                "title": local_text(lang, "回滚结果", "Rollback Result"),
                "body": {
                    "jobId": job_id,
                    "result": result,
                    "rollbackAvailable": false,
                },
                "actions": []
            }
        }))
    }

    fn dispatch_apply_reclassification(
        &self,
        session: &mut Value,
        arguments: &Value,
        lang: &str,
    ) -> Result<Value, String> {
        let job = self.tools.apply_reclassification_request(
            session,
            arguments.get("request").unwrap_or(arguments),
            arguments
                .get("applyPreferenceCapture")
                .or_else(|| {
                    arguments
                        .get("request")
                        .and_then(|request| request.get("applyPreferenceCapture"))
                })
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )?;
        persist::save_advisor_reclass_job(&self.state.db_path(), &job)?;
        let overview = self
            .tools
            .get_directory_overview_tool(session, Some("summaryTree"), None, None)?;
        if let Some(obj) = session.as_object_mut() {
            obj.insert(
                "workflowStage".to_string(),
                Value::String(WORKFLOW_UNDERSTAND.to_string()),
            );
            obj.insert("activeSelectionId".to_string(), Value::Null);
            obj.insert("activePreviewId".to_string(), Value::Null);
            obj.insert("rollbackAvailable".to_string(), Value::Bool(false));
            obj.insert(
                "derivedTree".to_string(),
                overview.get("tree").cloned().unwrap_or(Value::Null),
            );
        }
        Ok(json!({
            "result": job.pointer("/result").cloned().unwrap_or(Value::Null),
            "cards": [
                {
                    "cardType": CARD_RECLASS,
                    "status": "done",
                    "title": local_text(lang, "分类修改结果", "Reclassification Result"),
                    "body": {
                        "jobId": job.get("jobId").cloned().unwrap_or(Value::Null),
                        "reclassificationJobId": job.get("jobId").cloned().unwrap_or(Value::Null),
                        "summary": job.pointer("/result/message").cloned().unwrap_or(Value::String(String::new())),
                        "message": job.pointer("/result/message").cloned().unwrap_or(Value::String(String::new())),
                        "changeSummary": job.pointer("/result/changeSummary").cloned().unwrap_or(Value::String(String::new())),
                        "updatedTreeText": job.pointer("/result/updatedTreeText").cloned().unwrap_or(Value::String(String::new())),
                        "invalidated": job.pointer("/result/invalidated").cloned().unwrap_or_else(|| json!([])),
                    },
                    "actions": []
                },
                {
                    "cardType": CARD_TREE,
                    "status": "ready",
                    "title": local_text(lang, "当前分类树", "Current Tree"),
                    "body": {
                        "tree": overview.get("tree").cloned().unwrap_or(Value::Null)
                    },
                    "actions": []
                }
            ]
        }))
    }

    fn dispatch_rollback_reclassification(
        &self,
        session: &mut Value,
        arguments: &Value,
        lang: &str,
    ) -> Result<Value, String> {
        let job_id = arguments
            .get("reclassificationJobId")
            .or_else(|| arguments.get("jobId"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                "当前归类修改记录不存在或不可回滚，请先确认最近一次归类修改是否成功。"
                    .to_string()
            })?;
        let (_job, result, tree) = self.tools.rollback_reclassification(session, job_id)?;
        let overview = self
            .tools
            .get_directory_overview_tool(session, Some("summaryTree"), None, None)?;
        if let Some(obj) = session.as_object_mut() {
            obj.insert("activeSelectionId".to_string(), Value::Null);
            obj.insert("activePreviewId".to_string(), Value::Null);
            obj.insert("derivedTree".to_string(), tree.unwrap_or(Value::Null));
        }
        Ok(json!({
            "result": result,
            "cards": [
                {
                    "cardType": CARD_RECLASS,
                    "status": "rolled_back",
                    "title": local_text(lang, "分类回滚结果", "Reclassification Rollback"),
                    "body": {
                        "jobId": job_id,
                        "message": result.get("message").cloned().unwrap_or(Value::String(String::new())),
                        "updatedTreeText": result.get("updatedTreeText").cloned().unwrap_or(Value::String(String::new())),
                        "rolledBack": result.get("rolledBack").cloned().unwrap_or(Value::Bool(true)),
                        "invalidated": result.get("invalidated").cloned().unwrap_or_else(|| json!([])),
                    },
                    "actions": []
                },
                {
                    "cardType": CARD_TREE,
                    "status": "ready",
                    "title": local_text(lang, "当前分类树", "Current Tree"),
                    "body": {
                        "tree": overview.get("tree").cloned().unwrap_or(Value::Null)
                    },
                    "actions": []
                }
            ]
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

fn available_tools_for_stage(
    stage: &str,
    session: &Value,
    bootstrap_turn: bool,
) -> Vec<&'static str> {
    if bootstrap_turn {
        return vec![
            "get_directory_overview",
            "list_preferences",
            "read_only_file_summaries",
            "summarize_files",
        ];
    }

    let mut tools = vec![
        "get_directory_overview",
        "summarize_files",
        "read_only_file_summaries",
        "capture_preference",
        "list_preferences",
        "apply_reclassification",
        "rollback_reclassification",
        "rollback_plan",
    ];
    match stage {
        WORKFLOW_PREVIEW_READY => {
            tools.push("find_files");
            tools.push("preview_plan");
        }
        WORKFLOW_EXECUTE_READY => {
            tools.push("preview_plan");
            tools.push("execute_plan");
        }
        _ => {
            tools.push("find_files");
        }
    }
    if session
        .get("rollbackAvailable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && !tools.contains(&"rollback_plan")
    {
        tools.push("rollback_plan");
    }
    tools
}

fn build_tool_definitions(available_tools: &[&str]) -> Vec<Value> {
    available_tools
        .iter()
        .filter_map(|name| tool_definition(name))
        .collect()
}

fn tool_definition(name: &str) -> Option<Value> {
    Some(match name {
        "get_directory_overview" => json!({
            "type": "function",
            "function": {
                "name": "get_directory_overview",
                "description": "查看当前目录树视图和整体概览。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "viewType": { "type": "string", "enum": ["summaryTree", "sizeTree", "timeTree", "executionTree", "partialTree"] },
                        "rootCategoryId": { "type": "string" },
                        "maxDepth": { "type": "integer" }
                    }
                }
            }
        }),
        "find_files" => json!({
            "type": "function",
            "function": {
                "name": "find_files",
                "description": "按类别、名称、路径、大小和时间筛选文件候选集，并返回 selectionId。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "categoryIds": { "type": "array", "items": { "type": "string" } },
                        "nameQuery": { "type": "string" },
                        "nameExact": { "type": "string" },
                        "pathContains": { "type": "string" },
                        "extensions": { "type": "array", "items": { "type": "string" } },
                        "minSizeBytes": { "type": "integer" },
                        "maxSizeBytes": { "type": "integer" },
                        "olderThanDays": { "type": "integer" },
                        "newerThanDays": { "type": "integer" },
                        "sortBy": { "type": "string", "enum": ["name", "size", "modifiedAt"] },
                        "sortOrder": { "type": "string", "enum": ["asc", "desc"] },
                        "limit": { "type": "integer" }
                    }
                }
            }
        }),
        "summarize_files" => json!({
            "type": "function",
            "function": {
                "name": "summarize_files",
                "description": "为一批文件补摘要或刷新摘要，并写入顾问摘要库。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "paths": { "type": "array", "items": { "type": "string" } },
                        "categoryIds": { "type": "array", "items": { "type": "string" } },
                        "mode": { "type": "string", "enum": ["metadata_summary", "model_summary_short", "model_summary_normal"] },
                        "missingOnly": { "type": "boolean" },
                        "batchSize": { "type": "integer" },
                        "maxConcurrency": { "type": "integer" }
                    }
                }
            }
        }),
        "read_only_file_summaries" => json!({
            "type": "function",
            "function": {
                "name": "read_only_file_summaries",
                "description": "只读取已有摘要，不触发生成。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "paths": { "type": "array", "items": { "type": "string" } },
                        "categoryIds": { "type": "array", "items": { "type": "string" } },
                        "detailLevel": { "type": "string", "enum": ["short", "normal"] },
                        "limit": { "type": "integer" }
                    }
                }
            }
        }),
        "capture_preference" => json!({
            "type": "function",
            "function": {
                "name": "capture_preference",
                "description": "提炼并暂存用户偏好，后续由卡片动作确认保存。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "scope": { "type": "string", "enum": ["session", "global"] },
                        "text": { "type": "string" },
                        "sourceMessage": { "type": "string" }
                    },
                    "required": ["text", "sourceMessage"]
                }
            }
        }),
        "list_preferences" => json!({
            "type": "function",
            "function": {
                "name": "list_preferences",
                "description": "读取当前会话和全局偏好。"
            }
        }),
        "preview_plan" => json!({
            "type": "function",
            "function": {
                "name": "preview_plan",
                "description": "根据 plan JSON 和 selectionId 生成文件级预览。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "plan": {
                            "type": "object",
                            "properties": {
                                "intentSummary": { "type": "string" },
                                "targets": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "selectionId": { "type": "string" },
                                            "action": { "type": "string", "enum": ["archive", "move", "keep", "review", "delete"] }
                                        },
                                        "required": ["selectionId", "action"]
                                    }
                                }
                            },
                            "required": ["targets"]
                        }
                    },
                    "required": ["plan"]
                }
            }
        }),
        "execute_plan" => json!({
            "type": "function",
            "function": {
                "name": "execute_plan",
                "description": "执行已经通过预览的计划。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "previewId": { "type": "string" },
                        "intentSummary": { "type": "string" }
                    },
                    "required": ["previewId"]
                }
            }
        }),
        "rollback_plan" => json!({
            "type": "function",
            "function": {
                "name": "rollback_plan",
                "description": "回滚最近一次可回滚的执行计划。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "jobId": { "type": "string" }
                    },
                    "required": ["jobId"]
                }
            }
        }),
        "apply_reclassification" => json!({
            "type": "function",
            "function": {
                "name": "apply_reclassification",
                "description": "按结构化 reclassificationRequest 应用局部分类修正。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "request": {
                            "type": "object",
                            "properties": {
                                "intentSummary": { "type": "string" },
                                "change": {
                                    "type": "object",
                                    "properties": {
                                        "type": { "type": "string", "enum": ["rename_category", "move_selection_to_category", "split_selection_to_new_category", "merge_category_into_category", "delete_empty_category"] },
                                        "selectionId": { "type": "string" },
                                        "sourceCategoryId": { "type": "string" },
                                        "targetCategoryId": { "type": "string" },
                                        "newCategoryName": { "type": "string" }
                                    },
                                    "required": ["type"]
                                }
                            },
                            "required": ["change"]
                        },
                        "applyPreferenceCapture": { "type": "boolean" }
                    },
                    "required": ["request"]
                }
            }
        }),
        "rollback_reclassification" => json!({
            "type": "function",
            "function": {
                "name": "rollback_reclassification",
                "description": "回滚最近一次可回滚的分类修正。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "reclassificationJobId": { "type": "string" }
                    },
                    "required": ["reclassificationJobId"]
                }
            }
        }),
        _ => return None,
    })
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
