#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha1::{Digest, Sha1};
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

#[derive(Clone)]
struct ScanRootInput {
    path: String,
    parent_id: Option<String>,
    depth: usize,
}

#[derive(Clone, Serialize)]
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
    SkipSubtree {
        path: String,
    },
    CapSubtreeDepth {
        path: String,
        max_relative_depth: usize,
    },
    SkipReparsePoints,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ScanPruneRule {
    SkipSubtree {
        path: String,
    },
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

struct ScanContext {
    task_id: String,
    max_depth: Option<usize>,
    ignored_paths: Vec<String>,
    prune_rules: Vec<ScanPruneRule>,
    scanned_count: u64,
    buffered: Vec<NodeRecord>,
    last_progress_mark: u64,
}

impl ScanContext {
    fn new(
        task_id: String,
        max_depth: Option<usize>,
        ignored_paths: Vec<String>,
        prune_rules: Vec<ScanPruneRule>,
    ) -> Self {
        Self {
            task_id,
            max_depth,
            ignored_paths,
            prune_rules,
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
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.scanned_count += 1;
        self.buffered.push(record);

        if self.buffered.len() >= BATCH_SIZE {
            self.flush_nodes()?;
        }

        if self.scanned_count == 1 || self.scanned_count - self.last_progress_mark >= PROGRESS_EVERY
        {
            self.last_progress_mark = self.scanned_count;
            self.update_scan_progress(current_path, current_depth);
        }

        Ok(())
    }

    fn flush_nodes(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.buffered.is_empty() {
            return Ok(());
        }

        emit_event(json!({
            "type": "node_batch",
            "taskId": self.task_id,
            "nodes": self.buffered,
        }));
        self.buffered.clear();
        Ok(())
    }

    fn update_scan_progress(&self, current_path: &str, current_depth: usize) {
        emit_event(json!({
            "type": "scan_progress",
            "taskId": self.task_id,
            "current_path": current_path,
            "current_depth": current_depth,
            "scanned_count": self.scanned_count,
            "total_entries": self.scanned_count,
            "updated_at": now_iso(),
        }));
    }

    fn is_ignored(&self, path: &Path) -> bool {
        let path_key = normalize_path_key(&path_to_string(path));
        self.ignored_paths
            .iter()
            .any(|ignored| is_same_or_descendant_path(&path_key, ignored))
    }

    fn dir_traversal_mode(&self, path: &Path) -> DirTraversalMode {
        let path_key = normalize_path_key(&path_to_string(path));
        let mut mode = DirTraversalMode::Normal;
        for rule in &self.prune_rules {
            match rule {
                ScanPruneRule::SkipSubtree { path } => {
                    if is_same_or_descendant_path(&path_key, path) {
                        return DirTraversalMode::SkipSubtree;
                    }
                }
                ScanPruneRule::CapSubtreeDepth {
                    path,
                    max_relative_depth,
                } => {
                    if relative_path_depth(&path_key, path)
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
    let roots = if let Some(roots_json_path) = args.roots_json.as_deref() {
        let raw = fs::read_to_string(roots_json_path)?;
        let raw_values = serde_json::from_str::<Vec<serde_json::Value>>(&raw)?;
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
            item.path = path_to_string(&absolutize_path(Path::new(&item.path))?);
        }
        parsed
    } else {
        let root =
            absolutize_path(Path::new(args.root.as_deref().ok_or_else(|| {
                io::Error::new(ErrorKind::InvalidInput, "--root is required")
            })?))?;
        vec![ScanRootInput {
            path: path_to_string(&root),
            parent_id: None,
            depth: 0,
        }]
    };
    let root_string = roots
        .first()
        .map(|item| item.path.clone())
        .unwrap_or_default();
    let ignored_paths = load_ignored_paths(args.ignore_json.as_deref())?;
    let prune_rules = load_prune_rules(args.prune_rules_json.as_deref())?;

    emit_event(json!({
        "type": "task_started",
        "taskId": args.task_id,
        "current_path": root_string,
        "current_depth": 0,
        "scanned_count": 0,
        "total_entries": 0,
        "updated_at": now_iso(),
    }));

    let mut ctx = ScanContext::new(
        args.task_id.clone(),
        args.max_depth,
        ignored_paths,
        prune_rules,
    );
    for root in &roots {
        let abs = absolutize_path(Path::new(&root.path))?;
        if ctx.is_ignored(&abs) {
            continue;
        }
        let _ = scan_dir(&mut ctx, &abs, root.parent_id.clone(), root.depth)?;
    }
    ctx.flush_nodes()?;
    ctx.update_scan_progress(&root_string, 0);

    emit_event(json!({
        "type": "scan_completed",
        "taskId": ctx.task_id,
        "current_path": root_string,
        "current_depth": 0,
        "scanned_count": ctx.scanned_count,
        "total_entries": ctx.scanned_count,
        "updated_at": now_iso(),
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
    if ctx.is_ignored(&dir_abs) {
        return Ok(0);
    }
    let dir_string = path_to_string(&dir_abs);
    let dir_id = node_id_for(&dir_string);
    let metadata = fs::symlink_metadata(&dir_abs).ok();

    let mut total_size = 0_i64;
    let mut child_count = 0_i64;

    if ctx.should_skip_reparse_point(metadata.as_ref()) {
        let record = NodeRecord {
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

    let traversal_mode = ctx.dir_traversal_mode(&dir_abs);
    if matches!(traversal_mode, DirTraversalMode::SkipSubtree) {
        let record = NodeRecord {
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

    if matches!(traversal_mode, DirTraversalMode::CapSubtree)
        || !ctx.max_depth.map(|limit| depth < limit).unwrap_or(true)
    {
        total_size = compute_hidden_dir_size(ctx, &dir_abs)?;
        child_count = count_children(ctx, &dir_abs)? as i64;
    } else {
        let read_dir = match fs::read_dir(&dir_abs) {
            Ok(value) => value,
            Err(err) if is_permission_denied(&err) => {
                emit_permission_denied(&dir_abs, &err);
                let record = NodeRecord {
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
            if ctx.is_ignored(&child_abs) {
                continue;
            }
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
    }

    let record = NodeRecord {
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

fn compute_hidden_dir_size(
    ctx: &ScanContext,
    path: &Path,
) -> Result<i64, Box<dyn std::error::Error>> {
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
        if ctx.is_ignored(&child_path) {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(value) => value,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            let child_meta = fs::symlink_metadata(&child_path).ok();
            if ctx.should_skip_reparse_point(child_meta.as_ref()) {
                continue;
            }
            match ctx.dir_traversal_mode(&child_path) {
                DirTraversalMode::SkipSubtree => continue,
                DirTraversalMode::CapSubtree | DirTraversalMode::Normal => {
                    total += compute_hidden_dir_size(ctx, &child_path)?;
                }
            }
        } else if file_type.is_file() {
            if let Ok(meta) = entry.metadata() {
                total += meta.len() as i64;
            }
        }
    }
    Ok(total)
}

fn count_children(ctx: &ScanContext, path: &Path) -> Result<usize, Box<dyn std::error::Error>> {
    let mut count = 0_usize;
    let read_dir = match fs::read_dir(path) {
        Ok(value) => value,
        Err(err) if is_permission_denied(&err) => return Ok(0),
        Err(err) => return Err(Box::new(err)),
    };
    for entry_result in read_dir {
        if let Ok(entry) = entry_result {
            if ctx.is_ignored(&entry.path()) {
                continue;
            }
            count += 1;
        }
    }
    Ok(count)
}

fn load_prune_rules(path: Option<&str>) -> Result<Vec<ScanPruneRule>, Box<dyn std::error::Error>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let raw = fs::read_to_string(path)?;
    let values = serde_json::from_str::<Vec<PruneRuleJson>>(&raw)?;
    let mut out = Vec::new();
    for value in values {
        match value {
            PruneRuleJson::SkipSubtree { path } => {
                let absolute = absolutize_path(Path::new(path.trim()))?;
                let normalized = normalize_path_key(&path_to_string(&absolute));
                if normalized.is_empty() {
                    continue;
                }
                out.push(ScanPruneRule::SkipSubtree { path: normalized });
            }
            PruneRuleJson::CapSubtreeDepth {
                path,
                max_relative_depth,
            } => {
                let absolute = absolutize_path(Path::new(path.trim()))?;
                let normalized = normalize_path_key(&path_to_string(&absolute));
                if normalized.is_empty() {
                    continue;
                }
                out.push(ScanPruneRule::CapSubtreeDepth {
                    path: normalized,
                    max_relative_depth,
                });
            }
            PruneRuleJson::SkipReparsePoints => out.push(ScanPruneRule::SkipReparsePoints),
        }
    }
    Ok(out)
}

fn load_ignored_paths(path: Option<&str>) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let raw = fs::read_to_string(path)?;
    let values = serde_json::from_str::<Vec<String>>(&raw)?;
    let mut out = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let absolute = absolutize_path(Path::new(trimmed))?;
        let normalized = normalize_path_key(&path_to_string(&absolute));
        if normalized.is_empty() || out.iter().any(|item| item == &normalized) {
            continue;
        }
        out.push(normalized);
    }
    Ok(out)
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
    fn cap_subtree_depth_stops_after_relative_boundary_and_keeps_hidden_size() {
        let root = std::env::temp_dir().join(format!("scanner-cap-{}", std::process::id()));
        let target = root.join("Windows");
        let leaf = target.join("System32").join("drivers").join("etc");
        fs::create_dir_all(&leaf).expect("create nested fixture");
        fs::write(leaf.join("hosts"), b"127.0.0.1 localhost").expect("write hosts file");

        let mut ctx = ScanContext::new(
            "task".to_string(),
            None,
            Vec::new(),
            vec![ScanPruneRule::CapSubtreeDepth {
                path: normalize_path_key(&path_to_string(&target)),
                max_relative_depth: 2,
            }],
        );

        let total_size = scan_dir(&mut ctx, &target, None, 1).expect("scan target");

        let system32 = ctx
            .buffered
            .iter()
            .find(|node| {
                node.path
                    .eq_ignore_ascii_case(&path_to_string(&target.join("System32")))
            })
            .expect("system32 node exists");
        assert!(total_size > 0);
        assert!(system32.total_size > 0);
        assert_eq!(system32.child_count, 1);
        assert!(
            !ctx.buffered.iter().any(|node| {
                node.path.eq_ignore_ascii_case(&path_to_string(
                    &target.join("System32").join("drivers").join("etc"),
                ))
            }),
            "cap boundary should prevent deeper nodes from being emitted"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn skip_subtree_keeps_directory_node_without_descending() {
        let root = std::env::temp_dir().join(format!("scanner-skip-{}", std::process::id()));
        let target = root.join("Recovery").join("nested");
        fs::create_dir_all(&target).expect("create skipped tree");
        fs::write(target.join("hidden.bin"), b"skip").expect("write skipped file");

        let skip_root = root.join("Recovery");
        let mut ctx = ScanContext::new(
            "task".to_string(),
            None,
            Vec::new(),
            vec![ScanPruneRule::SkipSubtree {
                path: normalize_path_key(&path_to_string(&skip_root)),
            }],
        );

        let total_size = scan_dir(&mut ctx, &skip_root, None, 1).expect("scan skipped root");

        assert_eq!(total_size, 0);
        assert_eq!(ctx.buffered.len(), 1);
        let record = &ctx.buffered[0];
        assert_eq!(record.child_count, 0);
        assert_eq!(record.total_size, 0);

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
