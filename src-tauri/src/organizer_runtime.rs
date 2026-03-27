use crate::backend::{AppState, OrganizeSnapshot, OrganizeStartInput, TokenUsage};
use crate::persist;
use crate::web_search::{
    format_web_search_context, parse_web_search_request, tavily_search, web_search_trace_to_value,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Runtime, State};
use uuid::Uuid;
use walkdir::WalkDir;

const UNCATEGORIZED_NODE_NAME: &str = "\u{5176}\u{4ED6}\u{5F85}\u{5B9A}";
const DEFAULT_BATCH_SIZE: u32 = 20;
const CATEGORY_OTHER_PENDING: &str = "\u{5176}\u{4ED6}\u{5F85}\u{5B9A}";

const DEFAULT_EXCLUDED_PATTERNS: [&str; 11] = [
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "dist",
    "build",
    "out",
    "tmp",
    "temp",
    "$recycle.bin",
    "windows",
];

#[derive(Clone)]
struct RouteConfig {
    endpoint: String,
    api_key: String,
    model: String,
}

#[derive(Clone)]
struct DirectoryHint {
    marker_files: Vec<String>,
    app_signals: Vec<String>,
    top_level_entries: Vec<String>,
    dominant_extensions: Vec<String>,
    total_size: u64,
    file_count: u64,
    dir_count: u64,
}

#[derive(Clone)]
struct OrganizeUnit {
    name: String,
    path: String,
    relative_path: String,
    size: u64,
    created_at: Option<String>,
    modified_at: Option<String>,
    item_type: String,
    modality: String,
    directory_hint: Option<DirectoryHint>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CategoryTreeNode {
    node_id: String,
    name: String,
    #[serde(default)]
    children: Vec<CategoryTreeNode>,
}

const PROJECT_MARKER_NAMES: [&str; 17] = [
    ".git",
    "package.json",
    "pnpm-workspace.yaml",
    "yarn.lock",
    "pyproject.toml",
    "requirements.txt",
    "cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "dockerfile",
    "docker-compose.yml",
    "docker-compose.yaml",
    ".sln",
];

pub struct OrganizeTaskRuntime {
    pub stop: AtomicBool,
    pub snapshot: Mutex<OrganizeSnapshot>,
    routes: HashMap<String, RouteConfig>,
    search_api_key: String,
    response_language: String,
    pub job: Mutex<Option<JoinHandle<()>>>,
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn system_time_to_iso(value: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(value)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn is_zh_language(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized == "zh" || normalized.starts_with("zh-") || normalized.starts_with("zh_")
}

fn localized_language_name(prompt_language: &str, output_language: &str) -> &'static str {
    if is_zh_language(prompt_language) {
        if is_zh_language(output_language) {
            "简体中文"
        } else {
            "英文"
        }
    } else if is_zh_language(output_language) {
        "Simplified Chinese"
    } else {
        "English"
    }
}

fn organizer_unknown_label(value: &str) -> &'static str {
    if is_zh_language(value) {
        "（未知）"
    } else {
        "(unknown)"
    }
}

fn organizer_none_label(value: &str) -> &'static str {
    if is_zh_language(value) {
        "（无）"
    } else {
        "(none)"
    }
}

fn normalize_excluded(patterns: Option<Vec<String>>) -> Vec<String> {
    let mut set = DEFAULT_EXCLUDED_PATTERNS
        .iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>();
    for item in patterns.unwrap_or_default() {
        let trimmed = item.trim().to_lowercase();
        if !trimmed.is_empty() && !set.contains(&trimmed) {
            set.push(trimmed);
        }
    }
    set
}

fn normalize_batch_size(value: Option<u32>) -> u32 {
    value.unwrap_or(DEFAULT_BATCH_SIZE).clamp(1, 200)
}

fn supports_multimodal(model: &str, endpoint: &str) -> bool {
    let value = format!("{}|{}", endpoint.to_lowercase(), model.to_lowercase());
    ["gpt-4o", "gpt-4.1", "gemini", "claude", "glm-4v", "qwen-vl"]
        .iter()
        .any(|x| value.contains(x))
}

fn pick_modality(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if [".mp4", ".mov", ".mkv", ".avi", ".wmv", ".webm"]
        .iter()
        .any(|x| lower.ends_with(x))
    {
        "video"
    } else if [".mp3", ".wav", ".m4a", ".aac", ".flac", ".ogg"]
        .iter()
        .any(|x| lower.ends_with(x))
    {
        "audio"
    } else if [".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp"]
        .iter()
        .any(|x| lower.ends_with(x))
    {
        "image"
    } else {
        "text"
    }
}

fn sanitize_category_name(value: &str) -> String {
    let cleaned = value.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_");
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        CATEGORY_OTHER_PENDING.to_string()
    } else {
        trimmed.to_string()
    }
}

fn parse_routes(model_routing: &Option<Value>) -> HashMap<String, RouteConfig> {
    let mut routes = HashMap::new();
    let source = model_routing
        .as_ref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for modality in ["text", "image", "video", "audio"] {
        let config = source
            .get(modality)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let endpoint = config
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or("https://api.openai.com/v1")
            .trim()
            .to_string();
        let api_key = config
            .get("apiKey")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let model = config
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("gpt-4o-mini")
            .trim()
            .to_string();
        routes.insert(
            modality.to_string(),
            RouteConfig {
                endpoint,
                api_key,
                model,
            },
        );
    }
    routes
}

fn should_exclude(name: &str, patterns: &[String]) -> bool {
    let lower = name.to_lowercase();
    if lower.starts_with('.') {
        return true;
    }
    patterns.iter().any(|p| lower == *p || lower.contains(p))
}

fn extension_key(path: &Path) -> String {
    path.extension()
        .and_then(|x| x.to_str())
        .map(|x| format!(".{}", x.to_ascii_lowercase()))
        .unwrap_or_else(|| "(no_ext)".to_string())
}

fn summarize_directory_tree(
    path: &Path,
    stop: &AtomicBool,
) -> (u64, u64, u64, HashMap<String, u64>) {
    let mut total_size = 0_u64;
    let mut file_count = 0_u64;
    let mut dir_count = 0_u64;
    let mut ext_counts = HashMap::new();
    for entry in WalkDir::new(path)
        .min_depth(1)
        .into_iter()
        .filter_map(Result::ok)
    {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let entry_path = entry.path();
        if entry.file_type().is_dir() {
            dir_count = dir_count.saturating_add(1);
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        file_count = file_count.saturating_add(1);
        if let Ok(meta) = entry.metadata() {
            total_size = total_size.saturating_add(meta.len());
        }
        let key = extension_key(entry_path);
        *ext_counts.entry(key).or_insert(0) += 1;
    }
    (total_size, file_count, dir_count, ext_counts)
}

fn inspect_directory_hint(path: &Path, stop: &AtomicBool) -> Option<DirectoryHint> {
    let mut marker_files = Vec::new();
    let mut app_signals = Vec::new();
    let mut top_level_entries = Vec::new();
    let mut direct_ext_counts = HashMap::new();
    let mut has_readme = false;
    let mut has_src = false;
    let mut has_bin = false;
    let mut has_lib = false;
    let mut has_resources = false;
    let mut direct_exe_count = 0_u32;
    let mut direct_dll_count = 0_u32;

    let entries = fs::read_dir(path).ok()?;
    for entry in entries.filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if top_level_entries.len() < 18 {
            top_level_entries.push(name.clone());
        }

        if PROJECT_MARKER_NAMES.iter().any(|marker| lower == *marker) {
            marker_files.push(name.clone());
        }

        let entry_path = entry.path();
        if entry_path.is_dir() {
            match lower.as_str() {
                "src" | "app" => has_src = true,
                "bin" => has_bin = true,
                "lib" => has_lib = true,
                "resources" | "resource" => has_resources = true,
                _ => {}
            }
            continue;
        }

        if lower == "readme.md" || lower == "readme.txt" || lower == "readme" {
            has_readme = true;
        }
        if lower.ends_with(".exe") {
            direct_exe_count += 1;
        }
        if lower.ends_with(".dll") {
            direct_dll_count += 1;
        }
        let key = extension_key(&entry_path);
        *direct_ext_counts.entry(key).or_insert(0) += 1;
    }

    if has_readme && has_src {
        app_signals.push("readme+src".to_string());
    }
    if has_bin && has_lib {
        app_signals.push("bin+lib".to_string());
    }
    if has_resources {
        app_signals.push("resources".to_string());
    }
    if direct_exe_count > 0 {
        app_signals.push(format!("exe:{direct_exe_count}"));
    }
    if direct_dll_count > 0 {
        app_signals.push(format!("dll:{direct_dll_count}"));
    }

    let project_like = !marker_files.is_empty()
        || (has_readme && has_src)
        || (direct_exe_count > 0 && (direct_dll_count > 0 || has_resources || has_bin || has_lib))
        || (has_bin && has_lib);
    if !project_like {
        return None;
    }

    let (total_size, file_count, dir_count, ext_counts) = summarize_directory_tree(path, stop);
    let mut dominant_extensions = ext_counts.into_iter().collect::<Vec<_>>();
    dominant_extensions.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    Some(DirectoryHint {
        marker_files,
        app_signals,
        top_level_entries,
        dominant_extensions: dominant_extensions
            .into_iter()
            .take(8)
            .map(|(ext, count)| format!("{ext}:{count}"))
            .collect(),
        total_size,
        file_count,
        dir_count,
    })
}

fn collect_units_inner(
    scan_root: &Path,
    current_dir: &Path,
    recursive: bool,
    excluded: &[String],
    stop: &AtomicBool,
    out: &mut Vec<OrganizeUnit>,
) {
    let entries = match fs::read_dir(current_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_exclude(&name, excluded) {
            continue;
        }
        if path.is_dir() {
            if let Some(hint) = inspect_directory_hint(&path, stop) {
                out.push(OrganizeUnit {
                    name: name.clone(),
                    path: path.to_string_lossy().to_string(),
                    relative_path: path
                        .strip_prefix(scan_root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string(),
                    size: hint.total_size,
                    created_at: entry
                        .metadata()
                        .ok()
                        .and_then(|meta| meta.created().ok())
                        .map(system_time_to_iso),
                    modified_at: entry
                        .metadata()
                        .ok()
                        .and_then(|meta| meta.modified().ok())
                        .map(system_time_to_iso),
                    item_type: "directory".to_string(),
                    modality: "directory".to_string(),
                    directory_hint: Some(hint),
                });
                continue;
            }
            if recursive {
                collect_units_inner(scan_root, &path, true, excluded, stop, out);
            }
            continue;
        }
        if !path.is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            out.push(OrganizeUnit {
                name: name.clone(),
                path: path.to_string_lossy().to_string(),
                relative_path: path
                    .strip_prefix(scan_root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string(),
                size: meta.len(),
                created_at: meta.created().ok().map(system_time_to_iso),
                modified_at: meta.modified().ok().map(system_time_to_iso),
                item_type: "file".to_string(),
                modality: pick_modality(&path.to_string_lossy()).to_string(),
                directory_hint: None,
            });
        }
    }
}

fn collect_units(
    root: &Path,
    recursive: bool,
    excluded: &[String],
    stop: &AtomicBool,
) -> Vec<OrganizeUnit> {
    let mut out = Vec::new();
    collect_units_inner(root, root, recursive, excluded, stop, &mut out);
    out.sort_by(|a, b| {
        a.relative_path
            .to_lowercase()
            .cmp(&b.relative_path.to_lowercase())
            .then_with(|| a.item_type.cmp(&b.item_type))
    });
    out
}

fn summarize_directory_for_prompt(unit: &OrganizeUnit, response_language: &str) -> String {
    let Some(hint) = unit.directory_hint.as_ref() else {
        return if is_zh_language(response_language) {
            "暂无目录摘要。".to_string()
        } else {
            "No directory summary available.".to_string()
        };
    };
    if is_zh_language(response_language) {
        [
            format!("相对路径={}", unit.relative_path),
            format!("总大小={}", unit.size),
            format!(
                "创建时间={}",
                unit.created_at
                    .clone()
                    .unwrap_or_else(|| organizer_unknown_label(response_language).to_string())
            ),
            format!(
                "修改时间={}",
                unit.modified_at
                    .clone()
                    .unwrap_or_else(|| organizer_unknown_label(response_language).to_string())
            ),
            format!("文件数={}", hint.file_count),
            format!("目录数={}", hint.dir_count),
            format!(
                "标记文件={}",
                if hint.marker_files.is_empty() {
                    organizer_none_label(response_language).to_string()
                } else {
                    hint.marker_files.join(", ")
                }
            ),
            format!(
                "应用特征={}",
                if hint.app_signals.is_empty() {
                    organizer_none_label(response_language).to_string()
                } else {
                    hint.app_signals.join(", ")
                }
            ),
            format!(
                "顶层条目={}",
                if hint.top_level_entries.is_empty() {
                    organizer_none_label(response_language).to_string()
                } else {
                    hint.top_level_entries.join(", ")
                }
            ),
            format!(
                "主要扩展名={}",
                if hint.dominant_extensions.is_empty() {
                    organizer_none_label(response_language).to_string()
                } else {
                    hint.dominant_extensions.join(", ")
                }
            ),
        ]
        .join("\n")
    } else {
        [
            format!("relativePath={}", unit.relative_path),
            format!("totalSize={}", unit.size),
            format!(
                "createdAt={}",
                unit.created_at
                    .clone()
                    .unwrap_or_else(|| organizer_unknown_label(response_language).to_string())
            ),
            format!(
                "modifiedAt={}",
                unit.modified_at
                    .clone()
                    .unwrap_or_else(|| organizer_unknown_label(response_language).to_string())
            ),
            format!("totalFiles={}", hint.file_count),
            format!("totalDirectories={}", hint.dir_count),
            format!(
                "markerFiles={}",
                if hint.marker_files.is_empty() {
                    organizer_none_label(response_language).to_string()
                } else {
                    hint.marker_files.join(", ")
                }
            ),
            format!(
                "appSignals={}",
                if hint.app_signals.is_empty() {
                    organizer_none_label(response_language).to_string()
                } else {
                    hint.app_signals.join(", ")
                }
            ),
            format!(
                "topLevelEntries={}",
                if hint.top_level_entries.is_empty() {
                    organizer_none_label(response_language).to_string()
                } else {
                    hint.top_level_entries.join(", ")
                }
            ),
            format!(
                "dominantExtensions={}",
                if hint.dominant_extensions.is_empty() {
                    organizer_none_label(response_language).to_string()
                } else {
                    hint.dominant_extensions.join(", ")
                }
            ),
        ]
        .join("\n")
    }
}

fn build_reference_structure_context(
    root: &Path,
    excluded: &[String],
    stop: &AtomicBool,
    response_language: &str,
) -> String {
    let mut lines = Vec::new();
    let mut total_dirs = 0_u64;
    let mut total_files = 0_u64;
    let mut truncated = false;
    let max_lines = 240_usize;
    let max_depth = 10_usize;

    let walker = WalkDir::new(root)
        .min_depth(1)
        .max_depth(max_depth)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            !should_exclude(&name, excluded)
        });

    for entry in walker.filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) {
            truncated = true;
            break;
        }
        if lines.len() >= max_lines {
            truncated = true;
            break;
        }

        let relative = entry
            .path()
            .strip_prefix(root)
            .unwrap_or_else(|_| entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        let depth = entry.depth().saturating_sub(1);
        let indent = "  ".repeat(depth);

        if entry.file_type().is_dir() {
            total_dirs = total_dirs.saturating_add(1);
            if is_zh_language(response_language) {
                lines.push(format!("{indent}[目录] {relative}/"));
            } else {
                lines.push(format!("{indent}[D] {relative}/"));
            }
            continue;
        }
        if entry.file_type().is_file() {
            total_files = total_files.saturating_add(1);
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            if is_zh_language(response_language) {
                lines.push(format!("{indent}[文件] {relative} ({size} bytes)"));
            } else {
                lines.push(format!("{indent}[F] {relative} ({size} bytes)"));
            }
        }
    }

    let mut out = if is_zh_language(response_language) {
        vec![
            format!("根路径={}", root.to_string_lossy()),
            format!("参考树最大深度={max_depth}"),
            format!("参考树展示行数={}", lines.len()),
            format!("参考树目录数={total_dirs}"),
            format!("参考树文件数={total_files}"),
            format!("参考树是否截断={truncated}"),
            "参考树开始".to_string(),
        ]
    } else {
        vec![
            format!("rootPath={}", root.to_string_lossy()),
            format!("referenceTreeMaxDepth={max_depth}"),
            format!("referenceTreeLinesShown={}", lines.len()),
            format!("referenceTreeDirectoriesShown={total_dirs}"),
            format!("referenceTreeFilesShown={total_files}"),
            format!("referenceTreeTruncated={truncated}"),
            "referenceTreeStart".to_string(),
        ]
    };
    out.extend(lines);
    out.push(if is_zh_language(response_language) {
        "参考树结束".to_string()
    } else {
        "referenceTreeEnd".to_string()
    });
    out.join("\n")
}

fn default_tree() -> CategoryTreeNode {
    CategoryTreeNode {
        node_id: "root".to_string(),
        name: String::new(),
        children: Vec::new(),
    }
}

fn sanitize_node_name(value: &str) -> String {
    let cleaned = value.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_");
    cleaned.trim().to_string()
}

fn category_path_display(path: &[String]) -> String {
    if path.is_empty() {
        UNCATEGORIZED_NODE_NAME.to_string()
    } else {
        path.join(" / ")
    }
}

fn category_tree_to_value(node: &CategoryTreeNode) -> Value {
    json!({
        "nodeId": node.node_id,
        "name": node.name,
        "children": node.children.iter().map(category_tree_to_value).collect::<Vec<_>>(),
    })
}

fn tree_from_value(value: &Value) -> CategoryTreeNode {
    fn parse_node(value: &Value) -> Option<CategoryTreeNode> {
        let node_id = value
            .get("nodeId")
            .and_then(Value::as_str)?
            .trim()
            .to_string();
        if node_id.is_empty() {
            return None;
        }
        Some(CategoryTreeNode {
            node_id,
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            children: value
                .get("children")
                .and_then(Value::as_array)
                .map(|children| children.iter().filter_map(parse_node).collect())
                .unwrap_or_default(),
        })
    }

    parse_node(value).unwrap_or_else(default_tree)
}

fn collect_existing_node_ids(node: &CategoryTreeNode, out: &mut HashSet<String>) {
    out.insert(node.node_id.clone());
    for child in &node.children {
        collect_existing_node_ids(child, out);
    }
}

fn normalize_ai_tree(value: &Value, current: &CategoryTreeNode) -> CategoryTreeNode {
    fn parse_node(
        value: &Value,
        existing_ids: &HashSet<String>,
        is_root: bool,
    ) -> Option<CategoryTreeNode> {
        let mut name = value
            .get("name")
            .and_then(Value::as_str)
            .map(sanitize_node_name)
            .unwrap_or_default();
        if !is_root && name.is_empty() {
            return None;
        }
        let provided_id = value
            .get("nodeId")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let node_id = if is_root {
            "root".to_string()
        } else if !provided_id.is_empty() && existing_ids.contains(provided_id) {
            provided_id.to_string()
        } else {
            Uuid::new_v4().to_string()
        };
        if is_root {
            name.clear();
        }
        Some(CategoryTreeNode {
            node_id,
            name,
            children: value
                .get("children")
                .and_then(Value::as_array)
                .map(|children| {
                    children
                        .iter()
                        .filter_map(|child| parse_node(child, existing_ids, false))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    let mut existing_ids = HashSet::new();
    collect_existing_node_ids(current, &mut existing_ids);
    value
        .get("tree")
        .and_then(|tree| parse_node(tree, &existing_ids, true))
        .unwrap_or_else(|| current.clone())
}

fn ensure_path(node: &mut CategoryTreeNode, path: &[String]) -> String {
    if path.is_empty() {
        return node.node_id.clone();
    }
    let name = sanitize_node_name(&path[0]);
    if name.is_empty() {
        return ensure_path(node, &path[1..]);
    }
    let idx = node
        .children
        .iter()
        .position(|child| child.name == name)
        .unwrap_or_else(|| {
            node.children.push(CategoryTreeNode {
                node_id: Uuid::new_v4().to_string(),
                name: name.clone(),
                children: Vec::new(),
            });
            node.children.len() - 1
        });
    ensure_path(&mut node.children[idx], &path[1..])
}

fn ensure_uncategorized_leaf(node: &mut CategoryTreeNode) -> String {
    ensure_path(node, &[UNCATEGORIZED_NODE_NAME.to_string()])
}

fn find_path_by_id(node: &CategoryTreeNode, target_id: &str, path: &mut Vec<String>) -> bool {
    if node.node_id == target_id {
        return true;
    }
    for child in &node.children {
        path.push(child.name.clone());
        if find_path_by_id(child, target_id, path) {
            return true;
        }
        path.pop();
    }
    false
}

fn category_path_for_id(node: &CategoryTreeNode, target_id: &str) -> Option<Vec<String>> {
    let mut path = Vec::new();
    if find_path_by_id(node, target_id, &mut path) {
        Some(path)
    } else {
        None
    }
}

fn category_path_from_value(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(sanitize_node_name)
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn summarize_unit_for_batch(
    unit: &OrganizeUnit,
    response_language: &str,
) -> (String, bool, Vec<String>) {
    match unit.item_type.as_str() {
        "directory" => (
            summarize_directory_for_prompt(unit, response_language),
            false,
            Vec::new(),
        ),
        _ if unit.modality == "text" => {
            let snippet = fs::read_to_string(&unit.path)
                .ok()
                .map(|content| content.chars().take(1500).collect::<String>())
                .unwrap_or_default();
            if snippet.trim().is_empty() {
                if is_zh_language(response_language) {
                    (
                        format!(
                            "名称={}\n相对路径={}\n大小={}\n创建时间={}\n修改时间={}",
                            unit.name,
                            unit.relative_path,
                            unit.size,
                            unit.created_at
                                .clone()
                                .unwrap_or_else(
                                    || organizer_unknown_label(response_language).to_string()
                                ),
                            unit.modified_at
                                .clone()
                                .unwrap_or_else(
                                    || organizer_unknown_label(response_language).to_string()
                                )
                        ),
                        true,
                        vec!["text_summary_fallback".to_string()],
                    )
                } else {
                    (
                        format!(
                            "name={}\nrelativePath={}\nsize={}\ncreatedAt={}\nmodifiedAt={}",
                            unit.name,
                            unit.relative_path,
                            unit.size,
                            unit.created_at
                                .clone()
                                .unwrap_or_else(
                                    || organizer_unknown_label(response_language).to_string()
                                ),
                            unit.modified_at
                                .clone()
                                .unwrap_or_else(
                                    || organizer_unknown_label(response_language).to_string()
                                )
                        ),
                        true,
                        vec!["text_summary_fallback".to_string()],
                    )
                }
            } else {
                (snippet, false, Vec::new())
            }
        }
        _ => {
            if is_zh_language(response_language) {
                (
                    format!(
                        "名称={}\n相对路径={}\n模态={}\n大小={}\n创建时间={}\n修改时间={}",
                        unit.name,
                        unit.relative_path,
                        unit.modality,
                        unit.size,
                        unit.created_at
                            .clone()
                            .unwrap_or_else(
                                || organizer_unknown_label(response_language).to_string()
                            ),
                        unit.modified_at
                            .clone()
                            .unwrap_or_else(
                                || organizer_unknown_label(response_language).to_string()
                            )
                    ),
                    true,
                    vec!["metadata_only_summary".to_string()],
                )
            } else {
                (
                    format!(
                        "name={}\nrelativePath={}\nmodality={}\nsize={}\ncreatedAt={}\nmodifiedAt={}",
                        unit.name,
                        unit.relative_path,
                        unit.modality,
                        unit.size,
                        unit.created_at
                            .clone()
                            .unwrap_or_else(|| organizer_unknown_label(response_language).to_string()),
                        unit.modified_at
                            .clone()
                            .unwrap_or_else(|| organizer_unknown_label(response_language).to_string())
                    ),
                    true,
                    vec!["metadata_only_summary".to_string()],
                )
            }
        }
    }
}

fn sanitize_json_block(content: &str) -> String {
    content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_string()
}

async fn chat_completion(
    route: &RouteConfig,
    system_prompt: &str,
    user_prompt: &str,
    stop: &AtomicBool,
) -> Result<(String, TokenUsage), String> {
    let url = format!("{}/chat/completions", route.endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&json!({
            "model": route.model,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": user_prompt }
            ],
            "temperature": 0
        }));
    if !route.api_key.is_empty() {
        req = req
            .header("Authorization", format!("Bearer {}", route.api_key))
            .header("x-api-key", route.api_key.clone())
            .header("api-key", route.api_key.clone());
    }
    let request_future = async move {
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(body
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("classification request failed")
                .to_string());
        }
        let content = body
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let usage = TokenUsage {
            prompt: body
                .pointer("/usage/prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            completion: body
                .pointer("/usage/completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            total: body
                .pointer("/usage/total_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        };
        Ok((content, usage))
    };
    tokio::pin!(request_future);

    loop {
        if stop.load(Ordering::Relaxed) {
            return Err("stop_requested".to_string());
        }
        tokio::select! {
            result = &mut request_future => return result,
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
    }
}

fn build_organize_system_prompt(response_language: &str, allow_web_search: bool) -> String {
    let output_language = localized_language_name(response_language, response_language);
    let mut lines = vec![
        "You cluster file summaries into a hierarchical category tree.".to_string(),
        "Return JSON only.".to_string(),
        "Final schema: {\"tree\":{...},\"assignments\":[{\"itemId\":\"...\",\"leafNodeId\":\"... optional\",\"categoryPath\":[\"...\"],\"reason\":\"...\"}]}".to_string(),
        "Existing nodes already have stable nodeId values; keep nodeId when you reuse, rename, or move existing nodes.".to_string(),
        format!("Use {output_language} names and keep labels short."),
        format!("The assignment \"reason\" field must be written in {output_language} only."),
    ];
    if allow_web_search {
        lines.push(
            "If local metadata is insufficient and external context is necessary, you may return {\"action\":\"web_search\",\"query\":\"...\",\"reason\":\"...\"} instead of the final schema. Use one concise query only."
                .to_string(),
        );
    }
    lines.join("\n")
}

async fn classify_organize_batch(
    text_route: &RouteConfig,
    response_language: &str,
    stop: &AtomicBool,
    existing_tree: &CategoryTreeNode,
    batch_rows: &[Value],
    max_cluster_depth: Option<u32>,
    reference_structure: Option<&String>,
    use_web_search: bool,
    search_api_key: &str,
) -> Result<(Value, TokenUsage, Value), String> {
    let search_allowed = use_web_search && !search_api_key.trim().is_empty();
    let mut total_usage = TokenUsage::default();
    let mut search_context = None::<String>;
    let mut search_trace = Value::Null;
    let max_rounds = if search_allowed { 2 } else { 1 };

    for _round in 0..max_rounds {
        let system_prompt = build_organize_system_prompt(
            response_language,
            search_allowed && search_context.is_none(),
        );
        let mut payload = json!({
            "maxClusterDepth": max_cluster_depth,
            "existingTree": category_tree_to_value(existing_tree),
            "items": batch_rows,
            "useWebSearch": use_web_search,
        });
        if let Some(structure) = reference_structure {
            payload["referenceStructure"] = Value::String(structure.clone());
        }
        if let Some(context) = search_context.as_ref() {
            payload["webSearchContext"] = Value::String(context.clone());
            payload["webSearchFollowup"] =
                Value::String("Return the final tree and assignments JSON only.".to_string());
        }

        let (content, usage) =
            chat_completion(text_route, &system_prompt, &payload.to_string(), stop).await?;
        total_usage.prompt = total_usage.prompt.saturating_add(usage.prompt);
        total_usage.completion = total_usage.completion.saturating_add(usage.completion);
        total_usage.total = total_usage.total.saturating_add(usage.total);

        let parsed = serde_json::from_str::<Value>(&sanitize_json_block(&content))
            .unwrap_or_else(|_| json!({}));
        if search_allowed && search_context.is_none() {
            if let Some(request) = parse_web_search_request(&parsed) {
                let trace = tavily_search(search_api_key, &request).await?;
                search_context = Some(format_web_search_context(&trace, response_language));
                search_trace = web_search_trace_to_value(&trace);
                continue;
            }
        }

        if parsed.get("tree").is_some() || parsed.get("assignments").is_some() {
            return Ok((parsed, total_usage, search_trace));
        }

        return Err("classification response is not valid JSON schema".to_string());
    }

    Err("model requested web search but did not return final batch assignments".to_string())
}

fn build_preview(root_path: &str, results: &[Value]) -> Vec<Value> {
    let mut used = HashSet::new();
    let mut out = Vec::new();
    for row in results {
        let category_path = category_path_from_value(row.get("categoryPath"));
        let mut target_dir = PathBuf::from(root_path);
        for segment in if category_path.is_empty() {
            vec![UNCATEGORIZED_NODE_NAME.to_string()]
        } else {
            category_path.clone()
        } {
            target_dir = target_dir.join(sanitize_node_name(&segment));
        }
        let mut target = target_dir.join(row.get("name").and_then(Value::as_str).unwrap_or(""));
        let mut suffix = 1_u32;
        while used.contains(&target.to_string_lossy().to_lowercase()) {
            let name = target
                .file_stem()
                .and_then(|x| x.to_str())
                .unwrap_or("file");
            let ext = target.extension().and_then(|x| x.to_str()).unwrap_or("");
            let next_name = if ext.is_empty() {
                format!("{name} ({suffix})")
            } else {
                format!("{name} ({suffix}).{ext}")
            };
            target = target_dir.join(next_name);
            suffix += 1;
        }
        used.insert(target.to_string_lossy().to_lowercase());
        out.push(json!({
            "sourcePath": row.get("path").and_then(Value::as_str).unwrap_or(""),
            "category": category_path_display(&category_path),
            "categoryPath": category_path,
            "leafNodeId": row.get("leafNodeId").and_then(Value::as_str).unwrap_or(""),
            "targetPath": target.to_string_lossy().to_string(),
            "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file")
        }));
    }
    out
}

fn normalize_path_key(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").to_lowercase()
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

fn next_conflict_target_path(target: &Path, target_dir: &Path, suffix: u32) -> PathBuf {
    let stem = target
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or("file");
    let ext = target.extension().and_then(|x| x.to_str()).unwrap_or("");
    let next_name = if ext.is_empty() {
        format!("{stem} ({suffix})")
    } else {
        format!("{stem} ({suffix}).{ext}")
    };
    target_dir.join(next_name)
}

fn resolve_apply_target_path(source: &Path, planned_target: &Path) -> PathBuf {
    if normalize_path_key(source) == normalize_path_key(planned_target) {
        return planned_target.to_path_buf();
    }

    let target_dir = planned_target
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut resolved = planned_target.to_path_buf();
    let mut suffix = 1_u32;
    while resolved.exists() {
        if normalize_path_key(source) == normalize_path_key(&resolved) {
            return resolved;
        }
        resolved = next_conflict_target_path(planned_target, &target_dir, suffix);
        suffix = suffix.saturating_add(1);
    }
    resolved
}

fn prune_empty_dirs_upward(start_dir: &Path, stop_dir: &Path) {
    let stop_key = normalize_path_key(stop_dir);
    let mut current = start_dir.to_path_buf();

    loop {
        if normalize_path_key(&current) == stop_key {
            break;
        }

        let Some(parent) = current.parent().map(Path::to_path_buf) else {
            break;
        };

        match fs::remove_dir(&current) {
            Ok(_) => {
                current = parent;
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::AlreadyExists
                ) =>
            {
                current = parent;
            }
            Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                break;
            }
            Err(_) => {
                break;
            }
        }
    }
}

fn build_apply_plan(snapshot: &OrganizeSnapshot) -> Vec<Value> {
    let preview_rows = if snapshot.preview.is_empty() {
        build_preview(&snapshot.root_path, &snapshot.results)
    } else {
        snapshot.preview.clone()
    };

    let mut plan = preview_rows
        .into_iter()
        .map(|entry| {
            let source_path = entry
                .get("sourcePath")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let item_type = entry
                .get("itemType")
                .and_then(Value::as_str)
                .unwrap_or("file")
                .to_string();
            let category = sanitize_category_name(
                entry
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or(CATEGORY_OTHER_PENDING),
            );
            let fallback_name = Path::new(&source_path)
                .file_name()
                .and_then(|x| x.to_str())
                .unwrap_or("item")
                .to_string();
            let fallback_target = PathBuf::from(&snapshot.root_path)
                .join(&category)
                .join(&fallback_name);
            let planned_target = entry
                .get("targetPath")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(fallback_target);

            json!({
                "sourcePath": source_path,
                "targetPath": planned_target.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
            })
        })
        .collect::<Vec<_>>();

    // Move deeper children first so parent-directory moves do not invalidate nested source paths.
    plan.sort_by(|left, right| {
        let left_source = Path::new(left.get("sourcePath").and_then(Value::as_str).unwrap_or(""));
        let right_source = Path::new(
            right
                .get("sourcePath")
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        path_depth(right_source)
            .cmp(&path_depth(left_source))
            .then_with(|| {
                let left_type = left
                    .get("itemType")
                    .and_then(Value::as_str)
                    .unwrap_or("file");
                let right_type = right
                    .get("itemType")
                    .and_then(Value::as_str)
                    .unwrap_or("file");
                left_type.cmp(right_type)
            })
            .then_with(|| normalize_path_key(left_source).cmp(&normalize_path_key(right_source)))
    });

    plan
}

async fn emit_snapshot<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<OrganizeTaskRuntime>,
) -> Result<(), String> {
    let snap = task.snapshot.lock().clone();
    persist::save_organize_snapshot(&state.db_path, &snap)?;
    app.emit(
        "organize_progress",
        serde_json::to_value(&snap).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

async fn run_organize_task<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<OrganizeTaskRuntime>,
) -> Result<(), String> {
    let (
        root_path,
        recursive,
        excluded,
        batch_size,
        max_cluster_depth,
        use_web_search,
        reference_original_structure,
    ) = {
        let snap = task.snapshot.lock();
        (
            snap.root_path.clone(),
            snap.recursive,
            snap.excluded_patterns.clone(),
            snap.batch_size,
            snap.max_cluster_depth,
            snap.use_web_search,
            snap.reference_original_structure,
        )
    };
    {
        let mut snap = task.snapshot.lock();
        snap.status = "scanning".to_string();
    }
    emit_snapshot(app, state, task).await?;

    let units = collect_units(Path::new(&root_path), recursive, &excluded, &task.stop);
    if task.stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let reference_structure = if reference_original_structure {
        Some(build_reference_structure_context(
            Path::new(&root_path),
            &excluded,
            &task.stop,
            &task.response_language,
        ))
    } else {
        None
    };
    if task.stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let total_batches = if units.is_empty() {
        0
    } else {
        ((units.len() as u64) + batch_size as u64 - 1) / batch_size as u64
    };
    {
        let mut snap = task.snapshot.lock();
        snap.status = "classifying".to_string();
        snap.total_files = units.len() as u64;
        snap.total_batches = total_batches;
        snap.processed_files = 0;
        snap.processed_batches = 0;
        snap.results.clear();
        snap.preview.clear();
    }
    emit_snapshot(app, state, task).await?;

    let mut tree = {
        let snap = task.snapshot.lock();
        tree_from_value(&snap.tree)
    };
    ensure_uncategorized_leaf(&mut tree);
    let text_route = task.routes.get("text").cloned().unwrap_or(RouteConfig {
        endpoint: "https://api.openai.com/v1".to_string(),
        api_key: String::new(),
        model: "gpt-4o-mini".to_string(),
    });

    for (batch_idx, batch) in units.chunks(batch_size as usize).enumerate() {
        if task.stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        let mut batch_rows = Vec::new();
        for (offset, unit) in batch.iter().enumerate() {
            let route_key = if unit.item_type == "directory" {
                "text"
            } else {
                unit.modality.as_str()
            };
            let route = task
                .routes
                .get(route_key)
                .or_else(|| task.routes.get("text"))
                .cloned()
                .unwrap_or(RouteConfig {
                    endpoint: "https://api.openai.com/v1".to_string(),
                    api_key: String::new(),
                    model: "gpt-4o-mini".to_string(),
                });
            let (summary, degraded, warnings) =
                summarize_unit_for_batch(unit, &task.response_language);
            batch_rows.push(json!({
                "itemId": format!("batch{}_{}", batch_idx + 1, offset + 1),
                "name": unit.name,
                "path": unit.path,
                "relativePath": unit.relative_path,
                "size": unit.size,
                "createdAt": unit.created_at,
                "modifiedAt": unit.modified_at,
                "itemType": unit.item_type,
                "modality": unit.modality,
                "summary": summary,
                "summaryDegraded": degraded,
                "summaryWarnings": warnings,
                "provider": route.endpoint,
                "model": route.model,
            }));
        }

        let mut cluster_usage = TokenUsage::default();
        let mut cluster_failed = false;
        let mut assignment_map: HashMap<String, (String, Vec<String>, String)> = HashMap::new();

        if !text_route.api_key.trim().is_empty() {
            match classify_organize_batch(
                &text_route,
                &task.response_language,
                &task.stop,
                &tree,
                &batch_rows,
                max_cluster_depth,
                reference_structure.as_ref(),
                use_web_search,
                &task.search_api_key,
            )
            .await
            {
                Ok((parsed, usage, _search_trace)) => {
                    cluster_usage = usage;
                    tree = normalize_ai_tree(&parsed, &tree);
                    ensure_uncategorized_leaf(&mut tree);
                    for assignment in parsed
                        .get("assignments")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                    {
                        let Some(item_id) = assignment.get("itemId").and_then(Value::as_str) else {
                            continue;
                        };
                        let mut category_path = category_path_from_value(
                            assignment
                                .get("categoryPath")
                                .or_else(|| assignment.get("leafPath")),
                        );
                        let leaf_node_id = if let Some(node_id) =
                            assignment.get("leafNodeId").and_then(Value::as_str)
                        {
                            if let Some(path) = category_path_for_id(&tree, node_id) {
                                category_path = path;
                                node_id.to_string()
                            } else if !category_path.is_empty() {
                                ensure_path(&mut tree, &category_path)
                            } else {
                                ensure_uncategorized_leaf(&mut tree)
                            }
                        } else if !category_path.is_empty() {
                            ensure_path(&mut tree, &category_path)
                        } else {
                            ensure_uncategorized_leaf(&mut tree)
                        };
                        if category_path.is_empty() {
                            category_path = category_path_for_id(&tree, &leaf_node_id)
                                .unwrap_or_else(|| vec![UNCATEGORIZED_NODE_NAME.to_string()]);
                        }
                        assignment_map.insert(
                            item_id.to_string(),
                            (
                                leaf_node_id,
                                category_path,
                                assignment
                                    .get("reason")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                            ),
                        );
                    }
                }
                Err(_) => {
                    cluster_failed = true;
                }
            }
        } else if false {
            cluster_failed = true;
        }

        if false && !text_route.api_key.trim().is_empty() {
            let system_prompt = if is_zh_language(&task.response_language) {
                format!(
                    "你需要把一批文件摘要聚成一个分层分类树。只能返回 JSON，输出结构为 {{\"tree\":{{...}},\"assignments\":[{{\"itemId\":\"...\",\"leafNodeId\":\"... optional\",\"categoryPath\":[\"...\"],\"reason\":\"...\"}}]}}。现有节点已经有稳定的 nodeId；当你复用、重命名或移动已有节点时，必须保留原 nodeId。分类名称请使用{}，并保持简短。",
                    localized_language_name(&task.response_language, &task.response_language)
                )
            } else {
                format!(
                    "You cluster file summaries into a hierarchical category tree. Return JSON only with schema {{\"tree\":{{...}},\"assignments\":[{{\"itemId\":\"...\",\"leafNodeId\":\"... optional\",\"categoryPath\":[\"...\"],\"reason\":\"...\"}}]}}. Existing nodes already have stable nodeId values; keep nodeId when you reuse, rename, or move existing nodes. Use {} names and keep labels short.",
                    localized_language_name(&task.response_language, &task.response_language)
                )
            };
            let mut payload = json!({
                "maxClusterDepth": max_cluster_depth,
                "existingTree": category_tree_to_value(&tree),
                "items": batch_rows,
                "useWebSearch": use_web_search,
            });
            if let Some(structure) = reference_structure.as_ref() {
                payload["referenceStructure"] = Value::String(structure.clone());
            }
            match chat_completion(
                &text_route,
                &system_prompt,
                &payload.to_string(),
                &task.stop,
            )
            .await
            {
                Ok((content, usage)) => {
                    cluster_usage = usage;
                    if let Ok(parsed) =
                        serde_json::from_str::<Value>(&sanitize_json_block(&content))
                    {
                        tree = normalize_ai_tree(&parsed, &tree);
                        ensure_uncategorized_leaf(&mut tree);
                        for assignment in parsed
                            .get("assignments")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                        {
                            let Some(item_id) = assignment.get("itemId").and_then(Value::as_str)
                            else {
                                continue;
                            };
                            let mut category_path = category_path_from_value(
                                assignment
                                    .get("categoryPath")
                                    .or_else(|| assignment.get("leafPath")),
                            );
                            let leaf_node_id = if let Some(node_id) =
                                assignment.get("leafNodeId").and_then(Value::as_str)
                            {
                                if let Some(path) = category_path_for_id(&tree, node_id) {
                                    category_path = path;
                                    node_id.to_string()
                                } else if !category_path.is_empty() {
                                    ensure_path(&mut tree, &category_path)
                                } else {
                                    ensure_uncategorized_leaf(&mut tree)
                                }
                            } else if !category_path.is_empty() {
                                ensure_path(&mut tree, &category_path)
                            } else {
                                ensure_uncategorized_leaf(&mut tree)
                            };
                            if category_path.is_empty() {
                                category_path = category_path_for_id(&tree, &leaf_node_id)
                                    .unwrap_or_else(|| vec![UNCATEGORIZED_NODE_NAME.to_string()]);
                            }
                            assignment_map.insert(
                                item_id.to_string(),
                                (
                                    leaf_node_id,
                                    category_path,
                                    assignment
                                        .get("reason")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string(),
                                ),
                            );
                        }
                    } else {
                        cluster_failed = true;
                    }
                }
                Err(_) => {
                    cluster_failed = true;
                }
            }
        } else {
            cluster_failed = true;
        }
        if task.stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        for row in batch_rows {
            if task.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let item_id = row.get("itemId").and_then(Value::as_str).unwrap_or("");
            let (leaf_node_id, category_path, reason) =
                assignment_map.get(item_id).cloned().unwrap_or_else(|| {
                    let leaf = ensure_uncategorized_leaf(&mut tree);
                    let path = category_path_for_id(&tree, &leaf)
                        .unwrap_or_else(|| vec![UNCATEGORIZED_NODE_NAME.to_string()]);
                    (leaf, path, "fallback_uncategorized".to_string())
                });
            let warnings = row
                .get("summaryWarnings")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let next_index = {
                let snap = task.snapshot.lock();
                snap.processed_files + 1
            };
            let result_row = json!({
                "taskId": task.snapshot.lock().id,
                "index": next_index,
                "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
                "path": row.get("path").and_then(Value::as_str).unwrap_or(""),
                "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
                "size": row.get("size").and_then(Value::as_u64).unwrap_or(0),
                "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
                "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
                "summary": row.get("summary").and_then(Value::as_str).unwrap_or(""),
                "leafNodeId": leaf_node_id,
                "categoryPath": category_path,
                "category": category_path_display(&category_path),
                "reason": reason,
                "degraded": cluster_failed || row.get("summaryDegraded").and_then(Value::as_bool).unwrap_or(false),
                "warnings": warnings,
                "provider": row.get("provider").and_then(Value::as_str).unwrap_or(""),
                "model": row.get("model").and_then(Value::as_str).unwrap_or(""),
            });
            persist::upsert_organize_result(&state.db_path, &task.snapshot.lock().id, &result_row)?;
            {
                let mut snap = task.snapshot.lock();
                snap.results.push(result_row.clone());
                snap.processed_files = snap.processed_files.saturating_add(1);
            }
            app.emit("organize_file_done", result_row)
                .map_err(|e| e.to_string())?;
        }

        {
            let mut snap = task.snapshot.lock();
            snap.tree = category_tree_to_value(&tree);
            snap.tree_version = snap.tree_version.saturating_add(1);
            snap.processed_batches = (batch_idx + 1) as u64;
            snap.token_usage.prompt = snap.token_usage.prompt.saturating_add(cluster_usage.prompt);
            snap.token_usage.completion = snap
                .token_usage
                .completion
                .saturating_add(cluster_usage.completion);
            snap.token_usage.total = snap.token_usage.total.saturating_add(cluster_usage.total);
        }
        emit_snapshot(app, state, task).await?;
    }

    let final_snapshot = {
        let mut snap = task.snapshot.lock();
        snap.results
            .sort_by_key(|x| x.get("index").and_then(Value::as_u64).unwrap_or(0));
        snap.preview = build_preview(&snap.root_path, &snap.results);
        snap.tree = category_tree_to_value(&tree);
        snap.status = "completed".to_string();
        snap.completed_at = Some(now_iso());
        snap.clone()
    };
    persist::save_organize_snapshot(&state.db_path, &final_snapshot)?;
    persist::save_latest_organize_tree(
        &state.db_path,
        &final_snapshot.root_path,
        &final_snapshot.tree,
        final_snapshot.tree_version,
    )?;
    app.emit(
        "organize_done",
        serde_json::to_value(final_snapshot).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn organize_get_capability(state: State<'_, AppState>) -> Result<Value, String> {
    let settings = crate::backend::read_settings(&state.settings_path);
    let (endpoint, model) =
        crate::backend::resolve_provider_endpoint_and_model(state.inner(), None, None);
    Ok(json!({
        "selectedModel": model,
        "selectedModels": { "text": model, "image": model, "video": model, "audio": model },
        "selectedProviders": { "text": endpoint, "image": endpoint, "video": endpoint, "audio": endpoint },
        "supportsMultimodal": supports_multimodal(&model, &endpoint),
        "useWebSearch": settings.pointer("/searchApi/scopes/organizer").and_then(Value::as_bool).unwrap_or(false),
        "webSearchEnabled": settings.pointer("/searchApi/enabled").and_then(Value::as_bool).unwrap_or(false),
    }))
}

pub async fn organize_start<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    input: OrganizeStartInput,
) -> Result<Value, String> {
    if input.root_path.trim().is_empty() {
        return Err("rootPath is required".to_string());
    }
    let task_id = format!("org_{}", Uuid::new_v4().simple());
    let routes = parse_routes(&input.model_routing);
    let text_route = routes.get("text").cloned().unwrap_or(RouteConfig {
        endpoint: "https://api.openai.com/v1".to_string(),
        api_key: String::new(),
        model: "gpt-4o-mini".to_string(),
    });
    let (tree, tree_version) =
        persist::load_latest_organize_tree(&state.db_path, &input.root_path)?
            .unwrap_or_else(|| (category_tree_to_value(&default_tree()), 0));
    let snapshot = OrganizeSnapshot {
        id: task_id.clone(),
        status: "idle".to_string(),
        error: None,
        root_path: input.root_path.clone(),
        recursive: true,
        reference_original_structure: input.reference_original_structure.unwrap_or(false),
        excluded_patterns: normalize_excluded(input.excluded_patterns.clone()),
        batch_size: normalize_batch_size(input.batch_size),
        max_cluster_depth: input.max_cluster_depth.filter(|value| *value > 0),
        use_web_search: input.use_web_search.unwrap_or(false),
        web_search_enabled: input.use_web_search.unwrap_or(false)
            && input.search_api_key.as_deref().unwrap_or("").trim().len() > 0,
        selected_model: text_route.model.clone(),
        selected_models: json!({
            "text": routes.get("text").map(|x| x.model.clone()).unwrap_or_else(|| text_route.model.clone()),
            "image": routes.get("image").map(|x| x.model.clone()).unwrap_or_else(|| text_route.model.clone()),
            "video": routes.get("video").map(|x| x.model.clone()).unwrap_or_else(|| text_route.model.clone()),
            "audio": routes.get("audio").map(|x| x.model.clone()).unwrap_or_else(|| text_route.model.clone()),
        }),
        selected_providers: json!({
            "text": routes.get("text").map(|x| x.endpoint.clone()).unwrap_or_else(|| text_route.endpoint.clone()),
            "image": routes.get("image").map(|x| x.endpoint.clone()).unwrap_or_else(|| text_route.endpoint.clone()),
            "video": routes.get("video").map(|x| x.endpoint.clone()).unwrap_or_else(|| text_route.endpoint.clone()),
            "audio": routes.get("audio").map(|x| x.endpoint.clone()).unwrap_or_else(|| text_route.endpoint.clone()),
        }),
        supports_multimodal: supports_multimodal(&text_route.model, &text_route.endpoint),
        tree,
        tree_version,
        total_files: 0,
        processed_files: 0,
        total_batches: 0,
        processed_batches: 0,
        token_usage: TokenUsage::default(),
        results: Vec::new(),
        preview: Vec::new(),
        created_at: now_iso(),
        completed_at: None,
        job_id: None,
    };
    persist::init_organize_task(&state.db_path, &snapshot)?;
    let task = Arc::new(OrganizeTaskRuntime {
        stop: AtomicBool::new(false),
        snapshot: Mutex::new(snapshot.clone()),
        routes,
        search_api_key: input.search_api_key.unwrap_or_default(),
        response_language: input.response_language.unwrap_or_else(|| "zh".to_string()),
        job: Mutex::new(None),
    });
    state
        .organize_tasks
        .lock()
        .insert(task_id.clone(), task.clone());
    let state_clone = state.inner().clone();
    let task_id_clone = task_id.clone();
    let app_clone = app.clone();
    let runtime = task.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let result = run_organize_task(&app_clone, &state_clone, &runtime).await;
        if runtime.stop.load(Ordering::Relaxed) {
            let mut snap = runtime.snapshot.lock();
            snap.status = "stopped".to_string();
            snap.completed_at = Some(now_iso());
            let _ = persist::save_organize_snapshot(&state_clone.db_path, &snap);
            let payload = serde_json::to_value(&*snap).unwrap_or_else(|_| json!({}));
            drop(snap);
            let _ = app_clone.emit("organize_stopped", payload);
        } else if let Err(err) = result {
            let mut snap = runtime.snapshot.lock();
            snap.status = "error".to_string();
            snap.error = Some(err.clone());
            snap.completed_at = Some(now_iso());
            let _ = persist::save_organize_snapshot(&state_clone.db_path, &snap);
            let payload = json!({ "taskId": task_id_clone, "message": err, "snapshot": &*snap });
            drop(snap);
            let _ = app_clone.emit("organize_error", payload);
        }
        state_clone.organize_tasks.lock().remove(&task_id_clone);
    });
    *task.job.lock() = Some(handle);
    Ok(json!({
        "taskId": task_id,
        "selectedModel": snapshot.selected_model,
        "selectedModels": snapshot.selected_models,
        "selectedProviders": snapshot.selected_providers,
        "supportsMultimodal": snapshot.supports_multimodal
    }))
}

pub async fn organize_stop(state: State<'_, AppState>, task_id: String) -> Result<Value, String> {
    let task = state
        .organize_tasks
        .lock()
        .get(&task_id)
        .cloned()
        .ok_or_else(|| "Task not found".to_string())?;
    task.stop.store(true, Ordering::Relaxed);
    Ok(json!({ "success": true }))
}

pub async fn organize_get_result(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Value, String> {
    if let Some(task) = state.organize_tasks.lock().get(&task_id).cloned() {
        let snap = task.snapshot.lock().clone();
        return serde_json::to_value(snap).map_err(|e| e.to_string());
    }
    let snapshot = persist::load_organize_snapshot(&state.db_path, &task_id)?
        .ok_or_else(|| "Task not found".to_string())?;
    serde_json::to_value(snapshot).map_err(|e| e.to_string())
}

pub async fn organize_apply(state: State<'_, AppState>, task_id: String) -> Result<Value, String> {
    let mut snapshot = persist::load_organize_snapshot(&state.db_path, &task_id)?
        .ok_or_else(|| "Task not found".to_string())?;
    if snapshot.status != "completed" && snapshot.status != "done" {
        return Err(format!(
            "task status is {}, cannot apply move",
            snapshot.status
        ));
    }
    snapshot.status = "moving".to_string();
    persist::save_organize_snapshot(&state.db_path, &snapshot)?;

    let plan = build_apply_plan(&snapshot);
    let mut entries = Vec::new();
    for row in &plan {
        let source = PathBuf::from(row.get("sourcePath").and_then(Value::as_str).unwrap_or(""));
        let item_type = row
            .get("itemType")
            .and_then(Value::as_str)
            .unwrap_or("file")
            .to_string();
        let category = sanitize_category_name(
            row.get("category")
                .and_then(Value::as_str)
                .unwrap_or(CATEGORY_OTHER_PENDING),
        );
        let planned_target =
            PathBuf::from(row.get("targetPath").and_then(Value::as_str).unwrap_or(""));
        let target_base = if planned_target.as_os_str().is_empty() {
            let fallback_name = source
                .file_name()
                .and_then(|x| x.to_str())
                .unwrap_or("item");
            PathBuf::from(&snapshot.root_path)
                .join(&category)
                .join(fallback_name)
        } else {
            planned_target
        };
        let target_dir = target_base
            .parent()
            .unwrap_or_else(|| Path::new(&snapshot.root_path))
            .to_path_buf();
        if !source.exists() {
            entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target_base.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "failed",
                "error": "source_not_found"
            }));
            continue;
        }
        if let Err(err) = fs::create_dir_all(&target_dir) {
            entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target_base.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "failed",
                "error": err.to_string()
            }));
            continue;
        }
        let target = resolve_apply_target_path(&source, &target_base);
        if normalize_path_key(&source) == normalize_path_key(&target) {
            entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "skipped",
                "error": Value::Null
            }));
            continue;
        }
        match fs::rename(&source, &target) {
            Ok(_) => entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "moved",
                "error": Value::Null
            })),
            Err(err) => entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "failed",
                "error": err.to_string()
            })),
        }
    }
    let moved = entries
        .iter()
        .filter(|x| x.get("status").and_then(Value::as_str) == Some("moved"))
        .count();
    let skipped = entries
        .iter()
        .filter(|x| x.get("status").and_then(Value::as_str) == Some("skipped"))
        .count();
    let failed = entries
        .iter()
        .filter(|x| x.get("status").and_then(Value::as_str) == Some("failed"))
        .count();
    let job_id = format!("job_{}", Uuid::new_v4().simple());
    let manifest = json!({
        "jobId": job_id,
        "taskId": task_id,
        "rootPath": snapshot.root_path,
        "createdAt": now_iso(),
        "batchSize": snapshot.batch_size,
        "maxClusterDepth": snapshot.max_cluster_depth,
        "recursive": snapshot.recursive,
        "referenceOriginalStructure": snapshot.reference_original_structure,
        "entries": entries,
        "summary": {
            "moved": moved,
            "skipped": skipped,
            "failed": failed,
            "total": entries.len()
        }
    });
    persist::save_organize_manifest(&state.db_path, &manifest)?;
    snapshot.status = "done".to_string();
    snapshot.job_id = manifest
        .get("jobId")
        .and_then(Value::as_str)
        .map(|x| x.to_string());
    persist::save_organize_snapshot(&state.db_path, &snapshot)?;
    if let Some(task) = state.organize_tasks.lock().get(&task_id).cloned() {
        *task.snapshot.lock() = snapshot;
    }
    Ok(json!({ "success": true, "manifest": manifest }))
}

pub async fn organize_rollback(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<Value, String> {
    let manifest = persist::load_organize_job(&state.db_path, &job_id)?
        .ok_or_else(|| "job manifest not found".to_string())?;
    let task_id = manifest
        .get("taskId")
        .and_then(Value::as_str)
        .map(|value| value.to_string());
    let root_path = PathBuf::from(
        manifest
            .get("rootPath")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let mut entries = persist::load_organize_job_entries(&state.db_path, &job_id)?;
    entries.reverse();
    let mut rollback_entries = Vec::new();
    for entry in entries {
        let item_type = entry
            .get("itemType")
            .and_then(Value::as_str)
            .unwrap_or("file")
            .to_string();
        let source = PathBuf::from(
            entry
                .get("sourcePath")
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        let target = PathBuf::from(
            entry
                .get("targetPath")
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        if entry.get("status").and_then(Value::as_str) != Some("moved") {
            rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "status": "skipped",
                "error": "not_moved_in_apply"
            }));
            continue;
        }
        if !target.exists() {
            rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "status": "failed",
                "error": "target_not_found"
            }));
            continue;
        }
        if source.exists() {
            rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "status": "failed",
                "error": "source_already_exists"
            }));
            continue;
        }
        if let Some(parent) = source.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match fs::rename(&target, &source) {
            Ok(_) => {
                if let Some(target_parent) = target.parent() {
                    prune_empty_dirs_upward(target_parent, &root_path);
                }
                rollback_entries.push(json!({
                    "sourcePath": source.to_string_lossy().to_string(),
                    "targetPath": target.to_string_lossy().to_string(),
                    "itemType": item_type,
                    "status": "rolled_back",
                    "error": Value::Null
                }))
            }
            Err(err) => rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "status": "failed",
                "error": err.to_string()
            })),
        }
    }
    let rollback = json!({
        "at": now_iso(),
        "entries": rollback_entries,
        "summary": {
            "rolledBack": rollback_entries.iter().filter(|x| x.get("status").and_then(Value::as_str) == Some("rolled_back")).count(),
            "failed": rollback_entries.iter().filter(|x| x.get("status").and_then(Value::as_str) == Some("failed")).count(),
            "skipped": rollback_entries.iter().filter(|x| x.get("status").and_then(Value::as_str) == Some("skipped")).count(),
            "total": rollback_entries.len()
        }
    });
    persist::save_organize_rollback(&state.db_path, &job_id, &rollback)?;

    let failed = rollback
        .get("summary")
        .and_then(|summary| summary.get("failed"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if failed == 0 {
        if let Some(task_id) = task_id {
            if let Some(mut snapshot) = persist::load_organize_snapshot(&state.db_path, &task_id)? {
                snapshot.status = "completed".to_string();
                snapshot.job_id = None;
                persist::save_organize_snapshot(&state.db_path, &snapshot)?;
                if let Some(task) = state.organize_tasks.lock().get(&task_id).cloned() {
                    *task.snapshot.lock() = snapshot;
                }
            }
        }
    }

    Ok(json!({
        "success": true,
        "jobId": manifest.get("jobId").and_then(Value::as_str).unwrap_or(&job_id),
        "rollback": rollback
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_path_creates_nested_tree() {
        let mut tree = default_tree();
        let leaf = ensure_path(&mut tree, &["group".to_string(), "leaf".to_string()]);
        let path = category_path_for_id(&tree, &leaf).expect("path");
        assert_eq!(path, vec!["group".to_string(), "leaf".to_string()]);
    }

    #[test]
    fn build_preview_uses_nested_category_path() {
        let preview = build_preview(
            r"C:\root",
            &[json!({
                "name": "foo.txt",
                "path": r"C:\root\foo.txt",
                "itemType": "file",
                "leafNodeId": "leaf",
                "categoryPath": ["group", "leaf"]
            })],
        );
        assert_eq!(
            preview[0].get("targetPath").and_then(Value::as_str),
            Some(r"C:\root\group\leaf\foo.txt")
        );
    }
}
