fn ensure_session_owned(record: &Value, session_id: &str, label: &str) -> Result<(), String> {
    let owner = record
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if owner == session_id {
        Ok(())
    } else {
        Err(format!("{label} does not belong to the active session"))
    }
}

fn normalize_view_type(value: &str) -> &'static str {
    match value.trim() {
        "sizeTree" => "sizeTree",
        "timeTree" => "timeTree",
        "executionTree" => "executionTree",
        "partialTree" => "partialTree",
        _ => "summaryTree",
    }
}

fn build_tree_text(tree: &Value) -> String {
    let Some(obj) = tree.as_object() else {
        return String::new();
    };
    let mut lines = Vec::new();
    append_tree_text_lines(obj, 0, &mut lines);
    lines.join("\n")
}

fn append_tree_text_lines(
    node: &serde_json::Map<String, Value>,
    depth: usize,
    lines: &mut Vec<String>,
) {
    let indent = "  ".repeat(depth);
    let name = node.get("name").and_then(Value::as_str).unwrap_or("-");
    let count = node.get("itemCount").and_then(Value::as_u64).unwrap_or(0);
    lines.push(format!("{indent}{name}: {count}"));
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children.iter().take(12) {
            if let Some(child_obj) = child.as_object() {
                append_tree_text_lines(child_obj, depth + 1, lines);
            }
        }
    }
}

fn infer_preference_kind(text: &str) -> String {
    let lower = normalize_message(text);
    if ["归档", "archive"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        "archive_preferred".to_string()
    } else if ["不要删", "别删", "保留", "keep", "don't delete"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        "keep".to_string()
    } else if ["总是", "始终", "always"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        "long_term_preference".to_string()
    } else {
        "general_preference".to_string()
    }
}

fn summarize_find_query(args: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(category_ids) = args.get("categoryIds").and_then(Value::as_array) {
        let labels = category_ids
            .iter()
            .filter_map(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .collect::<Vec<_>>();
        if !labels.is_empty() {
            parts.push(labels.join(", "));
        }
    }
    for key in ["nameQuery", "nameExact", "pathContains"] {
        if let Some(value) = args
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            parts.push(value.to_string());
        }
    }
    if parts.is_empty() {
        "当前筛选".to_string()
    } else {
        parts.join(" | ")
    }
}

fn filter_inventory_by_args(inventory: &[InventoryItem], args: &Value) -> Vec<InventoryItem> {
    let category_ids = args
        .get("categoryIds")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(normalize_message)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let name_query = args
        .get("nameQuery")
        .and_then(Value::as_str)
        .map(normalize_message)
        .unwrap_or_default();
    let name_exact = args
        .get("nameExact")
        .and_then(Value::as_str)
        .map(normalize_message)
        .unwrap_or_default();
    let path_contains = args
        .get("pathContains")
        .and_then(Value::as_str)
        .map(normalize_message)
        .unwrap_or_default();
    let extensions = args
        .get("extensions")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let paths = args
        .get("paths")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(normalize_path_key)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let min_size = args
        .get("minSizeBytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let max_size = args.get("maxSizeBytes").and_then(Value::as_u64);
    let older_than_days = args.get("olderThanDays").and_then(Value::as_i64);
    let newer_than_days = args.get("newerThanDays").and_then(Value::as_i64);
    let sort_by = args.get("sortBy").and_then(Value::as_str).unwrap_or("size");
    let sort_order = args
        .get("sortOrder")
        .and_then(Value::as_str)
        .unwrap_or("desc");
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(24) as usize;

    let mut rows = inventory
        .iter()
        .filter(|item| {
            if !category_ids.is_empty()
                && !category_ids.iter().any(|needle| {
                    item.category_id == *needle
                        || item
                            .category_path
                            .iter()
                            .map(|value| normalize_message(value))
                            .any(|part| part == *needle)
                })
            {
                return false;
            }
            if !name_query.is_empty()
                && !normalize_message(&item.name).contains(&name_query)
                && !normalize_message(&item.path).contains(&name_query)
            {
                return false;
            }
            if !name_exact.is_empty() && normalize_message(&item.name) != name_exact {
                return false;
            }
            if !path_contains.is_empty() && !normalize_message(&item.path).contains(&path_contains)
            {
                return false;
            }
            if !paths.is_empty()
                && !paths
                    .iter()
                    .any(|path| path == &normalize_path_key(&item.path))
            {
                return false;
            }
            if !extensions.is_empty() {
                let ext = Path::new(&item.path)
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(|value| format!(".{}", value.to_ascii_lowercase()))
                    .unwrap_or_default();
                if !extensions.iter().any(|value| value == &ext) {
                    return false;
                }
            }
            if item.size < min_size {
                return false;
            }
            if max_size.is_some_and(|value| item.size > value) {
                return false;
            }
            if let Some(days) = older_than_days {
                let Some(modified_at) = item.modified_at.as_deref() else {
                    return false;
                };
                let Ok(modified) = chrono::DateTime::parse_from_rfc3339(modified_at) else {
                    return false;
                };
                if chrono::Utc::now()
                    .signed_duration_since(modified.with_timezone(&chrono::Utc))
                    .num_days()
                    < days
                {
                    return false;
                }
            }
            if let Some(days) = newer_than_days {
                let Some(modified_at) = item.modified_at.as_deref() else {
                    return false;
                };
                let Ok(modified) = chrono::DateTime::parse_from_rfc3339(modified_at) else {
                    return false;
                };
                if chrono::Utc::now()
                    .signed_duration_since(modified.with_timezone(&chrono::Utc))
                    .num_days()
                    > days
                {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect::<Vec<_>>();

    rows.sort_by(|left, right| {
        let ordering = match sort_by {
            "name" => left.name.cmp(&right.name),
            "modifiedAt" => left
                .modified_at
                .cmp(&right.modified_at)
                .then_with(|| left.path.cmp(&right.path)),
            _ => left
                .size
                .cmp(&right.size)
                .then_with(|| left.path.cmp(&right.path)),
        };
        if sort_order.eq_ignore_ascii_case("asc") {
            ordering
        } else {
            ordering.reverse()
        }
    });
    rows.truncate(limit);
    rows
}

fn format_size_text(size: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let size_f = size as f64;
    if size_f >= GB {
        format!("{:.1} GB", size_f / GB)
    } else if size_f >= MB {
        format!("{:.1} MB", size_f / MB)
    } else if size_f >= KB {
        format!("{:.1} KB", size_f / KB)
    } else {
        format!("{size} B")
    }
}

fn value_to_inventory_item(value: &Value) -> InventoryItem {
    InventoryItem {
        path: value
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        size: value.get("size").and_then(Value::as_u64).unwrap_or(0),
        created_at: value
            .get("createdAt")
            .and_then(Value::as_str)
            .map(str::to_string),
        modified_at: value
            .get("modifiedAt")
            .and_then(Value::as_str)
            .map(str::to_string),
        kind: value
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("review")
            .to_string(),
        category_id: value
            .get("categoryId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        parent_category_id: value
            .get("parentCategoryId")
            .and_then(Value::as_str)
            .map(str::to_string),
        category_path: array_of_strings(value.get("categoryPath")),
        representation: FileRepresentation::from_value(
            value.get("representation").unwrap_or(&Value::Null),
        ),
        risk: value
            .get("risk")
            .and_then(Value::as_str)
            .unwrap_or("medium")
            .to_string(),
    }
}

fn normalize_plan_action(action: &str) -> &'static str {
    match action.trim() {
        "delete" => "recycle",
        "archive" => "archive",
        "move" => "move",
        "keep" => "keep",
        "review" => "review",
        _ => "review",
    }
}

fn action_label(action: &str, lang: &str) -> String {
    match action.trim() {
        "delete" => local_text(lang, "回收", "Recycle").to_string(),
        "archive" => local_text(lang, "归档", "Archive").to_string(),
        "move" => local_text(lang, "移动", "Move").to_string(),
        "keep" => local_text(lang, "保留", "Keep").to_string(),
        _ => local_text(lang, "复核", "Review").to_string(),
    }
}

