use super::*;

pub(super) fn build_preview(root_path: &str, results: &[Value]) -> Vec<Value> {
    let mut used = HashSet::new();
    let mut out = Vec::new();
    for row in results {
        if row_has_classification_error(row) {
            continue;
        }
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

pub(super) fn hydrate_loaded_snapshot(mut snapshot: OrganizeSnapshot) -> OrganizeSnapshot {
    snapshot.preview = build_preview(&snapshot.root_path, &snapshot.results);
    snapshot
}

pub(super) fn normalize_path_key(path: &Path) -> String {
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

pub(super) fn resolve_apply_target_path(source: &Path, planned_target: &Path) -> PathBuf {
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

pub(super) fn prune_empty_dirs_upward(start_dir: &Path, stop_dir: &Path) {
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

pub(super) fn build_apply_plan(snapshot: &OrganizeSnapshot) -> Vec<Value> {
    let preview_rows = build_preview(&snapshot.root_path, &snapshot.results);

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
