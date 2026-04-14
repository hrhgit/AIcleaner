use super::agent::{materialize_card, AdvisorAgentRunner};
use super::payload::build_session_payload;
use super::tools::ToolService;
use super::types::{
    local_text, now_iso, AdvisorCardActionInput, AdvisorMessageSendInput, CARD_EXECUTION,
    WORKFLOW_UNDERSTAND,
};
use crate::backend::AppState;
use crate::persist;
use serde_json::{json, Value};
use uuid::Uuid;

pub(super) struct ConversationOrchestrator<'a> {
    state: &'a AppState,
    tools: ToolService<'a>,
    agent: AdvisorAgentRunner<'a>,
}

impl<'a> ConversationOrchestrator<'a> {
    pub(super) fn new(state: &'a AppState) -> Self {
        Self {
            state,
            tools: ToolService::new(state),
            agent: AdvisorAgentRunner::new(state),
        }
    }

    pub(super) async fn handle_message(
        &self,
        input: AdvisorMessageSendInput,
    ) -> Result<Value, String> {
        let message = input.message.trim().to_string();
        if message.is_empty() {
            return Err("message is required".to_string());
        }
        let mut session = super::load_session(self.state, input.session_id.trim())?;
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| "sessionId is required".to_string())?
            .to_string();

        persist::create_advisor_turn(&self.state.db_path(), &session_id, "user", &message)?;
        match self.agent.run_user_turn(&mut session, &message).await {
            Ok(agent_result) => {
                self.store_trace(&mut session, &agent_result.trace);
                super::save_session(self.state, &mut session)?;

                let assistant_turn = persist::create_advisor_turn(
                    &self.state.db_path(),
                    &session_id,
                    "assistant",
                    &agent_result.reply,
                )?;
                let assistant_turn_id = assistant_turn
                    .get("turnId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                for card in agent_result.cards {
                    persist::save_advisor_card(
                        &self.state.db_path(),
                        &materialize_card(&session_id, assistant_turn_id, &card),
                    )?;
                }
            }
            Err(error) => {
                super::save_session(self.state, &mut session)?;
                persist::create_advisor_turn(
                    &self.state.db_path(),
                    &session_id,
                    "assistant",
                    &error,
                )?;
            }
        }

        let session = super::load_session(self.state, &session_id)?;
        build_session_payload(self.state, &session)
    }

    pub(super) fn handle_card_action(
        &self,
        input: AdvisorCardActionInput,
    ) -> Result<Value, String> {
        let mut session = super::load_session(self.state, input.session_id.trim())?;
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| "sessionId is required".to_string())?
            .to_string();

        if input.action.trim() == "toggle_context_bar" {
            self.toggle_context_bar(&mut session, input.payload.as_ref())?;
            super::save_session(self.state, &mut session)?;
            let session = super::load_session(self.state, &session_id)?;
            return build_session_payload(self.state, &session);
        }

        let action = input.action.trim();
        let card_id = input.card_id.trim();
        if card_id.is_empty() {
            return Err(format!("cardId is required for advisor card action: {action}"));
        }
        let mut card = self.load_action_card(&session_id, card_id)?;
        match action {
            "dismiss_preference" => self.dismiss_preference(&mut card)?,
            "apply_preference" => {
                self.apply_preference(&session_id, &mut session, &mut card, input.payload.as_ref())?
            }
            "execute_plan" => self.execute_plan(&mut session, &card)?,
            "rollback_plan" => self.rollback_plan(&mut session, &card)?,
            "apply_reclassification" => self.apply_reclassification(&mut session, &mut card)?,
            "rollback_reclassification" => self.rollback_reclassification(&mut session, &card)?,
            other => return Err(format!("unsupported advisor card action: {other}")),
        }

        super::save_session(self.state, &mut session)?;
        let session = super::load_session(self.state, &session_id)?;
        build_session_payload(self.state, &session)
    }

    fn toggle_context_bar(&self, session: &mut Value, payload: Option<&Value>) -> Result<(), String> {
        let collapsed = payload
            .and_then(|value| value.get("collapsed"))
            .and_then(Value::as_bool)
            .unwrap_or_else(|| {
                session
                    .pointer("/contextBar/collapsed")
                    .and_then(Value::as_bool)
                    .map(|value| !value)
                    .unwrap_or(false)
            });
        if let Some(context_bar) = session.get_mut("contextBar").and_then(Value::as_object_mut) {
            context_bar.insert("collapsed".to_string(), Value::Bool(collapsed));
        } else if let Some(obj) = session.as_object_mut() {
            obj.insert("contextBar".to_string(), json!({ "collapsed": collapsed }));
        }
        Ok(())
    }

    fn dismiss_preference(&self, card: &mut Value) -> Result<(), String> {
        self.update_card(card, "dismissed", Vec::new());
        persist::save_advisor_card(&self.state.db_path(), card)
    }

    fn apply_preference(
        &self,
        session_id: &str,
        session: &mut Value,
        card: &mut Value,
        payload: Option<&Value>,
    ) -> Result<(), String> {
        let raw_scope = payload
            .and_then(|value| value.get("scope"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                card.pointer("/body/suggestedScope")
                    .and_then(Value::as_str)
                    .unwrap_or("session")
            });
        let scope = if raw_scope == "global" {
            "global"
        } else {
            "session"
        };
        let memory = json!({
            "memoryId": card.pointer("/body/preferenceId").cloned().unwrap_or(Value::String(Uuid::new_v4().to_string())),
            "sessionId": if scope == "global" { Value::Null } else { Value::String(session_id.to_string()) },
            "scope": scope,
            "enabled": true,
            "text": card.pointer("/body/message").and_then(Value::as_str).unwrap_or_default(),
            "summary": card.pointer("/body/summary").cloned().unwrap_or(Value::Null),
            "kind": card.pointer("/body/kind").cloned().unwrap_or(Value::Null),
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
        });
        persist::save_advisor_memory(&self.state.db_path(), &memory)?;
        self.update_card(card, "applied", Vec::new());
        persist::save_advisor_card(&self.state.db_path(), card)?;
        self.refresh_session_overview(session)?;
        persist::create_advisor_turn(
            &self.state.db_path(),
            session_id,
            "assistant",
            local_text(
                session
                    .get("responseLanguage")
                    .and_then(Value::as_str)
                    .unwrap_or("zh"),
                "偏好已保存，后续建议会继续遵守它。",
                "The preference is saved and will shape later advice.",
            ),
        )?;
        Ok(())
    }

    fn execute_plan(&self, session: &mut Value, card: &Value) -> Result<(), String> {
        let lang = session
            .get("responseLanguage")
            .and_then(Value::as_str)
            .unwrap_or("zh")
            .to_string();
        let preview_id = card
            .pointer("/body/previewId")
            .or_else(|| card.pointer("/body/jobId"))
            .and_then(Value::as_str)
            .ok_or_else(|| "previewId is missing".to_string())?;
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (_job, result, rollback) = self.tools.execute_plan_by_preview_id(session_id, preview_id)?;
        let turn = persist::create_advisor_turn(
            &self.state.db_path(),
            session_id,
            "assistant",
            local_text(
                &lang,
                "计划已执行，下面是程序执行结果。",
                "The plan has been executed. The program result is below.",
            ),
        )?;
        let exec_card = json!({
            "cardId": Uuid::new_v4().to_string(),
            "sessionId": session_id,
            "turnId": turn.get("turnId").and_then(Value::as_str).unwrap_or_default(),
            "cardType": CARD_EXECUTION,
            "status": "done",
            "title": local_text(&lang, "执行结果", "Execution Result"),
            "body": {
                "jobId": preview_id,
                "result": result,
                "rollbackAvailable": rollback.get("available").and_then(Value::as_bool).unwrap_or(false),
            },
            "actions": if rollback.get("available").and_then(Value::as_bool).unwrap_or(false) {
                vec![json!({ "action": "rollback_plan", "label": local_text(&lang, "撤销", "Rollback"), "variant": "secondary" })]
            } else {
                Vec::<Value>::new()
            },
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
        });
        persist::save_advisor_card(&self.state.db_path(), &exec_card)?;
        self.apply_session_state(
            session,
            WORKFLOW_UNDERSTAND,
            None,
            None,
            rollback.get("available").and_then(Value::as_bool).unwrap_or(false),
        );
        Ok(())
    }

    fn rollback_plan(&self, session: &mut Value, card: &Value) -> Result<(), String> {
        let lang = session
            .get("responseLanguage")
            .and_then(Value::as_str)
            .unwrap_or("zh")
            .to_string();
        let job_id = card
            .pointer("/body/jobId")
            .and_then(Value::as_str)
            .ok_or_else(|| "execution jobId is missing".to_string())?;
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (_job, rollback_result) = self.tools.rollback_plan(session_id, job_id)?;
        let turn = persist::create_advisor_turn(
            &self.state.db_path(),
            session_id,
            "assistant",
            local_text(
                &lang,
                "已尝试回滚最近一次可回滚操作。",
                "Rollback has been attempted for the latest reversible operation.",
            ),
        )?;
        let rollback_card = json!({
            "cardId": Uuid::new_v4().to_string(),
            "sessionId": session_id,
            "turnId": turn.get("turnId").and_then(Value::as_str).unwrap_or_default(),
            "cardType": CARD_EXECUTION,
            "status": "rolled_back",
            "title": local_text(&lang, "回滚结果", "Rollback Result"),
            "body": {
                "jobId": job_id,
                "result": rollback_result,
                "rollbackAvailable": false,
            },
            "actions": [],
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
        });
        persist::save_advisor_card(&self.state.db_path(), &rollback_card)?;
        self.apply_session_state(session, WORKFLOW_UNDERSTAND, None, None, false);
        Ok(())
    }

    fn apply_reclassification(&self, session: &mut Value, card: &mut Value) -> Result<(), String> {
        let lang = session
            .get("responseLanguage")
            .and_then(Value::as_str)
            .unwrap_or("zh")
            .to_string();
        let selection_id = card
            .pointer("/body/selectionId")
            .and_then(Value::as_str)
            .ok_or_else(|| "selectionId is missing".to_string())?;
        let category_path = card
            .pointer("/body/categoryPath")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect::<Vec<_>>();
        let job = self
            .tools
            .apply_reclassification(session, selection_id, &category_path)?;
        persist::save_advisor_reclass_job(&self.state.db_path(), &job)?;
        self.update_card(card, "applied", Vec::new());
        persist::save_advisor_card(&self.state.db_path(), card)?;
        self.refresh_session_overview(session)?;
        self.apply_session_state(session, WORKFLOW_UNDERSTAND, None, None, false);
        let turn = persist::create_advisor_turn(
            &self.state.db_path(),
            session.get("sessionId").and_then(Value::as_str).unwrap_or_default(),
            "assistant",
            local_text(
                &lang,
                "已应用分类修正，并刷新当前树。",
                "The reclassification is applied and the tree is refreshed.",
            ),
        )?;
        let result_card = json!({
            "cardId": Uuid::new_v4().to_string(),
            "sessionId": session.get("sessionId").cloned().unwrap_or(Value::Null),
            "turnId": turn.get("turnId").and_then(Value::as_str).unwrap_or_default(),
            "cardType": "reclassification_result",
            "status": "done",
            "title": local_text(&lang, "分类修改结果", "Reclassification Result"),
            "body": {
                "jobId": job.get("jobId").cloned().unwrap_or(Value::Null),
                "summary": job.pointer("/result/summary").cloned().unwrap_or(Value::Null),
            },
            "actions": [],
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
        });
        persist::save_advisor_card(&self.state.db_path(), &result_card)?;
        Ok(())
    }

    fn rollback_reclassification(&self, session: &mut Value, card: &Value) -> Result<(), String> {
        let lang = session
            .get("responseLanguage")
            .and_then(Value::as_str)
            .unwrap_or("zh")
            .to_string();
        let job_id = card
            .pointer("/body/jobId")
            .and_then(Value::as_str)
            .ok_or_else(|| "reclass jobId is missing".to_string())?;
        let (_job, _result, tree) = self.tools.rollback_reclassification(session, job_id)?;
        self.refresh_session_overview(session)?;
        self.apply_session_state(session, WORKFLOW_UNDERSTAND, None, None, false);
        let turn = persist::create_advisor_turn(
            &self.state.db_path(),
            session.get("sessionId").and_then(Value::as_str).unwrap_or_default(),
            "assistant",
            local_text(
                &lang,
                "已回滚最近一次分类修正。",
                "The latest reclassification has been rolled back.",
            ),
        )?;
        let result_card = json!({
            "cardId": Uuid::new_v4().to_string(),
            "sessionId": session.get("sessionId").cloned().unwrap_or(Value::Null),
            "turnId": turn.get("turnId").and_then(Value::as_str).unwrap_or_default(),
            "cardType": "reclassification_result",
            "status": "rolled_back",
            "title": local_text(&lang, "分类回滚结果", "Reclassification Rollback"),
            "body": {
                "jobId": job_id,
                "tree": tree,
            },
            "actions": [],
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
        });
        persist::save_advisor_card(&self.state.db_path(), &result_card)?;
        Ok(())
    }

    fn refresh_session_overview(&self, session: &mut Value) -> Result<(), String> {
        let lang = session
            .get("responseLanguage")
            .and_then(Value::as_str)
            .unwrap_or("zh")
            .to_string();
        let overview = self.tools.get_directory_overview(
            session
                .get("rootPath")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            session.get("scanTaskId").and_then(Value::as_str),
            Some(session),
            &lang,
        )?;
        if let Some(obj) = session.as_object_mut() {
            obj.insert("contextBar".to_string(), overview.context_bar);
            obj.insert(
                "derivedTree".to_string(),
                overview.derived_tree.unwrap_or(Value::Null),
            );
        }
        Ok(())
    }

    fn load_action_card(&self, session_id: &str, card_id: &str) -> Result<Value, String> {
        let card = persist::load_advisor_card(&self.state.db_path(), card_id)?
            .ok_or_else(|| "advisor card not found".to_string())?;
        let owner = card
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if owner == session_id {
            Ok(card)
        } else {
            Err("advisor card does not belong to the active session".to_string())
        }
    }

    fn apply_session_state(
        &self,
        session: &mut Value,
        workflow: &str,
        selection_id: Option<&str>,
        preview_id: Option<&str>,
        rollback_available: bool,
    ) {
        if let Some(obj) = session.as_object_mut() {
            obj.insert("workflowStage".to_string(), Value::String(workflow.to_string()));
            obj.insert(
                "activeSelectionId".to_string(),
                selection_id
                    .map(|value| Value::String(value.to_string()))
                    .unwrap_or(Value::Null),
            );
            obj.insert(
                "activePreviewId".to_string(),
                preview_id
                    .map(|value| Value::String(value.to_string()))
                    .unwrap_or(Value::Null),
            );
            obj.insert(
                "rollbackAvailable".to_string(),
                Value::Bool(rollback_available),
            );
        }
    }

    fn update_card(&self, card: &mut Value, status: &str, actions: Vec<Value>) {
        if let Some(obj) = card.as_object_mut() {
            obj.insert("status".to_string(), Value::String(status.to_string()));
            obj.insert("actions".to_string(), Value::Array(actions));
            obj.insert("updatedAt".to_string(), Value::String(now_iso()));
        }
    }

    fn store_trace(&self, session: &mut Value, trace: &Value) {
        if let Some(meta) = session.get_mut("sessionMeta").and_then(Value::as_object_mut) {
            meta.insert("lastAgentTrace".to_string(), trace.clone());
        }
    }
}
