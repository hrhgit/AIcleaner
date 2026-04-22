use super::*;

const DEFAULT_SESSION_STATUS: &str = "active";

fn open_advisor_db_for_read(db_path: &Path) -> Result<Connection, String> {
    ensure_advisor_read_ready(db_path)?;
    open_db_raw(db_path)
}

pub(crate) fn advisor_tables_exist(conn: &Connection) -> Result<bool, String> {
    let count = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN (
                'advisor_sessions',
                'advisor_turns',
                'advisor_cards',
                'advisor_memories',
                'advisor_file_summaries',
                'advisor_selections',
                'advisor_plan_jobs',
                'advisor_reclass_jobs'
            )",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(count > 0)
}

pub(crate) fn drop_advisor_tables(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS advisor_reclass_jobs;
        DROP TABLE IF EXISTS advisor_plan_jobs;
        DROP TABLE IF EXISTS advisor_selections;
        DROP TABLE IF EXISTS advisor_file_summaries;
        DROP TABLE IF EXISTS advisor_memories;
        DROP TABLE IF EXISTS advisor_cards;
        DROP TABLE IF EXISTS advisor_turns;
        DROP TABLE IF EXISTS advisor_sessions;
        "#,
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn create_advisor_tables(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS advisor_sessions (
            session_id TEXT PRIMARY KEY,
            root_path TEXT NOT NULL,
            root_path_key TEXT NOT NULL,
            scan_task_id TEXT,
            response_language TEXT NOT NULL,
            workflow_stage TEXT NOT NULL DEFAULT 'understand',
            status TEXT NOT NULL DEFAULT 'active',
            context_bar_json TEXT NOT NULL,
            derived_tree_json TEXT,
            active_selection_id TEXT,
            active_preview_id TEXT,
            rollback_available INTEGER NOT NULL DEFAULT 0,
            session_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_advisor_sessions_root
        ON advisor_sessions(root_path_key, updated_at DESC);

        CREATE TABLE IF NOT EXISTS advisor_turns (
            turn_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            idx INTEGER NOT NULL,
            role TEXT NOT NULL,
            text TEXT NOT NULL DEFAULT '',
            turn_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_advisor_turns_session_idx
        ON advisor_turns(session_id, idx);

        CREATE TABLE IF NOT EXISTS advisor_cards (
            card_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            card_type TEXT NOT NULL,
            status TEXT NOT NULL,
            title TEXT NOT NULL,
            card_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_advisor_cards_session_turn
        ON advisor_cards(session_id, turn_id, created_at ASC);

        CREATE TABLE IF NOT EXISTS advisor_memories (
            memory_id TEXT PRIMARY KEY,
            session_id TEXT,
            scope TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            text TEXT NOT NULL,
            memory_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_advisor_memories_scope
        ON advisor_memories(scope, session_id, updated_at DESC);

        CREATE TABLE IF NOT EXISTS advisor_file_summaries (
            root_path_key TEXT NOT NULL,
            path_key TEXT NOT NULL,
            summary_short TEXT,
            summary_normal TEXT,
            source TEXT NOT NULL,
            mode TEXT NOT NULL,
            summary_json TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (root_path_key, path_key)
        );

        CREATE INDEX IF NOT EXISTS idx_advisor_file_summaries_root
        ON advisor_file_summaries(root_path_key, updated_at DESC);

        CREATE TABLE IF NOT EXISTS advisor_selections (
            selection_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            query_summary TEXT NOT NULL,
            total INTEGER NOT NULL DEFAULT 0,
            selection_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_advisor_selections_session
        ON advisor_selections(session_id, updated_at DESC);

        CREATE TABLE IF NOT EXISTS advisor_plan_jobs (
            job_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            selection_id TEXT,
            preview_card_id TEXT,
            status TEXT NOT NULL,
            preview_json TEXT NOT NULL,
            result_json TEXT,
            rollback_json TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_advisor_plan_jobs_session
        ON advisor_plan_jobs(session_id, updated_at DESC);

        CREATE TABLE IF NOT EXISTS advisor_reclass_jobs (
            job_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            status TEXT NOT NULL,
            request_json TEXT NOT NULL,
            result_json TEXT,
            rollback_json TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_advisor_reclass_jobs_session
        ON advisor_reclass_jobs(session_id, updated_at DESC);
        "#,
    )
    .map_err(|e| e.to_string())
}

fn stringify(value: &Value) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| e.to_string())
}

fn parse_value(raw: String) -> Result<Value, String> {
    serde_json::from_str::<Value>(&raw).map_err(|e| e.to_string())
}

pub fn save_advisor_session(db_path: &Path, session: &Value) -> Result<(), String> {
    let session_id = session
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "session.sessionId is required".to_string())?;
    let root_path = session
        .get("rootPath")
        .and_then(Value::as_str)
        .ok_or_else(|| "session.rootPath is required".to_string())?;
    let response_language = session
        .get("responseLanguage")
        .and_then(Value::as_str)
        .unwrap_or("zh");
    let workflow_stage = session
        .get("workflowStage")
        .and_then(Value::as_str)
        .unwrap_or("understand");
    let status = session
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_SESSION_STATUS);
    let context_bar = session
        .get("contextBar")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let derived_tree = session.get("derivedTree").cloned().unwrap_or(Value::Null);
    let created_at = session
        .get("createdAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    let updated_at = session
        .get("updatedAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    let scan_task_id = session
        .get("scanTaskId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let active_selection_id = session
        .get("activeSelectionId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let active_preview_id = session
        .get("activePreviewId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let rollback_available = session
        .get("rollbackAvailable")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT OR REPLACE INTO advisor_sessions (
            session_id, root_path, root_path_key, scan_task_id, response_language,
            workflow_stage, status, context_bar_json, derived_tree_json,
            active_selection_id, active_preview_id, rollback_available,
            session_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            session_id,
            root_path,
            create_root_path_key(root_path),
            scan_task_id,
            response_language,
            workflow_stage,
            status,
            stringify(&context_bar)?,
            if derived_tree.is_null() {
                None::<String>
            } else {
                Some(stringify(&derived_tree)?)
            },
            active_selection_id,
            active_preview_id,
            bool_to_i64(rollback_available),
            stringify(session)?,
            created_at,
            updated_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_advisor_session(db_path: &Path, session_id: &str) -> Result<Option<Value>, String> {
    let conn = open_advisor_db_for_read(db_path)?;
    let raw = conn
        .query_row(
            "SELECT session_json FROM advisor_sessions WHERE session_id = ?1",
            params![session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    raw.map(parse_value).transpose()
}

pub fn create_advisor_turn(
    db_path: &Path,
    session_id: &str,
    role: &str,
    text: &str,
) -> Result<Value, String> {
    let conn = open_db(db_path)?;
    let idx = conn
        .query_row(
            "SELECT COALESCE(MAX(idx), -1) + 1 FROM advisor_turns WHERE session_id = ?1",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|e| e.to_string())?;
    let created_at = now_iso();
    let turn = json!({
        "turnId": create_node_id(&format!("turn:{session_id}:{idx}:{created_at}")),
        "sessionId": session_id,
        "idx": idx,
        "role": role,
        "text": text,
        "createdAt": created_at,
    });
    conn.execute(
        "INSERT INTO advisor_turns (turn_id, session_id, idx, role, text, turn_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            turn.get("turnId").and_then(Value::as_str).unwrap_or(""),
            session_id,
            idx,
            role,
            text,
            stringify(&turn)?,
            created_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(turn)
}

pub fn load_advisor_turns(db_path: &Path, session_id: &str) -> Result<Vec<Value>, String> {
    let conn = open_advisor_db_for_read(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT turn_json
             FROM advisor_turns
             WHERE session_id = ?1
             ORDER BY idx ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![session_id], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.map(|row| parse_value(row.map_err(|e| e.to_string())?))
        .collect()
}

pub fn save_advisor_card(db_path: &Path, card: &Value) -> Result<(), String> {
    let card_id = card
        .get("cardId")
        .and_then(Value::as_str)
        .ok_or_else(|| "card.cardId is required".to_string())?;
    let session_id = card
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "card.sessionId is required".to_string())?;
    let turn_id = card
        .get("turnId")
        .and_then(Value::as_str)
        .ok_or_else(|| "card.turnId is required".to_string())?;
    let card_type = card
        .get("cardType")
        .and_then(Value::as_str)
        .ok_or_else(|| "card.cardType is required".to_string())?;
    let status = card
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("ready");
    let title = card.get("title").and_then(Value::as_str).unwrap_or("");
    let created_at = card
        .get("createdAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    let updated_at = card
        .get("updatedAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT OR REPLACE INTO advisor_cards (
            card_id, session_id, turn_id, card_type, status, title, card_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, COALESCE((SELECT created_at FROM advisor_cards WHERE card_id = ?1), ?8), ?9)",
        params![
            card_id,
            session_id,
            turn_id,
            card_type,
            status,
            title,
            stringify(card)?,
            created_at,
            updated_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_advisor_cards(db_path: &Path, session_id: &str) -> Result<Vec<Value>, String> {
    let conn = open_advisor_db_for_read(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT card_json
             FROM advisor_cards
             WHERE session_id = ?1
             ORDER BY datetime(created_at) ASC, card_id ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![session_id], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.map(|row| parse_value(row.map_err(|e| e.to_string())?))
        .collect()
}

pub fn load_advisor_card(db_path: &Path, card_id: &str) -> Result<Option<Value>, String> {
    let conn = open_advisor_db_for_read(db_path)?;
    let raw = conn
        .query_row(
            "SELECT card_json FROM advisor_cards WHERE card_id = ?1",
            params![card_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    raw.map(parse_value).transpose()
}

pub fn save_advisor_memory(db_path: &Path, row: &Value) -> Result<(), String> {
    let memory_id = row
        .get("memoryId")
        .and_then(Value::as_str)
        .ok_or_else(|| "memory.memoryId is required".to_string())?;
    let scope = row
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("session");
    let text = row.get("text").and_then(Value::as_str).unwrap_or("");
    let enabled = row.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let session_id = row
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let created_at = row
        .get("createdAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    let updated_at = row
        .get("updatedAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT OR REPLACE INTO advisor_memories (
            memory_id, session_id, scope, enabled, text, memory_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            memory_id,
            session_id,
            scope,
            bool_to_i64(enabled),
            text,
            stringify(row)?,
            created_at,
            updated_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_advisor_memories(
    db_path: &Path,
    session_id: Option<&str>,
) -> Result<Vec<Value>, String> {
    let conn = open_advisor_db_for_read(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT memory_json
             FROM advisor_memories
             WHERE scope = 'global' OR (?1 IS NOT NULL AND session_id = ?1)
             ORDER BY datetime(updated_at) DESC, memory_id DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![session_id], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.map(|row| parse_value(row.map_err(|e| e.to_string())?))
        .collect()
}

pub fn save_advisor_file_summary(db_path: &Path, row: &Value) -> Result<(), String> {
    let root_path_key = row
        .get("rootPathKey")
        .and_then(Value::as_str)
        .ok_or_else(|| "summary.rootPathKey is required".to_string())?;
    let path_key = row
        .get("pathKey")
        .and_then(Value::as_str)
        .ok_or_else(|| "summary.pathKey is required".to_string())?;
    let source = row
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("advisor");
    let mode = row
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("metadata_summary");
    let updated_at = row
        .get("updatedAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT OR REPLACE INTO advisor_file_summaries (
            root_path_key, path_key, summary_short, summary_normal, source, mode, summary_json, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            root_path_key,
            path_key,
            row.get("summaryShort").and_then(Value::as_str),
            row.get("summaryNormal").and_then(Value::as_str),
            source,
            mode,
            stringify(row)?,
            updated_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_advisor_file_summary(
    db_path: &Path,
    root_path_key: &str,
    path_key: &str,
) -> Result<Option<Value>, String> {
    let conn = open_advisor_db_for_read(db_path)?;
    let raw = conn
        .query_row(
            "SELECT summary_json FROM advisor_file_summaries WHERE root_path_key = ?1 AND path_key = ?2",
            params![root_path_key, path_key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    raw.map(parse_value).transpose()
}

pub fn save_advisor_selection(db_path: &Path, row: &Value) -> Result<(), String> {
    let selection_id = row
        .get("selectionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "selection.selectionId is required".to_string())?;
    let session_id = row
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "selection.sessionId is required".to_string())?;
    let query_summary = row
        .get("querySummary")
        .and_then(Value::as_str)
        .unwrap_or("");
    let total = row.get("total").and_then(Value::as_i64).unwrap_or(0);
    let created_at = row
        .get("createdAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    let updated_at = row
        .get("updatedAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT OR REPLACE INTO advisor_selections (
            selection_id, session_id, query_summary, total, selection_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, COALESCE((SELECT created_at FROM advisor_selections WHERE selection_id = ?1), ?6), ?7)",
        params![
            selection_id,
            session_id,
            query_summary,
            total,
            stringify(row)?,
            created_at,
            updated_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_advisor_selection(db_path: &Path, selection_id: &str) -> Result<Option<Value>, String> {
    let conn = open_advisor_db_for_read(db_path)?;
    let raw = conn
        .query_row(
            "SELECT selection_json FROM advisor_selections WHERE selection_id = ?1",
            params![selection_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    raw.map(parse_value).transpose()
}

pub fn save_advisor_plan_job(db_path: &Path, row: &Value) -> Result<(), String> {
    let job_id = row
        .get("jobId")
        .and_then(Value::as_str)
        .ok_or_else(|| "planJob.jobId is required".to_string())?;
    let session_id = row
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "planJob.sessionId is required".to_string())?;
    let status = row
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("preview_ready");
    let preview = row.get("preview").cloned().unwrap_or_else(|| json!({}));
    let result = row.get("result").cloned().unwrap_or(Value::Null);
    let rollback = row.get("rollback").cloned().unwrap_or(Value::Null);
    let selection_id = row
        .get("selectionId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let preview_card_id = row
        .get("previewCardId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let created_at = row
        .get("createdAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    let updated_at = row
        .get("updatedAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT OR REPLACE INTO advisor_plan_jobs (
            job_id, session_id, selection_id, preview_card_id, status,
            preview_json, result_json, rollback_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, COALESCE((SELECT created_at FROM advisor_plan_jobs WHERE job_id = ?1), ?9), ?10)",
        params![
            job_id,
            session_id,
            selection_id,
            preview_card_id,
            status,
            stringify(&preview)?,
            if result.is_null() { None::<String> } else { Some(stringify(&result)?) },
            if rollback.is_null() { None::<String> } else { Some(stringify(&rollback)?) },
            created_at,
            updated_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_advisor_plan_job(db_path: &Path, job_id: &str) -> Result<Option<Value>, String> {
    let conn = open_advisor_db_for_read(db_path)?;
    let row = conn
        .query_row(
            "SELECT session_id, selection_id, preview_card_id, status, preview_json, result_json, rollback_json, created_at, updated_at
             FROM advisor_plan_jobs
             WHERE job_id = ?1",
            params![job_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((
        session_id,
        selection_id,
        preview_card_id,
        status,
        preview_json,
        result_json,
        rollback_json,
        created_at,
        updated_at,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(json!({
        "jobId": job_id,
        "sessionId": session_id,
        "selectionId": selection_id,
        "previewCardId": preview_card_id,
        "status": status,
        "preview": parse_value(preview_json)?,
        "result": result_json.map(parse_value).transpose()?,
        "rollback": rollback_json.map(parse_value).transpose()?,
        "createdAt": created_at,
        "updatedAt": updated_at,
    })))
}

pub fn save_advisor_reclass_job(db_path: &Path, row: &Value) -> Result<(), String> {
    let job_id = row
        .get("jobId")
        .and_then(Value::as_str)
        .ok_or_else(|| "reclassJob.jobId is required".to_string())?;
    let session_id = row
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "reclassJob.sessionId is required".to_string())?;
    let status = row
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    let request = row.get("request").cloned().unwrap_or_else(|| json!({}));
    let result = row.get("result").cloned().unwrap_or(Value::Null);
    let rollback = row.get("rollback").cloned().unwrap_or(Value::Null);
    let created_at = row
        .get("createdAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    let updated_at = row
        .get("updatedAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT OR REPLACE INTO advisor_reclass_jobs (
            job_id, session_id, status, request_json, result_json, rollback_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE((SELECT created_at FROM advisor_reclass_jobs WHERE job_id = ?1), ?7), ?8)",
        params![
            job_id,
            session_id,
            status,
            stringify(&request)?,
            if result.is_null() { None::<String> } else { Some(stringify(&result)?) },
            if rollback.is_null() { None::<String> } else { Some(stringify(&rollback)?) },
            created_at,
            updated_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_advisor_reclass_job(db_path: &Path, job_id: &str) -> Result<Option<Value>, String> {
    let conn = open_advisor_db_for_read(db_path)?;
    let row = conn
        .query_row(
            "SELECT session_id, status, request_json, result_json, rollback_json, created_at, updated_at
             FROM advisor_reclass_jobs
             WHERE job_id = ?1",
            params![job_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((
        session_id,
        status,
        request_json,
        result_json,
        rollback_json,
        created_at,
        updated_at,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(json!({
        "jobId": job_id,
        "sessionId": session_id,
        "status": status,
        "request": parse_value(request_json)?,
        "result": result_json.map(parse_value).transpose()?,
        "rollback": rollback_json.map(parse_value).transpose()?,
        "createdAt": created_at,
        "updatedAt": updated_at,
    })))
}
