use crate::backend::OrganizeSnapshot;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha1::Digest;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const ORGANIZER_SCHEMA_VERSION: &str = "tree_v2";

static FULL_DB_BOOTSTRAP_CACHE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static ORGANIZER_READ_READY_CACHE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static ADVISOR_READ_READY_CACHE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static ORGANIZER_MODULE_PREPARED_CACHE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn open_db_raw(db_path: &Path) -> Result<Connection, String> {
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

fn db_cache(cache: &'static OnceLock<Mutex<HashSet<String>>>) -> &'static Mutex<HashSet<String>> {
    cache.get_or_init(|| Mutex::new(HashSet::new()))
}

fn db_bootstrap_key(db_path: &Path) -> String {
    normalize_root_path(&db_path.to_string_lossy())
}

fn run_cached_db_action(
    cache: &'static OnceLock<Mutex<HashSet<String>>>,
    db_path: &Path,
    action: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let key = db_bootstrap_key(db_path);
    let mut cache = db_cache(cache)
        .lock()
        .map_err(|_| "failed to lock db bootstrap cache".to_string())?;
    if cache.contains(&key) {
        return Ok(());
    }
    action()?;
    cache.insert(key);
    Ok(())
}

fn ensure_db_bootstrapped(db_path: &Path) -> Result<(), String> {
    run_cached_db_action(&FULL_DB_BOOTSTRAP_CACHE, db_path, || {
        init_db(db_path)?;
        mark_stale_tasks(db_path)
    })
}

pub(crate) fn ensure_organizer_read_ready(db_path: &Path) -> Result<(), String> {
    run_cached_db_action(&ORGANIZER_READ_READY_CACHE, db_path, || {
        let conn = open_db_raw(db_path)?;
        ensure_organizer_read_schema(&conn)
    })
}

pub(crate) fn ensure_advisor_read_ready(db_path: &Path) -> Result<(), String> {
    run_cached_db_action(&ADVISOR_READ_READY_CACHE, db_path, || {
        let conn = open_db_raw(db_path)?;
        ensure_advisor_read_schema(&conn)
    })
}

pub(crate) fn prepare_organizer_module_access(db_path: &Path) -> Result<(), String> {
    ensure_organizer_read_ready(db_path)?;
    run_cached_db_action(&ORGANIZER_MODULE_PREPARED_CACHE, db_path, || {
        mark_stale_organize_tasks(db_path)
    })
}

fn open_db(db_path: &Path) -> Result<Connection, String> {
    ensure_db_bootstrapped(db_path)?;
    open_db_raw(db_path)
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn bool_to_i64(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

mod advisor;
mod organize;
mod schema;

pub use advisor::*;
pub use organize::*;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{default_organize_summary_strategy, TokenUsage};
    use std::fs;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn temp_db_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wipeout-{name}-{}.sqlite", Uuid::new_v4()))
    }

    fn table_exists(conn: &Connection, table_name: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            rusqlite::params![table_name],
            |_row| Ok(()),
        )
        .optional()
        .expect("query sqlite_master")
        .is_some()
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
            summary_strategy: default_organize_summary_strategy(),
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
                rusqlite::params![snapshot.id.clone()],
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
    fn advisor_read_ready_does_not_initialize_organizer_tables() {
        let db_path = temp_db_path("advisor-read-ready");
        ensure_advisor_read_ready(&db_path).expect("ensure advisor read ready");

        let conn = open_db_raw(&db_path).expect("open raw db");
        assert!(table_exists(&conn, "advisor_sessions"));
        assert!(table_exists(&conn, "advisor_turns"));
        assert!(table_exists(&conn, "advisor_cards"));
        assert!(!table_exists(&conn, "organize_tasks"));

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn organizer_read_ready_does_not_initialize_advisor_tables() {
        let db_path = temp_db_path("organizer-read-ready");
        ensure_organizer_read_ready(&db_path).expect("ensure organizer read ready");

        let conn = open_db_raw(&db_path).expect("open raw db");
        assert!(table_exists(&conn, "organize_tasks"));
        assert!(table_exists(&conn, "organize_results"));
        assert!(!table_exists(&conn, "advisor_sessions"));

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn prepare_organizer_module_access_marks_stale_organize_tasks() {
        let db_path = temp_db_path("organizer-module-prepare");
        init_db(&db_path).expect("init db");

        let mut snapshot = make_organize_snapshot("organize_stale_task", r"C:\root");
        snapshot.status = "classifying".to_string();
        snapshot.completed_at = None;
        init_organize_task(&db_path, &snapshot).expect("init organize task");
        save_organize_snapshot(&db_path, &snapshot).expect("save organize snapshot");

        prepare_organizer_module_access(&db_path).expect("prepare organizer module access");

        let prepared = load_organize_snapshot(&db_path, &snapshot.id)
            .expect("load prepared organize snapshot")
            .expect("snapshot exists");
        assert_eq!(prepared.status, "stopped");
        assert!(prepared.completed_at.is_some());

        let _ = fs::remove_file(db_path);
    }
}
