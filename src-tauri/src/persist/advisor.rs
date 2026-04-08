use super::*;

const DEFAULT_SESSION_STATUS: &str = "active";

pub(crate) fn advisor_tables_exist(conn: &Connection) -> Result<bool, String> {
    let count = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN (
                'advisor_sessions',
                'advisor_messages',
                'advisor_preferences',
                'advisor_suggestions',
                'advisor_execution_jobs'
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
        DROP TABLE IF EXISTS advisor_execution_jobs;
        DROP TABLE IF EXISTS advisor_suggestions;
        DROP TABLE IF EXISTS advisor_preferences;
        DROP TABLE IF EXISTS advisor_messages;
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
            mode TEXT NOT NULL,
            response_language TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'active',
            session_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_advisor_sessions_root
        ON advisor_sessions(root_path_key, updated_at DESC);

        CREATE TABLE IF NOT EXISTS advisor_messages (
            session_id TEXT NOT NULL,
            idx INTEGER NOT NULL,
            role TEXT NOT NULL,
            content_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (session_id, idx)
        );

        CREATE TABLE IF NOT EXISTS advisor_preferences (
            preference_id TEXT PRIMARY KEY,
            session_id TEXT,
            scope TEXT NOT NULL,
            kind TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            source TEXT NOT NULL DEFAULT 'advisor',
            reason TEXT,
            rule_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_advisor_preferences_scope
        ON advisor_preferences(scope, session_id, updated_at DESC);

        CREATE TABLE IF NOT EXISTS advisor_suggestions (
            session_id TEXT NOT NULL,
            suggestion_id TEXT NOT NULL,
            path TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'new',
            row_json TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (session_id, suggestion_id)
        );

        CREATE INDEX IF NOT EXISTS idx_advisor_suggestions_path
        ON advisor_suggestions(session_id, path, updated_at DESC);

        CREATE TABLE IF NOT EXISTS advisor_execution_jobs (
            job_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            preview_json TEXT NOT NULL,
            result_json TEXT,
            rollback_json TEXT
        );
        "#,
    )
    .map_err(|e| e.to_string())
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
    let mode = session
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("organize_first");
    let response_language = session
        .get("responseLanguage")
        .and_then(Value::as_str)
        .unwrap_or("zh");
    let status = session
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_SESSION_STATUS);
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

    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT OR REPLACE INTO advisor_sessions (
            session_id, root_path, root_path_key, scan_task_id, mode, response_language,
            status, session_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            session_id,
            root_path,
            create_root_path_key(root_path),
            scan_task_id,
            mode,
            response_language,
            status,
            serde_json::to_string(session).map_err(|e| e.to_string())?,
            created_at,
            updated_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_advisor_session(db_path: &Path, session_id: &str) -> Result<Option<Value>, String> {
    let conn = open_db(db_path)?;
    let raw = conn
        .query_row(
            "SELECT session_json FROM advisor_sessions WHERE session_id = ?1",
            params![session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    raw.map(|value| serde_json::from_str::<Value>(&value).map_err(|e| e.to_string()))
        .transpose()
}

pub fn save_advisor_message(
    db_path: &Path,
    session_id: &str,
    role: &str,
    content: &Value,
) -> Result<Value, String> {
    let conn = open_db(db_path)?;
    let idx = conn
        .query_row(
            "SELECT COALESCE(MAX(idx), -1) + 1 FROM advisor_messages WHERE session_id = ?1",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|e| e.to_string())?;
    let created_at = now_iso();
    conn.execute(
        "INSERT INTO advisor_messages (session_id, idx, role, content_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            session_id,
            idx,
            role,
            serde_json::to_string(content).map_err(|e| e.to_string())?,
            created_at
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({
        "sessionId": session_id,
        "idx": idx,
        "role": role,
        "content": content,
        "createdAt": created_at
    }))
}

pub fn load_advisor_messages(db_path: &Path, session_id: &str) -> Result<Vec<Value>, String> {
    let conn = open_db(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT idx, role, content_json, created_at
             FROM advisor_messages
             WHERE session_id = ?1
             ORDER BY idx ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    rows.map(|row| {
        let (idx, role, content_json, created_at) = row.map_err(|e| e.to_string())?;
        let content: Value = serde_json::from_str(&content_json).map_err(|e| e.to_string())?;
        Ok(json!({
            "sessionId": session_id,
            "idx": idx,
            "role": role,
            "content": content,
            "createdAt": created_at
        }))
    })
    .collect()
}

pub fn save_advisor_preference(db_path: &Path, row: &Value) -> Result<(), String> {
    let preference_id = row
        .get("preferenceId")
        .and_then(Value::as_str)
        .ok_or_else(|| "preference.preferenceId is required".to_string())?;
    let scope = row
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("session");
    let kind = row
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("keep");
    let enabled = row.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let session_id = row
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let source = row
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("advisor");
    let reason = row
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    let rule = row.get("rule").cloned().unwrap_or_else(|| json!({}));
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
        "INSERT OR REPLACE INTO advisor_preferences (
            preference_id, session_id, scope, kind, enabled, source, reason, rule_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            preference_id,
            session_id,
            scope,
            kind,
            bool_to_i64(enabled),
            source,
            reason,
            serde_json::to_string(&rule).map_err(|e| e.to_string())?,
            created_at,
            updated_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_advisor_preferences(
    db_path: &Path,
    session_id: Option<&str>,
) -> Result<Vec<Value>, String> {
    let conn = open_db(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT preference_id, session_id, scope, kind, enabled, source, reason, rule_json, created_at, updated_at
             FROM advisor_preferences
             WHERE scope = 'global' OR (?1 IS NOT NULL AND session_id = ?1)
             ORDER BY datetime(updated_at) DESC, preference_id DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)? != 0,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    rows.map(|row| {
        let (preference_id, session_id, scope, kind, enabled, source, reason, rule_json, created_at, updated_at) =
            row.map_err(|e| e.to_string())?;
        let rule: Value = serde_json::from_str(&rule_json).map_err(|e| e.to_string())?;
        Ok(json!({
            "preferenceId": preference_id,
            "sessionId": session_id,
            "scope": scope,
            "kind": kind,
            "enabled": enabled,
            "source": source,
            "reason": reason,
            "rule": rule,
            "createdAt": created_at,
            "updatedAt": updated_at
        }))
    })
    .collect()
}

pub fn save_advisor_suggestions(
    db_path: &Path,
    session_id: &str,
    rows: &[Value],
) -> Result<(), String> {
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM advisor_suggestions WHERE session_id = ?1",
        params![session_id],
    )
    .map_err(|e| e.to_string())?;
    let mut stmt = tx
        .prepare_cached(
            "INSERT INTO advisor_suggestions (
                session_id, suggestion_id, path, status, row_json, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(|e| e.to_string())?;
    for row in rows {
        stmt.execute(params![
            session_id,
            row.get("suggestionId").and_then(Value::as_str).unwrap_or(""),
            row.get("path").and_then(Value::as_str).unwrap_or(""),
            row.get("status").and_then(Value::as_str).unwrap_or("new"),
            serde_json::to_string(row).map_err(|e| e.to_string())?,
            now_iso()
        ])
        .map_err(|e| e.to_string())?;
    }
    drop(stmt);
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_advisor_suggestions(db_path: &Path, session_id: &str) -> Result<Vec<Value>, String> {
    let conn = open_db(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT row_json
             FROM advisor_suggestions
             WHERE session_id = ?1
             ORDER BY datetime(updated_at) DESC, suggestion_id DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![session_id], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.map(|row| {
        let raw = row.map_err(|e| e.to_string())?;
        serde_json::from_str::<Value>(&raw).map_err(|e| e.to_string())
    })
    .collect()
}

pub fn save_advisor_execution_job(
    db_path: &Path,
    job_id: &str,
    session_id: &str,
    preview: &Value,
    result: Option<&Value>,
    rollback: Option<&Value>,
) -> Result<(), String> {
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT OR REPLACE INTO advisor_execution_jobs (
            job_id, session_id, created_at, preview_json, result_json, rollback_json
         ) VALUES (
            ?1,
            ?2,
            COALESCE((SELECT created_at FROM advisor_execution_jobs WHERE job_id = ?1), ?3),
            ?4,
            ?5,
            ?6
         )",
        params![
            job_id,
            session_id,
            now_iso(),
            serde_json::to_string(preview).map_err(|e| e.to_string())?,
            result
                .map(|value| serde_json::to_string(value).map_err(|e| e.to_string()))
                .transpose()?,
            rollback
                .map(|value| serde_json::to_string(value).map_err(|e| e.to_string()))
                .transpose()?,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_advisor_execution_job(db_path: &Path, job_id: &str) -> Result<Option<Value>, String> {
    let conn = open_db(db_path)?;
    let row = conn
        .query_row(
            "SELECT session_id, created_at, preview_json, result_json, rollback_json
             FROM advisor_execution_jobs
             WHERE job_id = ?1",
            params![job_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((session_id, created_at, preview_json, result_json, rollback_json)) = row else {
        return Ok(None);
    };
    Ok(Some(json!({
        "jobId": job_id,
        "sessionId": session_id,
        "createdAt": created_at,
        "preview": serde_json::from_str::<Value>(&preview_json).map_err(|e| e.to_string())?,
        "result": result_json.map(|raw| serde_json::from_str::<Value>(&raw).map_err(|e| e.to_string())).transpose()?,
        "rollback": rollback_json.map(|raw| serde_json::from_str::<Value>(&raw).map_err(|e| e.to_string())).transpose()?,
    })))
}

pub fn load_latest_advisor_execution_for_session(
    db_path: &Path,
    session_id: &str,
) -> Result<Option<Value>, String> {
    let conn = open_db(db_path)?;
    let job_id = conn
        .query_row(
            "SELECT job_id
             FROM advisor_execution_jobs
             WHERE session_id = ?1
             ORDER BY datetime(created_at) DESC, job_id DESC
             LIMIT 1",
            params![session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    job_id
        .map(|value| load_advisor_execution_job(db_path, &value))
        .transpose()
        .map(|value| value.flatten())
}
