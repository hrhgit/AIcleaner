use super::payload::build_session_payload;
use super::tools::{build_tree_card, ToolService};
use super::types::{local_text, now_iso, AdvisorSessionStartInput, WORKFLOW_UNDERSTAND};
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
        let (use_web_search, web_search_enabled) =
            super::resolve_workflow_web_search_state(self.state);
        let overview =
            self.tools
                .get_directory_overview(&root_path, None, None, &response_language)?;

        let mut session = json!({
            "sessionId": uuid::Uuid::new_v4().to_string(),
            "rootPath": root_path,
            "responseLanguage": response_language,
            "workflowStage": WORKFLOW_UNDERSTAND,
            "status": "active",
            "contextBar": overview.context_bar,
            "derivedTree": overview.derived_tree,
            "useWebSearch": use_web_search,
            "webSearchEnabled": web_search_enabled,
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
        super::apply_workflow_web_search_state(&mut session, use_web_search, web_search_enabled);
        super::save_session(self.state, &mut session)?;

        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| "sessionId missing after creation".to_string())?;
        let tree_turn =
            persist::create_advisor_turn(&self.state.db_path(), session_id, "assistant", "")?;

        if let Some(tree) = session
            .get("derivedTree")
            .cloned()
            .filter(|value| !value.is_null())
        {
            persist::save_advisor_card(
                &self.state.db_path(),
                &build_tree_card(
                    session_id,
                    tree_turn
                        .get("turnId")
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

        persist::create_advisor_turn(
            &self.state.db_path(),
            session_id,
            "assistant",
            local_text(
                session
                    .get("responseLanguage")
                    .and_then(Value::as_str)
                    .unwrap_or("zh"),
                "会话已开始。你可以直接告诉我想先处理哪些文件或规则。",
                "The conversation is ready. Tell me which files or rules you want to handle first.",
            ),
        )?;

        let session = super::load_session(self.state, session_id)?;
        build_session_payload(self.state, &session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn make_test_state() -> AppState {
        let root =
            std::env::temp_dir().join(format!("wipeout-advisor-bootstrap-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create temp dir");
        AppState::bootstrap(PathBuf::from(root)).expect("bootstrap app state")
    }

    #[tokio::test]
    async fn start_creates_session_without_model_bootstrap() {
        let state = make_test_state();
        let root =
            std::env::temp_dir().join(format!("wipeout-advisor-root-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create advisor root");
        fs::write(root.join("notes.txt"), "temporary test file").expect("write test file");

        let payload = SessionBootstrap::new(&state)
            .start(AdvisorSessionStartInput {
                root_path: root.to_string_lossy().to_string(),
                mode: None,
                response_language: Some("zh".to_string()),
            })
            .await
            .expect("start session");

        assert!(payload["sessionId"].as_str().is_some());
        assert_eq!(payload["session"]["workflowStage"], WORKFLOW_UNDERSTAND);
        let timeline = payload["timeline"].as_array().expect("timeline");
        assert!(timeline.iter().any(|turn| {
            turn["text"]
                .as_str()
                .is_some_and(|text| text.contains("会话已开始"))
        }));
    }
}
