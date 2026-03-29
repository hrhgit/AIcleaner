use crate::backend::{OrganizeSnapshot, ScanResultItem, ScanSnapshot, TokenUsage};
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sha1::Digest;
use std::path::Path;

const ORGANIZER_SCHEMA_VERSION: &str = "tree_v1";

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
pub struct ScanNodeUpsert {
    pub node_id: String,
    pub parent_id: Option<String>,
    pub path: String,
    pub name: String,
    pub node_type: String,
    pub depth: u32,
    pub self_size: u64,
    pub size: u64,
    pub child_count: u64,
    pub mtime_ms: Option<i64>,
    pub ext: String,
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

fn organizer_tables_exist(conn: &Connection) -> Result<bool, String> {
    let count = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN (
                'organize_tasks',
                'organize_results',
                'organize_jobs',
                'organize_job_entries',
                'organize_latest_trees'
            )",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(count > 0)
}

fn drop_organizer_tables(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS organize_job_entries;
        DROP TABLE IF EXISTS organize_jobs;
        DROP TABLE IF EXISTS organize_results;
        DROP TABLE IF EXISTS organize_tasks;
        DROP TABLE IF EXISTS organize_latest_trees;
        "#,
    )
    .map_err(|e| e.to_string())
}

fn create_organizer_tables(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS organize_tasks (
            task_id TEXT PRIMARY KEY,
            root_path TEXT NOT NULL,
            root_path_key TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            completed_at TEXT,
            snapshot_json TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_organize_tasks_root
        ON organize_tasks(root_path_key, created_at DESC);

        CREATE TABLE IF NOT EXISTS organize_results (
            task_id TEXT NOT NULL,
            idx INTEGER NOT NULL,
            path TEXT NOT NULL,
            leaf_node_id TEXT NOT NULL,
            category_path_json TEXT NOT NULL DEFAULT '[]',
            row_json TEXT NOT NULL,
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
            entry_type TEXT NOT NULL DEFAULT 'file',
            category TEXT NOT NULL,
            status TEXT NOT NULL,
            error TEXT,
            PRIMARY KEY (job_id, idx)
        );

        CREATE TABLE IF NOT EXISTS organize_latest_trees (
            root_path_key TEXT PRIMARY KEY,
            root_path TEXT NOT NULL,
            tree_version INTEGER NOT NULL DEFAULT 0,
            tree_json TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        "#,
    )
    .map_err(|e| e.to_string())
}

pub fn create_node_id(path_value: &str) -> String {
    let mut sha = sha1::Sha1::new();
    sha.update(path_value.to_lowercase().as_bytes());
    format!("{:x}", sha.finalize())
}

pub fn create_root_path_key(path_value: &str) -> String {
    path_value.trim().to_lowercase()
}

pub fn init_db(db_path: &Path) -> Result<(), String> {
    let conn = open_db(db_path)?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS scan_tasks (
            task_id TEXT PRIMARY KEY,
            root_path TEXT NOT NULL,
            root_path_key TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL,
            scan_mode TEXT NOT NULL DEFAULT 'full_rescan_incremental',
            baseline_task_id TEXT,
            visible_latest INTEGER NOT NULL DEFAULT 1,
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
            should_expand INTEGER NOT NULL DEFAULT 0,
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
        CREATE INDEX IF NOT EXISTS idx_scan_tasks_root_visible
        ON scan_tasks(root_path_key, visible_latest, updated_at DESC);
        "#,
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS app_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
    let scan_task_columns = conn
        .prepare("PRAGMA table_info(scan_tasks)")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .map_err(|e| e.to_string())?;
    if !scan_task_columns.iter().any(|col| col == "root_path_key") {
        conn.execute(
            "ALTER TABLE scan_tasks ADD COLUMN root_path_key TEXT NOT NULL DEFAULT ''",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    if !scan_task_columns.iter().any(|col| col == "scan_mode") {
        conn.execute(
            "ALTER TABLE scan_tasks ADD COLUMN scan_mode TEXT NOT NULL DEFAULT 'full_rescan_incremental'",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    if !scan_task_columns
        .iter()
        .any(|col| col == "baseline_task_id")
    {
        conn.execute(
            "ALTER TABLE scan_tasks ADD COLUMN baseline_task_id TEXT",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    if !scan_task_columns.iter().any(|col| col == "visible_latest") {
        conn.execute(
            "ALTER TABLE scan_tasks ADD COLUMN visible_latest INTEGER NOT NULL DEFAULT 1",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    conn.execute(
        "UPDATE scan_tasks SET root_path_key = COALESCE(NULLIF(root_path_key, ''), lower(root_path)) WHERE root_path_key = '' OR root_path_key IS NULL",
        [],
    )
    .map_err(|e| e.to_string())?;
    let scan_finding_columns = conn
        .prepare("PRAGMA table_info(scan_findings)")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .map_err(|e| e.to_string())?;
    if !scan_finding_columns
        .iter()
        .any(|col| col == "should_expand")
    {
        conn.execute(
            "ALTER TABLE scan_findings ADD COLUMN should_expand INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_scan_tasks_root_visible ON scan_tasks(root_path_key, visible_latest, updated_at DESC)",
        [],
    )
    .map_err(|e| e.to_string())?;

    let current_organizer_schema = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = 'organizer_schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let needs_reset = current_organizer_schema
        .as_deref()
        .map(|value| value != ORGANIZER_SCHEMA_VERSION)
        .unwrap_or_else(|| organizer_tables_exist(&conn).unwrap_or(false));
    if needs_reset {
        drop_organizer_tables(&conn)?;
    }
    create_organizer_tables(&conn)?;
    conn.execute(
        "INSERT INTO app_meta(key, value) VALUES('organizer_schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![ORGANIZER_SCHEMA_VERSION],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
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
    let mut stmt = conn
        .prepare(
            "SELECT task_id, snapshot_json
             FROM organize_tasks
             WHERE status IN ('idle', 'scanning', 'classifying', 'moving')",
        )
        .map_err(|e| e.to_string())?;
    let stale_rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);
    for (task_id, snapshot_json) in stale_rows {
        let mut snapshot =
            serde_json::from_str::<OrganizeSnapshot>(&snapshot_json).map_err(|e| e.to_string())?;
        snapshot.status = "stopped".to_string();
        if snapshot.completed_at.is_none() {
            snapshot.completed_at = Some(now.clone());
        }
        conn.execute(
            "UPDATE organize_tasks
             SET status = ?2, completed_at = ?3, snapshot_json = ?4
             WHERE task_id = ?1",
            params![
                task_id,
                snapshot.status,
                snapshot.completed_at,
                serde_json::to_string(&snapshot).map_err(|e| e.to_string())?,
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn init_scan_task(
    db_path: &Path,
    task_id: &str,
    root_path: &str,
    target_size: u64,
    max_depth: Option<u32>,
    auto_analyze: bool,
    baseline_task_id: Option<&str>,
    scan_mode: &str,
) -> Result<(), String> {
    let mut conn = open_db(db_path)?;
    let now = now_iso();
    let root_path_key = create_root_path_key(root_path);
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE scan_tasks SET visible_latest = 0 WHERE root_path_key = ?1",
        params![root_path_key],
    )
    .map_err(|e| e.to_string())?;
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
            task_id, root_path, root_path_key, status, scan_mode, baseline_task_id, visible_latest, target_size, max_depth, auto_analyze,
            current_path, current_depth, scanned_count, total_entries, processed_entries,
            deletable_count, total_cleanable, token_prompt, token_completion, token_total,
            permission_denied_count, permission_denied_paths, error_message, created_at, updated_at, finished_at
        ) VALUES (?1, ?2, ?3, 'idle', ?4, ?5, 1, ?6, ?7, ?8, ?2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, '[]', NULL, ?9, ?9, NULL)",
        params![
            task_id,
            root_path,
            root_path_key,
            scan_mode,
            baseline_task_id,
            target_size as i64,
            max_depth.map(|value| value as i64),
            bool_to_i64(auto_analyze),
            now
        ],
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
    should_expand: bool,
) -> Result<(), String> {
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT INTO scan_findings (
            task_id, path, name, type, size, classification, should_expand, purpose, reason, risk, source, created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ON CONFLICT(task_id, path) DO UPDATE SET
            name = excluded.name,
            type = excluded.type,
            size = excluded.size,
            classification = excluded.classification,
            should_expand = excluded.should_expand,
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
            bool_to_i64(should_expand),
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

pub fn upsert_scan_nodes(
    db_path: &Path,
    task_id: &str,
    nodes: &[ScanNodeUpsert],
) -> Result<(), String> {
    if nodes.is_empty() {
        return Ok(());
    }
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut stmt = tx
        .prepare_cached(
            "INSERT INTO scan_nodes (
                task_id, node_id, parent_id, path, name, type, depth,
                self_size, total_size, child_count, mtime_ms, ext
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(task_id, node_id) DO UPDATE SET
                parent_id = excluded.parent_id,
                path = excluded.path,
                name = excluded.name,
                type = excluded.type,
                depth = excluded.depth,
                self_size = excluded.self_size,
                total_size = excluded.total_size,
                child_count = excluded.child_count,
                mtime_ms = excluded.mtime_ms,
                ext = excluded.ext",
        )
        .map_err(|e| e.to_string())?;
    for node in nodes {
        stmt.execute(params![
            task_id,
            node.node_id,
            node.parent_id,
            node.path,
            node.name,
            node.node_type,
            node.depth as i64,
            node.self_size as i64,
            node.size as i64,
            node.child_count as i64,
            node.mtime_ms,
            node.ext,
        ])
        .map_err(|e| e.to_string())?;
    }
    drop(stmt);
    tx.commit().map_err(|e| e.to_string())
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

pub fn load_scan_findings_map(
    db_path: &Path,
    task_id: &str,
) -> Result<std::collections::HashMap<String, ScanFindingRecord>, String> {
    let conn = open_db(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT name, path, size, type, purpose, reason, risk, classification, source, should_expand
             FROM scan_findings
             WHERE task_id = ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![task_id], |row| {
            Ok(ScanFindingRecord {
                item: ScanResultItem {
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
                },
                should_expand: row.get::<_, i64>(9)? != 0,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows
        .filter_map(Result::ok)
        .map(|record| (record.item.path.to_lowercase(), record))
        .collect())
}

pub fn load_scan_snapshot(db_path: &Path, task_id: &str) -> Result<Option<ScanSnapshot>, String> {
    let conn = open_db(db_path)?;
    let row = conn
        .query_row(
            "SELECT root_path, root_path_key, status, scan_mode, baseline_task_id, visible_latest,
                    auto_analyze, max_depth, current_path, current_depth, scanned_count,
                    total_entries, processed_entries, deletable_count, total_cleanable, target_size,
                    token_prompt, token_completion, token_total, permission_denied_count,
                    permission_denied_paths, error_message
             FROM scan_tasks WHERE task_id = ?1",
            params![task_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, i64>(16)?,
                    row.get::<_, i64>(17)?,
                    row.get::<_, i64>(18)?,
                    row.get::<_, i64>(19)?,
                    row.get::<_, String>(20)?,
                    row.get::<_, Option<String>>(21)?,
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
    let max_scanned_depth = conn
        .query_row(
            "SELECT COALESCE(MAX(depth), 0) FROM scan_nodes WHERE task_id = ?1",
            params![task_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|e| e.to_string())? as u32;
    Ok(Some(ScanSnapshot {
        id: task_id.to_string(),
        status: row.2,
        scan_mode: row.3,
        baseline_task_id: row.4,
        visible_latest: row.5 != 0,
        root_path_key: row.1,
        target_path: root_path.clone(),
        auto_analyze: row.6 != 0,
        root_node_id: create_node_id(&root_path),
        configured_max_depth: row.7.map(|value| value as u32),
        max_scanned_depth,
        current_path: row.8.unwrap_or_else(|| root_path.clone()),
        current_depth: row.9 as u32,
        scanned_count: row.10 as u64,
        total_entries: row.11 as u64,
        processed_entries: row.12 as u64,
        deletable_count: row.13 as u64,
        total_cleanable: row.14 as u64,
        target_size: row.15 as u64,
        token_usage: TokenUsage {
            prompt: row.16 as u64,
            completion: row.17 as u64,
            total: row.18 as u64,
        },
        deletable: findings,
        permission_denied_count: row.19 as u64,
        permission_denied_paths: parse_json_or_default(Some(row.20)),
        error_message: row.21.unwrap_or_default(),
    }))
}

pub fn list_scan_history(db_path: &Path, limit: u32) -> Result<Vec<Value>, String> {
    let conn = open_db(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT task_id, root_path, root_path_key, status, scan_mode, baseline_task_id, target_size, max_depth, auto_analyze, current_path,
                    current_depth, scanned_count, total_entries, processed_entries, deletable_count,
                    total_cleanable, token_prompt, token_completion, token_total, created_at,
                    updated_at, finished_at, error_message
             FROM scan_tasks
             WHERE visible_latest = 1
             ORDER BY datetime(updated_at) DESC, task_id DESC
             LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![limit as i64], |row| {
            Ok(json!({
                "taskId": row.get::<_, String>(0)?,
                "rootPath": row.get::<_, String>(1)?,
                "rootPathKey": row.get::<_, String>(2)?,
                "status": row.get::<_, String>(3)?,
                "scanMode": row.get::<_, String>(4)?,
                "baselineTaskId": row.get::<_, Option<String>>(5)?,
                "targetSize": row.get::<_, i64>(6)? as u64,
                "maxDepth": row.get::<_, Option<i64>>(7)?.map(|value| value as u64),
                "autoAnalyze": row.get::<_, i64>(8)? != 0,
                "currentPath": row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                "currentDepth": row.get::<_, i64>(10)? as u64,
                "scannedCount": row.get::<_, i64>(11)? as u64,
                "totalEntries": row.get::<_, i64>(12)? as u64,
                "processedEntries": row.get::<_, i64>(13)? as u64,
                "deletableCount": row.get::<_, i64>(14)? as u64,
                "totalCleanable": row.get::<_, i64>(15)? as u64,
                "tokenUsage": {
                    "prompt": row.get::<_, i64>(16)? as u64,
                    "completion": row.get::<_, i64>(17)? as u64,
                    "total": row.get::<_, i64>(18)? as u64,
                },
                "createdAt": row.get::<_, String>(19)?,
                "updatedAt": row.get::<_, String>(20)?,
                "finishedAt": row.get::<_, Option<String>>(21)?,
                "errorMessage": row.get::<_, Option<String>>(22)?.unwrap_or_default(),
            }))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn delete_scan_task(db_path: &Path, task_id: &str) -> Result<bool, String> {
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let meta = tx
        .query_row(
            "SELECT root_path_key, visible_latest FROM scan_tasks WHERE task_id = ?1",
            params![task_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((root_path_key, was_visible_latest)) = meta else {
        return Ok(false);
    };
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
    if changed > 0 && was_visible_latest != 0 {
        tx.execute(
            "UPDATE scan_tasks
             SET visible_latest = 1
             WHERE task_id = (
                SELECT task_id
                FROM scan_tasks
                WHERE root_path_key = ?1
                ORDER BY datetime(updated_at) DESC, task_id DESC
                LIMIT 1
             )",
            params![root_path_key],
        )
        .map_err(|e| e.to_string())?;
    }
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
        "SELECT node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms
         FROM scan_nodes
         WHERE task_id = ?1 AND parent_id = ?2 AND type = 'directory'
         ORDER BY total_size DESC, path COLLATE NOCASE ASC"
    } else {
        "SELECT node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms
         FROM scan_nodes
         WHERE task_id = ?1 AND parent_id = ?2
         ORDER BY total_size DESC, path COLLATE NOCASE ASC"
    };
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![task_id, node_id], |row| {
            Ok(ScanNode {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                path: row.get(2)?,
                name: row.get(3)?,
                node_type: row.get(4)?,
                depth: row.get::<_, i64>(5)? as u32,
                self_size: row.get::<_, i64>(6)? as u64,
                size: row.get::<_, i64>(7)? as u64,
                child_count: row.get::<_, i64>(8)? as u64,
                mtime_ms: row.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn load_scan_node_map(
    db_path: &Path,
    task_id: &str,
) -> Result<std::collections::HashMap<String, ScanNode>, String> {
    let conn = open_db(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms
             FROM scan_nodes
             WHERE task_id = ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![task_id], |row| {
            Ok(ScanNode {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                path: row.get(2)?,
                name: row.get(3)?,
                node_type: row.get(4)?,
                depth: row.get::<_, i64>(5)? as u32,
                self_size: row.get::<_, i64>(6)? as u64,
                size: row.get::<_, i64>(7)? as u64,
                child_count: row.get::<_, i64>(8)? as u64,
                mtime_ms: row.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows
        .filter_map(Result::ok)
        .map(|node| (node.path.to_lowercase(), node))
        .collect())
}

pub fn clone_scan_task_data(
    db_path: &Path,
    from_task_id: &str,
    to_task_id: &str,
) -> Result<(), String> {
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT INTO scan_nodes (task_id, node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms, ext)
         SELECT ?2, node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms, ext
         FROM scan_nodes WHERE task_id = ?1",
        params![from_task_id, to_task_id],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO scan_findings (task_id, path, name, type, size, classification, should_expand, purpose, reason, risk, source, created_at)
         SELECT ?2, path, name, type, size, classification, should_expand, purpose, reason, risk, source, created_at
         FROM scan_findings WHERE task_id = ?1",
        params![from_task_id, to_task_id],
    )
    .map_err(|e| e.to_string())?;
    refresh_scan_stats(db_path, to_task_id)
}

pub fn delete_scan_data_for_paths(
    db_path: &Path,
    task_id: &str,
    paths: &[String],
) -> Result<(), String> {
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for path in paths {
        let variants = [path.clone(), format!("{path}\\%"), format!("{path}/%")];
        tx.execute(
            "DELETE FROM scan_findings WHERE task_id = ?1 AND (path = ?2 OR path LIKE ?3 OR path LIKE ?4)",
            params![task_id, variants[0], variants[1], variants[2]],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM scan_nodes WHERE task_id = ?1 AND (path = ?2 OR path LIKE ?3 OR path LIKE ?4)",
            params![task_id, variants[0], variants[1], variants[2]],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    refresh_scan_stats(db_path, task_id)
}

pub fn find_latest_visible_scan_task_id_for_path(
    db_path: &Path,
    path: &str,
) -> Result<Option<String>, String> {
    let conn = open_db(db_path)?;
    conn.query_row(
        "SELECT task_id
         FROM scan_tasks
         WHERE root_path_key = ?1 AND visible_latest = 1
         ORDER BY datetime(updated_at) DESC, task_id DESC
         LIMIT 1",
        params![create_root_path_key(path)],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

pub fn list_boundary_scan_nodes(
    db_path: &Path,
    task_id: &str,
    depth: u32,
) -> Result<Vec<ScanNode>, String> {
    let conn = open_db(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT n.node_id, n.parent_id, n.path, n.name, n.type, n.depth, n.self_size, n.total_size, n.child_count, n.mtime_ms
             FROM scan_nodes n
             LEFT JOIN scan_findings f ON f.task_id = n.task_id AND f.path = n.path
             WHERE n.task_id = ?1
               AND n.type = 'directory'
               AND n.depth = ?2
               AND COALESCE(f.classification, '') <> 'safe_to_delete'
               AND COALESCE(f.should_expand, 1) <> 0
             ORDER BY n.total_size DESC, n.path COLLATE NOCASE ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![task_id, depth as i64], |row| {
            Ok(ScanNode {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                path: row.get(2)?,
                name: row.get(3)?,
                node_type: row.get(4)?,
                depth: row.get::<_, i64>(5)? as u32,
                self_size: row.get::<_, i64>(6)? as u64,
                size: row.get::<_, i64>(7)? as u64,
                child_count: row.get::<_, i64>(8)? as u64,
                mtime_ms: row.get(9)?,
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
            task_id, root_path, root_path_key, status, created_at, completed_at, snapshot_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            snapshot.id,
            snapshot.root_path,
            create_root_path_key(&snapshot.root_path),
            snapshot.status,
            snapshot.created_at,
            snapshot.completed_at,
            serde_json::to_string(snapshot).map_err(|e| e.to_string())?,
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

pub fn save_organize_snapshot(db_path: &Path, snapshot: &OrganizeSnapshot) -> Result<(), String> {
    let conn = open_db(db_path)?;
    conn.execute(
        "UPDATE organize_tasks SET
            root_path = ?2,
            root_path_key = ?3,
            status = ?4,
            completed_at = ?5,
            snapshot_json = ?6
         WHERE task_id = ?1",
        params![
            snapshot.id,
            snapshot.root_path,
            create_root_path_key(&snapshot.root_path),
            snapshot.status,
            snapshot.completed_at,
            serde_json::to_string(snapshot).map_err(|e| e.to_string())?,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}


pub fn upsert_organize_result(db_path: &Path, task_id: &str, row: &Value) -> Result<(), String> {
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT INTO organize_results (
            task_id, idx, path, leaf_node_id, category_path_json, row_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(task_id, idx) DO UPDATE SET
            path = excluded.path,
            leaf_node_id = excluded.leaf_node_id,
            category_path_json = excluded.category_path_json,
            row_json = excluded.row_json",
        params![
            task_id,
            row.get("index").and_then(Value::as_u64).unwrap_or(0) as i64,
            row.get("path").and_then(Value::as_str).unwrap_or(""),
            row.get("leafNodeId").and_then(Value::as_str).unwrap_or(""),
            serde_json::to_string(row.get("categoryPath").unwrap_or(&json!([])))
                .map_err(|e| e.to_string())?,
            serde_json::to_string(row).map_err(|e| e.to_string())?,
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
    let snapshot_json = conn
        .query_row(
            "SELECT snapshot_json FROM organize_tasks WHERE task_id = ?1",
            params![task_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some(snapshot_json) = snapshot_json else {
        return Ok(None);
    };

    let mut stmt = conn
        .prepare(
            "SELECT row_json
             FROM organize_results
             WHERE task_id = ?1
             ORDER BY idx ASC",
        )
        .map_err(|e| e.to_string())?;
    let results = stmt
        .query_map(params![task_id], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(|row| {
            row.ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        })
        .collect::<Vec<_>>();

    let mut snapshot =
        serde_json::from_str::<OrganizeSnapshot>(&snapshot_json).map_err(|e| e.to_string())?;
    snapshot.results = results;
    Ok(Some(snapshot))
}

pub fn save_latest_organize_tree(
    db_path: &Path,
    root_path: &str,
    tree: &Value,
    tree_version: u64,
) -> Result<(), String> {
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT INTO organize_latest_trees (
            root_path_key, root_path, tree_version, tree_json, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(root_path_key) DO UPDATE SET
            root_path = excluded.root_path,
            tree_version = excluded.tree_version,
            tree_json = excluded.tree_json,
            updated_at = excluded.updated_at",
        params![
            create_root_path_key(root_path),
            root_path,
            tree_version as i64,
            serde_json::to_string(tree).map_err(|e| e.to_string())?,
            now_iso(),
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_latest_organize_tree(
    db_path: &Path,
    root_path: &str,
) -> Result<Option<(Value, u64)>, String> {
    let conn = open_db(db_path)?;
    let row = conn
        .query_row(
            "SELECT tree_json, tree_version
             FROM organize_latest_trees
             WHERE root_path_key = ?1",
            params![create_root_path_key(root_path)],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((tree_json, tree_version)) = row else {
        return Ok(None);
    };
    Ok(Some((
        serde_json::from_str::<Value>(&tree_json).map_err(|e| e.to_string())?,
        tree_version as u64,
    )))
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
            "INSERT INTO organize_job_entries (job_id, idx, source_path, target_path, entry_type, category, status, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                job_id,
                idx as i64,
                entry.get("sourcePath").and_then(Value::as_str).unwrap_or(""),
                entry.get("targetPath").and_then(Value::as_str).unwrap_or(""),
                entry.get("itemType").and_then(Value::as_str).unwrap_or("file"),
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
            "SELECT idx, source_path, target_path, entry_type, category, status, error
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
                "itemType": row.get::<_, String>(3)?,
                "category": row.get::<_, String>(4)?,
                "status": row.get::<_, String>(5)?,
                "error": row.get::<_, Option<String>>(6)?,
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
