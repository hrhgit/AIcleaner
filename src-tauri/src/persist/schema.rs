use super::organize::{create_organizer_tables, drop_organizer_tables, organizer_tables_exist};
use super::scan::{
    cleanup_scan_drafts, normalize_existing_root_path_keys, run_scan_cache_maintenance,
};
use super::*;

pub fn init_db(db_path: &Path) -> Result<(), String> {
    let mut conn = open_db(db_path)?;
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
            last_error_json TEXT,
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
        CREATE INDEX IF NOT EXISTS idx_scan_findings_classification
        ON scan_findings(task_id, classification, size DESC);
        CREATE INDEX IF NOT EXISTS idx_scan_tasks_root_visible
        ON scan_tasks(root_path_key, visible_latest, updated_at DESC);

        CREATE TABLE IF NOT EXISTS scan_drafts (
            task_id TEXT PRIMARY KEY,
            root_path TEXT NOT NULL,
            root_path_key TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL,
            scan_mode TEXT NOT NULL DEFAULT 'full_rescan_incremental',
            baseline_task_id TEXT,
            visible_latest INTEGER NOT NULL DEFAULT 0,
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
            last_error_json TEXT,
            error_message TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            finished_at TEXT
        );

        CREATE TABLE IF NOT EXISTS scan_draft_nodes (
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

        CREATE TABLE IF NOT EXISTS scan_draft_findings (
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

        CREATE INDEX IF NOT EXISTS idx_scan_draft_nodes_parent
        ON scan_draft_nodes(task_id, parent_id, total_size DESC);
        CREATE INDEX IF NOT EXISTS idx_scan_draft_findings_classification
        ON scan_draft_findings(task_id, classification, size DESC);
        CREATE INDEX IF NOT EXISTS idx_scan_drafts_root
        ON scan_drafts(root_path_key, updated_at DESC);
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
    if !scan_task_columns.iter().any(|col| col == "last_error_json") {
        conn.execute(
            "ALTER TABLE scan_tasks ADD COLUMN last_error_json TEXT",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    let scan_draft_columns = conn
        .prepare("PRAGMA table_info(scan_drafts)")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .map_err(|e| e.to_string())?;
    if !scan_draft_columns.iter().any(|col| col == "last_error_json") {
        conn.execute(
            "ALTER TABLE scan_drafts ADD COLUMN last_error_json TEXT",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
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
    conn.execute("DROP INDEX IF EXISTS idx_scan_nodes_path", [])
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
    normalize_existing_root_path_keys(&conn)?;
    cleanup_scan_drafts(&conn)?;
    run_scan_cache_maintenance(db_path, &mut conn)?;
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
