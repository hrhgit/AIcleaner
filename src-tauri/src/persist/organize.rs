use super::*;

fn compact_organize_snapshot(snapshot: &OrganizeSnapshot) -> OrganizeSnapshot {
    let mut compact = snapshot.clone();
    compact.results.clear();
    compact.preview.clear();
    compact
}

pub(crate) fn organizer_tables_exist(conn: &Connection) -> Result<bool, String> {
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

pub(crate) fn drop_organizer_tables(conn: &Connection) -> Result<(), String> {
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

pub(crate) fn create_organizer_tables(conn: &Connection) -> Result<(), String> {
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
pub fn init_organize_task(db_path: &Path, snapshot: &OrganizeSnapshot) -> Result<(), String> {
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let compact_snapshot = compact_organize_snapshot(snapshot);
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
            serde_json::to_string(&compact_snapshot).map_err(|e| e.to_string())?,
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

pub fn save_organize_snapshot(db_path: &Path, snapshot: &OrganizeSnapshot) -> Result<(), String> {
    let compact_snapshot = compact_organize_snapshot(snapshot);
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
            serde_json::to_string(&compact_snapshot).map_err(|e| e.to_string())?,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn upsert_organize_results(
    db_path: &Path,
    task_id: &str,
    rows: &[Value],
) -> Result<(), String> {
    if rows.is_empty() {
        return Ok(());
    }

    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut stmt = tx
        .prepare_cached(
            "INSERT INTO organize_results (
                task_id, idx, path, leaf_node_id, category_path_json, row_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(task_id, idx) DO UPDATE SET
                path = excluded.path,
                leaf_node_id = excluded.leaf_node_id,
                category_path_json = excluded.category_path_json,
                row_json = excluded.row_json",
        )
        .map_err(|e| e.to_string())?;
    for row in rows {
        stmt.execute(params![
            task_id,
            row.get("index").and_then(Value::as_u64).unwrap_or(0) as i64,
            row.get("path").and_then(Value::as_str).unwrap_or(""),
            row.get("leafNodeId").and_then(Value::as_str).unwrap_or(""),
            serde_json::to_string(row.get("categoryPath").unwrap_or(&json!([])))
                .map_err(|e| e.to_string())?,
            serde_json::to_string(row).map_err(|e| e.to_string())?,
        ])
        .map_err(|e| e.to_string())?;
    }
    drop(stmt);
    tx.commit().map_err(|e| e.to_string())?;
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
    snapshot.preview.clear();
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

pub fn find_latest_organize_task_id_for_root(
    db_path: &Path,
    root_path: &str,
) -> Result<Option<String>, String> {
    let conn = open_db(db_path)?;
    conn.query_row(
        "SELECT task_id
         FROM organize_tasks
         WHERE root_path_key = ?1
         ORDER BY datetime(created_at) DESC, task_id DESC
         LIMIT 1",
        params![create_root_path_key(root_path)],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| e.to_string())
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
