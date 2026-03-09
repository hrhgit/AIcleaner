#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::{Parser, Subcommand};
use rusqlite::{params, Connection, Transaction};
use serde_json::json;
use sha1::{Digest, Sha1};
use std::cmp::max;
use std::env;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const BATCH_SIZE: usize = 512;
const PROGRESS_EVERY: u64 = 250;

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Scan(ScanArgs),
}

#[derive(Parser)]
struct ScanArgs {
    #[arg(long)]
    db: String,
    #[arg(long = "task-id")]
    task_id: String,
    #[arg(long)]
    root: String,
    #[arg(long = "max-depth")]
    max_depth: usize,
}

#[derive(Clone)]
struct NodeRecord {
    task_id: String,
    node_id: String,
    parent_id: Option<String>,
    path: String,
    name: String,
    node_type: String,
    depth: usize,
    self_size: i64,
    total_size: i64,
    child_count: i64,
    mtime_ms: Option<i64>,
    ext: String,
}

struct ScanContext {
    conn: Connection,
    task_id: String,
    max_depth: usize,
    scanned_count: u64,
    buffered: Vec<NodeRecord>,
    last_progress_mark: u64,
}

impl ScanContext {
    fn new(conn: Connection, task_id: String, max_depth: usize) -> Self {
        Self {
            conn,
            task_id,
            max_depth,
            scanned_count: 0,
            buffered: Vec::with_capacity(BATCH_SIZE),
            last_progress_mark: 0,
        }
    }

    fn queue_node(
        &mut self,
        record: NodeRecord,
        current_path: &str,
        current_depth: usize,
    ) -> rusqlite::Result<()> {
        self.scanned_count += 1;
        self.buffered.push(record);

        if self.buffered.len() >= BATCH_SIZE {
            self.flush_nodes()?;
        }

        if self.scanned_count == 1 || self.scanned_count - self.last_progress_mark >= PROGRESS_EVERY
        {
            self.last_progress_mark = self.scanned_count;
            self.update_scan_progress(current_path, current_depth)?;
            emit_event(json!({
                "type": "scan_progress",
                "current_path": current_path,
                "current_depth": current_depth,
                "scanned_count": self.scanned_count,
                "total_entries": self.scanned_count,
            }));
        }

        Ok(())
    }

    fn flush_nodes(&mut self) -> rusqlite::Result<()> {
        if self.buffered.is_empty() {
            return Ok(());
        }

        let tx = self.conn.transaction()?;
        insert_nodes(&tx, &self.buffered)?;
        tx.commit()?;
        self.buffered.clear();
        Ok(())
    }

    fn update_scan_progress(
        &self,
        current_path: &str,
        current_depth: usize,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE scan_tasks SET status = 'scanning', current_path = ?, current_depth = ?, scanned_count = ?, total_entries = ?, updated_at = ? WHERE task_id = ?",
            params![current_path, current_depth as i64, self.scanned_count as i64, self.scanned_count as i64, now_iso(), self.task_id],
        )?;
        Ok(())
    }
}

fn main() {
    if let Err(err) = run() {
        emit_event(json!({
            "type": "error",
            "message": err.to_string(),
        }));
        let _ = writeln!(io::stderr(), "{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Scan(args) => run_scan(args),
    }
}

fn run_scan(args: ScanArgs) -> Result<(), Box<dyn std::error::Error>> {
    let root = absolutize_path(Path::new(&args.root))?;
    let root_string = path_to_string(&root);

    let conn = Connection::open(&args.db)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    ensure_schema(&conn)?;

    conn.execute(
        "UPDATE scan_tasks SET status = 'scanning', current_path = ?, current_depth = 0, scanned_count = 0, total_entries = 0, updated_at = ?, error_message = NULL, finished_at = NULL WHERE task_id = ?",
        params![root_string, now_iso(), args.task_id],
    )?;

    emit_event(json!({
        "type": "task_started",
        "current_path": root_string,
        "current_depth": 0,
        "scanned_count": 0,
        "total_entries": 0,
    }));

    let mut ctx = ScanContext::new(conn, args.task_id.clone(), max(args.max_depth, 0));
    let _ = scan_dir(&mut ctx, &root, None, 0)?;
    ctx.flush_nodes()?;
    ctx.update_scan_progress(&root_string, 0)?;
    ctx.conn.execute(
        "UPDATE scan_tasks SET status = 'scanning', current_path = ?, current_depth = 0, scanned_count = ?, total_entries = ?, updated_at = ? WHERE task_id = ?",
        params![root_string, ctx.scanned_count as i64, ctx.scanned_count as i64, now_iso(), ctx.task_id],
    )?;

    emit_event(json!({
        "type": "scan_completed",
        "current_path": root_string,
        "current_depth": 0,
        "scanned_count": ctx.scanned_count,
        "total_entries": ctx.scanned_count,
    }));

    Ok(())
}

fn is_permission_denied(err: &io::Error) -> bool {
    err.kind() == ErrorKind::PermissionDenied
}

fn emit_permission_denied(path: &Path, err: &io::Error) {
    emit_event(json!({
        "type": "permission_denied",
        "path": path_to_string(path),
        "message": err.to_string(),
    }));
}

fn scan_dir(
    ctx: &mut ScanContext,
    dir_path: &Path,
    parent_id: Option<String>,
    depth: usize,
) -> Result<i64, Box<dyn std::error::Error>> {
    let dir_abs = absolutize_path(dir_path)?;
    let dir_string = path_to_string(&dir_abs);
    let dir_id = node_id_for(&dir_string);
    let metadata = fs::metadata(&dir_abs).ok();

    let mut total_size = 0_i64;
    let mut child_count = 0_i64;

    if depth < ctx.max_depth {
        let read_dir = match fs::read_dir(&dir_abs) {
            Ok(value) => value,
            Err(err) if is_permission_denied(&err) => {
                emit_permission_denied(&dir_abs, &err);
                let record = NodeRecord {
                    task_id: ctx.task_id.clone(),
                    node_id: dir_id,
                    parent_id,
                    path: dir_string.clone(),
                    name: directory_name(&dir_abs),
                    node_type: "directory".to_string(),
                    depth,
                    self_size: 0,
                    total_size: 0,
                    child_count: 0,
                    mtime_ms: metadata_to_mtime_ms(metadata.as_ref()),
                    ext: String::new(),
                };
                ctx.queue_node(record, &dir_string, depth)?;
                return Ok(0);
            }
            Err(err) => return Err(Box::new(err)),
        };
        for entry_result in read_dir {
            let entry = match entry_result {
                Ok(value) => value,
                Err(_) => continue,
            };

            let child_path = entry.path();
            let child_abs = match absolutize_path(&child_path) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let child_string = path_to_string(&child_abs);
            let file_type = match entry.file_type() {
                Ok(value) => value,
                Err(_) => continue,
            };

            child_count += 1;

            if file_type.is_dir() {
                let child_size = scan_dir(ctx, &child_abs, Some(dir_id.clone()), depth + 1)?;
                total_size += child_size;
            } else if file_type.is_file() {
                let file_meta = match entry.metadata() {
                    Ok(value) => value,
                    Err(err) if is_permission_denied(&err) => {
                        emit_permission_denied(&child_abs, &err);
                        continue;
                    }
                    Err(_) => continue,
                };
                let file_size = file_meta.len() as i64;
                total_size += file_size;

                let record = NodeRecord {
                    task_id: ctx.task_id.clone(),
                    node_id: node_id_for(&child_string),
                    parent_id: Some(dir_id.clone()),
                    path: child_string.clone(),
                    name: entry.file_name().to_string_lossy().to_string(),
                    node_type: "file".to_string(),
                    depth: depth + 1,
                    self_size: file_size,
                    total_size: file_size,
                    child_count: 0,
                    mtime_ms: metadata_to_mtime_ms(Some(&file_meta)),
                    ext: extension_for(&child_abs),
                };
                ctx.queue_node(record, &child_string, depth + 1)?;
            }
        }
    } else {
        total_size = compute_hidden_dir_size(&dir_abs)?;
        child_count = count_children(&dir_abs)? as i64;
    }

    let record = NodeRecord {
        task_id: ctx.task_id.clone(),
        node_id: dir_id,
        parent_id,
        path: dir_string.clone(),
        name: directory_name(&dir_abs),
        node_type: "directory".to_string(),
        depth,
        self_size: 0,
        total_size,
        child_count,
        mtime_ms: metadata_to_mtime_ms(metadata.as_ref()),
        ext: String::new(),
    };
    ctx.queue_node(record, &dir_string, depth)?;

    Ok(total_size)
}

fn compute_hidden_dir_size(path: &Path) -> Result<i64, Box<dyn std::error::Error>> {
    let mut total = 0_i64;
    let read_dir = match fs::read_dir(path) {
        Ok(value) => value,
        Err(err) if is_permission_denied(&err) => {
            emit_permission_denied(path, &err);
            return Ok(0);
        }
        Err(err) => return Err(Box::new(err)),
    };
    for entry_result in read_dir {
        let entry = match entry_result {
            Ok(value) => value,
            Err(_) => continue,
        };
        let child_path = entry.path();
        let file_type = match entry.file_type() {
            Ok(value) => value,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            total += compute_hidden_dir_size(&child_path)?;
        } else if file_type.is_file() {
            if let Ok(meta) = entry.metadata() {
                total += meta.len() as i64;
            }
        }
    }
    Ok(total)
}

fn count_children(path: &Path) -> Result<usize, Box<dyn std::error::Error>> {
    let mut count = 0_usize;
    let read_dir = match fs::read_dir(path) {
        Ok(value) => value,
        Err(err) if is_permission_denied(&err) => return Ok(0),
        Err(err) => return Err(Box::new(err)),
    };
    for entry_result in read_dir {
        if entry_result.is_ok() {
            count += 1;
        }
    }
    Ok(count)
}

fn insert_nodes(tx: &Transaction<'_>, nodes: &[NodeRecord]) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO scan_nodes (task_id, node_id, parent_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms, ext)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
            ext = excluded.ext"
    )?;

    for node in nodes {
        stmt.execute(params![
            node.task_id,
            node.node_id,
            node.parent_id,
            node.path,
            node.name,
            node.node_type,
            node.depth as i64,
            node.self_size,
            node.total_size,
            node.child_count,
            node.mtime_ms,
            node.ext,
        ])?;
    }

    Ok(())
}

fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
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
        "#,
    )?;
    Ok(())
}

fn absolutize_path(path: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn node_id_for(path_value: &str) -> String {
    let lowered = path_value.to_lowercase();
    let mut hasher = Sha1::new();
    hasher.update(lowered.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn directory_name(path: &Path) -> String {
    path.file_name()
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| path_to_string(path))
}

fn extension_for(path: &Path) -> String {
    path.extension()
        .map(|value| format!(".{}", value.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}

fn metadata_to_mtime_ms(metadata: Option<&fs::Metadata>) -> Option<i64> {
    let modified = metadata?.modified().ok()?;
    let duration = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(duration.as_millis() as i64)
}

fn emit_event(value: serde_json::Value) {
    let mut stdout = io::stdout();
    let _ = writeln!(stdout, "{}", value);
    let _ = stdout.flush();
}

fn now_iso() -> String {
    let now = chrono_like_now();
    now
}

fn chrono_like_now() -> String {
    let now = SystemTime::now();
    let datetime = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = datetime.as_secs() as i64;
    let nanos = datetime.subsec_nanos();
    unix_to_iso(secs, nanos)
}

fn unix_to_iso(secs: i64, nanos: u32) -> String {
    use std::fmt::Write as _;

    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;
    let millis = nanos / 1_000_000;

    let mut output = String::with_capacity(24);
    let _ = write!(
        output,
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, month, day, hour, minute, second, millis
    );
    output
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year, m, d)
}
