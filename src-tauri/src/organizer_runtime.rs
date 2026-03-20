use crate::backend::{
    AppState, OrganizeSnapshot, OrganizeStartInput, OrganizeSuggestInput, TokenUsage,
};
use crate::persist;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Runtime, State};
use uuid::Uuid;
use walkdir::WalkDir;

const CATEGORY_WORK_STUDY: &str = "\u{5DE5}\u{4F5C}\u{5B66}\u{4E60}";
const CATEGORY_FINANCE_RECEIPTS: &str = "\u{8D22}\u{52A1}\u{7968}\u{636E}";
const CATEGORY_MEDIA_ASSETS: &str = "\u{5A92}\u{4F53}\u{7D20}\u{6750}";
const CATEGORY_DEV_PROJECTS: &str = "\u{5F00}\u{53D1}\u{9879}\u{76EE}";
const CATEGORY_INSTALL_ARCHIVES: &str = "\u{5B89}\u{88C5}\u{4E0E}\u{538B}\u{7F29}";
const CATEGORY_TEMP_DOWNLOADS: &str = "\u{4E34}\u{65F6}\u{4E0B}\u{8F7D}";
const CATEGORY_OTHER_PENDING: &str = "\u{5176}\u{4ED6}\u{5F85}\u{5B9A}";

const DEFAULT_CATEGORY_LIST: [&str; 7] = [
    CATEGORY_WORK_STUDY,
    CATEGORY_FINANCE_RECEIPTS,
    CATEGORY_MEDIA_ASSETS,
    CATEGORY_DEV_PROJECTS,
    CATEGORY_INSTALL_ARCHIVES,
    CATEGORY_TEMP_DOWNLOADS,
    CATEGORY_OTHER_PENDING,
];

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
    modified_at: Option<String>,
    item_type: String,
    modality: String,
    directory_hint: Option<DirectoryHint>,
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
    search_api_key: Option<String>,
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

fn prompt_language_name(value: &str) -> &'static str {
    if is_zh_language(value) {
        "Simplified Chinese"
    } else {
        "English"
    }
}

fn normalize_categories(categories: Option<Vec<String>>) -> Vec<String> {
    let mut out = categories
        .unwrap_or_default()
        .into_iter()
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>();
    if out.is_empty() {
        out = DEFAULT_CATEGORY_LIST
            .iter()
            .map(|x| (*x).to_string())
            .collect();
    }
    if !out.iter().any(|x| x == CATEGORY_OTHER_PENDING) {
        out.push(CATEGORY_OTHER_PENDING.to_string());
    }
    out
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

fn fallback_category(name: &str, categories: &[String]) -> String {
    let lower = name.to_lowercase();
    let pick = |value: &str| {
        if categories.iter().any(|x| x == value) {
            value.to_string()
        } else {
            CATEGORY_OTHER_PENDING.to_string()
        }
    };
    if [
        ".jpg", ".jpeg", ".png", ".gif", ".webp", ".mp4", ".mov", ".mp3", ".wav",
    ]
    .iter()
    .any(|x| lower.ends_with(x))
    {
        return pick(CATEGORY_MEDIA_ASSETS);
    }
    if [".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".pdf"]
        .iter()
        .any(|x| lower.ends_with(x))
    {
        return pick(CATEGORY_WORK_STUDY);
    }
    if [".zip", ".rar", ".7z", ".msi", ".exe", ".dmg"]
        .iter()
        .any(|x| lower.ends_with(x))
    {
        return pick(CATEGORY_INSTALL_ARCHIVES);
    }
    if [".js", ".ts", ".py", ".java", ".go", ".rs", ".cpp"]
        .iter()
        .any(|x| lower.ends_with(x))
    {
        return pick(CATEGORY_DEV_PROJECTS);
    }
    CATEGORY_OTHER_PENDING.to_string()
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

fn normalize_candidate_category(
    candidate: &str,
    categories: &[String],
    allow_new_categories: bool,
) -> (String, bool) {
    let candidate = candidate.trim();
    let normalized = if categories.iter().any(|x| x == candidate) {
        candidate.to_string()
    } else if allow_new_categories && !candidate.is_empty() {
        candidate.to_string()
    } else {
        CATEGORY_OTHER_PENDING.to_string()
    };
    let created =
        normalized != CATEGORY_OTHER_PENDING && !categories.iter().any(|x| x == &normalized);
    (normalized, created)
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

fn summarize_directory_for_prompt(unit: &OrganizeUnit) -> String {
    let Some(hint) = unit.directory_hint.as_ref() else {
        return "No directory summary available.".to_string();
    };
    [
        format!("relativePath={}", unit.relative_path),
        format!("totalSize={}", unit.size),
        format!(
            "modifiedAt={}",
            unit.modified_at
                .clone()
                .unwrap_or_else(|| "(unknown)".to_string())
        ),
        format!("totalFiles={}", hint.file_count),
        format!("totalDirectories={}", hint.dir_count),
        format!(
            "markerFiles={}",
            if hint.marker_files.is_empty() {
                "(none)".to_string()
            } else {
                hint.marker_files.join(", ")
            }
        ),
        format!(
            "appSignals={}",
            if hint.app_signals.is_empty() {
                "(none)".to_string()
            } else {
                hint.app_signals.join(", ")
            }
        ),
        format!(
            "topLevelEntries={}",
            if hint.top_level_entries.is_empty() {
                "(none)".to_string()
            } else {
                hint.top_level_entries.join(", ")
            }
        ),
        format!(
            "dominantExtensions={}",
            if hint.dominant_extensions.is_empty() {
                "(none)".to_string()
            } else {
                hint.dominant_extensions.join(", ")
            }
        ),
    ]
    .join("\n")
}

fn build_reference_structure_context(
    root: &Path,
    excluded: &[String],
    stop: &AtomicBool,
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
            lines.push(format!("{indent}[D] {relative}/"));
            continue;
        }
        if entry.file_type().is_file() {
            total_files = total_files.saturating_add(1);
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            lines.push(format!("{indent}[F] {relative} ({size} bytes)"));
        }
    }

    let mut out = vec![
        format!("rootPath={}", root.to_string_lossy()),
        format!("referenceTreeMaxDepth={max_depth}"),
        format!("referenceTreeLinesShown={}", lines.len()),
        format!("referenceTreeDirectoriesShown={total_dirs}"),
        format!("referenceTreeFilesShown={total_files}"),
        format!("referenceTreeTruncated={truncated}"),
        "referenceTreeStart".to_string(),
    ];
    out.extend(lines);
    out.push("referenceTreeEnd".to_string());
    out.join("\n")
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

fn parse_category_response(
    content: &str,
    categories: &[String],
    allow_new_categories: bool,
) -> (String, bool) {
    let parsed: Value =
        serde_json::from_str(&sanitize_json_block(content)).unwrap_or_else(|_| json!({}));
    let candidate = parsed
        .get("category")
        .and_then(Value::as_str)
        .unwrap_or(CATEGORY_OTHER_PENDING);
    normalize_candidate_category(candidate, categories, allow_new_categories)
}

fn estimate_unit_total_size(unit: &OrganizeUnit) -> u64 {
    if unit.item_type == "directory" {
        unit.directory_hint
            .as_ref()
            .map(|hint| hint.total_size)
            .unwrap_or(unit.size)
    } else {
        unit.size
    }
}

async fn chat_completion(
    route: &RouteConfig,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<(String, TokenUsage), String> {
    let url = format!("{}/chat/completions", route.endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
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
}

async fn tavily_search(api_key: &str, query: &str) -> Result<Option<String>, String> {
    if api_key.trim().is_empty() {
        return Ok(None);
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post("https://api.tavily.com/search")
        .json(&json!({
            "api_key": api_key,
            "query": format!("What is this file or software used for: \"{}\"? Give a short technical summary.", query),
            "search_depth": "basic",
            "include_answer": true,
            "max_results": 3
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(body
        .get("answer")
        .and_then(Value::as_str)
        .map(|x| x.to_string()))
}

async fn classify_file(
    route: &RouteConfig,
    file: &OrganizeUnit,
    categories: &[String],
    allow_new_categories: bool,
    use_web_search: bool,
    search_api_key: Option<&str>,
    reference_structure: Option<&str>,
    response_language: &str,
) -> (String, bool, bool, Vec<String>, TokenUsage) {
    if route.api_key.trim().is_empty() {
        return (
            fallback_category(&file.name, categories),
            false,
            true,
            vec!["missing_api_key_fallback".to_string()],
            TokenUsage::default(),
        );
    }
    let system_prompt = format!(
        "You classify one file into one category. Return JSON only. Schema: {{\"category\":\"...\"}}. Preferred categories: {}. {} If unsure choose {}. If you create a new category, its name must be in {}. If original structure context is provided, use it only as supporting context to preserve grouping intent while still classifying the current item only.",
        categories.join(" | "),
        if allow_new_categories { "You may create one short new category if none fits." } else { "You must choose from the preferred categories." },
        CATEGORY_OTHER_PENDING,
        prompt_language_name(response_language),
    );
    let mut user_prompt = format!(
        "name={}\nrelativePath={}\nsize={}\nmodifiedAt={}\nmodality={}\nChoose the best category.",
        file.name,
        file.relative_path,
        estimate_unit_total_size(file),
        file.modified_at
            .clone()
            .unwrap_or_else(|| "(unknown)".to_string()),
        file.modality
    );
    if file.modality == "text" {
        if let Ok(text) = fs::read_to_string(&file.path) {
            let snippet = text.chars().take(4000).collect::<String>();
            if !snippet.trim().is_empty() {
                user_prompt.push_str("\ncontentStart\n");
                user_prompt.push_str(&snippet);
                user_prompt.push_str("\ncontentEnd");
            }
        }
    }
    if let Some(structure) = reference_structure.filter(|value| !value.trim().is_empty()) {
        user_prompt.push_str("\noriginalStructureContext\n");
        user_prompt.push_str(structure);
        user_prompt.push_str(
            "\nUse the original structure only as context. Do not classify siblings; classify the current file only.\noriginalStructureContextEnd",
        );
    }
    let mut warnings = Vec::new();
    let mut total_usage = TokenUsage::default();
    let (mut category, mut created) =
        match chat_completion(route, &system_prompt, &user_prompt).await {
            Ok((content, usage)) => {
                total_usage = usage;
                parse_category_response(&content, categories, allow_new_categories)
            }
            Err(err) => {
                warnings.push(format!("classify_failed:{err}"));
                return (
                    fallback_category(&file.name, categories),
                    false,
                    true,
                    warnings,
                    total_usage,
                );
            }
        };
    if use_web_search && category == CATEGORY_OTHER_PENDING {
        if let Some(api_key) = search_api_key {
            if let Ok(Some(context)) = tavily_search(api_key, &file.name).await {
                let second_prompt = format!(
                    "{}\nwebSearchContext={}",
                    user_prompt,
                    context.chars().take(2000).collect::<String>()
                );
                if let Ok((content, usage)) =
                    chat_completion(route, &system_prompt, &second_prompt).await
                {
                    total_usage.prompt += usage.prompt;
                    total_usage.completion += usage.completion;
                    total_usage.total += usage.total;
                    let parsed =
                        parse_category_response(&content, categories, allow_new_categories);
                    category = parsed.0;
                    created = parsed.1;
                    warnings.push("web_search_refined".to_string());
                }
            }
        }
    }
    (category, created, false, warnings, total_usage)
}

async fn classify_directory(
    route: &RouteConfig,
    unit: &OrganizeUnit,
    categories: &[String],
    allow_new_categories: bool,
    reference_structure: Option<&str>,
    response_language: &str,
) -> (String, bool, bool, Vec<String>, TokenUsage) {
    if route.api_key.trim().is_empty() {
        return (
            fallback_category(&unit.name, categories),
            false,
            true,
            vec!["missing_api_key_fallback".to_string()],
            TokenUsage::default(),
        );
    }
    let system_prompt = format!(
        "You classify one application or project directory into one category. Return JSON only. Schema: {{\"category\":\"...\"}}. Preferred categories: {}. {} Treat the directory as one bundle and do not split its children. If unsure choose {}. If you create a new category, its name must be in {}. If original structure context is provided, use it only as supporting context to preserve grouping intent while still classifying the current directory only.",
        categories.join(" | "),
        if allow_new_categories { "You may create one short new category if none fits." } else { "You must choose from the preferred categories." },
        CATEGORY_OTHER_PENDING,
        prompt_language_name(response_language),
    );
    let mut user_prompt = format!(
        "name={}\npath={}\nentryType=directory\n{}\nChoose the best category for moving the whole directory as one unit.",
        unit.name,
        unit.path,
        summarize_directory_for_prompt(unit)
    );
    if let Some(structure) = reference_structure.filter(|value| !value.trim().is_empty()) {
        user_prompt.push_str("\noriginalStructureContext\n");
        user_prompt.push_str(structure);
        user_prompt.push_str(
            "\nUse the original structure only as context. Do not classify siblings; classify the current directory bundle only.\noriginalStructureContextEnd",
        );
    }
    match chat_completion(route, &system_prompt, &user_prompt).await {
        Ok((content, usage)) => {
            let (category, created) =
                parse_category_response(&content, categories, allow_new_categories);
            (category, created, false, Vec::new(), usage)
        }
        Err(err) => (
            fallback_category(&unit.name, categories),
            false,
            true,
            vec![format!("directory_classify_failed:{err}")],
            TokenUsage::default(),
        ),
    }
}

fn build_preview(root_path: &str, results: &[Value]) -> Vec<Value> {
    let mut used = HashSet::new();
    let mut out = Vec::new();
    for row in results {
        let category = sanitize_category_name(
            row.get("category")
                .and_then(Value::as_str)
                .unwrap_or("其他待定"),
        );
        let mut target = PathBuf::from(root_path)
            .join(&category)
            .join(row.get("name").and_then(Value::as_str).unwrap_or(""));
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
            target = target
                .parent()
                .unwrap_or(Path::new(root_path))
                .join(next_name);
            suffix += 1;
        }
        used.insert(target.to_string_lossy().to_lowercase());
        out.push(json!({
            "sourcePath": row.get("path").and_then(Value::as_str).unwrap_or(""),
            "category": row.get("category").and_then(Value::as_str).unwrap_or("其他待定"),
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
        allow_new_categories,
        use_web_search,
        reference_original_structure,
    ) = {
        let snap = task.snapshot.lock();
        (
            snap.root_path.clone(),
            snap.recursive,
            snap.excluded_patterns.clone(),
            snap.allow_new_categories,
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
    let reference_structure = if reference_original_structure {
        Some(build_reference_structure_context(
            Path::new(&root_path),
            &excluded,
            &task.stop,
        ))
    } else {
        None
    };
    {
        let mut snap = task.snapshot.lock();
        snap.status = "classifying".to_string();
        snap.total_files = units.len() as u64;
    }
    emit_snapshot(app, state, task).await?;

    for (idx, unit) in units.iter().enumerate() {
        if task.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let route_key = if unit.item_type == "directory" {
            "text".to_string()
        } else {
            unit.modality.clone()
        };
        let route = task
            .routes
            .get(&route_key)
            .or_else(|| task.routes.get("text"))
            .cloned()
            .unwrap_or(RouteConfig {
                endpoint: "https://api.openai.com/v1".to_string(),
                api_key: String::new(),
                model: "gpt-4o-mini".to_string(),
            });
        let categories = task.snapshot.lock().categories.clone();
        let (category, created_category, degraded, warnings, usage) =
            if unit.item_type == "directory" {
                classify_directory(
                    &route,
                    unit,
                    &categories,
                    allow_new_categories,
                    reference_structure.as_deref(),
                    &task.response_language,
                )
                .await
            } else {
                classify_file(
                    &route,
                    unit,
                    &categories,
                    allow_new_categories,
                    use_web_search,
                    task.search_api_key.as_deref(),
                    reference_structure.as_deref(),
                    &task.response_language,
                )
                .await
            };
        let row = json!({
            "taskId": task.snapshot.lock().id,
            "index": idx + 1,
            "name": unit.name,
            "path": unit.path,
            "relativePath": unit.relative_path,
            "size": estimate_unit_total_size(unit),
            "itemType": unit.item_type,
            "category": category,
            "createdCategory": created_category,
            "degraded": degraded,
            "warnings": warnings,
            "modality": unit.modality,
            "provider": route.endpoint.clone(),
            "model": route.model.clone(),
        });
        persist::upsert_organize_result(&state.db_path, &task.snapshot.lock().id, &row)?;
        {
            let mut snap = task.snapshot.lock();
            if created_category {
                let new_category = row
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or("其他待定")
                    .to_string();
                if !snap.categories.iter().any(|x| x == &new_category) {
                    snap.categories.push(new_category);
                }
            }
            snap.results.push(row.clone());
            snap.processed_files = snap.processed_files.saturating_add(1);
            snap.token_usage.prompt = snap.token_usage.prompt.saturating_add(usage.prompt);
            snap.token_usage.completion =
                snap.token_usage.completion.saturating_add(usage.completion);
            snap.token_usage.total = snap.token_usage.total.saturating_add(usage.total);
        }
        app.emit("organize_file_done", row)
            .map_err(|e| e.to_string())?;
        emit_snapshot(app, state, task).await?;
    }

    let final_snapshot = {
        let mut snap = task.snapshot.lock();
        snap.results
            .sort_by_key(|x| x.get("index").and_then(Value::as_u64).unwrap_or(0));
        snap.preview = build_preview(&snap.root_path, &snap.results);
        snap.status = "completed".to_string();
        snap.completed_at = Some(now_iso());
        snap.clone()
    };
    persist::save_organize_snapshot(&state.db_path, &final_snapshot)?;
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

pub async fn organize_suggest_categories(input: OrganizeSuggestInput) -> Result<Value, String> {
    let root = PathBuf::from(&input.root_path);
    if !root.exists() {
        return Err("rootPath is required".to_string());
    }
    let excluded = normalize_excluded(input.excluded_patterns);
    let files = collect_units(&root, true, &excluded, &AtomicBool::new(false));
    let mut set = input.manual_categories.unwrap_or_default();
    if !set.iter().any(|x| x == CATEGORY_OTHER_PENDING) {
        set.push(CATEGORY_OTHER_PENDING.to_string());
    }
    for file in files.iter().take(400) {
        let cat = fallback_category(&file.name, &set);
        if cat != CATEGORY_OTHER_PENDING && !set.contains(&cat) {
            set.push(cat);
        }
    }
    Ok(json!({ "suggestedCategories": set, "source": "filename_scan" }))
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
    let snapshot = OrganizeSnapshot {
        id: task_id.clone(),
        status: "idle".to_string(),
        error: None,
        root_path: input.root_path.clone(),
        recursive: true,
        reference_original_structure: input.reference_original_structure.unwrap_or(false),
        mode: input.mode.clone().unwrap_or_else(|| "fast".to_string()),
        categories: normalize_categories(input.categories.clone()),
        allow_new_categories: input.allow_new_categories.unwrap_or(true),
        excluded_patterns: normalize_excluded(input.excluded_patterns.clone()),
        parallelism: input.parallelism.unwrap_or(5).clamp(1, 20),
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
        total_files: 0,
        processed_files: 0,
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
        search_api_key: input.search_api_key.clone(),
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
        "mode": snapshot.mode,
        "recursive": snapshot.recursive,
        "referenceOriginalStructure": snapshot.reference_original_structure,
        "categories": snapshot.categories,
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
            Ok(_) => rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "status": "rolled_back",
                "error": Value::Null
            })),
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
    Ok(json!({
        "success": true,
        "jobId": manifest.get("jobId").and_then(Value::as_str).unwrap_or(&job_id),
        "rollback": rollback
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::TokenUsage;
    use std::fs::File;

    #[test]
    fn normalize_categories_keeps_other_pending_bucket() {
        let categories = normalize_categories(Some(vec![CATEGORY_WORK_STUDY.to_string()]));
        assert!(categories.iter().any(|item| item == CATEGORY_OTHER_PENDING));
    }

    #[test]
    fn parse_category_response_falls_back_to_other_pending() {
        let categories = normalize_categories(Some(vec![CATEGORY_WORK_STUDY.to_string()]));
        let (category, created) = parse_category_response("{}", &categories, false);
        assert_eq!(category, CATEGORY_OTHER_PENDING.to_string());
        assert!(!created);
    }

    fn sample_snapshot() -> OrganizeSnapshot {
        OrganizeSnapshot {
            id: "org_test".to_string(),
            status: "completed".to_string(),
            error: None,
            root_path: "C:\\root".to_string(),
            recursive: true,
            reference_original_structure: false,
            mode: "fast".to_string(),
            categories: vec![
                CATEGORY_WORK_STUDY.to_string(),
                CATEGORY_OTHER_PENDING.to_string(),
            ],
            allow_new_categories: true,
            excluded_patterns: Vec::new(),
            parallelism: 1,
            use_web_search: false,
            web_search_enabled: false,
            selected_model: "gpt-4o-mini".to_string(),
            selected_models: json!({}),
            selected_providers: json!({}),
            supports_multimodal: false,
            total_files: 1,
            processed_files: 1,
            token_usage: TokenUsage::default(),
            results: vec![json!({
                "path": "C:\\root\\foo.txt",
                "name": "foo.txt",
                "category": CATEGORY_WORK_STUDY,
                "itemType": "file"
            })],
            preview: vec![json!({
                "sourcePath": "C:\\root\\foo.txt",
                "targetPath": "C:\\root\\工作学习\\子目录\\foo.txt",
                "category": CATEGORY_WORK_STUDY,
                "itemType": "file"
            })],
            created_at: "2026-03-20T00:00:00Z".to_string(),
            completed_at: Some("2026-03-20T00:00:01Z".to_string()),
            job_id: None,
        }
    }

    #[test]
    fn build_apply_plan_prefers_preview_target_path() {
        let snapshot = sample_snapshot();
        let plan = build_apply_plan(&snapshot);
        assert_eq!(plan.len(), 1);
        assert_eq!(
            plan[0].get("targetPath").and_then(Value::as_str),
            Some("C:\\root\\工作学习\\子目录\\foo.txt")
        );
    }

    #[test]
    fn resolve_apply_target_path_keeps_same_source_target_path() {
        let temp_dir = std::env::temp_dir().join(format!("wipeout-organize-{}", Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).expect("create temp dir");
        let source = temp_dir.join("same.txt");
        File::create(&source).expect("create source file");

        let resolved = resolve_apply_target_path(&source, &source);
        assert_eq!(resolved, source);

        fs::remove_dir_all(&temp_dir).expect("cleanup temp dir");
    }
}
