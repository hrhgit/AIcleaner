use crate::backend::{OrganizeSnapshot, ScanSnapshot};
use crate::persist::{self, ScanFindingRecord};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;

pub(super) const CARD_TREE: &str = "tree";
pub(super) const CARD_RECLASS: &str = "reclassification_result";
pub(super) const CARD_PREFERENCE: &str = "preference_draft";
pub(super) const CARD_PLAN_PREVIEW: &str = "plan_preview";
pub(super) const CARD_EXECUTION: &str = "execution_result";

pub(super) const WORKFLOW_UNDERSTAND: &str = "understand";
pub(super) const WORKFLOW_PREVIEW_READY: &str = "preview_ready";
pub(super) const WORKFLOW_EXECUTE_READY: &str = "execute_ready";

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AdvisorSessionStartInput {
    pub root_path: String,
    pub scan_task_id: Option<String>,
    pub mode: Option<String>,
    pub response_language: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AdvisorMessageSendInput {
    pub session_id: String,
    pub message: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AdvisorCardActionInput {
    pub session_id: String,
    pub card_id: String,
    pub action: String,
    pub payload: Option<Value>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ContextAssets {
    pub scan_task_id: Option<String>,
    pub scan_snapshot: Option<ScanSnapshot>,
    pub organize_task_id: Option<String>,
    pub organize_snapshot: Option<OrganizeSnapshot>,
    pub latest_tree: Option<Value>,
    pub finding_map: HashMap<String, ScanFindingRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct InventoryItem {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub created_at: Option<String>,
    pub modified_at: Option<String>,
    pub kind: String,
    pub category_id: String,
    pub parent_category_id: Option<String>,
    pub category_path: Vec<String>,
    pub summary_short: Option<String>,
    pub summary_normal: Option<String>,
    pub risk: String,
    pub source: String,
}

#[derive(Clone, Debug, Default)]
pub(super) struct DirectoryOverview {
    pub assets: ContextAssets,
    pub inventory: Vec<InventoryItem>,
    pub derived_tree: Option<Value>,
    pub context_bar: Value,
}

pub(super) fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(super) fn is_english(lang: &str) -> bool {
    lang.trim().eq_ignore_ascii_case("en")
}

pub(super) fn local_text<'a>(lang: &str, zh: &'a str, en: &'a str) -> &'a str {
    if is_english(lang) {
        en
    } else {
        zh
    }
}

pub(super) fn normalize_path_key(value: &str) -> String {
    persist::create_root_path_key(value)
}

pub(super) fn normalize_message(value: &str) -> String {
    value.trim().to_lowercase()
}

pub(super) fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| path.to_string())
}

pub(super) fn array_of_strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(|row| row.trim().to_string())
                .filter(|row| !row.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub(super) fn session_meta_mut<'a>(
    session: &'a mut Value,
) -> Result<&'a mut Map<String, Value>, String> {
    let obj = session
        .as_object_mut()
        .ok_or_else(|| "advisor session must be an object".to_string())?;
    let entry = obj
        .entry("sessionMeta".to_string())
        .or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    entry
        .as_object_mut()
        .ok_or_else(|| "sessionMeta must be an object".to_string())
}

pub(super) fn session_meta<'a>(session: &'a Value) -> Option<&'a Map<String, Value>> {
    session.get("sessionMeta").and_then(Value::as_object)
}

pub(super) fn inventory_overrides(session: &Value) -> HashMap<String, Vec<String>> {
    session_meta(session)
        .and_then(|meta| meta.get("inventoryOverrides"))
        .and_then(Value::as_object)
        .map(|rows| {
            rows.iter()
                .map(|(path, value)| (path.clone(), array_of_strings(Some(value))))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default()
}

pub(super) fn set_inventory_override(
    session: &mut Value,
    path: &str,
    category_path: &[String],
) -> Result<Option<Vec<String>>, String> {
    let meta = session_meta_mut(session)?;
    let entry = meta
        .entry("inventoryOverrides".to_string())
        .or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    let map = entry
        .as_object_mut()
        .ok_or_else(|| "inventoryOverrides must be an object".to_string())?;
    let previous = map
        .get(path)
        .map(|value| array_of_strings(Some(value)))
        .filter(|rows| !rows.is_empty());
    map.insert(path.to_string(), json!(category_path));
    Ok(previous)
}

pub(super) fn remove_inventory_override(session: &mut Value, path: &str) -> Result<(), String> {
    let meta = session_meta_mut(session)?;
    let Some(overrides) = meta.get_mut("inventoryOverrides") else {
        return Ok(());
    };
    if let Some(map) = overrides.as_object_mut() {
        map.remove(path);
    }
    Ok(())
}
