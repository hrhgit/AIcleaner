use crate::backend::{OrganizeSnapshot, ScanResultItem, ScanSnapshot, TokenUsage};
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sha1::Digest;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct ScanNode {
    pub id: String,
    pub path: String,
    pub name: String,
    pub node_type: String,
    pub depth: u32,
    pub size: u64,
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

pub fn create_node_id(path_value: &str) -> String {
    let mut sha = sha1::Sha1::new();
    sha.update(path_value.to_lowercase().as_bytes());
    format!("{:x}", sha.finalize())
}

pub fn init_db(db_path: &Path) -> Result<(), String> {
    let conn = open_db(db_path)?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS scan_tasks (
            task_id TEXT PRIMARY KEY,
            root_path TEXT NOT NULL,
            status TEXT NOT NULL,
            target_size INTEGER,
            max_depth INTEGER,
            auto_analyze INTEGER NOT NULL DEFAULT 1,
            current_path TEXT,
            current_depth INTEGER NOT NULL DEFAULT 0,
            scanned_count INTEGER NOT NULL DEFAULT 0,
            total_entries INTEGER NOT NULL DEFAULT 0,
            processed_entries INTEGER NOT NULL DEFAULT 0,
            deletable_count INTEGER NOT NULL DEFAULT 0,
            total_cleanable INTEGER NOT NULL DEFAULT 0,
            token_prompt INTEGER NOT NULL DEFAULT 0,
            token_completion INTEGER NOT NULL DEFAULT 0,
            token_total INTEGER NOT NULL DEFAULT 0,
            permission_denied_count INTEGER NOT NULL DEFAULT 0,
            permission_denied_paths TEXT NOT NULL DEFAULT '[]',
            error_message TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            finished_at TEXT
        );

        CREATE TABLE IF NOT EXISTS scan_nodes (
            task_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            parent_id TEXT,
            path TEXT NOT NULL,
            name TEXT NOT NULL,
            type TEXT NOT NULL,
            depth INTEGER NOT NULL,
            self_size INTEGER NOT NULL DEFAULT 0,
            total_size INTEGER NOT NULL DEFAULT 0,
            child_count INTEGER NOT NULL DEFAULT 0,
            mtime_ms INTEGER,
            ext TEXT,
            PRIMARY KEY (task_id, node_id),
            UNIQUE (task_id, path)
        );

        CREATE TABLE IF NOT EXISTS scan_findings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id TEXT NOT NULL,
            path TEXT NOT NULL,
            name TEXT NOT NULL,
            type TEXT NOT NULL,
            size INTEGER NOT NULL DEFAULT 0,
            classification TEXT NOT NULL,
            purpose TEXT,
            reason TEXT,
            risk TEXT,
            source TEXT NOT NULL,
            created_at TEXT NOT NULL,
            UNIQUE (task_id, path)
        );

        CREATE INDEX IF NOT EXISTS idx_scan_nodes_parent
        ON scan_nodes(task_id, parent_id, total_size DESC);
        CREATE INDEX IF NOT EXISTS idx_scan_nodes_path
        ON scan_nodes(task_id, path);
        CREATE INDEX IF NOT EXISTS idx_scan_findings_classification
        ON scan_findings(task_id, classification, size DESC);

        CREATE TABLE IF NOT EXISTS organize_tasks (
            task_id TEXT PRIMARY KEY,
            status TEXT NOT NULL,
            error TEXT,
            root_path TEXT NOT NULL,
            recursive INTEGER NOT NULL DEFAULT 1,
            mode TEXT NOT NULL,
            categories_json TEXT NOT NULL,
            allow_new_categories INTEGER NOT NULL DEFAULT 1,
            excluded_patterns_json TEXT NOT NULL,
            parallelism INTEGER NOT NULL DEFAULT 5,
            use_web_search INTEGER NOT NULL DEFAULT 0,
            web_search_enabled INTEGER NOT NULL DEFAULT 0,
            selected_model TEXT NOT NULL,
            selected_models_json TEXT NOT NULL,
            selected_providers_json TEXT NOT NULL,
            supports_multimodal INTEGER NOT NULL DEFAULT 0,
            total_files INTEGER NOT NULL DEFAULT 0,
            processed_files INTEGER NOT NULL DEFAULT 0,
            token_prompt INTEGER NOT NULL DEFAULT 0,
            token_completion INTEGER NOT NULL DEFAULT 0,
            token_total INTEGER NOT NULL DEFAULT 0,
            preview_json TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL,
            completed_at TEXT,
            job_id TEXT
        );

        CREATE TABLE IF NOT EXISTS organize_results (
            task_id TEXT NOT NULL,
            idx INTEGER NOT NULL,
            name TEXT NOT NULL,
            path TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            size INTEGER NOT NULL DEFAULT 0,
            category TEXT NOT NULL,
            created_category INTEGER NOT NULL DEFAULT 0,
            degraded INTEGER NOT NULL DEFAULT 0,
            warnings_json TEXT NOT NULL DEFAULT '[]',
            modality TEXT NOT NULL,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            PRIMARY KEY (task_id, idx),
            UNIQUE (task_id, path)
        );

        CREATE TABLE IF NOT EXISTS organize_jobs (
            job_id TEXT PRIMARY KEY,
            task_id TEXT NOT NULL,
            root_path TEXT NOT NULL,
            created_at TEXT NOT NULL,
            manifest_json TEXT NOT NULL,
            rollback_json TEXT
        );

        CREATE TABLE IF NOT EXISTS organize_job_entries (
            job_id TEXT NOT NULL,
            idx INTEGER NOT NULL,
            source_path TEXT NOT NULL,
            target_path TEXT NOT NULL,
            category TEXT NOT NULL,
            status TEXT NOT NULL,
            error TEXT,
            PRIMARY KEY (job_id, idx)
        );
        "#,
    )
    .map_err(|e| e.to_string())
}

pub fn mark_stale_tasks(db_path: &Path) -> Result<(), String> {
    let conn = open_db(db_path)?;
    let now = now_iso();
    conn.execute(
        "UPDATE scan_tasks
         SET status='stopped', updated_at=?1, finished_at=COALESCE(finished_at, ?1)
         WHERE status IN ('idle', 'scanning', 'analyzing')",
        params![now],
    )
    .map_err(|e| e.to_string())?;
    let now = now_iso();
    conn.execute(
        "UPDATE organize_tasks
         SET status='stopped', completed_at=COALESCE(completed_at, ?1)
         WHERE status IN ('idle', 'scanning', 'classifying', 'moving')",
        params![now],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn init_scan_task(
    db_path: &Path,
    task_id: &str,
    root_path: &str,
    target_size: u64,
    max_depth: u32,
    auto_analyze: bool,
) -> Result<(), String> {
    let mut conn = open_db(db_path)?;
    let now = now_iso();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM scan_nodes WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM scan_findings WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT OR REPLACE INTO scan_tasks (
            task_id, root_path, status, target_size, max_depth, auto_analyze,
            current_path, current_depth, scanned_count, total_entries, processed_entries,
            deletable_count, total_cleanable, token_prompt, token_completion, token_total,
            permission_denied_count, permission_denied_paths, error_message, created_at, updated_at, finished_at
        ) VALUES (?1, ?2, 'idle', ?3, ?4, ?5, ?2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, '[]', NULL, ?6, ?6, NULL)",
        params![task_id, root_path, target_size as i64, max_depth as i64, bool_to_i64(auto_analyze), now],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

pub fn save_scan_snapshot(db_path: &Path, snapshot: &ScanSnapshot) -> Result<(), String> {
    let conn = open_db(db_path)?;
    let finished_at = if matches!(snapshot.status.as_str(), "done" | "stopped" | "error") {
        Some(now_iso())
    } else {
        None
    };
    conn.execute(
        "UPDATE scan_tasks SET
            status = ?2,
            current_path = ?3,
            current_depth = ?4,
            scanned_count = ?5,
            total_entries = ?6,
            processed_entries = ?7,
            deletable_count = ?8,
            total_cleanable = ?9,
            token_prompt = ?10,
            token_completion = ?11,
            token_total = ?12,
            permission_denied_count = ?13,
            permission_denied_paths = ?14,
            error_message = ?15,
            updated_at = ?16,
            finished_at = ?17
         WHERE task_id = ?1",
        params![
            snapshot.id,
            snapshot.status,
            snapshot.current_path,
            snapshot.current_depth as i64,
            snapshot.scanned_count as i64,
            snapshot.total_entries as i64,
            snapshot.processed_entries as i64,
            snapshot.deletable_count as i64,
            snapshot.total_cleanable as i64,
            snapshot.token_usage.prompt as i64,
            snapshot.token_usage.completion as i64,
            snapshot.token_usage.total as i64,
            snapshot.permission_denied_count as i64,
            serde_json::to_string(&snapshot.permission_denied_paths).map_err(|e| e.to_string())?,
            if snapshot.error_message.is_empty() {
                None::<String>
            } else {
                Some(snapshot.error_message.clone())
            },
            now_iso(),
            finished_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn upsert_scan_finding(
    db_path: &Path,
    task_id: &str,
    item: &ScanResultItem,
) -> Result<(), String> {
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT INTO scan_findings (
            task_id, path, name, type, size, classification, purpose, reason, risk, source, created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        ON CONFLICT(task_id, path) DO UPDATE SET
            name = excluded.name,
            type = excluded.type,
            size = excluded.size,
            classification = excluded.classification,
            purpose = excluded.purpose,
            reason = excluded.reason,
            risk = excluded.risk,
            source = excluded.source,
            created_at = excluded.created_at",
        params![
            task_id,
            item.path,
            item.name,
            item.item_type,
            item.size as i64,
            item.classification,
            item.purpose,
            item.reason,
            item.risk,
            item.source,
            now_iso(),
        ],
    )
    .map_err(|e| e.to_string())?;
    refresh_scan_stats(db_path, task_id)
}

pub fn refresh_scan_stats(db_path: &Path, task_id: &str) -> Result<(), String> {
    let conn = open_db(db_path)?;
    let (count, total) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(size), 0)
             FROM scan_findings
             WHERE task_id = ?1 AND classification = 'safe_to_delete'",
            params![task_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE scan_tasks SET deletable_count = ?2, total_cleanable = ?3, updated_at = ?4 WHERE task_id = ?1",
        params![task_id, count, total, now_iso()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete_scan_findings_by_paths(
    db_path: &Path,
    task_id: &str,
    paths: &[String],
) -> Result<(), String> {
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for path in paths {
        tx.execute(
            "DELETE FROM scan_findings WHERE task_id = ?1 AND path = ?2",
            params![task_id, path],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    refresh_scan_stats(db_path, task_id)
}

fn load_scan_findings(conn: &Connection, task_id: &str) -> Result<Vec<ScanResultItem>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT name, path, size, type, purpose, reason, risk, classification, source
             FROM scan_findings
             WHERE task_id = ?1 AND classification = 'safe_to_delete'
             ORDER BY size DESC, path ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![task_id], |row| {
            Ok(ScanResultItem {
                name: row.get(0)?,
                path: row.get(1)?,
                size: row.get::<_, i64>(2)? as u64,
                item_type: row.get(3)?,
                purpose: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                reason: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                risk: row
                    .get::<_, Option<String>>(6)?
                    .unwrap_or_else(|| "medium".to_string()),
                classification: row.get(7)?,
                source: row.get(8)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn load_scan_snapshot(db_path: &Path, task_id: &str) -> Result<Option<ScanSnapshot>, String> {
    let conn = open_db(db_path)?;
    let row = conn
        .query_row(
            "SELECT root_path, status, auto_analyze, current_path, current_depth, scanned_count,
                    total_entries, processed_entries, deletable_count, total_cleanable, target_size,
                    token_prompt, token_completion, token_total, permission_denied_count,
                    permission_denied_paths, error_message
             FROM scan_tasks WHERE task_id = ?1",
            params![task_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, Option<String>>(16)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some(row) = row else {
        return Ok(None);
    };
    let findings = load_scan_findings(&conn, task_id)?;
    let root_path = row.0.clone();
    Ok(Some(ScanSnapshot {
        id: task_id.to_string(),
        status: row.1,
        target_path: root_path.clone(),
        auto_analyze: row.2 != 0,
        root_node_id: create_node_id(&root_path),
        current_path: row.3.unwrap_or_else(|| root_path.clone()),
        current_depth: row.4 as u32,
        scanned_count: row.5 as u64,
        total_entries: row.6 as u64,
        processed_entries: row.7 as u64,
        deletable_count: row.8 as u64,
        total_cleanable: row.9 as u64,
        target_size: row.10 as u64,
        token_usage: TokenUsage {
            prompt: row.11 as u64,
            completion: row.12 as u64,
            total: row.13 as u64,
        },
        deletable: findings,
        permission_denied_count: row.14 as u64,
        permission_denied_paths: parse_json_or_default(Some(row.15)),
        error_message: row.16.unwrap_or_default(),
    }))
}

pub fn list_scan_history(db_path: &Path, limit: u32) -> Result<Vec<Value>, String> {
    let conn = open_db(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT task_id, root_path, status, target_size, max_depth, auto_analyze, current_path,
                    current_depth, scanned_count, total_entries, processed_entries, deletable_count,
                    total_cleanable, token_prompt, token_completion, token_total, created_at,
                    updated_at, finished_at, error_message
             FROM scan_tasks
             ORDER BY datetime(updated_at) DESC, task_id DESC
             LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![limit as i64], |row| {
            Ok(json!({
                "taskId": row.get::<_, String>(0)?,
                "rootPath": row.get::<_, String>(1)?,
                "status": row.get::<_, String>(2)?,
                "targetSize": row.get::<_, i64>(3)? as u64,
                "maxDepth": row.get::<_, i64>(4)? as u64,
                "autoAnalyze": row.get::<_, i64>(5)? != 0,
                "currentPath": row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                "currentDepth": row.get::<_, i64>(7)? as u64,
                "scannedCount": row.get::<_, i64>(8)? as u64,
                "totalEntries": row.get::<_, i64>(9)? as u64,
                "processedEntries": row.get::<_, i64>(10)? as u64,
                "deletableCount": row.get::<_, i64>(11)? as u64,
                "totalCleanable": row.get::<_, i64>(12)? as u64,
                "tokenUsage": {
                    "prompt": row.get::<_, i64>(13)? as u64,
                    "completion": row.get::<_, i64>(14)? as u64,
                    "total": row.get::<_, i64>(15)? as u64,
                },
                "createdAt": row.get::<_, String>(16)?,
                "updatedAt": row.get::<_, String>(17)?,
                "finishedAt": row.get::<_, Option<String>>(18)?,
                "errorMessage": row.get::<_, Option<String>>(19)?.unwrap_or_default(),
            }))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn delete_scan_task(db_path: &Path, task_id: &str) -> Result<bool, String> {
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM scan_findings WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM scan_nodes WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    let changed = tx
        .execute(
            "DELETE FROM scan_tasks WHERE task_id = ?1",
            params![task_id],
        )
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(changed > 0)
}

pub fn load_scan_children(
    db_path: &Path,
    task_id: &str,
    node_id: &str,
    dirs_only: bool,
) -> Result<Vec<ScanNode>, String> {
    let conn = open_db(db_path)?;
    let sql = if dirs_only {
        "SELECT node_id, path, name, type, depth, total_size
         FROM scan_nodes
         WHERE task_id = ?1 AND parent_id = ?2 AND type = 'directory'
         ORDER BY total_size DESC, path COLLATE NOCASE ASC"
    } else {
        "SELECT node_id, path, name, type, depth, total_size
         FROM scan_nodes
         WHERE task_id = ?1 AND parent_id = ?2
         ORDER BY total_size DESC, path COLLATE NOCASE ASC"
    };
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![task_id, node_id], |row| {
            Ok(ScanNode {
                id: row.get(0)?,
                path: row.get(1)?,
                name: row.get(2)?,
                node_type: row.get(3)?,
                depth: row.get::<_, i64>(4)? as u32,
                size: row.get::<_, i64>(5)? as u64,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn init_organize_task(db_path: &Path, snapshot: &OrganizeSnapshot) -> Result<(), String> {
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM organize_results WHERE task_id = ?1",
        params![snapshot.id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT OR REPLACE INTO organize_tasks (
            task_id, status, error, root_path, recursive, mode, categories_json, allow_new_categories,
            excluded_patterns_json, parallelism, use_web_search, web_search_enabled, selected_model,
            selected_models_json, selected_providers_json, supports_multimodal, total_files,
            processed_files, token_prompt, token_completion, token_total, preview_json, created_at,
            completed_at, job_id
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                  ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
        params![
            snapshot.id,
            snapshot.status,
            snapshot.error,
            snapshot.root_path,
            bool_to_i64(snapshot.recursive),
            snapshot.mode,
            serde_json::to_string(&snapshot.categories).map_err(|e| e.to_string())?,
            bool_to_i64(snapshot.allow_new_categories),
            serde_json::to_string(&snapshot.excluded_patterns).map_err(|e| e.to_string())?,
            snapshot.parallelism as i64,
            bool_to_i64(snapshot.use_web_search),
            bool_to_i64(snapshot.web_search_enabled),
            snapshot.selected_model,
            serde_json::to_string(&snapshot.selected_models).map_err(|e| e.to_string())?,
            serde_json::to_string(&snapshot.selected_providers).map_err(|e| e.to_string())?,
            bool_to_i64(snapshot.supports_multimodal),
            snapshot.total_files as i64,
            snapshot.processed_files as i64,
            snapshot.token_usage.prompt as i64,
            snapshot.token_usage.completion as i64,
            snapshot.token_usage.total as i64,
            serde_json::to_string(&snapshot.preview).map_err(|e| e.to_string())?,
            snapshot.created_at,
            snapshot.completed_at,
            snapshot.job_id,
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

pub fn save_organize_snapshot(db_path: &Path, snapshot: &OrganizeSnapshot) -> Result<(), String> {
    let conn = open_db(db_path)?;
    conn.execute(
        "UPDATE organize_tasks SET
            status = ?2,
            error = ?3,
            categories_json = ?4,
            excluded_patterns_json = ?5,
            parallelism = ?6,
            use_web_search = ?7,
            web_search_enabled = ?8,
            selected_model = ?9,
            selected_models_json = ?10,
            selected_providers_json = ?11,
            supports_multimodal = ?12,
            total_files = ?13,
            processed_files = ?14,
            token_prompt = ?15,
            token_completion = ?16,
            token_total = ?17,
            preview_json = ?18,
            completed_at = ?19,
            job_id = ?20
         WHERE task_id = ?1",
        params![
            snapshot.id,
            snapshot.status,
            snapshot.error,
            serde_json::to_string(&snapshot.categories).map_err(|e| e.to_string())?,
            serde_json::to_string(&snapshot.excluded_patterns).map_err(|e| e.to_string())?,
            snapshot.parallelism as i64,
            bool_to_i64(snapshot.use_web_search),
            bool_to_i64(snapshot.web_search_enabled),
            snapshot.selected_model,
            serde_json::to_string(&snapshot.selected_models).map_err(|e| e.to_string())?,
            serde_json::to_string(&snapshot.selected_providers).map_err(|e| e.to_string())?,
            bool_to_i64(snapshot.supports_multimodal),
            snapshot.total_files as i64,
            snapshot.processed_files as i64,
            snapshot.token_usage.prompt as i64,
            snapshot.token_usage.completion as i64,
            snapshot.token_usage.total as i64,
            serde_json::to_string(&snapshot.preview).map_err(|e| e.to_string())?,
            snapshot.completed_at,
            snapshot.job_id,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn upsert_organize_result(db_path: &Path, task_id: &str, row: &Value) -> Result<(), String> {
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT INTO organize_results (
            task_id, idx, name, path, relative_path, size, category, created_category, degraded,
            warnings_json, modality, provider, model
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(task_id, idx) DO UPDATE SET
            name = excluded.name,
            path = excluded.path,
            relative_path = excluded.relative_path,
            size = excluded.size,
            category = excluded.category,
            created_category = excluded.created_category,
            degraded = excluded.degraded,
            warnings_json = excluded.warnings_json,
            modality = excluded.modality,
            provider = excluded.provider,
            model = excluded.model",
        params![
            task_id,
            row.get("index").and_then(Value::as_u64).unwrap_or(0) as i64,
            row.get("name").and_then(Value::as_str).unwrap_or(""),
            row.get("path").and_then(Value::as_str).unwrap_or(""),
            row.get("relativePath")
                .and_then(Value::as_str)
                .unwrap_or(""),
            row.get("size").and_then(Value::as_u64).unwrap_or(0) as i64,
            row.get("category")
                .and_then(Value::as_str)
                .unwrap_or("其他待定"),
            bool_to_i64(
                row.get("createdCategory")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            ),
            bool_to_i64(
                row.get("degraded")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            ),
            serde_json::to_string(row.get("warnings").unwrap_or(&json!([])))
                .map_err(|e| e.to_string())?,
            row.get("modality")
                .and_then(Value::as_str)
                .unwrap_or("text"),
            row.get("provider").and_then(Value::as_str).unwrap_or(""),
            row.get("model").and_then(Value::as_str).unwrap_or(""),
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_organize_snapshot(
    db_path: &Path,
    task_id: &str,
) -> Result<Option<OrganizeSnapshot>, String> {
    let conn = open_db(db_path)?;
    let row = conn
        .query_row(
            "SELECT status, error, root_path, recursive, mode, categories_json, allow_new_categories,
                    excluded_patterns_json, parallelism, use_web_search, web_search_enabled,
                    selected_model, selected_models_json, selected_providers_json, supports_multimodal,
                    total_files, processed_files, token_prompt, token_completion, token_total,
                    preview_json, created_at, completed_at, job_id
             FROM organize_tasks WHERE task_id = ?1",
            params![task_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, i64>(16)?,
                    row.get::<_, i64>(17)?,
                    row.get::<_, i64>(18)?,
                    row.get::<_, i64>(19)?,
                    row.get::<_, String>(20)?,
                    row.get::<_, String>(21)?,
                    row.get::<_, Option<String>>(22)?,
                    row.get::<_, Option<String>>(23)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some(row) = row else {
        return Ok(None);
    };

    let mut stmt = conn
        .prepare(
            "SELECT idx, name, path, relative_path, size, category, created_category, degraded,
                    warnings_json, modality, provider, model
             FROM organize_results
             WHERE task_id = ?1
             ORDER BY idx ASC",
        )
        .map_err(|e| e.to_string())?;
    let results = stmt
        .query_map(params![task_id], |r| {
            Ok(json!({
                "index": r.get::<_, i64>(0)? as u64,
                "name": r.get::<_, String>(1)?,
                "path": r.get::<_, String>(2)?,
                "relativePath": r.get::<_, String>(3)?,
                "size": r.get::<_, i64>(4)? as u64,
                "category": r.get::<_, String>(5)?,
                "createdCategory": r.get::<_, i64>(6)? != 0,
                "degraded": r.get::<_, i64>(7)? != 0,
                "warnings": parse_json_or_default::<Vec<String>>(Some(r.get::<_, String>(8)?)),
                "modality": r.get::<_, String>(9)?,
                "provider": r.get::<_, String>(10)?,
                "model": r.get::<_, String>(11)?,
            }))
        })
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();

    Ok(Some(OrganizeSnapshot {
        id: task_id.to_string(),
        status: row.0,
        error: row.1,
        root_path: row.2,
        recursive: row.3 != 0,
        mode: row.4,
        categories: parse_json_or_default(Some(row.5)),
        allow_new_categories: row.6 != 0,
        excluded_patterns: parse_json_or_default(Some(row.7)),
        parallelism: row.8 as u32,
        use_web_search: row.9 != 0,
        web_search_enabled: row.10 != 0,
        selected_model: row.11,
        selected_models: parse_json_or_default(Some(row.12)),
        selected_providers: parse_json_or_default(Some(row.13)),
        supports_multimodal: row.14 != 0,
        total_files: row.15 as u64,
        processed_files: row.16 as u64,
        token_usage: TokenUsage {
            prompt: row.17 as u64,
            completion: row.18 as u64,
            total: row.19 as u64,
        },
        results,
        preview: parse_json_or_default(Some(row.20)),
        created_at: row.21,
        completed_at: row.22,
        job_id: row.23,
    }))
}

pub fn save_organize_manifest(db_path: &Path, manifest: &Value) -> Result<(), String> {
    let mut conn = open_db(db_path)?;
    let job_id = manifest
        .get("jobId")
        .and_then(Value::as_str)
        .ok_or_else(|| "manifest.jobId is required".to_string())?;
    let task_id = manifest
        .get("taskId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let root_path = manifest
        .get("rootPath")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let created_at = manifest
        .get("createdAt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let entries = manifest
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT OR REPLACE INTO organize_jobs (job_id, task_id, root_path, created_at, manifest_json, rollback_json)
         VALUES (?1, ?2, ?3, ?4, ?5, COALESCE((SELECT rollback_json FROM organize_jobs WHERE job_id = ?1), NULL))",
        params![job_id, task_id, root_path, created_at, manifest.to_string()],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM organize_job_entries WHERE job_id = ?1",
        params![job_id],
    )
    .map_err(|e| e.to_string())?;
    for (idx, entry) in entries.iter().enumerate() {
        tx.execute(
            "INSERT INTO organize_job_entries (job_id, idx, source_path, target_path, category, status, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                job_id,
                idx as i64,
                entry.get("sourcePath").and_then(Value::as_str).unwrap_or(""),
                entry.get("targetPath").and_then(Value::as_str).unwrap_or(""),
                entry.get("category").and_then(Value::as_str).unwrap_or(""),
                entry.get("status").and_then(Value::as_str).unwrap_or(""),
                entry.get("error").and_then(Value::as_str),
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_organize_job(db_path: &Path, job_id: &str) -> Result<Option<Value>, String> {
    let conn = open_db(db_path)?;
    let row = conn
        .query_row(
            "SELECT manifest_json, rollback_json FROM organize_jobs WHERE job_id = ?1",
            params![job_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((manifest_json, rollback_json)) = row else {
        return Ok(None);
    };
    let mut manifest = serde_json::from_str::<Value>(&manifest_json).map_err(|e| e.to_string())?;
    if let Some(rollback_text) = rollback_json {
        if let Ok(rollback) = serde_json::from_str::<Value>(&rollback_text) {
            manifest["rollback"] = rollback;
        }
    }
    Ok(Some(manifest))
}

pub fn load_organize_job_entries(db_path: &Path, job_id: &str) -> Result<Vec<Value>, String> {
    let conn = open_db(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT idx, source_path, target_path, category, status, error
             FROM organize_job_entries
             WHERE job_id = ?1
             ORDER BY idx ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![job_id], |row| {
            Ok(json!({
                "sourcePath": row.get::<_, String>(1)?,
                "targetPath": row.get::<_, String>(2)?,
                "category": row.get::<_, String>(3)?,
                "status": row.get::<_, String>(4)?,
                "error": row.get::<_, Option<String>>(5)?,
            }))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn save_organize_rollback(
    db_path: &Path,
    job_id: &str,
    rollback: &Value,
) -> Result<(), String> {
    let conn = open_db(db_path)?;
    conn.execute(
        "UPDATE organize_jobs SET rollback_json = ?2 WHERE job_id = ?1",
        params![job_id, rollback.to_string()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
