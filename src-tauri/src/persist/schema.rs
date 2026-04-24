use super::advisor::{advisor_tables_exist, create_advisor_tables, drop_advisor_tables};
use super::organize::{create_organizer_tables, drop_organizer_tables, organizer_tables_exist};
use super::*;

const ADVISOR_SCHEMA_VERSION: &str = "advisor_v4";

fn ensure_app_meta_table(conn: &Connection) -> Result<(), String> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS app_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn ensure_organizer_read_schema(conn: &Connection) -> Result<(), String> {
    ensure_app_meta_table(conn)?;
    let had_schema_version = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = 'organizer_schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .is_some();
    create_organizer_tables(conn)?;
    if !had_schema_version {
        conn.execute(
            "INSERT INTO app_meta(key, value) VALUES('organizer_schema_version', ?1)
             ON CONFLICT(key) DO NOTHING",
            params![ORGANIZER_SCHEMA_VERSION],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) fn ensure_advisor_read_schema(conn: &Connection) -> Result<(), String> {
    ensure_app_meta_table(conn)?;
    let had_schema_version = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = 'advisor_schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .is_some();
    create_advisor_tables(conn)?;
    if !had_schema_version {
        conn.execute(
            "INSERT INTO app_meta(key, value) VALUES('advisor_schema_version', ?1)
             ON CONFLICT(key) DO NOTHING",
            params![ADVISOR_SCHEMA_VERSION],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn ensure_organizer_schema_full(conn: &Connection) -> Result<(), String> {
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
        .unwrap_or_else(|| organizer_tables_exist(conn).unwrap_or(false));
    if needs_reset {
        drop_organizer_tables(conn)?;
    }
    create_organizer_tables(conn)?;
    conn.execute(
        "INSERT INTO app_meta(key, value) VALUES('organizer_schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![ORGANIZER_SCHEMA_VERSION],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn ensure_advisor_schema_full(conn: &Connection) -> Result<(), String> {
    let current_advisor_schema = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = 'advisor_schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let needs_reset = current_advisor_schema
        .as_deref()
        .map(|value| value != ADVISOR_SCHEMA_VERSION)
        .unwrap_or_else(|| advisor_tables_exist(conn).unwrap_or(false));
    if needs_reset {
        drop_advisor_tables(conn)?;
    }
    create_advisor_tables(conn)?;
    conn.execute(
        "INSERT INTO app_meta(key, value) VALUES('advisor_schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![ADVISOR_SCHEMA_VERSION],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn init_db(db_path: &Path) -> Result<(), String> {
    let conn = open_db_raw(db_path)?;
    ensure_app_meta_table(&conn)?;
    ensure_organizer_schema_full(&conn)?;
    ensure_advisor_schema_full(&conn)?;
    Ok(())
}

pub fn mark_stale_organize_tasks(db_path: &Path) -> Result<(), String> {
    let conn = open_db_raw(db_path)?;
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

pub fn mark_stale_tasks(db_path: &Path) -> Result<(), String> {
    mark_stale_organize_tasks(db_path)
}
