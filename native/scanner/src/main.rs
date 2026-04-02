#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::{Parser, Subcommand};
use crossbeam_channel::{unbounded, Receiver, Sender};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha1::{Digest, Sha1};
use std::env;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

const WRITE_BATCH_SIZE: usize = 4096;
const LOCAL_BATCH_SIZE: usize = 256;
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
    root: Option<String>,
    #[arg(long = "roots-json")]
    roots_json: Option<String>,
    #[arg(long = "ignore-json")]
    ignore_json: Option<String>,
    #[arg(long = "prune-rules-json")]
    prune_rules_json: Option<String>,
    #[arg(long = "max-depth")]
    max_depth: Option<usize>,
}

#[derive(Clone, Debug)]
struct ScanRootInput {
    path: String,
    parent_id: Option<String>,
    depth: usize,
}

#[derive(Clone, Debug, Serialize)]
struct NodeRecord {
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PruneRuleJson {
    SkipSubtree { path: String },
    CapSubtreeDepth {
        path: String,
        max_relative_depth: usize,
    },
    SkipReparsePoints,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ScanPruneRule {
    SkipSubtree { path: String },
    CapSubtreeDepth {
        path: String,
        max_relative_depth: usize,
    },
    SkipReparsePoints,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirTraversalMode {
    Normal,
    CapSubtree,
    SkipSubtree,
}

#[derive(Clone, Copy, Debug, Default)]
struct HiddenSummary {
    total_size: i64,
    child_count: i64,
}

#[derive(Clone, Debug)]
struct ScanTask {
    path: String,
    parent_id: Option<String>,
    depth: usize,
    result_tx: Sender<Result<i64, String>>,
}

#[derive(Clone)]
struct EventEmitter {
    stdout_lock: Arc<Mutex<()>>,
}

impl EventEmitter {
    fn new() -> Self {
        Self {
            stdout_lock: Arc::new(Mutex::new(())),
        }
    }

    fn emit(&self, value: serde_json::Value) {
        let _guard = self.stdout_lock.lock().ok();
        let mut stdout = io::stdout();
        let _ = writeln!(stdout, "{}", value);
        let _ = stdout.flush();
    }
}

struct ProgressReporter {
    task_id: String,
    emitter: EventEmitter,
    scanned_count: AtomicU64,
    last_progress_mark: AtomicU64,
}

impl ProgressReporter {
    fn new(task_id: String, emitter: EventEmitter) -> Self {
        Self {
            task_id,
            emitter,
            scanned_count: AtomicU64::new(0),
            last_progress_mark: AtomicU64::new(0),
        }
    }

    fn record(&self, path: &str, depth: usize) {
        let count = self.scanned_count.fetch_add(1, Ordering::Relaxed) + 1;
        if count == 1 {
            if self
                .last_progress_mark
                .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                self.emit_progress(path, depth, count);
            }
            return;
        }
        loop {
            let previous = self.last_progress_mark.load(Ordering::Relaxed);
            if count.saturating_sub(previous) < PROGRESS_EVERY {
                break;
            }
            if self
                .last_progress_mark
                .compare_exchange(previous, count, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                self.emit_progress(path, depth, count);
                break;
            }
        }
    }

    fn emit_progress(&self, path: &str, depth: usize, count: u64) {
        self.emitter.emit(json!({
            "type": "scan_progress",
            "taskId": self.task_id,
            "current_path": path,
            "current_depth": depth,
            "scanned_count": count,
            "total_entries": count,
            "updated_at": now_iso(),
        }));
    }

    fn count(&self) -> u64 {
        self.scanned_count.load(Ordering::Relaxed)
    }
}

struct SharedScanContext {
    max_depth: Option<usize>,
    ignored_paths: Vec<String>,
    prune_rules: Vec<ScanPruneRule>,
    writer_tx: Sender<Vec<NodeRecord>>,
    progress: Arc<ProgressReporter>,
}

struct RecordBuffer {
    writer_tx: Sender<Vec<NodeRecord>>,
    progress: Arc<ProgressReporter>,
    buffered: Vec<NodeRecord>,
}

impl RecordBuffer {
    fn new(writer_tx: Sender<Vec<NodeRecord>>, progress: Arc<ProgressReporter>) -> Self {
        Self {
            writer_tx,
            progress,
            buffered: Vec::with_capacity(LOCAL_BATCH_SIZE),
        }
    }

    fn queue(&mut self, record: NodeRecord) -> Result<(), String> {
        self.progress.record(&record.path, record.depth);
        self.buffered.push(record);
        if self.buffered.len() >= LOCAL_BATCH_SIZE {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), String> {
        if self.buffered.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(&mut self.buffered);
        self.writer_tx
            .send(batch)
            .map_err(|err| format!("writer channel closed: {err}"))
    }
}

fn main() {
    if let Err(err) = run() {
        let emitter = EventEmitter::new();
        emitter.emit(json!({
            "type": "error",
            "message": err,
        }));
        let _ = writeln!(io::stderr(), "{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Scan(args) => run_scan(args),
    }
}

fn run_scan(args: ScanArgs) -> Result<(), String> {
    let roots = load_roots(&args)?;
    let root_string = roots
        .first()
        .map(|item| item.path.clone())
        .unwrap_or_default();
    let ignored_paths = load_ignored_paths(args.ignore_json.as_deref())?;
    let prune_rules = load_prune_rules(args.prune_rules_json.as_deref())?;
    let emitter = EventEmitter::new();
    let progress = Arc::new(ProgressReporter::new(args.task_id.clone(), emitter.clone()));
    let (writer_tx, writer_handle) = spawn_writer(&args.db, &args.task_id)?;
    let shared = Arc::new(SharedScanContext {
        max_depth: args.max_depth,
        ignored_paths,
        prune_rules,
        writer_tx,
        progress: progress.clone(),
    });

    emitter.emit(json!({
        "type": "task_started",
        "taskId": args.task_id,
        "current_path": root_string,
        "current_depth": 0,
        "scanned_count": 0,
        "total_entries": 0,
        "updated_at": now_iso(),
    }));

    let worker_count = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4)
        .clamp(1, 8);
    let (task_tx, task_rx) = unbounded::<ScanTask>();
    let worker_handles = spawn_workers(shared.clone(), task_rx, worker_count);

    let scan_result = if roots.len() == 1 && roots[0].parent_id.is_none() && roots[0].depth == 0 {
        scan_primary_root(shared.clone(), &task_tx, &roots[0])
    } else {
        scan_roots_via_workers(shared.clone(), &task_tx, &roots)
    };

    drop(task_tx);
    let mut join_error = None;
    for handle in worker_handles {
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(err)) => join_error = Some(err),
            Err(_) => join_error = Some("worker thread panicked".to_string()),
        }
    }
    drop(shared);

    let writer_error = match writer_handle.join() {
        Ok(Ok(())) => None,
        Ok(Err(err)) => Some(err),
        Err(_) => Some("writer thread panicked".to_string()),
    };

    scan_result?;
    if let Some(err) = join_error {
        return Err(err);
    }
    if let Some(err) = writer_error {
        return Err(err);
    }

    let scanned_count = progress.count();
    emitter.emit(json!({
        "type": "scan_completed",
        "taskId": args.task_id,
        "current_path": root_string,
        "current_depth": 0,
        "scanned_count": scanned_count,
        "total_entries": scanned_count,
        "updated_at": now_iso(),
    }));
    Ok(())
}

fn scan_roots_via_workers(
    shared: Arc<SharedScanContext>,
    task_tx: &Sender<ScanTask>,
    roots: &[ScanRootInput],
) -> Result<(), String> {
    let mut receivers = Vec::new();
    for root in roots {
        let root_key = normalize_path_key(&root.path);
        if shared.is_ignored_path_key(&root_key) {
            continue;
        }
        let (result_tx, result_rx) = unbounded();
        task_tx
            .send(ScanTask {
                path: root.path.clone(),
                parent_id: root.parent_id.clone(),
                depth: root.depth,
                result_tx,
            })
            .map_err(|err| format!("scan worker queue closed: {err}"))?;
        receivers.push(result_rx);
    }

    for rx in receivers {
        rx.recv()
            .map_err(|err| format!("worker result channel closed: {err}"))??;
    }
    Ok(())
}

fn scan_primary_root(
    shared: Arc<SharedScanContext>,
    task_tx: &Sender<ScanTask>,
    root: &ScanRootInput,
) -> Result<(), String> {
    let dir_abs = absolutize_path(Path::new(&root.path)).map_err(|e| e.to_string())?;
    let dir_string = path_to_string(&dir_abs);
    let dir_key = normalize_path_key(&dir_string);
    if shared.is_ignored_path_key(&dir_key) {
        return Ok(());
    }

    let dir_id = node_id_for(&dir_string);
    let metadata = fs::symlink_metadata(&dir_abs).ok();
    let traversal_mode = shared.dir_traversal_mode(&dir_key);
    let mut buffer = RecordBuffer::new(shared.writer_tx.clone(), shared.progress.clone());

    if shared.should_skip_reparse_point(metadata.as_ref())
        || matches!(traversal_mode, DirTraversalMode::SkipSubtree)
    {
        buffer.queue(make_directory_record(
            &dir_abs,
            dir_id,
            root.parent_id.clone(),
            root.depth,
            0,
            0,
            metadata.as_ref(),
        ))?;
        buffer.flush()?;
        return Ok(());
    }

    if matches!(traversal_mode, DirTraversalMode::CapSubtree)
        || !shared.max_depth.map(|limit| root.depth < limit).unwrap_or(true)
    {
        let summary = summarize_hidden_directory(shared.as_ref(), &dir_abs)?;
        buffer.queue(make_directory_record(
            &dir_abs,
            dir_id,
            root.parent_id.clone(),
            root.depth,
            summary.total_size,
            summary.child_count,
            metadata.as_ref(),
        ))?;
        buffer.flush()?;
        return Ok(());
    }

    let read_dir = match fs::read_dir(&dir_abs) {
        Ok(value) => value,
        Err(err) if is_permission_denied(&err) => {
            emit_permission_denied(shared.as_ref(), &dir_string, &err);
            buffer.queue(make_directory_record(
                &dir_abs,
                dir_id,
                root.parent_id.clone(),
                root.depth,
                0,
                0,
                metadata.as_ref(),
            ))?;
            buffer.flush()?;
            return Ok(());
        }
        Err(err) => return Err(err.to_string()),
    };

    let mut total_size = 0_i64;
    let mut child_count = 0_i64;
    let mut receivers = Vec::new();

    for entry_result in read_dir {
        let entry = match entry_result {
            Ok(value) => value,
            Err(_) => continue,
        };
        let child_path = entry.path();
        let child_string = path_to_string(&child_path);
        let child_key = normalize_path_key(&child_string);
        if shared.is_ignored_path_key(&child_key) {
            continue;
        }
        child_count += 1;

        let file_type = match entry.file_type() {
            Ok(value) => value,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            let (result_tx, result_rx) = unbounded();
            task_tx
                .send(ScanTask {
                    path: child_string,
                    parent_id: Some(dir_id.clone()),
                    depth: root.depth + 1,
                    result_tx,
                })
                .map_err(|err| format!("scan worker queue closed: {err}"))?;
            receivers.push(result_rx);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        let file_meta = match entry.metadata() {
            Ok(value) => value,
            Err(err) if is_permission_denied(&err) => {
                emit_permission_denied(shared.as_ref(), &child_path.to_string_lossy(), &err);
                continue;
            }
            Err(_) => continue,
        };
        let file_size = file_meta.len() as i64;
        total_size += file_size;
        buffer.queue(NodeRecord {
            node_id: node_id_for(&child_path.to_string_lossy()),
            parent_id: Some(dir_id.clone()),
            path: child_path.to_string_lossy().to_string(),
            name: entry.file_name().to_string_lossy().to_string(),
            node_type: "file".to_string(),
            depth: root.depth + 1,
            self_size: file_size,
            total_size: file_size,
            child_count: 0,
            mtime_ms: metadata_to_mtime_ms(Some(&file_meta)),
            ext: extension_for(&child_path),
        })?;
    }

    for rx in receivers {
        total_size += rx
            .recv()
            .map_err(|err| format!("worker result channel closed: {err}"))??;
    }

    buffer.queue(make_directory_record(
        &dir_abs,
        dir_id,
        root.parent_id.clone(),
        root.depth,
        total_size,
        child_count,
        metadata.as_ref(),
    ))?;
    buffer.flush()?;
    Ok(())
}

fn spawn_workers(
    shared: Arc<SharedScanContext>,
    task_rx: Receiver<ScanTask>,
    worker_count: usize,
) -> Vec<JoinHandle<Result<(), String>>> {
    (0..worker_count)
        .map(|_| {
            let shared = shared.clone();
            let task_rx = task_rx.clone();
            thread::spawn(move || worker_loop(shared, task_rx))
        })
        .collect()
}

fn worker_loop(shared: Arc<SharedScanContext>, task_rx: Receiver<ScanTask>) -> Result<(), String> {
    while let Ok(task) = task_rx.recv() {
        let result = scan_directory_subtree(shared.as_ref(), &task.path, task.parent_id, task.depth);
        let _ = task.result_tx.send(result);
    }
    Ok(())
}

fn spawn_writer(
    db_path: &str,
    task_id: &str,
) -> Result<(Sender<Vec<NodeRecord>>, JoinHandle<Result<(), String>>), String> {
    let (tx, rx) = unbounded::<Vec<NodeRecord>>();
    let db_path = db_path.to_string();
    let task_id = task_id.to_string();
    let handle = thread::spawn(move || writer_loop(PathBuf::from(db_path), task_id, rx));
    Ok((tx, handle))
}

fn writer_loop(db_path: PathBuf, task_id: String, rx: Receiver<Vec<NodeRecord>>) -> Result<(), String> {
    let mut conn = open_db(&db_path)?;
    let node_table = if conn
        .query_row(
            "SELECT 1 FROM scan_drafts WHERE task_id = ?1 LIMIT 1",
            params![task_id.as_str()],
            |_row| Ok(()),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .is_some()
    {
        "scan_draft_nodes"
    } else {
        "scan_nodes"
    };
    let mut pending = Vec::<NodeRecord>::with_capacity(WRITE_BATCH_SIZE);
    while let Ok(mut batch) = rx.recv() {
        pending.append(&mut batch);
        if pending.len() >= WRITE_BATCH_SIZE {
            flush_node_records(&mut conn, node_table, &task_id, &pending)?;
            pending.clear();
        }
    }
    if !pending.is_empty() {
        flush_node_records(&mut conn, node_table, &task_id, &pending)?;
    }
    Ok(())
}

fn scan_directory_subtree(
    shared: &SharedScanContext,
    dir_path: &str,
    parent_id: Option<String>,
    depth: usize,
) -> Result<i64, String> {
    let dir_abs = absolutize_path(Path::new(dir_path)).map_err(|e| e.to_string())?;
    let mut buffer = RecordBuffer::new(shared.writer_tx.clone(), shared.progress.clone());
    let total = scan_directory_recursive(shared, &mut buffer, &dir_abs, parent_id, depth)?;
    buffer.flush()?;
    Ok(total)
}

fn scan_directory_recursive(
    shared: &SharedScanContext,
    buffer: &mut RecordBuffer,
    dir_path: &Path,
    parent_id: Option<String>,
    depth: usize,
) -> Result<i64, String> {
    let dir_abs = absolutize_path(dir_path).map_err(|e| e.to_string())?;
    let dir_string = path_to_string(&dir_abs);
    let dir_key = normalize_path_key(&dir_string);
    if shared.is_ignored_path_key(&dir_key) {
        return Ok(0);
    }

    let dir_id = node_id_for(&dir_string);
    let metadata = fs::symlink_metadata(&dir_abs).ok();
    if shared.should_skip_reparse_point(metadata.as_ref()) {
        buffer.queue(make_directory_record(
            &dir_abs,
            dir_id,
            parent_id,
            depth,
            0,
            0,
            metadata.as_ref(),
        ))?;
        return Ok(0);
    }

    let traversal_mode = shared.dir_traversal_mode(&dir_key);
    if matches!(traversal_mode, DirTraversalMode::SkipSubtree) {
        buffer.queue(make_directory_record(
            &dir_abs,
            dir_id,
            parent_id,
            depth,
            0,
            0,
            metadata.as_ref(),
        ))?;
        return Ok(0);
    }

    if matches!(traversal_mode, DirTraversalMode::CapSubtree)
        || !shared.max_depth.map(|limit| depth < limit).unwrap_or(true)
    {
        let summary = summarize_hidden_directory(shared, &dir_abs)?;
        buffer.queue(make_directory_record(
            &dir_abs,
            dir_id,
            parent_id,
            depth,
            summary.total_size,
            summary.child_count,
            metadata.as_ref(),
        ))?;
        return Ok(summary.total_size);
    }

    let read_dir = match fs::read_dir(&dir_abs) {
        Ok(value) => value,
        Err(err) if is_permission_denied(&err) => {
            emit_permission_denied(shared, &dir_string, &err);
            buffer.queue(make_directory_record(
                &dir_abs,
                dir_id,
                parent_id,
                depth,
                0,
                0,
                metadata.as_ref(),
            ))?;
            return Ok(0);
        }
        Err(err) => return Err(err.to_string()),
    };

    let mut total_size = 0_i64;
    let mut child_count = 0_i64;
    for entry_result in read_dir {
        let entry = match entry_result {
            Ok(value) => value,
            Err(_) => continue,
        };
        let child_path = entry.path();
        let child_string = path_to_string(&child_path);
        let child_key = normalize_path_key(&child_string);
        if shared.is_ignored_path_key(&child_key) {
            continue;
        }
        child_count += 1;

        let file_type = match entry.file_type() {
            Ok(value) => value,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            total_size +=
                scan_directory_recursive(shared, buffer, &child_path, Some(dir_id.clone()), depth + 1)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        let file_meta = match entry.metadata() {
            Ok(value) => value,
            Err(err) if is_permission_denied(&err) => {
                emit_permission_denied(shared, &child_string, &err);
                continue;
            }
            Err(_) => continue,
        };
        let file_size = file_meta.len() as i64;
        total_size += file_size;
        buffer.queue(NodeRecord {
            node_id: node_id_for(&child_string),
            parent_id: Some(dir_id.clone()),
            path: child_string,
            name: entry.file_name().to_string_lossy().to_string(),
            node_type: "file".to_string(),
            depth: depth + 1,
            self_size: file_size,
            total_size: file_size,
            child_count: 0,
            mtime_ms: metadata_to_mtime_ms(Some(&file_meta)),
            ext: extension_for(&child_path),
        })?;
    }

    buffer.queue(make_directory_record(
        &dir_abs,
        dir_id,
        parent_id,
        depth,
        total_size,
        child_count,
        metadata.as_ref(),
    ))?;
    Ok(total_size)
}

fn summarize_hidden_directory(shared: &SharedScanContext, path: &Path) -> Result<HiddenSummary, String> {
    let mut summary = HiddenSummary::default();
    let read_dir = match fs::read_dir(path) {
        Ok(value) => value,
        Err(err) if is_permission_denied(&err) => {
            emit_permission_denied(shared, &path_to_string(path), &err);
            return Ok(summary);
        }
        Err(err) => return Err(err.to_string()),
    };

    for entry_result in read_dir {
        let entry = match entry_result {
            Ok(value) => value,
            Err(_) => continue,
        };
        let child_path = entry.path();
        let child_string = path_to_string(&child_path);
        let child_key = normalize_path_key(&child_string);
        if shared.is_ignored_path_key(&child_key) {
            continue;
        }
        summary.child_count += 1;

        let file_type = match entry.file_type() {
            Ok(value) => value,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            let child_meta = fs::symlink_metadata(&child_path).ok();
            if shared.should_skip_reparse_point(child_meta.as_ref()) {
                continue;
            }
            match shared.dir_traversal_mode(&child_key) {
                DirTraversalMode::SkipSubtree => {}
                DirTraversalMode::CapSubtree | DirTraversalMode::Normal => {
                    summary.total_size += summarize_hidden_total(shared, &child_path)?;
                }
            }
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            summary.total_size += meta.len() as i64;
        }
    }

    Ok(summary)
}

fn summarize_hidden_total(shared: &SharedScanContext, path: &Path) -> Result<i64, String> {
    let mut total = 0_i64;
    let read_dir = match fs::read_dir(path) {
        Ok(value) => value,
        Err(err) if is_permission_denied(&err) => {
            emit_permission_denied(shared, &path_to_string(path), &err);
            return Ok(0);
        }
        Err(err) => return Err(err.to_string()),
    };

    for entry_result in read_dir {
        let entry = match entry_result {
            Ok(value) => value,
            Err(_) => continue,
        };
        let child_path = entry.path();
        let child_string = path_to_string(&child_path);
        let child_key = normalize_path_key(&child_string);
        if shared.is_ignored_path_key(&child_key) {
            continue;
        }

        let file_type = match entry.file_type() {
            Ok(value) => value,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            let child_meta = fs::symlink_metadata(&child_path).ok();
            if shared.should_skip_reparse_point(child_meta.as_ref()) {
                continue;
            }
            match shared.dir_traversal_mode(&child_key) {
                DirTraversalMode::SkipSubtree => {}
                DirTraversalMode::CapSubtree | DirTraversalMode::Normal => {
                    total += summarize_hidden_total(shared, &child_path)?;
                }
            }
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            total += meta.len() as i64;
        }
    }

    Ok(total)
}

impl SharedScanContext {
    fn is_ignored_path_key(&self, path_key: &str) -> bool {
        self.ignored_paths
            .iter()
            .any(|ignored| is_same_or_descendant_path(path_key, ignored))
    }

    fn dir_traversal_mode(&self, path_key: &str) -> DirTraversalMode {
        let mut mode = DirTraversalMode::Normal;
        for rule in &self.prune_rules {
            match rule {
                ScanPruneRule::SkipSubtree { path } => {
                    if is_same_or_descendant_path(path_key, path) {
                        return DirTraversalMode::SkipSubtree;
                    }
                }
                ScanPruneRule::CapSubtreeDepth {
                    path,
                    max_relative_depth,
                } => {
                    if relative_path_depth(path_key, path)
                        .map(|depth| depth >= *max_relative_depth)
                        .unwrap_or(false)
                    {
                        mode = DirTraversalMode::CapSubtree;
                    }
                }
                ScanPruneRule::SkipReparsePoints => {}
            }
        }
        mode
    }

    fn should_skip_reparse_point(&self, metadata: Option<&fs::Metadata>) -> bool {
        self.prune_rules
            .iter()
            .any(|rule| matches!(rule, ScanPruneRule::SkipReparsePoints))
            && metadata.map(metadata_is_reparse_point).unwrap_or(false)
    }
}

fn emit_permission_denied(shared: &SharedScanContext, path: &str, err: &io::Error) {
    shared.progress.emitter.emit(json!({
        "type": "permission_denied",
        "path": path,
        "message": err.to_string(),
    }));
}

fn make_directory_record(
    path: &Path,
    node_id: String,
    parent_id: Option<String>,
    depth: usize,
    total_size: i64,
    child_count: i64,
    metadata: Option<&fs::Metadata>,
) -> NodeRecord {
    NodeRecord {
        node_id,
        parent_id,
        path: path_to_string(path),
        name: directory_name(path),
        node_type: "directory".to_string(),
        depth,
        self_size: 0,
        total_size,
        child_count,
        mtime_ms: metadata_to_mtime_ms(metadata),
        ext: String::new(),
    }
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

fn flush_node_records(
    conn: &mut Connection,
    node_table: &str,
    task_id: &str,
    nodes: &[NodeRecord],
) -> Result<(), String> {
    if nodes.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut stmt = tx
        .prepare_cached(
            &format!(
                "INSERT INTO {} (
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
                node_table
            ),
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
            node.self_size,
            node.total_size,
            node.child_count,
            node.mtime_ms,
            node.ext,
        ])
        .map_err(|e| e.to_string())?;
    }
    drop(stmt);
    tx.commit().map_err(|e| e.to_string())
}

fn load_roots(args: &ScanArgs) -> Result<Vec<ScanRootInput>, String> {
    if let Some(roots_json_path) = args.roots_json.as_deref() {
        let raw = fs::read_to_string(roots_json_path).map_err(|e| e.to_string())?;
        let raw_values =
            serde_json::from_str::<Vec<serde_json::Value>>(&raw).map_err(|e| e.to_string())?;
        let mut parsed = raw_values
            .into_iter()
            .map(|value| ScanRootInput {
                path: value
                    .get("path")
                    .and_then(|entry| entry.as_str())
                    .unwrap_or_default()
                    .to_string(),
                parent_id: value
                    .get("parent_id")
                    .and_then(|entry| entry.as_str())
                    .map(|entry| entry.to_string()),
                depth: value
                    .get("depth")
                    .and_then(|entry| entry.as_u64())
                    .unwrap_or(0) as usize,
            })
            .collect::<Vec<_>>();
        for item in &mut parsed {
            item.path = path_to_string(&absolutize_path(Path::new(&item.path)).map_err(|e| e.to_string())?);
        }
        return Ok(parsed);
    }

    let root = absolutize_path(Path::new(args.root.as_deref().ok_or_else(|| {
        io::Error::new(ErrorKind::InvalidInput, "--root is required")
    })
    .map_err(|e| e.to_string())?))
    .map_err(|e| e.to_string())?;
    Ok(vec![ScanRootInput {
        path: path_to_string(&root),
        parent_id: None,
        depth: 0,
    }])
}

fn load_prune_rules(path: Option<&str>) -> Result<Vec<ScanPruneRule>, String> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let values = serde_json::from_str::<Vec<PruneRuleJson>>(&raw).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for value in values {
        match value {
            PruneRuleJson::SkipSubtree { path } => {
                let absolute = absolutize_path(Path::new(path.trim())).map_err(|e| e.to_string())?;
                let normalized = normalize_path_key(&path_to_string(&absolute));
                if !normalized.is_empty() {
                    out.push(ScanPruneRule::SkipSubtree { path: normalized });
                }
            }
            PruneRuleJson::CapSubtreeDepth {
                path,
                max_relative_depth,
            } => {
                let absolute = absolutize_path(Path::new(path.trim())).map_err(|e| e.to_string())?;
                let normalized = normalize_path_key(&path_to_string(&absolute));
                if !normalized.is_empty() {
                    out.push(ScanPruneRule::CapSubtreeDepth {
                        path: normalized,
                        max_relative_depth,
                    });
                }
            }
            PruneRuleJson::SkipReparsePoints => out.push(ScanPruneRule::SkipReparsePoints),
        }
    }
    Ok(out)
}

fn load_ignored_paths(path: Option<&str>) -> Result<Vec<String>, String> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let values = serde_json::from_str::<Vec<String>>(&raw).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let absolute = absolutize_path(Path::new(trimmed)).map_err(|e| e.to_string())?;
        let normalized = normalize_path_key(&path_to_string(&absolute));
        if normalized.is_empty() || out.iter().any(|item| item == &normalized) {
            continue;
        }
        out.push(normalized);
    }
    Ok(out)
}

fn absolutize_path(path: &Path) -> io::Result<PathBuf> {
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

fn normalize_path_key(value: &str) -> String {
    let mut normalized = value.trim().replace('/', "\\").to_lowercase();
    while normalized.len() > 3 && normalized.ends_with('\\') {
        normalized.pop();
    }
    normalized
}

fn is_same_or_descendant_path(path: &str, parent: &str) -> bool {
    path == parent || path.starts_with(&format!("{parent}\\"))
}

fn relative_path_depth(path: &str, parent: &str) -> Option<usize> {
    if !is_same_or_descendant_path(path, parent) {
        return None;
    }
    if path == parent {
        return Some(0);
    }
    let suffix = path.strip_prefix(parent)?.strip_prefix('\\')?;
    Some(suffix.split('\\').count())
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

fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn is_permission_denied(err: &io::Error) -> bool {
    err.kind() == ErrorKind::PermissionDenied
}

fn now_iso() -> String {
    let datetime = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    unix_to_iso(datetime.as_secs() as i64, datetime.subsec_nanos())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn normalized_paths_compare_case_insensitively() {
        let ignored = normalize_path_key("C:/Temp/Test/");
        let child = normalize_path_key("c:\\temp\\test\\child.log");
        assert!(is_same_or_descendant_path(&child, &ignored));
    }

    #[test]
    fn sibling_paths_do_not_match_ignore_prefix() {
        let ignored = normalize_path_key("C:\\Temp\\foo");
        let sibling = normalize_path_key("C:\\Temp\\foobar.txt");
        assert!(!is_same_or_descendant_path(&sibling, &ignored));
    }

    #[test]
    fn hidden_summary_counts_direct_children() {
        let root = std::env::temp_dir().join(format!("scanner-cap-{}", std::process::id()));
        let target = root.join("Windows");
        let leaf = target.join("System32").join("drivers").join("etc");
        fs::create_dir_all(&leaf).expect("create nested fixture");
        fs::write(leaf.join("hosts"), b"127.0.0.1 localhost").expect("write hosts file");

        let (writer_tx, writer_rx) = unbounded::<Vec<NodeRecord>>();
        let emitter = EventEmitter::new();
        let progress = Arc::new(ProgressReporter::new("task".to_string(), emitter));
        let shared = SharedScanContext {
            max_depth: None,
            ignored_paths: Vec::new(),
            prune_rules: vec![ScanPruneRule::CapSubtreeDepth {
                path: normalize_path_key(&path_to_string(&target)),
                max_relative_depth: 2,
            }],
            writer_tx,
            progress,
        };

        let summary = summarize_hidden_directory(&shared, &target.join("System32")).expect("summarize");
        drop(writer_rx);
        assert_eq!(summary.child_count, 1);
        assert!(summary.total_size > 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn metadata_reparse_point_detection_checks_windows_attribute_bit() {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            let root = std::env::temp_dir().join(format!("scanner-reparse-{}", std::process::id()));
            fs::create_dir_all(&root).expect("create reparse fixture");
            let metadata = fs::metadata(&root).expect("read metadata");
            let attrs = metadata.file_attributes();
            assert_eq!(
                metadata_is_reparse_point(&metadata),
                attrs & 0x400 != 0 || metadata.file_type().is_symlink()
            );
            let _ = fs::remove_dir_all(&root);
        }
        #[cfg(not(windows))]
        {
            let root = std::env::temp_dir().join(format!("scanner-reparse-{}", std::process::id()));
            fs::create_dir_all(&root).expect("create fixture");
            let metadata = fs::metadata(&root).expect("read metadata");
            assert!(!metadata_is_reparse_point(&metadata));
            let _ = fs::remove_dir_all(&root);
        }
    }
}
