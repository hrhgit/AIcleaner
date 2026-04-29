use super::llm::AdvisorLlm;
use super::types::{
    array_of_strings, basename, local_text, normalize_message, normalize_path_key, now_iso,
    set_inventory_override, ContextAssets, DirectoryOverview, InventoryItem,
};
use crate::backend::AppState;
use crate::file_representation::{FileRepresentation, RepresentationLevel};
use crate::llm_protocol::{
    apply_auth_headers, build_completion_payload, build_messages_url, detect_api_format,
    parse_completion_response, DEFAULT_MAX_TOKENS,
};
use crate::model_boundary::ModelIdMap;
use crate::persist;
use crate::system_ops;
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use uuid::Uuid;

const FS_FALLBACK_MAX_FILES: usize = 500;
const FS_FALLBACK_MAX_DIRS: usize = 1500;
const FS_FALLBACK_MAX_DEPTH: usize = 5;
const FS_FALLBACK_SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "dist",
    "build",
    "out",
    "target",
    "Windows",
    "Program Files",
    "Program Files (x86)",
];

#[derive(Default)]
struct TreeDraftNode {
    name: String,
    item_count: u64,
    children: BTreeMap<String, TreeDraftNode>,
}

#[derive(Clone, Debug, Default)]
struct SummarySchedulerStats {
    initial_concurrency: usize,
    final_concurrency: usize,
    retry_rounds: usize,
    degraded_concurrency: bool,
}

#[derive(Clone, Debug)]
struct SummaryErrorRow {
    path: String,
    reason: String,
    retryable: bool,
}

#[derive(Clone, Debug)]
struct SummaryFailedItem {
    item: InventoryItem,
    error: SummaryErrorRow,
}

pub(crate) struct ToolService<'a> {
    state: &'a AppState,
}

pub(crate) fn advisor_model_id_map(session: &Value) -> ModelIdMap {
    session
        .get("derivedTree")
        .map(|tree| ModelIdMap::from_values(&[tree]))
        .unwrap_or_default()
}

pub(crate) fn compact_advisor_model_value(session: &Value, value: &Value) -> Value {
    advisor_model_id_map(session).compact_value(value)
}

pub(crate) fn expand_advisor_model_args(session: &Value, args: &Value) -> Value {
    advisor_model_id_map(session).expand_value(args)
}

pub(crate) fn compact_value_with_tree(tree: &Value, value: &Value) -> Value {
    ModelIdMap::from_values(&[tree]).compact_value(value)
}

// Utility free functions (define first so tools can reference them)
include!("tools/context.rs");

include!("tools/plan.rs");

include!("tools/summary.rs");

include!("tools/query.rs");

// ToolService struct and shared helpers
include!("tools/service_helpers.rs");

// Individual tool implementations
include!("tools/tool_directory_overview.rs");

include!("tools/tool_find_files.rs");

include!("tools/tool_capture_preference.rs");

include!("tools/tool_list_preferences.rs");

include!("tools/tool_summarize_files.rs");

include!("tools/tool_preview_plan.rs");

include!("tools/tool_execute_plan.rs");

include!("tools/tool_rollback_plan.rs");

include!("tools/tool_apply_reclassification.rs");

#[cfg(test)]
include!("tools/tests.rs");
