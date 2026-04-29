use super::types::CARD_EXECUTION;
use crate::backend::AppState;
use crate::persist;
use serde_json::{json, Value};
use std::collections::HashMap;

fn build_composer(session: &Value) -> Value {
    let english = session
        .get("responseLanguage")
        .and_then(Value::as_str)
        .unwrap_or("zh")
        .eq_ignore_ascii_case("en");
    json!({
        "placeholder": if english {
            "Tell me which files you want to handle first."
        } else {
            "告诉我你想先处理哪些文件。"
        },
        "submitLabel": if english { "Send" } else { "发送" },
    })
}

pub(super) fn build_session_payload(state: &AppState, session: &Value) -> Result<Value, String> {
    let db_path = state.db_path();
    let session_id = session
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "sessionId is required".to_string())?;
    let turns = persist::load_advisor_turns(&db_path, session_id)?;
    let cards = persist::load_advisor_cards(&db_path, session_id)?;
    let mut cards_by_turn = HashMap::<String, Vec<Value>>::new();
    let mut latest_execution = None;

    for card in cards {
        if card.get("cardType").and_then(Value::as_str) == Some(CARD_EXECUTION) {
            latest_execution = Some(card.clone());
        }
        let turn_id = card
            .get("turnId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        cards_by_turn.entry(turn_id).or_default().push(card);
    }

    let timeline = turns
        .into_iter()
        .map(|turn| {
            let turn_id = turn
                .get("turnId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            json!({
                "turnId": turn.get("turnId").cloned().unwrap_or(Value::Null),
                "role": turn.get("role").cloned().unwrap_or_else(|| Value::String("assistant".to_string())),
                "text": turn.get("text").cloned().unwrap_or_else(|| Value::String(String::new())),
                "createdAt": turn.get("createdAt").cloned().unwrap_or(Value::Null),
                "agentTrace": turn.get("agentTrace").cloned().unwrap_or(Value::Null),
                "cards": cards_by_turn.remove(&turn_id).unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();

    let active_selection_card = session
        .get("activeSelectionId")
        .and_then(Value::as_str)
        .and_then(|selection_id| {
            timeline.iter().find_map(|turn| {
                turn.get("cards")
                    .and_then(Value::as_array)
                    .and_then(|cards| {
                        cards.iter().find(|card| {
                            card.pointer("/body/selectionId").and_then(Value::as_str)
                                == Some(selection_id)
                        })
                    })
                    .cloned()
            })
        });

    let active_preview_card = session
        .get("activePreviewId")
        .and_then(Value::as_str)
        .map(|card_id| persist::load_advisor_card(&db_path, card_id))
        .transpose()?
        .flatten();

    Ok(json!({
        "session": {
            "sessionId": session.get("sessionId").cloned().unwrap_or(Value::Null),
            "workflowStage": session.get("workflowStage").cloned().unwrap_or_else(|| Value::String("understand".to_string())),
            "rollbackAvailable": session.get("rollbackAvailable").cloned().unwrap_or(Value::Bool(false)),
            "useWebSearch": session.get("useWebSearch").cloned().unwrap_or(Value::Bool(false)),
            "webSearchEnabled": session.get("webSearchEnabled").cloned().unwrap_or(Value::Bool(false)),
        },
        "sessionId": session.get("sessionId").cloned().unwrap_or(Value::Null),
        "rootPath": session.get("rootPath").cloned().unwrap_or(Value::Null),
        "responseLanguage": session.get("responseLanguage").cloned().unwrap_or_else(|| Value::String("zh".to_string())),
        "workflowStage": session.get("workflowStage").cloned().unwrap_or_else(|| Value::String("understand".to_string())),
        "useWebSearch": session.get("useWebSearch").cloned().unwrap_or(Value::Bool(false)),
        "webSearchEnabled": session.get("webSearchEnabled").cloned().unwrap_or(Value::Bool(false)),
        "contextBar": session.get("contextBar").cloned().unwrap_or_else(|| json!({})),
        "timeline": timeline,
        "composer": build_composer(session),
        "rollbackAvailable": session.get("rollbackAvailable").cloned().unwrap_or(Value::Bool(false)),
        "activeSelectionCard": active_selection_card,
        "activePreviewCard": active_preview_card,
        "latestExecutionCard": latest_execution,
        "derivedTree": session.get("derivedTree").cloned().unwrap_or(Value::Null),
    }))
}
