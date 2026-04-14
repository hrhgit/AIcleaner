use super::agent::{materialize_card, AdvisorAgentRunner};
use super::payload::build_session_payload;
use super::tools::{build_tree_card, ToolService};
use super::types::{now_iso, AdvisorSessionStartInput, WORKFLOW_UNDERSTAND};
use crate::backend::AppState;
use crate::persist;
use serde_json::{json, Value};

pub(super) struct SessionBootstrap<'a> {
    state: &'a AppState,
    tools: ToolService<'a>,
}

impl<'a> SessionBootstrap<'a> {
    pub(super) fn new(state: &'a AppState) -> Self {
        Self {
            state,
            tools: ToolService::new(state),
        }
    }

    pub(super) async fn start(&self, input: AdvisorSessionStartInput) -> Result<Value, String> {
        let root_path = input.root_path.trim().to_string();
        if root_path.is_empty() {
            return Err("rootPath is required".to_string());
        }
        let _ = input.mode;
        let response_language = input.response_language.unwrap_or_else(|| "zh".to_string());
        let overview = self.tools.get_directory_overview(
            &root_path,
            input.scan_task_id.as_deref(),
            None,
            &response_language,
        )?;

        let mut session = json!({
            "sessionId": uuid::Uuid::new_v4().to_string(),
            "rootPath": root_path,
            "scanTaskId": overview.assets.scan_task_id,
            "responseLanguage": response_language,
            "workflowStage": WORKFLOW_UNDERSTAND,
            "status": "active",
            "contextBar": overview.context_bar,
            "derivedTree": overview.derived_tree,
            "activeSelectionId": Value::Null,
            "activePreviewId": Value::Null,
            "rollbackAvailable": false,
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
            "sessionMeta": {
                "organizeTaskId": overview.assets.organize_task_id,
                "inventoryOverrides": {},
            }
        });
        super::save_session(self.state, &mut session)?;

        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| "sessionId missing after creation".to_string())?;
        let tree_turn = persist::create_advisor_turn(&self.state.db_path(), session_id, "assistant", "")?;

        if let Some(tree) = session
            .get("derivedTree")
            .cloned()
            .filter(|value| !value.is_null())
        {
            persist::save_advisor_card(
                &self.state.db_path(),
                &build_tree_card(
                    session_id,
                    tree_turn.get("turnId")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    &tree,
                    &overview.inventory,
                    session
                        .get("responseLanguage")
                        .and_then(Value::as_str)
                        .unwrap_or("zh"),
                ),
            )?;
        }

        let mut session = super::load_session(self.state, session_id)?;
        match AdvisorAgentRunner::new(self.state)
            .run_bootstrap_turn(&mut session)
            .await
        {
            Ok(agent_result) => {
                super::save_session(self.state, &mut session)?;
                let advice_turn = persist::create_advisor_turn(
                    &self.state.db_path(),
                    session_id,
                    "assistant",
                    &agent_result.reply,
                )?;
                let advice_turn_id = advice_turn
                    .get("turnId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                for card in agent_result.cards {
                    persist::save_advisor_card(
                        &self.state.db_path(),
                        &materialize_card(session_id, advice_turn_id, &card),
                    )?;
                }
            }
            Err(error) => {
                super::save_session(self.state, &mut session)?;
                persist::create_advisor_turn(&self.state.db_path(), session_id, "assistant", &error)?;
            }
        }

        let session = super::load_session(self.state, session_id)?;
        build_session_payload(self.state, &session)
    }
}
