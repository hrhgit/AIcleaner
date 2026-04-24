mod agent;
mod bootstrap;
mod llm;
mod orchestrator;
mod payload;
mod tools;
mod types;

use crate::backend::AppState;
use crate::persist;
use serde_json::Value;
use tauri::State;

pub use types::{AdvisorCardActionInput, AdvisorMessageSendInput, AdvisorSessionStartInput};

use bootstrap::SessionBootstrap;
use orchestrator::ConversationOrchestrator;

pub(super) fn resolve_workflow_web_search_state(state: &AppState) -> (bool, bool) {
    let settings = crate::backend::read_settings(&state.settings_path());
    let search_api = settings.get("searchApi").and_then(Value::as_object);
    let scopes = search_api
        .and_then(|value| value.get("scopes"))
        .and_then(Value::as_object);
    let enabled = search_api
        .and_then(|value| value.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let classify = scopes
        .and_then(|value| value.get("classify"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let organizer = scopes
        .and_then(|value| value.get("organizer"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let use_web_search = enabled || classify || organizer;
    let web_search_enabled = use_web_search
        && crate::backend::resolve_search_api_key(state)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
    (use_web_search, web_search_enabled)
}

pub(super) fn apply_workflow_web_search_state(
    session: &mut Value,
    use_web_search: bool,
    web_search_enabled: bool,
) {
    if let Some(obj) = session.as_object_mut() {
        obj.insert("useWebSearch".to_string(), Value::Bool(use_web_search));
        obj.insert(
            "webSearchEnabled".to_string(),
            Value::Bool(web_search_enabled),
        );
        let context_bar = obj
            .entry("contextBar".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !context_bar.is_object() {
            *context_bar = serde_json::json!({});
        }
        if let Some(context_bar_obj) = context_bar.as_object_mut() {
            context_bar_obj.insert(
                "webSearch".to_string(),
                serde_json::json!({
                    "useWebSearch": use_web_search,
                    "webSearchEnabled": web_search_enabled,
                }),
            );
        }
    }
}

pub(super) fn save_session(state: &AppState, session: &mut Value) -> Result<(), String> {
    if let Some(obj) = session.as_object_mut() {
        obj.insert("updatedAt".to_string(), Value::String(types::now_iso()));
    }
    persist::save_advisor_session(&state.db_path(), session)
}

pub(super) fn load_session(state: &AppState, session_id: &str) -> Result<Value, String> {
    persist::load_advisor_session(&state.db_path(), session_id)?
        .ok_or_else(|| format!("advisor session not found: {session_id}"))
}

pub async fn advisor_session_start(
    state: State<'_, AppState>,
    input: AdvisorSessionStartInput,
) -> Result<Value, String> {
    SessionBootstrap::new(state.inner()).start(input).await
}

pub async fn advisor_session_get(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Value, String> {
    let session = load_session(state.inner(), session_id.trim())?;
    payload::build_session_payload(state.inner(), &session)
}

pub async fn advisor_message_send(
    state: State<'_, AppState>,
    input: AdvisorMessageSendInput,
) -> Result<Value, String> {
    ConversationOrchestrator::new(state.inner())
        .handle_message(input)
        .await
}

pub async fn advisor_card_action(
    state: State<'_, AppState>,
    input: AdvisorCardActionInput,
) -> Result<Value, String> {
    ConversationOrchestrator::new(state.inner()).handle_card_action(input)
}
