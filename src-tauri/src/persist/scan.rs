use super::*;

#[derive(Clone, Debug)]
struct ScanTaskMeta {
    task_id: String,
    root_path: String,
    root_path_key: String,
    visible_latest: bool,
    updated_at: String,
}

fn open_scan_db_for_read(db_path: &Path) -> Result<Connection, String> {
    ensure_scan_read_ready(db_path)?;
    open_db_raw(db_path)
}

pub(crate) fn normalize_existing_root_path_keys(conn: &Connection) -> Result<(), String> {
    let scan_rows = conn
        .prepare("SELECT task_id, root_path FROM scan_tasks")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .map_err(|e| e.to_string())?;
    for (task_id, root_path) in scan_rows {
        conn.execute(
            "UPDATE scan_tasks SET root_path_key = ?2 WHERE task_id = ?1",
            params![task_id, create_root_path_key(&root_path)],
        )
        .map_err(|e| e.to_string())?;
    }

    let draft_rows = conn
        .prepare("SELECT task_id, root_path FROM scan_drafts")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .map_err(|e| e.to_string())?;
    for (task_id, root_path) in draft_rows {
        conn.execute(
            "UPDATE scan_drafts SET root_path_key = ?2 WHERE task_id = ?1",
            params![task_id, create_root_path_key(&root_path)],
        )
        .map_err(|e| e.to_string())?;
    }

    let organize_rows = conn
        .prepare("SELECT task_id, root_path FROM organize_tasks")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .map_err(|e| e.to_string())?;
    for (task_id, root_path) in organize_rows {
        conn.execute(
            "UPDATE organize_tasks SET root_path_key = ?2 WHERE task_id = ?1",
            params![task_id, create_root_path_key(&root_path)],
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(())
}

pub(crate) fn cleanup_scan_drafts(conn: &Connection) -> Result<(), String> {
    conn.execute("DELETE FROM scan_draft_findings", [])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM scan_draft_nodes", [])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM scan_drafts", [])
        .map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO app_meta(key, value) VALUES('scan_draft_schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![SCAN_DRAFT_SCHEMA_VERSION],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn load_scan_task_meta(conn: &Connection) -> Result<Vec<ScanTaskMeta>, String> {
    conn.prepare(
        "SELECT task_id, root_path, root_path_key, visible_latest, updated_at
         FROM scan_tasks",
    )
    .and_then(|mut stmt| {
        stmt.query_map([], |row| {
            Ok(ScanTaskMeta {
                task_id: row.get(0)?,
                root_path: row.get(1)?,
                root_path_key: row.get(2)?,
                visible_latest: row.get::<_, i64>(3)? != 0,
                updated_at: row.get(4)?,
            })
        })
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
    })
    .map_err(|e| e.to_string())
}

fn choose_exact_root_task<'a>(tasks: &'a [ScanTaskMeta]) -> Option<&'a ScanTaskMeta> {
    tasks.iter().max_by(|left, right| {
        left.visible_latest
            .cmp(&right.visible_latest)
            .then_with(|| left.updated_at.cmp(&right.updated_at))
            .then_with(|| left.task_id.cmp(&right.task_id))
    })
}

fn delete_scan_task_rows(tx: &Connection, task_id: &str) -> Result<(), String> {
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
    tx.execute(
        "DELETE FROM scan_tasks WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn compact_scan_tasks_by_exact_root(conn: &mut Connection) -> Result<(), String> {
    let tasks = load_scan_task_meta(conn)?;
    let mut grouped = HashMap::<String, Vec<ScanTaskMeta>>::new();
    for task in tasks {
        grouped
            .entry(task.root_path_key.clone())
            .or_default()
            .push(task);
    }

    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for grouped_tasks in grouped.into_values() {
        let Some(keep) = choose_exact_root_task(&grouped_tasks).cloned() else {
            continue;
        };
        tx.execute(
            "UPDATE scan_tasks SET visible_latest = CASE WHEN task_id = ?1 THEN 1 ELSE 0 END
             WHERE root_path_key = ?2",
            params![keep.task_id, keep.root_path_key],
        )
        .map_err(|e| e.to_string())?;
        for task in grouped_tasks {
            if task.task_id != keep.task_id {
                delete_scan_task_rows(&tx, &task.task_id)?;
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

fn dedupe_organize_latest_trees(conn: &mut Connection) -> Result<(), String> {
    let rows = conn
        .prepare(
            "SELECT root_path_key, root_path, tree_version, tree_json, updated_at
             FROM organize_latest_trees",
        )
        .and_then(|mut stmt| {
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .map_err(|e| e.to_string())?;
    let mut deduped = HashMap::<String, (String, i64, String, String)>::new();
    for (_, root_path, tree_version, tree_json, updated_at) in rows {
        let key = create_root_path_key(&root_path);
        let replace = deduped
            .get(&key)
            .map(|existing| updated_at > existing.3)
            .unwrap_or(true);
        if replace {
            deduped.insert(key, (root_path, tree_version, tree_json, updated_at));
        }
    }

    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM organize_latest_trees", [])
        .map_err(|e| e.to_string())?;
    for (root_path_key, (root_path, tree_version, tree_json, updated_at)) in deduped {
        tx.execute(
            "INSERT INTO organize_latest_trees (
                root_path_key, root_path, tree_version, tree_json, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                root_path_key,
                root_path,
                tree_version,
                tree_json,
                updated_at
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) fn run_scan_cache_maintenance(
    db_path: &Path,
    conn: &mut Connection,
) -> Result<(), String> {
    let current = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = 'scan_cache_maintenance_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if current.as_deref() == Some(SCAN_CACHE_MAINTENANCE_VERSION) {
        return Ok(());
    }

    compact_scan_tasks_by_exact_root(conn)?;
    dedupe_organize_latest_trees(conn)?;

    let visible_tasks = load_scan_task_meta(conn)?
        .into_iter()
        .filter(|task| task.visible_latest)
        .collect::<Vec<_>>();
    for task in &visible_tasks {
        let descendant_roots = visible_tasks
            .iter()
            .filter(|candidate| {
                candidate.task_id != task.task_id
                    && is_same_or_descendant_path(
                        &create_root_path_key(&candidate.root_path),
                        &task.root_path_key,
                    )
                    && candidate.root_path_key != task.root_path_key
            })
            .map(|candidate| candidate.root_path.clone())
            .collect::<Vec<_>>();
        if !descendant_roots.is_empty() {
            delete_scan_descendants_for_paths(db_path, &task.task_id, &descendant_roots)?;
        }
    }

    conn.execute(
        "INSERT INTO app_meta(key, value) VALUES('scan_cache_maintenance_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![SCAN_CACHE_MAINTENANCE_VERSION],
    )
    .map_err(|e| e.to_string())?;
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
        .map_err(|e| e.to_string())?;
    conn.execute("VACUUM", []).map_err(|e| e.to_string())?;
    Ok(())
}

fn init_scan_task_in_storage(
    conn: &mut Connection,
    storage: ScanStorage,
    task_id: &str,
    root_path: &str,
    target_size: u64,
    max_depth: Option<u32>,
    auto_analyze: bool,
    baseline_task_id: Option<&str>,
    scan_mode: &str,
    visible_latest: bool,
) -> Result<(), String> {
    let now = now_iso();
    let root_path_key = create_root_path_key(root_path);
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        &format!("DELETE FROM {} WHERE task_id = ?1", storage.nodes_table()),
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        &format!(
            "DELETE FROM {} WHERE task_id = ?1",
            storage.findings_table()
        ),
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        &format!(
            "INSERT OR REPLACE INTO {} (
                task_id, root_path, root_path_key, status, scan_mode, baseline_task_id, visible_latest, target_size, max_depth, auto_analyze,
                current_path, current_depth, scanned_count, total_entries, processed_entries,
                deletable_count, total_cleanable, token_prompt, token_completion, token_total,
                permission_denied_count, permission_denied_paths, error_message, created_at, updated_at, finished_at
            ) VALUES (?1, ?2, ?3, 'idle', ?4, ?5, ?6, ?7, ?8, ?9, ?2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, '[]', NULL, ?10, ?10, NULL)",
            storage.tasks_table()
        ),
        params![
            task_id,
            root_path,
            root_path_key,
            scan_mode,
            baseline_task_id,
            bool_to_i64(visible_latest),
            target_size as i64,
            max_depth.map(|value| value as i64),
            bool_to_i64(auto_analyze),
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

#[cfg(test)]
pub fn init_scan_task(
    db_path: &Path,
    task_id: &str,
    root_path: &str,
    target_size: u64,
    max_depth: Option<u32>,
    auto_analyze: bool,
    baseline_task_id: Option<&str>,
    scan_mode: &str,
    visible_latest: bool,
) -> Result<(), String> {
    let mut conn = open_db(db_path)?;
    init_scan_task_in_storage(
        &mut conn,
        ScanStorage::Committed,
        task_id,
        root_path,
        target_size,
        max_depth,
        auto_analyze,
        baseline_task_id,
        scan_mode,
        visible_latest,
    )
}

pub fn init_full_scan_draft(
    db_path: &Path,
    task_id: &str,
    root_path: &str,
    target_size: u64,
    max_depth: Option<u32>,
    auto_analyze: bool,
    baseline_task_id: Option<&str>,
) -> Result<(), String> {
    let mut conn = open_db(db_path)?;
    init_scan_task_in_storage(
        &mut conn,
        ScanStorage::Draft,
        task_id,
        root_path,
        target_size,
        max_depth,
        auto_analyze,
        baseline_task_id,
        "full_rescan_incremental",
        false,
    )
}

fn save_scan_snapshot_in_storage(
    conn: &Connection,
    storage: ScanStorage,
    snapshot: &ScanSnapshot,
) -> Result<(), String> {
    let finished_at = if matches!(snapshot.status.as_str(), "done" | "stopped" | "error") {
        Some(now_iso())
    } else {
        None
    };
    conn.execute(
        &format!(
            "UPDATE {} SET
                status = ?2,
                scan_mode = ?3,
                baseline_task_id = ?4,
                visible_latest = ?5,
                target_size = ?6,
                max_depth = ?7,
                auto_analyze = ?8,
                current_path = ?9,
                current_depth = ?10,
                scanned_count = ?11,
                total_entries = ?12,
                processed_entries = ?13,
                deletable_count = ?14,
                total_cleanable = ?15,
                token_prompt = ?16,
                token_completion = ?17,
                token_total = ?18,
                permission_denied_count = ?19,
                permission_denied_paths = ?20,
                error_message = ?21,
                updated_at = ?22,
                finished_at = ?23
             WHERE task_id = ?1",
            storage.tasks_table()
        ),
        params![
            snapshot.id,
            snapshot.status,
            snapshot.scan_mode,
            snapshot.baseline_task_id,
            bool_to_i64(snapshot.visible_latest),
            snapshot.target_size as i64,
            snapshot.configured_max_depth.map(|value| value as i64),
            bool_to_i64(snapshot.auto_analyze),
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

pub fn save_scan_snapshot(db_path: &Path, snapshot: &ScanSnapshot) -> Result<(), String> {
    let conn = open_db(db_path)?;
    let storage = resolve_scan_storage_by_task_id(&conn, &snapshot.id)?;
    save_scan_snapshot_in_storage(&conn, storage, snapshot)
}

#[cfg(test)]
pub fn save_full_scan_draft_snapshot(
    db_path: &Path,
    snapshot: &ScanSnapshot,
) -> Result<(), String> {
    let conn = open_db(db_path)?;
    save_scan_snapshot_in_storage(&conn, ScanStorage::Draft, snapshot)
}

#[cfg(test)]
fn upsert_scan_finding_in_storage(
    conn: &Connection,
    storage: ScanStorage,
    task_id: &str,
    item: &ScanResultItem,
    should_expand: bool,
) -> Result<(), String> {
    conn.execute(
        &format!(
            "INSERT INTO {} (
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
            storage.findings_table()
        ),
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
    Ok(())
}

#[cfg(test)]
pub fn upsert_scan_finding(
    db_path: &Path,
    task_id: &str,
    item: &ScanResultItem,
    should_expand: bool,
) -> Result<(), String> {
    let conn = open_db(db_path)?;
    let storage = resolve_scan_storage_by_task_id(&conn, task_id)?;
    upsert_scan_finding_in_storage(&conn, storage, task_id, item, should_expand)
}

fn refresh_scan_stats_in_storage(
    conn: &Connection,
    storage: ScanStorage,
    task_id: &str,
) -> Result<(), String> {
    let (count, total) = conn
        .query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(size), 0)
                 FROM {}
                 WHERE task_id = ?1 AND classification = 'safe_to_delete'",
                storage.findings_table()
            ),
            params![task_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(|e| e.to_string())?;
    conn.execute(
        &format!(
            "UPDATE {} SET deletable_count = ?2, total_cleanable = ?3, updated_at = ?4 WHERE task_id = ?1",
            storage.tasks_table()
        ),
        params![task_id, count, total, now_iso()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn refresh_scan_stats(db_path: &Path, task_id: &str) -> Result<(), String> {
    let conn = open_db(db_path)?;
    refresh_scan_stats_in_storage(&conn, ScanStorage::Committed, task_id)
}

fn load_scan_findings_in_storage(
    conn: &Connection,
    storage: ScanStorage,
    task_id: &str,
) -> Result<Vec<ScanResultItem>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT name, path, size, type, purpose, reason, risk, classification, source
             FROM {}
             WHERE task_id = ?1 AND classification = 'safe_to_delete'
             ORDER BY size DESC, path ASC",
            storage.findings_table()
        ))
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

fn load_scan_findings_map_exact_in_storage(
    conn: &Connection,
    storage: ScanStorage,
    task_id: &str,
) -> Result<HashMap<String, ScanFindingRecord>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT name, path, size, type, purpose, reason, risk, classification, source, should_expand
             FROM {}
             WHERE task_id = ?1",
            storage.findings_table()
        ))
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

pub fn load_scan_findings_map(
    db_path: &Path,
    task_id: &str,
) -> Result<HashMap<String, ScanFindingRecord>, String> {
    let conn = open_scan_db_for_read(db_path)?;
    let storage = resolve_scan_storage_by_task_id(&conn, task_id)?;
    load_scan_findings_map_exact_in_storage(&conn, storage, task_id)
}

fn load_scan_snapshot_from_conn(
    conn: &Connection,
    storage: ScanStorage,
    task_id: &str,
    include_findings: bool,
) -> Result<Option<ScanSnapshot>, String> {
    let row = conn
        .query_row(
            &format!(
                "SELECT root_path, root_path_key, status, scan_mode, baseline_task_id, visible_latest,
                        auto_analyze, max_depth, current_path, current_depth, scanned_count,
                        total_entries, processed_entries, deletable_count, total_cleanable, target_size,
                        token_prompt, token_completion, token_total, permission_denied_count,
                        permission_denied_paths, error_message
                 FROM {} WHERE task_id = ?1",
                storage.tasks_table()
            ),
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
    let findings = if include_findings {
        load_scan_findings_in_storage(conn, storage, task_id)?
    } else {
        Vec::new()
    };
    let root_path = row.0.clone();
    let max_scanned_depth = conn
        .query_row(
            &format!(
                "SELECT COALESCE(MAX(depth), 0) FROM {} WHERE task_id = ?1",
                storage.nodes_table()
            ),
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

fn load_scan_snapshot_internal(
    db_path: &Path,
    task_id: &str,
    include_findings: bool,
) -> Result<Option<ScanSnapshot>, String> {
    let conn = open_scan_db_for_read(db_path)?;
    let storage = resolve_scan_storage_by_task_id(&conn, task_id)?;
    load_scan_snapshot_from_conn(&conn, storage, task_id, include_findings)
}

pub fn load_scan_snapshot(db_path: &Path, task_id: &str) -> Result<Option<ScanSnapshot>, String> {
    load_scan_snapshot_internal(db_path, task_id, true)
}

#[cfg(test)]
pub fn load_full_scan_draft_snapshot(
    db_path: &Path,
    task_id: &str,
) -> Result<Option<ScanSnapshot>, String> {
    let conn = open_scan_db_for_read(db_path)?;
    load_scan_snapshot_from_conn(&conn, ScanStorage::Draft, task_id, true)
}

pub fn list_scan_history(db_path: &Path, limit: u32) -> Result<Vec<Value>, String> {
    let conn = open_scan_db_for_read(db_path)?;
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

fn list_visible_ancestor_scan_tasks(
    conn: &Connection,
    path: &str,
    exclude_task_id: &str,
) -> Result<Vec<ScanTaskMeta>, String> {
    let normalized_path = create_root_path_key(path);
    let mut tasks = load_visible_scan_tasks(conn)?
        .into_iter()
        .filter(|task| {
            task.task_id != exclude_task_id
                && task.root_path_key != normalized_path
                && is_same_or_descendant_path(&normalized_path, &task.root_path_key)
        })
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| {
        path_depth(&left.root_path)
            .cmp(&path_depth(&right.root_path))
            .then_with(|| left.updated_at.cmp(&right.updated_at))
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    Ok(tasks)
}

fn copy_scan_subtree_between_tasks(
    conn: &Connection,
    source_task_id: &str,
    dest_task_id: &str,
    root_path: &str,
) -> Result<(), String> {
    if source_task_id == dest_task_id {
        return Ok(());
    }

    if let Some((self_size, total_size, child_count, mtime_ms)) = conn
        .query_row(
            "SELECT self_size, total_size, child_count, mtime_ms
             FROM scan_nodes
             WHERE task_id = ?1 AND path = ?2",
            params![source_task_id, root_path],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?
    {
        conn.execute(
            "UPDATE scan_nodes
             SET self_size = ?3, total_size = ?4, child_count = ?5, mtime_ms = ?6
             WHERE task_id = ?1 AND path = ?2",
            params![
                dest_task_id,
                root_path,
                self_size,
                total_size,
                child_count,
                mtime_ms
            ],
        )
        .map_err(|e| e.to_string())?;
    }

    let descendant_nodes = conn
        .prepare(
            "SELECT node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms, ext
             FROM scan_nodes
             WHERE task_id = ?1
               AND (path LIKE ?2 || '\\%' OR path LIKE ?2 || '/%')
             ORDER BY depth ASC, path COLLATE NOCASE ASC",
        )
        .and_then(|mut stmt| {
            stmt.query_map(params![source_task_id, root_path], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .map_err(|e| e.to_string())?;

    let mut insert_node = conn
        .prepare_cached(
            "INSERT INTO scan_nodes (
                task_id, node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms, ext
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
    for node in descendant_nodes {
        insert_node
            .execute(params![
                dest_task_id,
                node.0,
                node.1,
                node.2,
                node.3,
                node.4,
                node.5,
                node.6,
                node.7,
                node.8,
                node.9,
                node.10,
            ])
            .map_err(|e| e.to_string())?;
    }

    let findings = conn
        .prepare(
            "SELECT path, name, type, size, classification, should_expand, purpose, reason, risk, source
             FROM scan_findings
             WHERE task_id = ?1
               AND (path = ?2 OR path LIKE ?2 || '\\%' OR path LIKE ?2 || '/%')",
        )
        .and_then(|mut stmt| {
            stmt.query_map(params![source_task_id, root_path], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .map_err(|e| e.to_string())?;
    let mut insert_finding = conn
        .prepare_cached(
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
        )
        .map_err(|e| e.to_string())?;
    for finding in findings {
        insert_finding
            .execute(params![
                dest_task_id,
                finding.0,
                finding.1,
                finding.2,
                finding.3,
                finding.4,
                finding.5,
                finding.6,
                finding.7,
                finding.8,
                finding.9,
                now_iso(),
            ])
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

pub fn delete_committed_scan_task(db_path: &Path, task_id: &str) -> Result<bool, String> {
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let meta = tx
        .query_row(
            "SELECT root_path, root_path_key, visible_latest FROM scan_tasks WHERE task_id = ?1",
            params![task_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((root_path, root_path_key, was_visible_latest)) = meta else {
        return Ok(false);
    };

    let ancestor_task_ids = list_visible_ancestor_scan_tasks(&tx, &root_path, task_id)?
        .into_iter()
        .map(|task| task.task_id)
        .collect::<Vec<_>>();
    for ancestor_task_id in &ancestor_task_ids {
        copy_scan_subtree_between_tasks(&tx, task_id, ancestor_task_id, &root_path)?;
    }

    delete_scan_task_rows(&tx, task_id)?;
    if was_visible_latest != 0 {
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
    for ancestor_task_id in ancestor_task_ids {
        refresh_scan_stats(db_path, &ancestor_task_id)?;
    }
    Ok(true)
}

pub fn discard_full_scan_draft(db_path: &Path, task_id: &str) -> Result<bool, String> {
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let exists = tx
        .query_row(
            "SELECT 1 FROM scan_drafts WHERE task_id = ?1 LIMIT 1",
            params![task_id],
            |_row| Ok(()),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .is_some();
    if !exists {
        return Ok(false);
    }
    tx.execute(
        "DELETE FROM scan_draft_findings WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM scan_draft_nodes WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM scan_drafts WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(true)
}

fn load_scan_children_exact_in_storage(
    conn: &Connection,
    storage: ScanStorage,
    task_id: &str,
    node_id: &str,
    dirs_only: bool,
) -> Result<Vec<ScanNode>, String> {
    let sql = if dirs_only {
        format!(
            "SELECT node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms
             FROM {}
             WHERE task_id = ?1 AND parent_id = ?2 AND type = 'directory'
             ORDER BY total_size DESC, path COLLATE NOCASE ASC",
            storage.nodes_table()
        )
    } else {
        format!(
            "SELECT node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms
             FROM {}
             WHERE task_id = ?1 AND parent_id = ?2
             ORDER BY total_size DESC, path COLLATE NOCASE ASC",
            storage.nodes_table()
        )
    };
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
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

#[cfg(test)]
fn load_scan_node_map_exact_in_storage(
    conn: &Connection,
    storage: ScanStorage,
    task_id: &str,
) -> Result<HashMap<String, ScanNode>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms
             FROM {}
             WHERE task_id = ?1",
            storage.nodes_table()
        ))
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

fn load_scan_node_exact_in_storage(
    conn: &Connection,
    storage: ScanStorage,
    task_id: &str,
    node_id: &str,
) -> Result<Option<ScanNode>, String> {
    conn.query_row(
        &format!(
            "SELECT node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms
             FROM {}
             WHERE task_id = ?1 AND node_id = ?2",
            storage.nodes_table()
        ),
        params![task_id, node_id],
        |row| {
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
        },
    )
    .optional()
    .map_err(|e| e.to_string())
}

#[allow(dead_code)]
pub fn load_scan_children(
    db_path: &Path,
    task_id: &str,
    node_id: &str,
    dirs_only: bool,
) -> Result<Vec<ScanNode>, String> {
    let conn = open_scan_db_for_read(db_path)?;
    let storage = resolve_scan_storage_by_task_id(&conn, task_id)?;
    let local = load_scan_children_exact_in_storage(&conn, storage, task_id, node_id, dirs_only)?;
    if !local.is_empty() {
        return Ok(local);
    }
    if storage == ScanStorage::Draft {
        return Ok(Vec::new());
    }
    let Some(boundary_node) =
        load_scan_node_exact_in_storage(&conn, ScanStorage::Committed, task_id, node_id)?
    else {
        return Ok(Vec::new());
    };
    let Some((owner_task_id, owner_root_path)) =
        find_owner_visible_scan_task_for_path(db_path, &boundary_node.path)?
    else {
        return Ok(Vec::new());
    };
    if owner_task_id == task_id || !owner_root_path.eq_ignore_ascii_case(&boundary_node.path) {
        return Ok(Vec::new());
    }
    load_scan_children_exact_in_storage(
        &conn,
        ScanStorage::Committed,
        &owner_task_id,
        &create_node_id(&owner_root_path),
        dirs_only,
    )
}

#[cfg(test)]
pub fn load_scan_node_map(
    db_path: &Path,
    task_id: &str,
) -> Result<HashMap<String, ScanNode>, String> {
    let conn = open_scan_db_for_read(db_path)?;
    let storage = resolve_scan_storage_by_task_id(&conn, task_id)?;
    load_scan_node_map_exact_in_storage(&conn, storage, task_id)
}

fn delete_scan_data_for_paths_in_tx(
    tx: &Connection,
    storage: ScanStorage,
    task_id: &str,
    paths: &[String],
) -> Result<(), String> {
    tx.execute(
        "CREATE TEMP TABLE IF NOT EXISTS temp_prune_paths (
            path TEXT PRIMARY KEY
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM temp_prune_paths", [])
        .map_err(|e| e.to_string())?;

    {
        let mut stmt = tx
            .prepare_cached("INSERT OR IGNORE INTO temp_prune_paths(path) VALUES (?1)")
            .map_err(|e| e.to_string())?;
        for path in paths {
            stmt.execute(params![path]).map_err(|e| e.to_string())?;
        }
    }

    tx.execute(
        &format!(
            "DELETE FROM {}
         WHERE task_id = ?1
           AND EXISTS (
             SELECT 1
             FROM temp_prune_paths p
             WHERE {}.path = p.path
                OR {}.path LIKE p.path || '\\%'
                OR {}.path LIKE p.path || '/%'
           )",
            storage.findings_table(),
            storage.findings_table(),
            storage.findings_table(),
            storage.findings_table(),
        ),
        params![task_id],
    )
    .map_err(|e| e.to_string())?;

    tx.execute(
        &format!(
            "DELETE FROM {}
         WHERE task_id = ?1
           AND EXISTS (
             SELECT 1
             FROM temp_prune_paths p
             WHERE {}.path = p.path
                OR {}.path LIKE p.path || '\\%'
                OR {}.path LIKE p.path || '/%'
           )",
            storage.nodes_table(),
            storage.nodes_table(),
            storage.nodes_table(),
            storage.nodes_table(),
        ),
        params![task_id],
    )
    .map_err(|e| e.to_string())?;

    tx.execute("DELETE FROM temp_prune_paths", [])
        .map_err(|e| e.to_string())?;

    Ok(())
}

pub fn delete_scan_data_for_paths(
    db_path: &Path,
    task_id: &str,
    paths: &[String],
) -> Result<(), String> {
    if paths.is_empty() {
        return refresh_scan_stats(db_path, task_id);
    }

    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    delete_scan_data_for_paths_in_tx(&tx, ScanStorage::Committed, task_id, paths)?;
    tx.commit().map_err(|e| e.to_string())?;
    refresh_scan_stats(db_path, task_id)
}

fn delete_scan_descendants_for_paths_in_tx(
    tx: &Connection,
    storage: ScanStorage,
    task_id: &str,
    paths: &[String],
) -> Result<(), String> {
    tx.execute(
        "CREATE TEMP TABLE IF NOT EXISTS temp_prune_paths (
            path TEXT PRIMARY KEY
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM temp_prune_paths", [])
        .map_err(|e| e.to_string())?;

    {
        let mut stmt = tx
            .prepare_cached("INSERT OR IGNORE INTO temp_prune_paths(path) VALUES (?1)")
            .map_err(|e| e.to_string())?;
        for path in paths {
            stmt.execute(params![path]).map_err(|e| e.to_string())?;
        }
    }

    tx.execute(
        &format!(
            "DELETE FROM {}
             WHERE task_id = ?1
               AND EXISTS (
                 SELECT 1
                 FROM temp_prune_paths p
                 WHERE {}.path LIKE p.path || '\\%'
                    OR {}.path LIKE p.path || '/%'
               )",
            storage.findings_table(),
            storage.findings_table(),
            storage.findings_table(),
        ),
        params![task_id],
    )
    .map_err(|e| e.to_string())?;

    tx.execute(
        &format!(
            "DELETE FROM {}
             WHERE task_id = ?1
               AND EXISTS (
                 SELECT 1
                 FROM temp_prune_paths p
                 WHERE {}.path LIKE p.path || '\\%'
                    OR {}.path LIKE p.path || '/%'
               )",
            storage.nodes_table(),
            storage.nodes_table(),
            storage.nodes_table(),
        ),
        params![task_id],
    )
    .map_err(|e| e.to_string())?;

    tx.execute("DELETE FROM temp_prune_paths", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete_scan_descendants_for_paths(
    db_path: &Path,
    task_id: &str,
    paths: &[String],
) -> Result<(), String> {
    if paths.is_empty() {
        return refresh_scan_stats(db_path, task_id);
    }

    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    delete_scan_descendants_for_paths_in_tx(&tx, ScanStorage::Committed, task_id, paths)?;
    tx.commit().map_err(|e| e.to_string())?;
    refresh_scan_stats(db_path, task_id)
}

pub fn find_latest_visible_scan_task_id_for_path(
    db_path: &Path,
    path: &str,
) -> Result<Option<String>, String> {
    let conn = open_scan_db_for_read(db_path)?;
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

fn load_visible_scan_tasks(conn: &Connection) -> Result<Vec<ScanTaskMeta>, String> {
    Ok(load_scan_task_meta(conn)?
        .into_iter()
        .filter(|task| task.visible_latest)
        .collect())
}

fn list_visible_descendant_scan_root_paths(
    conn: &Connection,
    root_path: &str,
) -> Result<Vec<String>, String> {
    let root_key = create_root_path_key(root_path);
    let mut tasks = load_visible_scan_tasks(conn)?
        .into_iter()
        .filter(|task| {
            task.root_path_key != root_key
                && is_same_or_descendant_path(&task.root_path_key, &root_key)
        })
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| {
        path_depth(&right.root_path)
            .cmp(&path_depth(&left.root_path))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| right.task_id.cmp(&left.task_id))
    });
    Ok(tasks.into_iter().map(|task| task.root_path).collect())
}

#[allow(dead_code)]
pub fn find_owner_visible_scan_task_for_path(
    db_path: &Path,
    path: &str,
) -> Result<Option<(String, String)>, String> {
    let conn = open_scan_db_for_read(db_path)?;
    let normalized_path = create_root_path_key(path);
    let mut tasks = load_visible_scan_tasks(&conn)?
        .into_iter()
        .filter(|task| is_same_or_descendant_path(&normalized_path, &task.root_path_key))
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| {
        path_depth(&right.root_path)
            .cmp(&path_depth(&left.root_path))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| right.task_id.cmp(&left.task_id))
    });
    Ok(tasks
        .into_iter()
        .next()
        .map(|task| (task.task_id, task.root_path)))
}

fn sync_committed_ancestor_boundary_node_from_draft(
    tx: &Connection,
    draft_task_id: &str,
    ancestor_task_id: &str,
    root_path: &str,
) -> Result<(), String> {
    let Some((self_size, total_size, child_count, mtime_ms)) = tx
        .query_row(
            "SELECT self_size, total_size, child_count, mtime_ms
             FROM scan_draft_nodes
             WHERE task_id = ?1 AND path = ?2",
            params![draft_task_id, root_path],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    tx.execute(
        "UPDATE scan_nodes
         SET self_size = ?3, total_size = ?4, child_count = ?5, mtime_ms = ?6
         WHERE task_id = ?1 AND path = ?2",
        params![
            ancestor_task_id,
            root_path,
            self_size,
            total_size,
            child_count,
            mtime_ms
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn finalize_full_scan_draft(db_path: &Path, task_id: &str) -> Result<bool, String> {
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let Some((root_path, root_path_key, scan_mode)) = tx
        .query_row(
            "SELECT root_path, root_path_key, scan_mode FROM scan_drafts WHERE task_id = ?1",
            params![task_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?
    else {
        return Ok(false);
    };
    if scan_mode != "full_rescan_incremental" {
        return Err("Only full scan drafts can be finalized".to_string());
    }

    let descendant_roots = list_visible_descendant_scan_root_paths(&tx, &root_path)?;
    if !descendant_roots.is_empty() {
        delete_scan_descendants_for_paths_in_tx(
            &tx,
            ScanStorage::Draft,
            task_id,
            &descendant_roots,
        )?;
        refresh_scan_stats_in_storage(&tx, ScanStorage::Draft, task_id)?;
    }

    let ancestor_task_ids = list_visible_ancestor_scan_tasks(&tx, &root_path, "")?
        .into_iter()
        .map(|task| task.task_id)
        .collect::<Vec<_>>();
    for ancestor_task_id in &ancestor_task_ids {
        delete_scan_descendants_for_paths_in_tx(
            &tx,
            ScanStorage::Committed,
            ancestor_task_id,
            std::slice::from_ref(&root_path),
        )?;
        sync_committed_ancestor_boundary_node_from_draft(
            &tx,
            task_id,
            ancestor_task_id,
            &root_path,
        )?;
        refresh_scan_stats_in_storage(&tx, ScanStorage::Committed, ancestor_task_id)?;
    }

    let stale_task_ids = tx
        .prepare(
            "SELECT task_id FROM scan_tasks
             WHERE root_path_key = ?1",
        )
        .and_then(|mut stmt| {
            stmt.query_map(params![root_path_key], |row| row.get::<_, String>(0))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .map_err(|e| e.to_string())?;
    for stale_task_id in stale_task_ids {
        delete_scan_task_rows(&tx, &stale_task_id)?;
    }

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
    tx.execute(
        "DELETE FROM scan_tasks WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO scan_tasks (
            task_id, root_path, root_path_key, status, scan_mode, baseline_task_id, visible_latest, target_size, max_depth, auto_analyze,
            current_path, current_depth, scanned_count, total_entries, processed_entries,
            deletable_count, total_cleanable, token_prompt, token_completion, token_total,
            permission_denied_count, permission_denied_paths, error_message, created_at, updated_at, finished_at
        )
        SELECT
            task_id, root_path, root_path_key, status, scan_mode, baseline_task_id, 1, target_size, max_depth, auto_analyze,
            current_path, current_depth, scanned_count, total_entries, processed_entries,
            deletable_count, total_cleanable, token_prompt, token_completion, token_total,
            permission_denied_count, permission_denied_paths, error_message, created_at, updated_at, finished_at
        FROM scan_drafts
        WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO scan_nodes (
            task_id, node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms, ext
        )
        SELECT task_id, node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms, ext
        FROM scan_draft_nodes
        WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO scan_findings (
            task_id, path, name, type, size, classification, should_expand, purpose, reason, risk, source, created_at
        )
        SELECT task_id, path, name, type, size, classification, should_expand, purpose, reason, risk, source, created_at
        FROM scan_draft_findings
        WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM scan_draft_findings WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM scan_draft_nodes WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM scan_drafts WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(true)
}

pub fn list_boundary_scan_nodes(
    db_path: &Path,
    task_id: &str,
    depth: u32,
) -> Result<Vec<ScanNode>, String> {
    let conn = open_scan_db_for_read(db_path)?;
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
