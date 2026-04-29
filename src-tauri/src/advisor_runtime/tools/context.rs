pub(crate) fn build_context_bar(
    root_path: &str,
    session: Option<&Value>,
    assets: &ContextAssets,
    inventory: &[InventoryItem],
    preferences: &[Value],
    lang: &str,
    tree_available: bool,
) -> Value {
    let memory_summary_message = if preferences.is_empty() {
        local_text(
            lang,
            "当前没有已保存偏好。",
            "No saved preferences are active.",
        )
        .to_string()
    } else {
        local_text(
            lang,
            &format!("已加载 {} 条已保存偏好。", preferences.len()),
            &format!("{} saved preferences are active.", preferences.len()),
        )
        .to_string()
    };
    json!({
        "collapsed": session
            .and_then(|value| value.pointer("/contextBar/collapsed"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        "rootPath": session
            .and_then(|value| value.get("rootPath"))
            .and_then(Value::as_str)
            .unwrap_or(root_path),
        "mode": {
            "id": "single_agent",
            "label": local_text(lang, "顾问模式：单智能体", "Advisor Mode: Single Agent")
        },
        "organizeTaskId": assets.organize_task_id,
        "organizeSummary": {
            "totalFiles": assets.organize_snapshot.as_ref().map(|row| row.total_files).unwrap_or(0),
            "processedFiles": assets.organize_snapshot.as_ref().map(|row| row.processed_files).unwrap_or(0),
        },
        "memorySummary": {
            "activeCount": preferences.len(),
            "message": memory_summary_message,
        },
        "directorySummary": {
            "itemCount": inventory.len(),
            "treeAvailable": tree_available,
            "message": if tree_available {
                local_text(lang, "已载入最近一次归类结果，可直接对话收敛方案。", "Latest organize result loaded and ready for guided cleanup.")
            } else {
                local_text(lang, "当前没有可复用的归类结果，顾问会先基于目录元信息工作。", "No reusable organize result is available yet, so advisor starts from directory metadata.")
            }
        }
    })
}

pub(crate) fn build_tree_card(
    session_id: &str,
    turn_id: &str,
    tree: &Value,
    inventory: &[InventoryItem],
    lang: &str,
) -> Value {
    json!({
        "cardId": Uuid::new_v4().to_string(),
        "sessionId": session_id,
        "turnId": turn_id,
        "cardType": super::types::CARD_TREE,
        "status": "ready",
        "title": local_text(lang, "当前分类树", "Current Tree"),
        "body": {
            "tree": tree,
            "stats": tree_stats(inventory),
        },
        "actions": [],
        "createdAt": now_iso(),
        "updatedAt": now_iso(),
    })
}

fn tree_insert(node: &mut TreeDraftNode, path: &[String]) {
    node.item_count += 1;
    if let Some(head) = path.first() {
        let child = node
            .children
            .entry(head.clone())
            .or_insert_with(|| TreeDraftNode {
                name: head.clone(),
                ..Default::default()
            });
        tree_insert(child, &path[1..]);
    }
}

fn tree_to_value(prefix: &str, path: &[String], node: &TreeDraftNode) -> Value {
    let mut children = node
        .children
        .values()
        .map(|child| {
            let mut child_path = path.to_vec();
            child_path.push(child.name.clone());
            tree_to_value(&format!("{prefix}/{}", child.name), &child_path, child)
        })
        .collect::<Vec<_>>();
    children.sort_by(|left, right| {
        right["itemCount"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&left["itemCount"].as_u64().unwrap_or(0))
            .then_with(|| {
                left["name"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["name"].as_str().unwrap_or_default())
            })
    });
    let node_id = if path.is_empty() {
        "root".to_string()
    } else {
        category_id_from_path(path)
    };
    json!({
        "nodeId": node_id,
        "categoryId": node_id,
        "parentCategoryId": parent_category_id_from_path(path),
        "categoryPath": path,
        "name": node.name,
        "itemCount": node.item_count,
        "children": children,
    })
}

fn build_derived_tree(root_path: &str, inventory: &[InventoryItem]) -> Option<Value> {
    if inventory.is_empty() {
        return None;
    }
    let mut root = TreeDraftNode {
        name: basename(root_path),
        ..Default::default()
    };
    for item in inventory {
        let category_path = if item.category_path.is_empty() {
            vec!["其他待定".to_string()]
        } else {
            item.category_path.clone()
        };
        tree_insert(&mut root, &category_path);
    }
    Some(tree_to_value(root_path, &[], &root))
}

fn tree_stats(inventory: &[InventoryItem]) -> Value {
    let mut kinds = HashMap::<String, u64>::new();
    for item in inventory {
        *kinds.entry(item.kind.clone()).or_default() += 1;
    }
    json!({
        "itemCount": inventory.len(),
        "kinds": kinds,
    })
}

fn infer_kind(path: &str, summary: &str, fallback: &str) -> String {
    let lower = format!("{} {}", normalize_message(path), normalize_message(summary));
    if lower.contains("screenshot")
        || lower.contains("截图")
        || lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
    {
        "screenshot".to_string()
    } else if lower.ends_with(".exe")
        || lower.ends_with(".msi")
        || lower.contains("installer")
        || lower.contains("安装")
    {
        "installer".to_string()
    } else if lower.ends_with(".zip")
        || lower.ends_with(".rar")
        || lower.ends_with(".7z")
        || lower.contains("压缩")
    {
        "archive".to_string()
    } else if lower.ends_with(".log") || lower.contains("日志") {
        "log".to_string()
    } else if lower.contains("\\temp\\")
        || lower.contains("\\tmp\\")
        || lower.contains("缓存")
        || lower.contains("cache")
        || lower.contains("临时")
    {
        "temp".to_string()
    } else if lower.ends_with(".mp4")
        || lower.ends_with(".mov")
        || lower.ends_with(".mkv")
        || lower.contains("视频")
    {
        "video".to_string()
    } else if lower.ends_with(".mp3")
        || lower.ends_with(".wav")
        || lower.ends_with(".flac")
        || lower.contains("音频")
    {
        "audio".to_string()
    } else if lower.ends_with(".pdf")
        || lower.ends_with(".doc")
        || lower.ends_with(".docx")
        || lower.ends_with(".xls")
        || lower.ends_with(".xlsx")
        || lower.contains("文档")
    {
        "document".to_string()
    } else if !fallback.trim().is_empty() {
        fallback.to_string()
    } else {
        "file".to_string()
    }
}

