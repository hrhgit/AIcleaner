fn sanitize_summary_confidence(value: Option<&str>) -> Option<String> {
    match value.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "high" => Some("high".to_string()),
        "medium" => Some("medium".to_string()),
        "low" => Some("low".to_string()),
        _ => None,
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
    patterns.iter().any(|pattern| {
        let normalized = pattern.trim().to_lowercase();
        if normalized.is_empty() {
            return false;
        }
        if normalized.contains('*') {
            let needle = normalized.replace('*', "");
            !needle.is_empty() && lower.contains(&needle)
        } else {
            lower == normalized
        }
    })
}

fn extension_key(path: &Path) -> String {
    path.extension()
        .and_then(|x| x.to_str())
        .map(|x| format!(".{}", x.to_ascii_lowercase()))
        .unwrap_or_else(|| "(no_ext)".to_string())
}

fn relative_path_string(scan_root: &Path, path: &Path) -> String {
    path.strip_prefix(scan_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn classify_extension_family(ext: &str) -> &'static str {
    match ext {
        ".exe" | ".msi" | ".app" | ".apk" | ".dll" | ".bin" | ".pak" | ".pck" | ".3dsx"
        | ".firm" => "app",
        ".json" | ".yaml" | ".yml" | ".toml" | ".ini" | ".cfg" | ".conf" | ".config" | ".xml" => {
            "config"
        }
        ".md" | ".txt" | ".pdf" | ".doc" | ".docx" | ".rtf" | ".epub" | ".csv" | ".xlsx"
        | ".xls" | ".bib" => "document",
        ".png" | ".jpg" | ".jpeg" | ".webp" | ".gif" | ".bmp" | ".ani" | ".ico" => "image",
        ".mp4" | ".mov" | ".mkv" | ".avi" | ".wmv" | ".webm" => "video",
        ".mp3" | ".wav" | ".m4a" | ".aac" | ".flac" | ".ogg" => "audio",
        ".zip" | ".rar" | ".7z" | ".tar" | ".gz" | ".bz2" | ".xz" => "archive",
        ".ttf" | ".otf" | ".woff" | ".woff2" => "font",
        ".log" | ".tmp" | ".cache" | ".dat" | ".db" => "runtime",
        ".ps1" | ".bat" | ".cmd" | ".sh" => "script",
        _ => "other",
    }
}

fn normalize_name_family(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or(name)
        .to_ascii_lowercase();
    let mut value = stem.trim().to_string();
    loop {
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() {
            return "(empty)".to_string();
        }
        if let Some(inner) = trimmed.strip_suffix(')') {
            if let Some(pos) = inner.rfind(" (") {
                let suffix = &inner[pos + 2..];
                if !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()) {
                    value = inner[..pos].to_string();
                    continue;
                }
            }
        }
        let bytes = trimmed.as_bytes();
        let mut idx = bytes.len();
        while idx > 0 && bytes[idx - 1].is_ascii_digit() {
            idx -= 1;
        }
        if idx < bytes.len() && idx > 0 {
            let separator = bytes[idx - 1] as char;
            if matches!(separator, '-' | '_' | ' ') {
                value = trimmed[..idx - 1].to_string();
                continue;
            }
        }
        return trimmed
            .trim_matches(|ch: char| matches!(ch, '-' | '_' | ' ' | '.'))
            .to_string();
    }
}

fn strip_bundle_suffix_tokens(mut value: String) -> String {
    const SUFFIXES: [&str; 11] = [
        "x64",
        "x86",
        "arm64",
        "arm32",
        "amd64",
        "win64",
        "win32",
        "64bit",
        "32bit",
        "setup",
        "installer",
    ];
    loop {
        let trimmed = value
            .trim_end_matches(|ch: char| matches!(ch, '-' | '_' | ' ' | '.'))
            .to_string();
        let mut changed = false;
        for suffix in SUFFIXES {
            if let Some(stripped) = trimmed.strip_suffix(suffix) {
                let candidate = stripped
                    .trim_end_matches(|ch: char| matches!(ch, '-' | '_' | ' ' | '.'))
                    .to_string();
                if !candidate.is_empty() {
                    value = candidate;
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            return trimmed;
        }
    }
}

fn canonical_bundle_key(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or(name)
        .to_ascii_lowercase();
    let stripped = strip_bundle_suffix_tokens(stem);
    let cutoff = stripped
        .char_indices()
        .find_map(|(idx, ch)| (idx >= 3 && ch.is_ascii_digit()).then_some(idx))
        .unwrap_or(stripped.len());
    let base = stripped[..cutoff]
        .trim_end_matches(|ch: char| matches!(ch, '-' | '_' | ' ' | '.'))
        .to_string();
    let cleaned = base
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>();
    if cleaned.is_empty() {
        stripped
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .collect::<String>()
    } else {
        cleaned
    }
}

fn matches_bundle_root(root_key: &str, entry_name: &str) -> bool {
    if root_key.len() < 3 {
        return false;
    }
    let entry_key = canonical_bundle_key(entry_name);
    !entry_key.is_empty() && (entry_key.starts_with(root_key) || root_key.starts_with(&entry_key))
}

fn is_package_doc_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = extension_key(Path::new(name));
    let is_doc_ext = matches!(
        ext.as_str(),
        ".txt" | ".md" | ".pdf" | ".doc" | ".docx" | ".rtf"
    );
    is_doc_ext
        && [
            "readme",
            "guide",
            "manual",
            "install",
            "setup",
            "usage",
            "license",
            "璇存槑",
            "瀹夎",
            "浣跨敤",
            "鏁欑▼",
            "杩愯",
            "鐗堟潈",
        ]
        .iter()
        .any(|token| lower.contains(token))
}

fn format_ranked_entries(map: HashMap<String, u64>, limit: usize) -> Vec<String> {
    let mut rows = map.into_iter().collect::<Vec<_>>();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.into_iter()
        .take(limit)
        .map(|(key, count)| format!("{key}:{count}"))
        .collect()
}

fn summarize_name_families(file_names: &[String], limit: usize) -> (Vec<String>, usize) {
    let mut families = HashMap::<String, u64>::new();
    for name in file_names {
        let family = normalize_name_family(name);
        if family.is_empty() || family == "(empty)" {
            continue;
        }
        *families.entry(family).or_insert(0) += 1;
    }
    let max_family_count = families.values().copied().max().unwrap_or(0) as usize;
    let formatted = format_ranked_entries(
        families
            .into_iter()
            .filter(|(_, count)| *count >= 2)
            .collect::<HashMap<_, _>>(),
        limit,
    );
    (formatted, max_family_count)
}

fn summarize_sidecars(file_names: &[String], limit: usize) -> Vec<String> {
    let mut families = HashMap::<String, HashSet<String>>::new();
    for name in file_names {
        let family = normalize_name_family(name);
        if family.is_empty() || family == "(empty)" {
            continue;
        }
        families
            .entry(family)
            .or_default()
            .insert(extension_key(Path::new(name)));
    }
    let mut rows = families
        .into_iter()
        .filter_map(|(family, exts)| {
            if exts.len() < 2 {
                return None;
            }
            let mut ext_list = exts.into_iter().collect::<Vec<_>>();
            ext_list.sort();
            Some((family, ext_list))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    rows.into_iter()
        .take(limit)
        .map(|(family, exts)| format!("{family}=>{}", exts.join("+")))
        .collect()
}

