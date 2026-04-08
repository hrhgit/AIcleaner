use crate::backend::{OrganizeSnapshot, ScanResultItem, ScanSnapshot, TokenUsage};
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sha1::Digest;
use std::collections::{HashMap, HashSet};
use std::path::Path;

const ORGANIZER_SCHEMA_VERSION: &str = "tree_v2";
const SCAN_CACHE_MAINTENANCE_VERSION: &str = "scan_cache_v2";
const SCAN_DRAFT_SCHEMA_VERSION: &str = "scan_draft_v1";

#[derive(Clone, Debug)]
pub struct ScanNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub path: String,
    pub name: String,
    pub node_type: String,
    pub depth: u32,
    pub self_size: u64,
    pub size: u64,
    pub child_count: u64,
    pub mtime_ms: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct ScanFindingRecord {
    pub item: ScanResultItem,
    pub should_expand: bool,
}

fn open_db(db_path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "busy_timeout", 5000)
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn parse_json_or_default<T: DeserializeOwned + Default>(raw: Option<String>) -> T {
    raw.and_then(|x| serde_json::from_str::<T>(&x).ok())
        .unwrap_or_default()
}

fn bool_to_i64(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

mod organize;
mod advisor;
mod scan;
mod schema;

pub use advisor::*;
pub use organize::*;
pub use scan::*;
pub use schema::*;

pub fn create_node_id(path_value: &str) -> String {
    let mut sha = sha1::Sha1::new();
    sha.update(path_value.to_lowercase().as_bytes());
    format!("{:x}", sha.finalize())
}

pub fn normalize_root_path(path_value: &str) -> String {
    let mut normalized = path_value.trim().replace('/', "\\").to_lowercase();
    while normalized.len() > 3 && normalized.ends_with('\\') {
        normalized.pop();
    }
    normalized
}

pub fn create_root_path_key(path_value: &str) -> String {
    normalize_root_path(path_value)
}

fn is_same_or_descendant_path(path: &str, parent: &str) -> bool {
    path == parent || path.starts_with(&format!("{parent}\\"))
}

fn path_depth(path: &str) -> usize {
    normalize_root_path(path)
        .split('\\')
        .filter(|segment| !segment.is_empty())
        .count()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScanStorage {
    Committed,
    Draft,
}

impl ScanStorage {
    fn tasks_table(self) -> &'static str {
        match self {
            Self::Committed => "scan_tasks",
            Self::Draft => "scan_drafts",
        }
    }

    fn nodes_table(self) -> &'static str {
        match self {
            Self::Committed => "scan_nodes",
            Self::Draft => "scan_draft_nodes",
        }
    }

    fn findings_table(self) -> &'static str {
        match self {
            Self::Committed => "scan_findings",
            Self::Draft => "scan_draft_findings",
        }
    }
}

fn resolve_scan_storage_by_task_id(
    conn: &Connection,
    task_id: &str,
) -> Result<ScanStorage, String> {
    let is_draft = conn
        .query_row(
            "SELECT 1 FROM scan_drafts WHERE task_id = ?1 LIMIT 1",
            params![task_id],
            |_row| Ok(()),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .is_some();
    Ok(if is_draft {
        ScanStorage::Draft
    } else {
        ScanStorage::Committed
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{
        default_organize_summary_mode, OrganizeSnapshot, ScanSnapshot, TokenUsage,
    };
    use std::fs;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn temp_db_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wipeout-{name}-{}.sqlite", Uuid::new_v4()))
    }

    fn make_scan_snapshot(task_id: &str, root_path: &str) -> ScanSnapshot {
        ScanSnapshot {
            id: task_id.to_string(),
            status: "idle".to_string(),
            scan_mode: "full_rescan_incremental".to_string(),
            baseline_task_id: None,
            visible_latest: true,
            root_path_key: create_root_path_key(root_path),
            target_path: root_path.to_string(),
            auto_analyze: true,
            root_node_id: create_node_id(root_path),
            configured_max_depth: Some(3),
            max_scanned_depth: 0,
            current_path: root_path.to_string(),
            current_depth: 0,
            scanned_count: 0,
            total_entries: 0,
            processed_entries: 0,
            deletable_count: 0,
            total_cleanable: 0,
            target_size: 0,
            token_usage: TokenUsage::default(),
            deletable: Vec::new(),
            permission_denied_count: 0,
            permission_denied_paths: Vec::new(),
            error_message: String::new(),
        }
    }

    fn make_organize_snapshot(task_id: &str, root_path: &str) -> OrganizeSnapshot {
        OrganizeSnapshot {
            id: task_id.to_string(),
            status: "completed".to_string(),
            error: None,
            root_path: root_path.to_string(),
            recursive: true,
            excluded_patterns: vec!["node_modules".to_string()],
            batch_size: 20,
            summary_mode: default_organize_summary_mode(),
            max_cluster_depth: None,
            use_web_search: false,
            web_search_enabled: false,
            selected_model: "gpt-4o-mini".to_string(),
            selected_models: json!({ "text": "gpt-4o-mini" }),
            selected_providers: json!({ "text": "https://api.openai.com/v1" }),
            supports_multimodal: false,
            tree: json!({ "nodeId": "root", "name": "", "children": [] }),
            tree_version: 3,
            total_files: 2,
            processed_files: 2,
            total_batches: 1,
            processed_batches: 1,
            token_usage: TokenUsage {
                prompt: 10,
                completion: 5,
                total: 15,
            },
            results: vec![
                json!({
                    "index": 1,
                    "path": format!("{root_path}\\alpha.txt"),
                    "name": "alpha.txt",
                    "leafNodeId": "leaf-a",
                    "categoryPath": ["Docs"],
                }),
                json!({
                    "index": 2,
                    "path": format!("{root_path}\\beta.txt"),
                    "name": "beta.txt",
                    "leafNodeId": "leaf-b",
                    "categoryPath": ["Docs", "Notes"],
                }),
            ],
            preview: vec![json!({
                "sourcePath": format!("{root_path}\\alpha.txt"),
                "targetPath": format!("{root_path}\\Docs\\alpha.txt"),
            })],
            created_at: "2026-03-28T00:00:00Z".to_string(),
            completed_at: Some("2026-03-28T00:05:00Z".to_string()),
            job_id: Some("job_1".to_string()),
        }
    }

    #[test]
    fn organizer_snapshot_persistence_is_compacted_and_results_are_loaded_from_rows() {
        let db_path = temp_db_path("organizer-compact");
        init_db(&db_path).expect("init db");

        let root_path = r"C:\root";
        let snapshot = make_organize_snapshot("org_task", root_path);
        init_organize_task(&db_path, &snapshot).expect("init organize task");
        upsert_organize_results(&db_path, &snapshot.id, &snapshot.results)
            .expect("write organize rows");
        save_organize_snapshot(&db_path, &snapshot).expect("save compact snapshot");

        let conn = open_db(&db_path).expect("open db");
        let raw_snapshot = conn
            .query_row(
                "SELECT snapshot_json FROM organize_tasks WHERE task_id = ?1",
                params![snapshot.id.clone()],
                |row| row.get::<_, String>(0),
            )
            .expect("read snapshot json");
        let persisted_value: Value =
            serde_json::from_str(&raw_snapshot).expect("parse snapshot json");
        assert_eq!(
            persisted_value.get("results"),
            Some(&Value::Array(Vec::new()))
        );
        assert_eq!(
            persisted_value.get("preview"),
            Some(&Value::Array(Vec::new()))
        );

        let loaded = load_organize_snapshot(&db_path, &snapshot.id)
            .expect("load snapshot")
            .expect("snapshot exists");
        assert_eq!(loaded.results.len(), 2);
        assert!(loaded.preview.is_empty());
        assert_eq!(
            loaded.results[1].get("path").and_then(Value::as_str),
            Some(r"C:\root\beta.txt")
        );

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn organizer_batch_upsert_updates_existing_rows() {
        let db_path = temp_db_path("organizer-batch-upsert");
        init_db(&db_path).expect("init db");

        let snapshot = make_organize_snapshot("org_batch", r"C:\root");
        init_organize_task(&db_path, &snapshot).expect("init organize task");
        upsert_organize_results(
            &db_path,
            &snapshot.id,
            &[json!({
                "index": 1,
                "path": r"C:\root\alpha.txt",
                "name": "alpha.txt",
                "leafNodeId": "leaf-a",
                "categoryPath": ["Docs"],
            })],
        )
        .expect("insert first batch");
        upsert_organize_results(
            &db_path,
            &snapshot.id,
            &[json!({
                "index": 1,
                "path": r"C:\root\alpha.txt",
                "name": "alpha.txt",
                "leafNodeId": "leaf-b",
                "categoryPath": ["Docs", "Updated"],
            })],
        )
        .expect("update existing row");

        let loaded = load_organize_snapshot(&db_path, &snapshot.id)
            .expect("load snapshot")
            .expect("snapshot exists");
        assert_eq!(loaded.results.len(), 1);
        assert_eq!(
            loaded.results[0]
                .get("categoryPath")
                .and_then(Value::as_array)
                .map(|items| items.len()),
            Some(2)
        );
        assert_eq!(
            loaded.results[0].get("leafNodeId").and_then(Value::as_str),
            Some("leaf-b")
        );

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn organizer_schema_version_is_upgraded() {
        let db_path = temp_db_path("organizer-schema-version");
        init_db(&db_path).expect("init db");

        let conn = open_db(&db_path).expect("open db");
        let version = conn
            .query_row(
                "SELECT value FROM app_meta WHERE key = 'organizer_schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("load schema version");
        assert_eq!(version, ORGANIZER_SCHEMA_VERSION);

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn scan_finding_write_skips_immediate_stat_refresh_but_delete_paths_recomputes_stats() {
        let db_path = temp_db_path("scan-finding-stats");
        init_db(&db_path).expect("init db");

        let task_id = "scan_task";
        let root_path = r"C:\scan-root";
        init_scan_task(
            &db_path,
            task_id,
            root_path,
            0,
            Some(3),
            true,
            None,
            "full_rescan_incremental",
            true,
        )
        .expect("init scan task");
        let snapshot = make_scan_snapshot(task_id, root_path);
        save_scan_snapshot(&db_path, &snapshot).expect("seed scan snapshot");

        let item = ScanResultItem {
            name: "alpha.txt".to_string(),
            path: r"C:\scan-root\folder\alpha.txt".to_string(),
            size: 4096,
            item_type: "file".to_string(),
            purpose: String::new(),
            reason: "safe".to_string(),
            risk: "low".to_string(),
            classification: "safe_to_delete".to_string(),
            source: "model".to_string(),
        };
        upsert_scan_finding(&db_path, task_id, &item, false).expect("upsert finding");

        let before_refresh = load_scan_snapshot(&db_path, task_id)
            .expect("load before refresh")
            .expect("snapshot exists");
        assert_eq!(before_refresh.deletable_count, 0);
        assert_eq!(before_refresh.total_cleanable, 0);

        refresh_scan_stats(&db_path, task_id).expect("refresh scan stats");
        let after_refresh = load_scan_snapshot(&db_path, task_id)
            .expect("load after refresh")
            .expect("snapshot exists");
        assert_eq!(after_refresh.deletable_count, 1);
        assert_eq!(after_refresh.total_cleanable, 4096);

        delete_scan_data_for_paths(&db_path, task_id, &[r"C:\scan-root\folder".to_string()])
            .expect("delete scan data");
        let after_delete = load_scan_snapshot(&db_path, task_id)
            .expect("load after delete")
            .expect("snapshot exists");
        assert_eq!(after_delete.deletable_count, 0);
        assert_eq!(after_delete.total_cleanable, 0);

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn delete_scan_descendants_recomputes_stats_after_pruning() {
        let db_path = temp_db_path("scan-descendants-stats");
        init_db(&db_path).expect("init db");

        let task_id = "scan_desc";
        let root_path = r"C:\scan-root";
        init_scan_task(
            &db_path,
            task_id,
            root_path,
            0,
            Some(3),
            true,
            None,
            "full_rescan_incremental",
            true,
        )
        .expect("init scan task");
        let snapshot = make_scan_snapshot(task_id, root_path);
        save_scan_snapshot(&db_path, &snapshot).expect("seed scan snapshot");

        let item = ScanResultItem {
            name: "deep.txt".to_string(),
            path: r"C:\scan-root\folder\deep\deep.txt".to_string(),
            size: 2048,
            item_type: "file".to_string(),
            purpose: String::new(),
            reason: "safe".to_string(),
            risk: "low".to_string(),
            classification: "safe_to_delete".to_string(),
            source: "model".to_string(),
        };
        upsert_scan_finding(&db_path, task_id, &item, false).expect("upsert finding");
        refresh_scan_stats(&db_path, task_id).expect("refresh scan stats");

        delete_scan_descendants_for_paths(&db_path, task_id, &[r"C:\scan-root\folder".to_string()])
            .expect("delete descendants");
        let after_delete = load_scan_snapshot(&db_path, task_id)
            .expect("load after delete")
            .expect("snapshot exists");
        assert_eq!(after_delete.deletable_count, 0);
        assert_eq!(after_delete.total_cleanable, 0);

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn init_db_cleans_residual_full_scan_drafts() {
        let db_path = temp_db_path("scan-draft-cleanup");
        init_db(&db_path).expect("init db");

        let task_id = "scan_draft_task";
        let root_path = r"C:\scan-root";
        init_full_scan_draft(&db_path, task_id, root_path, 0, Some(2), true, None)
            .expect("init full scan draft");
        let mut snapshot = make_scan_snapshot(task_id, root_path);
        snapshot.status = "scanning".to_string();
        snapshot.visible_latest = false;
        save_full_scan_draft_snapshot(&db_path, &snapshot).expect("save draft snapshot");

        let before_cleanup = load_full_scan_draft_snapshot(&db_path, task_id)
            .expect("load draft before cleanup")
            .expect("draft exists before cleanup");
        assert_eq!(before_cleanup.status, "scanning");

        init_db(&db_path).expect("re-init db cleans drafts");
        let after_cleanup =
            load_full_scan_draft_snapshot(&db_path, task_id).expect("load draft after cleanup");
        assert!(after_cleanup.is_none());

        let _ = fs::remove_file(db_path);
    }
}
