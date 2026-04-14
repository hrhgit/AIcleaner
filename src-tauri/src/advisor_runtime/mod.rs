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
