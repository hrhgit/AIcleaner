fn session_organize_task_id(session: &Value) -> Option<&str> {
    session
        .pointer("/sessionMeta/organizeTaskId")
        .and_then(Value::as_str)
}

fn create_selection(
    db_path: &Path,
    session_id: &str,
    query_summary: &str,
    items: &[InventoryItem],
) -> Result<Value, String> {
    let now = now_iso();
    let selection = json!({
        "selectionId": Uuid::new_v4().to_string(),
        "sessionId": session_id,
        "querySummary": query_summary,
        "total": items.len(),
        "items": items.iter().map(|item| json!({
            "path": item.path,
            "name": item.name,
            "size": item.size,
            "sizeText": format_size_text(item.size),
            "createdAt": item.created_at,
            "modifiedAt": item.modified_at,
            "modifiedAgeText": modified_age_text(item),
            "kind": item.kind,
            "categoryId": item.category_id,
            "parentCategoryId": item.parent_category_id,
            "categoryPath": item.category_path,
            "representation": item.representation.to_value(),
            "risk": item.risk,
        })).collect::<Vec<_>>(),
        "createdAt": now,
        "updatedAt": now,
    });
    persist::save_advisor_selection(db_path, &selection)?;
    Ok(selection)
}

fn archive_target_path(root_path: &str, item: &InventoryItem) -> String {
    PathBuf::from(root_path)
        .join("_advisor_archive")
        .join(item.name.clone())
        .to_string_lossy()
        .to_string()
}

fn move_target_path(root_path: &str, item: &InventoryItem) -> String {
    let mut base = PathBuf::from(root_path).join("_advisor_sorted");
    for part in &item.category_path {
        base.push(part);
    }
    base.push(item.name.clone());
    base.to_string_lossy().to_string()
}

pub(crate) fn build_plan_preview_for_matches(
    root_path: &str,
    session_id: &str,
    selection_id: &str,
    query_summary: &str,
    action: &str,
    label: &str,
    matches: &[InventoryItem],
    lang: &str,
) -> Value {
    let entries = matches
        .iter()
        .map(|item| {
            let warnings = if action == "recycle" && item.risk == "high" {
                vec![local_text(
                    lang,
                    "高风险项默认不直接回收",
                    "High-risk items are blocked from direct recycle by default",
                )
                .to_string()]
            } else {
                Vec::new()
            };
            let can_execute = warnings.is_empty() && !matches!(action, "keep" | "review");
            json!({
                "name": item.name,
                "sourcePath": item.path,
                "targetPath": match action {
                    "archive" => Value::String(archive_target_path(root_path, item)),
                    "move" => Value::String(move_target_path(root_path, item)),
                    _ => Value::Null,
                },
                "action": action,
                "kind": item.kind,
                "risk": item.risk,
                "canExecute": can_execute,
                "warnings": warnings,
            })
        })
        .collect::<Vec<_>>();
    let can_execute = entries
        .iter()
        .filter(|row| {
            row.get("canExecute")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    json!({
        "jobId": Uuid::new_v4().to_string(),
        "sessionId": session_id,
        "selectionId": selection_id,
        "querySummary": query_summary,
        "action": action,
        "label": label,
        "summary": {
            "total": entries.len(),
            "canExecute": can_execute,
            "blocked": entries.len().saturating_sub(can_execute),
        },
        "entries": entries,
        "createdAt": now_iso(),
        "updatedAt": now_iso(),
    })
}

fn execute_plan_job(job: &Value) -> Result<(Value, Value), String> {
    let preview = job.get("preview").cloned().unwrap_or_else(|| json!({}));
    let entries = preview
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut rollback_entries = Vec::new();
    let mut result_entries = Vec::new();

    for entry in entries {
        let source_path = entry
            .get("sourcePath")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let can_execute = entry
            .get("canExecute")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !can_execute {
            result_entries.push(json!({
                "sourcePath": source_path,
                "status": "blocked",
                "error": "blocked_in_preview",
            }));
            continue;
        }

        let action = entry
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let outcome = if action == "recycle" {
            system_ops::move_to_recycle_bin(Path::new(source_path)).map(|_| {
                json!({
                    "sourcePath": source_path,
                    "status": "recycled",
                    "targetPath": Value::Null,
                    "action": action,
                })
            })
        } else {
            let target_path = entry
                .get("targetPath")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if source_path.is_empty() || target_path.is_empty() {
                Err("missing_path".to_string())
            } else if Path::new(target_path).exists() {
                Err("target_conflict".to_string())
            } else {
                if let Some(parent) = Path::new(target_path).parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::rename(source_path, target_path).map_err(|e| e.to_string())?;
                rollback_entries.push(json!({
                    "fromPath": source_path,
                    "toPath": target_path,
                }));
                Ok(json!({
                    "sourcePath": source_path,
                    "targetPath": target_path,
                    "status": "moved",
                    "action": action,
                }))
            }
        };

        match outcome {
            Ok(value) => result_entries.push(value),
            Err(error) => result_entries.push(json!({
                "sourcePath": source_path,
                "status": "failed",
                "error": error,
            })),
        }
    }

    let moved_count = result_entries
        .iter()
        .filter(|row| row.get("status").and_then(Value::as_str) == Some("moved"))
        .count();
    Ok((
        json!({
            "entries": result_entries,
            "summary": {
                "total": result_entries.len(),
                "moved": moved_count,
                "archived": result_entries.iter().filter(|row| row.get("action").and_then(Value::as_str) == Some("archive") && row.get("status").and_then(Value::as_str) == Some("moved")).count(),
                "recycled": result_entries.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("recycled")).count(),
                "failed": result_entries.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("failed")).count(),
                "blocked": result_entries.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("blocked")).count(),
            }
        }),
        json!({
            "entries": rollback_entries,
            "available": !rollback_entries.is_empty(),
        }),
    ))
}

fn rollback_plan_job(job: &Value) -> Result<Value, String> {
    let entries = job
        .pointer("/rollback/entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut results = Vec::new();
    for entry in entries {
        let from_path = entry
            .get("fromPath")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let to_path = entry
            .get("toPath")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let result = if from_path.is_empty() || to_path.is_empty() {
            Err("missing_path".to_string())
        } else if !Path::new(to_path).exists() {
            Err("target_not_found".to_string())
        } else if Path::new(from_path).exists() {
            Err("source_already_exists".to_string())
        } else {
            if let Some(parent) = Path::new(from_path).parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::rename(to_path, from_path).map_err(|e| e.to_string())
        };
        match result {
            Ok(()) => results.push(json!({
                "fromPath": from_path,
                "toPath": to_path,
                "status": "rolled_back",
            })),
            Err(error) => results.push(json!({
                "fromPath": from_path,
                "toPath": to_path,
                "status": "failed",
                "error": error,
            })),
        }
    }
    Ok(json!({
        "rollbackId": Uuid::new_v4().to_string(),
        "entries": results,
        "summary": {
            "rolledBack": results.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("rolled_back")).count(),
            "notRollbackable": 0,
            "failed": results.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("failed")).count(),
        }
    }))
}

pub(crate) fn apply_reclassification_selection(
    session: &mut Value,
    items: &[Value],
    category_path: &[String],
) -> Result<Vec<Value>, String> {
    let mut rollback_rows = Vec::new();
    for row in items {
        let Some(path) = row.get("path").and_then(Value::as_str) else {
            continue;
        };
        let key = normalize_path_key(path);
        let previous = set_inventory_override(session, &key, category_path)?;
        rollback_rows.push(json!({
            "path": key,
            "previousCategoryPath": previous,
        }));
    }
    Ok(rollback_rows)
}

pub(crate) fn rollback_reclassification_rows(
    session: &mut Value,
    rows: &[Value],
) -> Result<(), String> {
    for row in rows {
        let Some(path) = row.get("path").and_then(Value::as_str) else {
            continue;
        };
        let previous = array_of_strings(row.get("previousCategoryPath"));
        if previous.is_empty() {
            super::types::remove_inventory_override(session, path)?;
        } else {
            set_inventory_override(session, path, &previous)?;
        }
    }
    Ok(())
}

fn required_change_str<'a>(change: &'a Value, key: &str) -> Result<&'a str, String> {
    change
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string())
}

fn category_id_from_path(category_path: &[String]) -> String {
    persist::create_node_id(&format!("advisor-category:{}", category_path.join("/")))
}

fn parent_category_id_from_path(category_path: &[String]) -> Option<String> {
    if category_path.len() <= 1 {
        None
    } else {
        Some(category_id_from_path(
            &category_path[..category_path.len() - 1],
        ))
    }
}

fn category_path_from_category_id(
    session: &Value,
    category_id: &str,
) -> Result<Option<Vec<String>>, String> {
    let tree = session.get("derivedTree").cloned().unwrap_or(Value::Null);
    Ok(find_category_path_in_tree(&tree, category_id))
}

fn find_category_path_in_tree(tree: &Value, category_id: &str) -> Option<Vec<String>> {
    let obj = tree.as_object()?;
    if obj.get("categoryId").and_then(Value::as_str) == Some(category_id) {
        return Some(array_of_strings(obj.get("categoryPath")));
    }
    obj.get("children")
        .and_then(Value::as_array)
        .and_then(|children| {
            children
                .iter()
                .find_map(|child| find_category_path_in_tree(child, category_id))
        })
}

