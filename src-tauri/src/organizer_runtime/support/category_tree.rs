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

fn row_has_classification_error(row: &Value) -> bool {
    if row.get("reason").and_then(Value::as_str).map(str::trim)
        == Some(RESULT_REASON_CLASSIFICATION_ERROR)
    {
        return true;
    }
    !row.get("classificationError")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .is_empty()
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

fn trim_to_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect::<String>()
}

fn normalize_multiline_text(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut last_blank = false;
    for raw_line in value.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let line = raw_line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            if !last_blank && !out.is_empty() {
                out.push('\n');
            }
            last_blank = true;
            continue;
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&line);
        last_blank = false;
    }
    trim_to_chars(out.trim(), max_chars)
}

async fn emit_snapshot<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<OrganizeTaskRuntime>,
) -> Result<(), String> {
    let snap = task.snapshot.lock().clone();
    persist::save_organize_snapshot(&state.db_path(), &snap)?;
    app.emit(
        "organize_progress",
        serde_json::to_value(&snap).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

