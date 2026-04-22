use crate::backend::{AppState, ScanSnapshot, ScanStartInput, TokenUsage};
use crate::persist;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use uuid::Uuid;

const SCAN_PROGRESS_PERSIST_INTERVAL: Duration = Duration::from_millis(1000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanMode {
    FullRescanIncremental,
    DeepenIncremental,
}

impl ScanMode {
    fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or("").trim() {
            "deepen_incremental" => Self::DeepenIncremental,
            _ => Self::FullRescanIncremental,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::FullRescanIncremental => "full_rescan_incremental",
            Self::DeepenIncremental => "deepen_incremental",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ScanPruneRule {
    SkipSubtree {
        path: String,
    },
    CapSubtreeDepth {
        path: String,
        max_relative_depth: u32,
    },
    SkipReparsePoints,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PruneRuleJson {
    SkipSubtree {
        path: String,
    },
    CapSubtreeDepth {
        path: String,
        max_relative_depth: u32,
    },
    SkipReparsePoints,
}

impl From<&ScanPruneRule> for PruneRuleJson {
    fn from(value: &ScanPruneRule) -> Self {
        match value {
            ScanPruneRule::SkipSubtree { path } => Self::SkipSubtree { path: path.clone() },
            ScanPruneRule::CapSubtreeDepth {
                path,
                max_relative_depth,
            } => Self::CapSubtreeDepth {
                path: path.clone(),
                max_relative_depth: *max_relative_depth,
            },
            ScanPruneRule::SkipReparsePoints => Self::SkipReparsePoints,
        }
    }
}

fn normalize_scan_path_key(value: &str) -> String {
    let mut normalized = value.trim().replace('/', "\\").to_lowercase();
    while normalized.len() > 3 && normalized.ends_with('\\') {
        normalized.pop();
    }
    normalized
}

fn is_same_or_descendant_path(path: &str, parent: &str) -> bool {
    path == parent || path.starts_with(&format!("{parent}\\"))
}

fn relative_path_depth(path: &str, parent: &str) -> Option<u32> {
    if !is_same_or_descendant_path(path, parent) {
        return None;
    }
    if path == parent {
        return Some(0);
    }
    let suffix = path.strip_prefix(parent)?.strip_prefix('\\')?;
    Some(suffix.split('\\').count() as u32)
}

fn is_c_drive_root(path: &str) -> bool {
    normalize_scan_path_key(path) == "c:\\"
}

fn build_scan_prune_rules(target_path: &str) -> Vec<ScanPruneRule> {
    if !is_c_drive_root(target_path) {
        return Vec::new();
    }

    vec![
        ScanPruneRule::SkipSubtree {
            path: "C:\\System Volume Information".to_string(),
        },
        ScanPruneRule::SkipSubtree {
            path: "C:\\Recovery".to_string(),
        },
        ScanPruneRule::SkipSubtree {
            path: "C:\\$Recycle.Bin".to_string(),
        },
        ScanPruneRule::SkipSubtree {
            path: "C:\\Documents and Settings".to_string(),
        },
        ScanPruneRule::SkipSubtree {
            path: "C:\\Config.Msi".to_string(),
        },
        ScanPruneRule::SkipSubtree {
            path: "C:\\PerfLogs".to_string(),
        },
        ScanPruneRule::CapSubtreeDepth {
            path: "C:\\Windows".to_string(),
            max_relative_depth: 2,
        },
        ScanPruneRule::CapSubtreeDepth {
            path: "C:\\Program Files".to_string(),
            max_relative_depth: 2,
        },
        ScanPruneRule::CapSubtreeDepth {
            path: "C:\\Program Files (x86)".to_string(),
            max_relative_depth: 2,
        },
        ScanPruneRule::CapSubtreeDepth {
            path: "C:\\ProgramData".to_string(),
            max_relative_depth: 3,
        },
        ScanPruneRule::SkipReparsePoints,
    ]
}

fn should_exclude_deepen_boundary_node(
    node: &persist::ScanNode,
    prune_rules: &[ScanPruneRule],
) -> bool {
    if node.child_count == 0 {
        return true;
    }

    let path_key = normalize_scan_path_key(&node.path);
    prune_rules.iter().any(|rule| match rule {
        ScanPruneRule::SkipSubtree { path } => {
            let rule_path = normalize_scan_path_key(path);
            is_same_or_descendant_path(&path_key, &rule_path)
        }
        ScanPruneRule::CapSubtreeDepth {
            path,
            max_relative_depth,
        } => {
            let rule_path = normalize_scan_path_key(path);
            relative_path_depth(&path_key, &rule_path)
                .map(|depth| depth >= *max_relative_depth)
                .unwrap_or(false)
        }
        ScanPruneRule::SkipReparsePoints => false,
    })
}

fn resolve_deepen_boundary_nodes(
    db_path: &Path,
    baseline: &ScanSnapshot,
    requested_max_depth: Option<u32>,
    prune_rules: &[ScanPruneRule],
) -> Result<(u32, Vec<persist::ScanNode>), String> {
    let boundary_depth = baseline.max_scanned_depth;
    if requested_max_depth
        .map(|value| value <= boundary_depth)
        .unwrap_or(false)
    {
        return Err("New maxDepth must be greater than the current depth".to_string());
    }
    let nodes = persist::list_boundary_scan_nodes(db_path, &baseline.id, boundary_depth)?
        .into_iter()
        .filter(|node| !should_exclude_deepen_boundary_node(node, prune_rules))
        .collect::<Vec<_>>();
    if nodes.is_empty() {
        return Err("No boundary directories available for deeper scan".to_string());
    }
    Ok((boundary_depth, nodes))
}

#[derive(Clone, Debug, serde::Serialize)]
struct SidecarRoot {
    path: String,
    parent_id: Option<String>,
    depth: u32,
}

pub struct ScanTaskRuntime {
    pub stop: AtomicBool,
    pub child_pid: Mutex<Option<u32>>,
    pub snapshot: Mutex<ScanSnapshot>,
    pub max_depth: Option<u32>,
    scan_mode: ScanMode,
    sidecar_roots: Vec<SidecarRoot>,
    ignored_paths: Vec<String>,
    prune_rules: Vec<ScanPruneRule>,
    last_progress_persist_at: Mutex<Option<Instant>>,
    pub job: Mutex<Option<JoinHandle<()>>>,
}

fn scanner_binary_candidates<R: Runtime>(app: &AppHandle<R>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        out.push(resource_dir.join("scanner.exe"));
        out.push(resource_dir.join("bin").join("scanner.exe"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            out.push(parent.join("scanner.exe"));
            out.push(parent.join("bin").join("scanner.exe"));
            out.push(parent.join("..").join("..").join("bin").join("scanner.exe"));
            out.push(
                parent
                    .join("..")
                    .join("..")
                    .join("..")
                    .join("bin")
                    .join("scanner.exe"),
            );
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join("bin").join("scanner.exe"));
    }
    out
}

fn scanner_binary_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    scanner_binary_candidates(app)
        .into_iter()
        .find(|p| p.exists())
        .ok_or_else(|| "scanner.exe not found in dev or bundle paths".to_string())
}

fn kill_pid(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .status()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

async fn emit_cache_event<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    action: &str,
    node: Option<&persist::ScanNode>,
    count: Option<u64>,
) -> Result<(), String> {
    let payload = json!({
        "taskId": task_id,
        "action": action,
        "path": node.map(|item| item.path.clone()).unwrap_or_default(),
        "name": node.map(|item| item.name.clone()).unwrap_or_default(),
        "nodeType": node.map(|item| item.node_type.clone()).unwrap_or_default(),
        "count": count.unwrap_or(0),
    });
    app.emit("scan_cache", payload).map_err(|e| e.to_string())
}

async fn emit_progress<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
) -> Result<(), String> {
    emit_progress_with_options(app, state, task, true).await
}

async fn emit_progress_with_options<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
    persist_snapshot: bool,
) -> Result<(), String> {
    let snap = task.snapshot.lock().clone();
    if persist_snapshot {
        persist::save_scan_snapshot(&state.db_path(), &snap)?;
    }
    app.emit(
        "scan_progress",
        serde_json::to_value(&snap).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn should_persist_scan_progress(task: &Arc<ScanTaskRuntime>, force: bool) -> bool {
    let mut last_persisted_at = task.last_progress_persist_at.lock();
    if force {
        *last_persisted_at = Some(Instant::now());
        return true;
    }
    let now = Instant::now();
    match *last_persisted_at {
        Some(previous) if now.duration_since(previous) < SCAN_PROGRESS_PERSIST_INTERVAL => false,
        _ => {
            *last_persisted_at = Some(now);
            true
        }
    }
}

async fn handle_sidecar_line<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
    payload: &Value,
) -> Result<(), String> {
    let task_id = task.snapshot.lock().id.clone();
    match payload.get("type").and_then(Value::as_str).unwrap_or("") {
        "task_started" | "scan_progress" | "scan_completed" => {
            let force_persist = matches!(
                payload.get("type").and_then(Value::as_str).unwrap_or(""),
                "task_started" | "scan_completed"
            );
            {
                let mut snap = task.snapshot.lock();
                snap.status = "scanning".to_string();
                if let Some(path) = payload.get("current_path").and_then(Value::as_str) {
                    snap.current_path = path.to_string();
                }
                snap.current_depth = payload
                    .get("current_depth")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32;
                let scanned_delta = payload
                    .get("scanned_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let total_delta = payload
                    .get("total_entries")
                    .and_then(Value::as_u64)
                    .unwrap_or(scanned_delta);
                snap.scanned_count = scanned_delta;
                snap.total_entries = total_delta;
                snap.max_scanned_depth = snap.max_scanned_depth.max(snap.current_depth);
            }
            emit_progress_with_options(
                app,
                state,
                task,
                should_persist_scan_progress(task, force_persist),
            )
            .await?;
        }
        "permission_denied" => {
            let warning = {
                let mut snap = task.snapshot.lock();
                snap.permission_denied_count = snap.permission_denied_count.saturating_add(1);
                if let Some(path) = payload.get("path").and_then(Value::as_str) {
                    if !snap
                        .permission_denied_paths
                        .iter()
                        .any(|x| x.eq_ignore_ascii_case(path))
                    {
                        snap.permission_denied_paths.push(path.to_string());
                        if snap.permission_denied_paths.len() > 20 {
                            let drain_to = snap.permission_denied_paths.len() - 20;
                            snap.permission_denied_paths.drain(..drain_to);
                        }
                    }
                }
                json!({
                    "taskId": task_id,
                    "type": "permission_denied",
                    "path": payload.get("path").and_then(Value::as_str).unwrap_or(""),
                    "message": payload.get("message").and_then(Value::as_str).unwrap_or("Access denied"),
                    "count": snap.permission_denied_count,
                })
            };
            emit_progress_with_options(app, state, task, should_persist_scan_progress(task, false))
                .await?;
            app.emit("scan_warning", warning)
                .map_err(|e| e.to_string())?;
        }
        _ => {}
    }
    Ok(())
}

async fn run_sidecar_scan<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
) -> Result<(), String> {
    let bin = scanner_binary_path(app)?;
    let snap = task.snapshot.lock().clone();
    let mut child = Command::new(bin);
    child
        .arg("scan")
        .arg("--db")
        .arg(state.db_path())
        .arg("--task-id")
        .arg(&snap.id)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let roots_path = if task.sidecar_roots.len() > 1
        || task
            .sidecar_roots
            .first()
            .map(|root| {
                root.depth > 0
                    || root.parent_id.is_some()
                    || !root.path.eq_ignore_ascii_case(&snap.target_path)
            })
            .unwrap_or(false)
    {
        let file_path = std::env::temp_dir().join(format!("wipeout-scan-roots-{}.json", snap.id));
        fs::write(
            &file_path,
            serde_json::to_vec(&task.sidecar_roots).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        child.arg("--roots-json").arg(&file_path);
        Some(file_path)
    } else {
        child.arg("--root").arg(&snap.target_path);
        None
    };
    let ignore_path_file = if task.ignored_paths.is_empty() {
        None
    } else {
        let file_path = std::env::temp_dir().join(format!("wipeout-scan-ignore-{}.json", snap.id));
        fs::write(
            &file_path,
            serde_json::to_vec(&task.ignored_paths).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        child.arg("--ignore-json").arg(&file_path);
        Some(file_path)
    };
    let prune_rule_file = if task.prune_rules.is_empty() {
        None
    } else {
        let file_path = std::env::temp_dir().join(format!("wipeout-scan-prune-{}.json", snap.id));
        let payload = task
            .prune_rules
            .iter()
            .map(PruneRuleJson::from)
            .collect::<Vec<_>>();
        fs::write(
            &file_path,
            serde_json::to_vec(&payload).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        child.arg("--prune-rules-json").arg(&file_path);
        Some(file_path)
    };
    if let Some(max_depth) = task.max_depth {
        child.arg("--max-depth").arg(max_depth.to_string());
    }
    let mut child = child.spawn().map_err(|e| e.to_string())?;

    *task.child_pid.lock() = child.id().into();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "scanner stdout unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "scanner stderr unavailable".to_string())?;
    let stderr_last = Arc::new(Mutex::new(String::new()));
    let stderr_last_clone = stderr_last.clone();
    let stderr_handle = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let trimmed = line.trim().to_string();
            if !trimmed.is_empty() {
                *stderr_last_clone.lock() = trimmed;
            }
        }
    });

    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if task.stop.load(Ordering::Relaxed) {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(payload) = serde_json::from_str::<Value>(&line) {
            handle_sidecar_line(app, state, task, &payload).await?;
        }
    }

    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = stderr_handle.join();
    *task.child_pid.lock() = None;
    if let Some(file_path) = roots_path {
        let _ = fs::remove_file(file_path);
    }
    if let Some(file_path) = ignore_path_file {
        let _ = fs::remove_file(file_path);
    }
    if let Some(file_path) = prune_rule_file {
        let _ = fs::remove_file(file_path);
    }
    if task.stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    if !status.success() {
        let msg = stderr_last.lock().clone();
        return Err(if msg.is_empty() {
            format!("scanner.exe exited with {status}")
        } else {
            msg
        });
    }
    Ok(())
}

async fn run_scan_task<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
) -> Result<(), String> {
    {
        let mut snap = task.snapshot.lock();
        snap.status = "scanning".to_string();
    }
    emit_progress(app, state, task).await?;
    prepare_runtime_scan_data(app, state, task).await?;
    run_sidecar_scan(app, state, task).await?;
    if task.stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    if !task.stop.load(Ordering::Relaxed) {
        let mut snap = task.snapshot.lock();
        if task.scan_mode == ScanMode::FullRescanIncremental {
            snap.visible_latest = true;
        }
        snap.status = "done".to_string();
        persist::save_scan_snapshot(&state.db_path(), &snap)?;
        if task.scan_mode == ScanMode::FullRescanIncremental {
            persist::finalize_full_scan_draft(&state.db_path(), &snap.id)?;
        }
        let payload = serde_json::to_value(&*snap).map_err(|e| e.to_string())?;
        drop(snap);
        app.emit("scan_done", payload).map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn prepare_runtime_scan_data<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<ScanTaskRuntime>,
) -> Result<(), String> {
    if task.scan_mode != ScanMode::DeepenIncremental {
        return Ok(());
    }

    let task_id = task.snapshot.lock().id.clone();
    let affected_paths = task
        .sidecar_roots
        .iter()
        .map(|root| root.path.clone())
        .collect::<Vec<_>>();

    emit_cache_event(
        app,
        &task_id,
        "prepare_incremental_reuse_tree",
        None,
        Some(affected_paths.len() as u64),
    )
    .await?;

    emit_cache_event(
        app,
        &task_id,
        "prepare_incremental_prune",
        None,
        Some(affected_paths.len() as u64),
    )
    .await?;

    if !affected_paths.is_empty() {
        persist::delete_scan_descendants_for_paths(&state.db_path(), &task_id, &affected_paths)?;
    }
    if !task.ignored_paths.is_empty() {
        persist::delete_scan_data_for_paths(&state.db_path(), &task_id, &task.ignored_paths)?;
    }

    if let Some(prepared_snapshot) = persist::load_scan_snapshot(&state.db_path(), &task_id)? {
        let mut snap = task.snapshot.lock();
        snap.deletable = prepared_snapshot.deletable;
        snap.deletable_count = prepared_snapshot.deletable_count;
        snap.total_cleanable = prepared_snapshot.total_cleanable;
        snap.token_usage = prepared_snapshot.token_usage;
        snap.permission_denied_count = prepared_snapshot.permission_denied_count;
        snap.permission_denied_paths = prepared_snapshot.permission_denied_paths;
        snap.error_message = prepared_snapshot.error_message;
    }

    emit_cache_event(app, &task_id, "prepare_incremental_ready", None, None).await?;

    Ok(())
}

fn resolve_latest_baseline_task_id(
    state: &AppState,
    target_path: &str,
    requested: Option<&str>,
) -> Result<Option<String>, String> {
    let requested = requested.unwrap_or("").trim();
    if !requested.is_empty() {
        return Ok(Some(requested.to_string()));
    }
    persist::find_latest_visible_scan_task_id_for_path(&state.db_path(), target_path)
}

pub async fn scan_start<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    input: ScanStartInput,
) -> Result<Value, String> {
    if input.target_path.trim().is_empty() {
        return Err("targetPath is required".to_string());
    }
    let target_path = PathBuf::from(&input.target_path)
        .to_string_lossy()
        .to_string();
    let ignored_paths = crate::backend::read_scan_ignore_paths(&state.settings_path());
    let scan_mode = ScanMode::parse(input.scan_mode.as_deref());
    let prune_rules = build_scan_prune_rules(&target_path);
    let target_root_key = persist::create_root_path_key(&target_path);
    if state.scan_tasks.lock().values().any(|task| {
        let snapshot = task.snapshot.lock();
        matches!(snapshot.status.as_str(), "idle" | "scanning" | "analyzing")
            && snapshot.root_path_key == target_root_key
    }) {
        return Err("A scan task for this path is already running".to_string());
    }
    let baseline_task_id = resolve_latest_baseline_task_id(
        state.inner(),
        &target_path,
        input.baseline_task_id.as_deref(),
    )?;
    let baseline_snapshot = if let Some(task_id) = baseline_task_id.as_deref() {
        persist::load_scan_snapshot(&state.db_path(), task_id)?
    } else {
        None
    };
    if matches!(scan_mode, ScanMode::DeepenIncremental) && baseline_snapshot.is_none() {
        return Err("A baseline task is required for deepen_incremental".to_string());
    }
    let task_id = if matches!(scan_mode, ScanMode::DeepenIncremental) {
        let existing_task_id = baseline_task_id
            .as_ref()
            .ok_or_else(|| "Missing baseline snapshot".to_string())?
            .clone();
        if state.scan_tasks.lock().contains_key(&existing_task_id) {
            return Err("The selected scan task is already running".to_string());
        }
        existing_task_id
    } else {
        format!("scan_{}", Uuid::new_v4().simple())
    };
    let deepen_boundary_nodes = if matches!(scan_mode, ScanMode::DeepenIncremental) {
        let baseline = baseline_snapshot
            .as_ref()
            .ok_or_else(|| "Missing baseline snapshot".to_string())?;
        Some(resolve_deepen_boundary_nodes(
            &state.db_path(),
            baseline,
            input.max_depth,
            &prune_rules,
        )?)
    } else {
        None
    };
    let max_depth = input.max_depth.map(|value| value.clamp(1, 16));
    let mut snapshot = if matches!(scan_mode, ScanMode::DeepenIncremental) {
        let mut existing = baseline_snapshot
            .clone()
            .ok_or_else(|| "Missing baseline snapshot".to_string())?;
        existing.status = "idle".to_string();
        existing.scan_mode = scan_mode.as_str().to_string();
        existing.baseline_task_id = None;
        existing.visible_latest = true;
        existing.root_path_key = persist::create_root_path_key(&target_path);
        existing.target_path = target_path.clone();
        existing.auto_analyze = false;
        existing.root_node_id = persist::create_node_id(&target_path);
        existing.configured_max_depth = max_depth;
        existing.current_path = target_path.clone();
        existing.current_depth = 0;
        existing.scanned_count = 0;
        existing.total_entries = 0;
        existing.processed_entries = 0;
        existing.deletable_count = 0;
        existing.total_cleanable = 0;
        existing.target_size = 0;
        existing.token_usage = TokenUsage::default();
        existing.deletable.clear();
        existing.error_message.clear();
        existing.id = task_id.clone();
        existing
    } else {
        persist::init_full_scan_draft(
            &state.db_path(),
            &task_id,
            &target_path,
            0,
            max_depth,
            false,
            baseline_task_id.as_deref(),
        )?;
        ScanSnapshot {
            id: task_id.clone(),
            status: "idle".to_string(),
            scan_mode: scan_mode.as_str().to_string(),
            baseline_task_id: baseline_task_id.clone(),
            visible_latest: false,
            root_path_key: persist::create_root_path_key(&target_path),
            target_path: target_path.clone(),
            auto_analyze: false,
            root_node_id: persist::create_node_id(&target_path),
            configured_max_depth: max_depth,
            max_scanned_depth: baseline_snapshot
                .as_ref()
                .map(|item| item.max_scanned_depth)
                .unwrap_or(0),
            current_path: target_path.clone(),
            current_depth: 0,
            scanned_count: 0,
            total_entries: 0,
            processed_entries: 0,
            deletable_count: 0,
            total_cleanable: 0,
            target_size: 0,
            token_usage: TokenUsage::default(),
            deletable: Vec::new(),
            permission_denied_count: 0,
            permission_denied_paths: Vec::new(),
            error_message: String::new(),
        }
    };
    let mut sidecar_roots = vec![SidecarRoot {
        path: target_path.clone(),
        parent_id: None,
        depth: 0,
    }];
    if matches!(scan_mode, ScanMode::DeepenIncremental) {
        let (boundary_depth, boundary_nodes) = deepen_boundary_nodes
            .clone()
            .ok_or_else(|| "No boundary directories available for deeper scan".to_string())?;
        snapshot.current_depth = boundary_depth;
        snapshot.current_path = boundary_nodes
            .first()
            .map(|node| node.path.clone())
            .unwrap_or_else(|| target_path.clone());
        sidecar_roots = boundary_nodes
            .into_iter()
            .map(|node| SidecarRoot {
                path: node.path,
                parent_id: node.parent_id,
                depth: node.depth,
            })
            .collect();
    }
    persist::save_scan_snapshot(&state.db_path(), &snapshot)?;
    let task = Arc::new(ScanTaskRuntime {
        stop: AtomicBool::new(false),
        child_pid: Mutex::new(None),
        snapshot: Mutex::new(snapshot),
        max_depth,
        scan_mode,
        sidecar_roots,
        ignored_paths,
        prune_rules,
        last_progress_persist_at: Mutex::new(None),
        job: Mutex::new(None),
    });
    state
        .scan_tasks
        .lock()
        .insert(task_id.clone(), task.clone());

    let state_clone = state.inner().clone();
    let task_id_clone = task_id.clone();
    let app_clone = app.clone();
    let runtime = task.clone();
    let handle = std::thread::spawn(move || {
        tauri::async_runtime::block_on(async move {
            let result = run_scan_task(&app_clone, &state_clone, &runtime).await;
            if let Err(err) = result {
                let mut snap = runtime.snapshot.lock();
                snap.status = "error".to_string();
                snap.error_message = err.clone();
                let payload =
                    json!({ "taskId": task_id_clone, "message": err, "snapshot": &*snap });
                if runtime.scan_mode == ScanMode::FullRescanIncremental {
                    let _ =
                        persist::discard_full_scan_draft(&state_clone.db_path(), &task_id_clone);
                } else {
                    let _ = persist::save_scan_snapshot(&state_clone.db_path(), &snap);
                }
                drop(snap);
                let _ = app_clone.emit("scan_error", payload);
            } else if runtime.stop.load(Ordering::Relaxed) {
                let mut snap = runtime.snapshot.lock();
                snap.status = "stopped".to_string();
                let payload = serde_json::to_value(&*snap).unwrap_or_else(|_| json!({}));
                if runtime.scan_mode == ScanMode::FullRescanIncremental {
                    let _ =
                        persist::discard_full_scan_draft(&state_clone.db_path(), &task_id_clone);
                } else {
                    let _ = persist::save_scan_snapshot(&state_clone.db_path(), &snap);
                }
                drop(snap);
                let _ = app_clone.emit("scan_stopped", payload);
            }
            state_clone.scan_tasks.lock().remove(&task_id_clone);
        });
    });
    *task.job.lock() = Some(handle);
    Ok(json!({ "taskId": task_id, "status": "started" }))
}

pub async fn scan_stop(state: State<'_, AppState>, task_id: String) -> Result<Value, String> {
    let task = state
        .scan_tasks
        .lock()
        .get(&task_id)
        .cloned()
        .ok_or_else(|| "Task not found".to_string())?;
    task.stop.store(true, Ordering::Relaxed);
    if let Some(pid) = *task.child_pid.lock() {
        let _ = kill_pid(pid);
    }
    Ok(json!({ "success": true }))
}

pub async fn scan_get_active(state: State<'_, AppState>) -> Result<Vec<Value>, String> {
    let map = state.scan_tasks.lock();
    Ok(map
        .values()
        .map(|task| task.snapshot.lock().clone())
        .filter(|snap| matches!(snap.status.as_str(), "idle" | "scanning" | "analyzing"))
        .map(|snap| {
            json!({
                "taskId": snap.id,
                "id": snap.id,
                "status": snap.status,
                "scanMode": snap.scan_mode,
                "baselineTaskId": snap.baseline_task_id,
                "visibleLatest": snap.visible_latest,
                "rootPathKey": snap.root_path_key,
                "targetPath": snap.target_path,
                "autoAnalyze": snap.auto_analyze,
                "rootNodeId": snap.root_node_id,
                "configuredMaxDepth": snap.configured_max_depth,
                "maxScannedDepth": snap.max_scanned_depth,
                "currentPath": snap.current_path,
                "currentDepth": snap.current_depth,
                "scannedCount": snap.scanned_count,
                "totalEntries": snap.total_entries,
                "processedEntries": snap.processed_entries,
                "deletableCount": snap.deletable_count,
                "totalCleanable": snap.total_cleanable,
                "targetSize": snap.target_size,
                "tokenUsage": snap.token_usage,
                "deletable": snap.deletable,
                "permissionDeniedCount": snap.permission_denied_count,
                "permissionDeniedPaths": snap.permission_denied_paths,
                "errorMessage": snap.error_message
            })
        })
        .collect())
}

pub async fn scan_list_history(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<Value>, String> {
    persist::prepare_scan_module_access(&state.db_path())?;
    persist::list_scan_history(&state.db_path(), limit.unwrap_or(20).clamp(1, 200))
}

pub async fn scan_delete_history(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Value, String> {
    if let Some(task) = state.scan_tasks.lock().get(&task_id).cloned() {
        let status = task.snapshot.lock().status.clone();
        if matches!(status.as_str(), "idle" | "scanning" | "analyzing") {
            return Err("Task is still running".to_string());
        }
    }
    if !persist::delete_committed_scan_task(&state.db_path(), &task_id)? {
        return Err("Task not found".to_string());
    }
    Ok(json!({ "success": true }))
}

pub async fn scan_get_result(state: State<'_, AppState>, task_id: String) -> Result<Value, String> {
    if let Some(task) = state.scan_tasks.lock().get(&task_id).cloned() {
        let snap = task.snapshot.lock().clone();
        return serde_json::to_value(snap).map_err(|e| e.to_string());
    }
    persist::prepare_scan_module_access(&state.db_path())?;
    let snapshot = persist::load_scan_snapshot(&state.db_path(), &task_id)?
        .ok_or_else(|| "Task not found".to_string())?;
    serde_json::to_value(snapshot).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use uuid::Uuid;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wipeout-{name}-{}", Uuid::new_v4()))
    }

    fn create_scan_fixture(root: &Path) {
        fs::create_dir_all(root.join("A").join("nested")).expect("create nested dir");
        fs::create_dir_all(root.join("B")).expect("create B dir");
        fs::write(root.join("root.log"), b"root").expect("write root file");
        fs::write(root.join("A").join("a.txt"), b"aaa").expect("write a.txt");
        fs::write(root.join("A").join("nested").join("deep.txt"), b"deep").expect("write deep.txt");
        fs::write(root.join("B").join("b.txt"), b"bbb").expect("write b.txt");
    }

    fn test_scanner_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("native")
            .join("scanner")
            .join("target")
            .join("debug")
            .join("scanner.exe")
    }

    fn make_snapshot(
        task_id: &str,
        root: &Path,
        max_depth: Option<u32>,
        scan_mode: &str,
        baseline_task_id: Option<String>,
    ) -> ScanSnapshot {
        let root_string = root.to_string_lossy().to_string();
        ScanSnapshot {
            id: task_id.to_string(),
            status: "idle".to_string(),
            scan_mode: scan_mode.to_string(),
            baseline_task_id,
            visible_latest: true,
            root_path_key: persist::create_root_path_key(&root_string),
            target_path: root_string.clone(),
            auto_analyze: false,
            root_node_id: persist::create_node_id(&root_string),
            configured_max_depth: max_depth,
            max_scanned_depth: 0,
            current_path: root_string,
            current_depth: 0,
            scanned_count: 0,
            total_entries: 0,
            processed_entries: 0,
            deletable_count: 0,
            total_cleanable: 0,
            target_size: 0,
            token_usage: TokenUsage::default(),
            deletable: Vec::new(),
            permission_denied_count: 0,
            permission_denied_paths: Vec::new(),
            error_message: String::new(),
        }
    }

    fn apply_sidecar_output_to_db_internal(
        db_path: &Path,
        task_id: &str,
        root: &Path,
        scan_mode: &str,
        baseline_task_id: Option<String>,
        max_depth: Option<u32>,
        stdout_lines: &[String],
        finalize_full_scan: bool,
    ) {
        let mut snapshot = make_snapshot(task_id, root, max_depth, scan_mode, baseline_task_id);

        for line in stdout_lines {
            let payload: Value = serde_json::from_str(line).expect("parse sidecar event");
            match payload.get("type").and_then(Value::as_str).unwrap_or("") {
                "task_started" | "scan_progress" | "scan_completed" => {
                    if let Some(path) = payload.get("current_path").and_then(Value::as_str) {
                        snapshot.current_path = path.to_string();
                    }
                    snapshot.current_depth = payload
                        .get("current_depth")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32;
                    snapshot.scanned_count = payload
                        .get("scanned_count")
                        .and_then(Value::as_u64)
                        .unwrap_or(snapshot.scanned_count);
                    snapshot.total_entries = payload
                        .get("total_entries")
                        .and_then(Value::as_u64)
                        .unwrap_or(snapshot.total_entries);
                    snapshot.max_scanned_depth =
                        snapshot.max_scanned_depth.max(snapshot.current_depth);
                    snapshot.status = "scanning".to_string();
                }
                "permission_denied" => {
                    snapshot.permission_denied_count =
                        snapshot.permission_denied_count.saturating_add(1);
                    if let Some(path) = payload.get("path").and_then(Value::as_str) {
                        snapshot.permission_denied_paths.push(path.to_string());
                    }
                }
                other => panic!("unexpected sidecar event: {other}"),
            }
        }

        snapshot.status = "done".to_string();
        persist::save_scan_snapshot(db_path, &snapshot).expect("save final snapshot");
        if finalize_full_scan && scan_mode == "full_rescan_incremental" {
            persist::finalize_full_scan_draft(db_path, task_id).expect("finalize full scan draft");
        }
    }

    fn apply_sidecar_output_to_db(
        db_path: &Path,
        task_id: &str,
        root: &Path,
        scan_mode: &str,
        baseline_task_id: Option<String>,
        max_depth: Option<u32>,
        stdout_lines: &[String],
    ) {
        apply_sidecar_output_to_db_internal(
            db_path,
            task_id,
            root,
            scan_mode,
            baseline_task_id,
            max_depth,
            stdout_lines,
            true,
        )
    }

    fn run_scanner_command(args: &[String]) -> Vec<String> {
        let scanner_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("native")
            .join("scanner");
        let build_status = Command::new("cargo")
            .arg("build")
            .current_dir(&scanner_dir)
            .status()
            .expect("build scanner");
        assert!(
            build_status.success(),
            "scanner build failed with {build_status}"
        );

        let scanner = test_scanner_path();
        assert!(
            scanner.exists(),
            "scanner.exe missing at {}",
            scanner.display()
        );

        let mut child = Command::new(scanner)
            .args(args)
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn scanner");
        let stdout = child.stdout.take().expect("scanner stdout");
        let lines = BufReader::new(stdout)
            .lines()
            .map(|line| line.expect("read scanner line"))
            .collect::<Vec<_>>();
        let status = child.wait().expect("wait scanner");
        assert!(status.success(), "scanner exited with {status}");
        lines
    }

    #[test]
    fn full_scan_sidecar_output_persists_snapshot_and_nodes() {
        let root = temp_path("scan-root");
        let db_path = temp_path("scan-db.sqlite");
        fs::create_dir_all(&root).expect("create root");
        create_scan_fixture(&root);
        persist::init_db(&db_path).expect("init db");

        let task_id = format!("scan_{}", Uuid::new_v4().simple());
        let root_string = root.to_string_lossy().to_string();
        persist::init_full_scan_draft(&db_path, &task_id, &root_string, 0, Some(1), false, None)
            .expect("init full scan task");

        let lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            task_id.clone(),
            "--root".to_string(),
            root_string.clone(),
            "--max-depth".to_string(),
            "1".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &task_id,
            &root,
            "full_rescan_incremental",
            None,
            Some(1),
            &lines,
        );

        let snapshot = persist::load_scan_snapshot(&db_path, &task_id)
            .expect("load snapshot")
            .expect("snapshot exists");
        assert_eq!(snapshot.status, "done");
        assert_eq!(snapshot.configured_max_depth, Some(1));
        assert_eq!(snapshot.max_scanned_depth, 1);
        assert!(snapshot.scanned_count >= 4);

        let root_children =
            persist::load_scan_children(&db_path, &task_id, &snapshot.root_node_id, false)
                .expect("load root children");
        assert!(root_children.iter().any(|node| node.name == "A"));
        assert!(root_children.iter().any(|node| node.name == "B"));

        let boundaries =
            persist::list_boundary_scan_nodes(&db_path, &task_id, 1).expect("list boundaries");
        assert!(boundaries.iter().any(|node| node.name == "A"));
        assert!(boundaries.iter().any(|node| node.name == "B"));

        let latest = persist::find_latest_visible_scan_task_id_for_path(&db_path, &root_string)
            .expect("find latest")
            .expect("latest task");
        assert_eq!(latest, task_id);

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn deepen_incremental_scan_updates_existing_tree_in_place() {
        let root = temp_path("deepen-root");
        let db_path = temp_path("deepen-db.sqlite");
        fs::create_dir_all(&root).expect("create root");
        create_scan_fixture(&root);
        persist::init_db(&db_path).expect("init db");

        let baseline_task_id = format!("scan_{}", Uuid::new_v4().simple());
        let root_string = root.to_string_lossy().to_string();
        persist::init_full_scan_draft(
            &db_path,
            &baseline_task_id,
            &root_string,
            0,
            Some(1),
            false,
            None,
        )
        .expect("init baseline task");
        let baseline_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            baseline_task_id.clone(),
            "--root".to_string(),
            root_string.clone(),
            "--max-depth".to_string(),
            "1".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &baseline_task_id,
            &root,
            "full_rescan_incremental",
            None,
            Some(1),
            &baseline_lines,
        );

        let boundary_nodes =
            persist::list_boundary_scan_nodes(&db_path, &baseline_task_id, 1).expect("boundaries");
        assert!(
            !boundary_nodes.is_empty(),
            "baseline boundary nodes missing"
        );

        persist::delete_scan_descendants_for_paths(
            &db_path,
            &baseline_task_id,
            &boundary_nodes
                .iter()
                .map(|node| node.path.clone())
                .collect::<Vec<_>>(),
        )
        .expect("delete boundary descendants");

        let roots_json = temp_path("deepen-roots.json");
        let roots_payload = boundary_nodes
            .iter()
            .map(|node| {
                json!({
                    "path": node.path,
                    "parent_id": node.parent_id,
                    "depth": node.depth,
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            &roots_json,
            serde_json::to_vec(&roots_payload).expect("serialize roots"),
        )
        .expect("write roots payload");

        let deepen_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            baseline_task_id.clone(),
            "--roots-json".to_string(),
            roots_json.to_string_lossy().to_string(),
            "--max-depth".to_string(),
            "2".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &baseline_task_id,
            &root,
            "deepen_incremental",
            None,
            Some(2),
            &deepen_lines,
        );

        let deepen_snapshot = persist::load_scan_snapshot(&db_path, &baseline_task_id)
            .expect("load deepen snapshot")
            .expect("deepen snapshot exists");
        assert_eq!(deepen_snapshot.status, "done");
        assert_eq!(deepen_snapshot.id, baseline_task_id);
        assert_eq!(deepen_snapshot.configured_max_depth, Some(2));
        assert_eq!(deepen_snapshot.max_scanned_depth, 2);

        let node_map =
            persist::load_scan_node_map(&db_path, &baseline_task_id).expect("load node map");
        assert!(node_map.contains_key(&root.join("A").to_string_lossy().to_lowercase()));
        assert!(node_map.contains_key(
            &root
                .join("A")
                .join("nested")
                .to_string_lossy()
                .to_lowercase()
        ));
        assert!(!node_map.contains_key(
            &root
                .join("A")
                .join("nested")
                .join("deep.txt")
                .to_string_lossy()
                .to_lowercase()
        ));

        let dir_a_id = persist::create_node_id(&root.join("A").to_string_lossy());
        let dir_a_children =
            persist::load_scan_children(&db_path, &baseline_task_id, &dir_a_id, false)
                .expect("load A children");
        assert!(dir_a_children.iter().any(|node| node.name == "nested"));
        assert!(dir_a_children.iter().any(|node| node.name == "a.txt"));

        let latest = persist::find_latest_visible_scan_task_id_for_path(&db_path, &root_string)
            .expect("find latest task")
            .expect("latest task exists");
        assert_eq!(latest, baseline_task_id);

        let _ = fs::remove_file(&roots_json);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn deepen_incremental_uses_actual_scanned_depth_for_stopped_baseline() {
        let root = temp_path("deepen-stopped-root");
        let db_path = temp_path("deepen-stopped-db.sqlite");
        fs::create_dir_all(&root).expect("create root");
        create_scan_fixture(&root);
        persist::init_db(&db_path).expect("init db");

        let baseline_task_id = format!("scan_{}", Uuid::new_v4().simple());
        let root_string = root.to_string_lossy().to_string();
        persist::init_full_scan_draft(
            &db_path,
            &baseline_task_id,
            &root_string,
            0,
            Some(1),
            false,
            None,
        )
        .expect("init baseline task");
        let baseline_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            baseline_task_id.clone(),
            "--root".to_string(),
            root_string.clone(),
            "--max-depth".to_string(),
            "1".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &baseline_task_id,
            &root,
            "full_rescan_incremental",
            None,
            Some(1),
            &baseline_lines,
        );

        let mut stopped_snapshot = persist::load_scan_snapshot(&db_path, &baseline_task_id)
            .expect("load stopped snapshot")
            .expect("stopped snapshot exists");
        stopped_snapshot.scan_mode = "deepen_incremental".to_string();
        stopped_snapshot.configured_max_depth = Some(3);
        stopped_snapshot.status = "stopped".to_string();
        stopped_snapshot.current_path = root_string.clone();
        stopped_snapshot.current_depth = 1;
        persist::save_scan_snapshot(&db_path, &stopped_snapshot).expect("save stopped snapshot");

        let stopped_snapshot = persist::load_scan_snapshot(&db_path, &baseline_task_id)
            .expect("reload stopped snapshot")
            .expect("reloaded stopped snapshot exists");
        assert_eq!(stopped_snapshot.configured_max_depth, Some(3));
        assert_eq!(stopped_snapshot.max_scanned_depth, 1);

        let (boundary_depth, boundary_nodes) =
            resolve_deepen_boundary_nodes(&db_path, &stopped_snapshot, Some(4), &[])
                .expect("resolve deepen boundary nodes");
        assert_eq!(boundary_depth, 1);
        assert!(boundary_nodes.iter().any(|node| node.name == "A"));
        assert!(boundary_nodes.iter().any(|node| node.name == "B"));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn full_scan_draft_finalize_prunes_ancestor_cache_and_switches_owner() {
        let root = temp_path("child-owner-root");
        let db_path = temp_path("child-owner-db.sqlite");
        fs::create_dir_all(&root).expect("create root");
        create_scan_fixture(&root);
        persist::init_db(&db_path).expect("init db");

        let root_task_id = format!("scan_{}", Uuid::new_v4().simple());
        let root_string = root.to_string_lossy().to_string();
        persist::init_full_scan_draft(
            &db_path,
            &root_task_id,
            &root_string,
            0,
            Some(3),
            false,
            None,
        )
        .expect("init root draft");
        let root_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            root_task_id.clone(),
            "--root".to_string(),
            root_string.clone(),
            "--max-depth".to_string(),
            "3".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &root_task_id,
            &root,
            "full_rescan_incremental",
            None,
            Some(3),
            &root_lines,
        );

        let deep_txt = root.join("A").join("nested").join("deep.txt");
        fs::remove_file(&deep_txt).expect("remove deep file");
        fs::write(root.join("A").join("nested").join("fresh.txt"), b"fresh")
            .expect("write fresh file");

        let child_root = root.join("A");
        let child_task_id = format!("scan_{}", Uuid::new_v4().simple());
        let child_root_string = child_root.to_string_lossy().to_string();
        persist::init_full_scan_draft(
            &db_path,
            &child_task_id,
            &child_root_string,
            0,
            Some(2),
            false,
            Some(&root_task_id),
        )
        .expect("init child draft");
        let child_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            child_task_id.clone(),
            "--root".to_string(),
            child_root_string.clone(),
            "--max-depth".to_string(),
            "2".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &child_task_id,
            &child_root,
            "full_rescan_incremental",
            Some(root_task_id.clone()),
            Some(2),
            &child_lines,
        );

        let root_node_map =
            persist::load_scan_node_map(&db_path, &root_task_id).expect("root node map");
        assert!(root_node_map.contains_key(&child_root_string.to_lowercase()));
        assert!(!root_node_map.contains_key(&deep_txt.to_string_lossy().to_lowercase()));
        assert!(!root_node_map.contains_key(
            &root
                .join("A")
                .join("nested")
                .join("fresh.txt")
                .to_string_lossy()
                .to_lowercase()
        ));

        let child_node_map =
            persist::load_scan_node_map(&db_path, &child_task_id).expect("child node map");
        assert_eq!(
            root_node_map
                .get(&child_root_string.to_lowercase())
                .expect("ancestor boundary node")
                .size,
            child_node_map
                .get(&child_root_string.to_lowercase())
                .expect("child root node")
                .size
        );

        let dir_a_id = persist::create_node_id(&child_root_string);
        let handed_off_children =
            persist::load_scan_children(&db_path, &root_task_id, &dir_a_id, false)
                .expect("load handed off children");
        assert!(handed_off_children.iter().any(|node| node.name == "nested"));
        assert!(handed_off_children.iter().any(|node| node.name == "a.txt"));

        let nested_children = persist::load_scan_children(
            &db_path,
            &child_task_id,
            &persist::create_node_id(&root.join("A").join("nested").to_string_lossy()),
            false,
        )
        .expect("load nested children");
        assert!(nested_children.iter().any(|node| node.name == "fresh.txt"));
        assert!(!nested_children.iter().any(|node| node.name == "deep.txt"));

        let latest_child =
            persist::find_latest_visible_scan_task_id_for_path(&db_path, &child_root_string)
                .expect("find child owner")
                .expect("child owner exists");
        assert_eq!(latest_child, child_task_id);

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn discarding_full_scan_draft_keeps_ancestor_cache_unchanged() {
        let root = temp_path("discard-draft-root");
        let db_path = temp_path("discard-draft-db.sqlite");
        fs::create_dir_all(&root).expect("create root");
        create_scan_fixture(&root);
        persist::init_db(&db_path).expect("init db");

        let root_task_id = format!("scan_{}", Uuid::new_v4().simple());
        let root_string = root.to_string_lossy().to_string();
        persist::init_full_scan_draft(
            &db_path,
            &root_task_id,
            &root_string,
            0,
            Some(2),
            false,
            None,
        )
        .expect("init root draft");
        let root_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            root_task_id.clone(),
            "--root".to_string(),
            root_string.clone(),
            "--max-depth".to_string(),
            "2".to_string(),
        ]);
        apply_sidecar_output_to_db(
            &db_path,
            &root_task_id,
            &root,
            "full_rescan_incremental",
            None,
            Some(2),
            &root_lines,
        );

        let child_root = root.join("A");
        let child_root_string = child_root.to_string_lossy().to_string();
        let root_before_discard =
            persist::load_scan_node_map(&db_path, &root_task_id).expect("load root baseline");
        let baseline_child_size = root_before_discard
            .get(&child_root_string.to_lowercase())
            .expect("baseline child node")
            .size;

        fs::remove_file(root.join("A").join("nested").join("deep.txt")).expect("remove deep file");
        let draft_task_id = format!("scan_{}", Uuid::new_v4().simple());
        persist::init_full_scan_draft(
            &db_path,
            &draft_task_id,
            &child_root_string,
            0,
            Some(2),
            false,
            Some(&root_task_id),
        )
        .expect("init child draft");
        let child_lines = run_scanner_command(&[
            "scan".to_string(),
            "--db".to_string(),
            db_path.to_string_lossy().to_string(),
            "--task-id".to_string(),
            draft_task_id.clone(),
            "--root".to_string(),
            child_root_string.clone(),
            "--max-depth".to_string(),
            "2".to_string(),
        ]);
        apply_sidecar_output_to_db_internal(
            &db_path,
            &draft_task_id,
            &child_root,
            "full_rescan_incremental",
            Some(root_task_id.clone()),
            Some(2),
            &child_lines,
            false,
        );

        assert!(
            persist::load_full_scan_draft_snapshot(&db_path, &draft_task_id)
                .expect("load draft snapshot")
                .is_some()
        );
        persist::discard_full_scan_draft(&db_path, &draft_task_id).expect("discard child draft");

        let root_after_discard =
            persist::load_scan_node_map(&db_path, &root_task_id).expect("load root after discard");
        assert_eq!(
            root_after_discard
                .get(&child_root_string.to_lowercase())
                .expect("child node after discard")
                .size,
            baseline_child_size
        );
        assert!(
            persist::load_full_scan_draft_snapshot(&db_path, &draft_task_id)
                .expect("reload draft snapshot")
                .is_none()
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn build_scan_prune_rules_only_applies_to_c_drive_root() {
        let c_rules = build_scan_prune_rules("C:\\");
        assert!(c_rules.iter().any(|rule| matches!(
            rule,
            ScanPruneRule::SkipSubtree { path } if path == "C:\\System Volume Information"
        )));
        assert!(c_rules.iter().any(|rule| matches!(
            rule,
            ScanPruneRule::CapSubtreeDepth {
                path,
                max_relative_depth
            } if path == "C:\\Windows" && *max_relative_depth == 2
        )));
        assert!(c_rules
            .iter()
            .any(|rule| matches!(rule, ScanPruneRule::SkipReparsePoints)));

        assert!(build_scan_prune_rules("D:\\").is_empty());
        assert!(build_scan_prune_rules("C:\\Users\\32858\\AppData").is_empty());
    }

    #[test]
    fn deepen_boundary_filter_skips_pruned_and_non_expandable_nodes() {
        let prune_rules = build_scan_prune_rules("C:\\");
        let windows_cap = persist::ScanNode {
            id: "1".to_string(),
            parent_id: Some("root".to_string()),
            path: "C:\\Windows\\System32\\Config".to_string(),
            name: "Config".to_string(),
            node_type: "directory".to_string(),
            depth: 3,
            self_size: 0,
            size: 1024,
            child_count: 2,
            mtime_ms: None,
        };
        let users_node = persist::ScanNode {
            id: "2".to_string(),
            parent_id: Some("root".to_string()),
            path: "C:\\Users\\32858".to_string(),
            name: "32858".to_string(),
            node_type: "directory".to_string(),
            depth: 2,
            self_size: 0,
            size: 2048,
            child_count: 3,
            mtime_ms: None,
        };
        let empty_leaf = persist::ScanNode {
            id: "3".to_string(),
            parent_id: Some("root".to_string()),
            path: "C:\\Temp\\Empty".to_string(),
            name: "Empty".to_string(),
            node_type: "directory".to_string(),
            depth: 2,
            self_size: 0,
            size: 0,
            child_count: 0,
            mtime_ms: None,
        };

        assert!(should_exclude_deepen_boundary_node(
            &windows_cap,
            &prune_rules
        ));
        assert!(!should_exclude_deepen_boundary_node(
            &users_node,
            &prune_rules
        ));
        assert!(should_exclude_deepen_boundary_node(
            &empty_leaf,
            &prune_rules
        ));
    }
}
