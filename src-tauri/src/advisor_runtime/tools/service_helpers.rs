impl<'a> ToolService<'a> {
    pub(crate) fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    fn load_assets(
        &self,
        root_path: &str,
        organize_task_id: Option<&str>,
    ) -> Result<ContextAssets, String> {
        let db_path = self.state.db_path();
        let resolved_organize_task_id = if let Some(task_id) = organize_task_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(task_id.to_string())
        } else {
            persist::find_latest_organize_task_id_for_root(&db_path, root_path)?
        };
        let organize_snapshot = resolved_organize_task_id
            .as_deref()
            .map(|task_id| persist::load_organize_snapshot(&db_path, task_id))
            .transpose()?
            .flatten();
        let latest_tree = persist::load_latest_organize_tree(&db_path, root_path)?
            .map(|(tree, _)| tree)
            .or_else(|| organize_snapshot.as_ref().map(|row| row.tree.clone()));

        Ok(ContextAssets {
            organize_task_id: resolved_organize_task_id,
            organize_snapshot,
            latest_tree,
        })
    }

    fn build_inventory(
        &self,
        root_path: &str,
        assets: &ContextAssets,
        session: Option<&Value>,
    ) -> Vec<InventoryItem> {
        let overrides = session
            .map(super::types::inventory_overrides)
            .unwrap_or_default();
        let mut map = std::collections::HashMap::<String, InventoryItem>::new();

        if let Some(snapshot) = &assets.organize_snapshot {
            for row in &snapshot.results {
                let path = row
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if path.is_empty() {
                    continue;
                }
                let key = normalize_path_key(&path);
                let representation = representation_from_organize_row(row);
                let summary = representation.best_text();
                let mut category_path = overrides
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| array_of_strings(row.get("categoryPath")));
                if category_path.is_empty() {
                    category_path.push("其他待定".to_string());
                }
                let category_id = category_id_from_path(&category_path);
                let parent_category_id = parent_category_id_from_path(&category_path);
                let (created_at, modified_at) =
                    infer_inventory_timestamps(&path, row.get("createdAt"), row.get("modifiedAt"));
                map.insert(
                    key,
                    InventoryItem {
                        name: row
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| basename(&path)),
                        path: path.clone(),
                        size: row.get("size").and_then(Value::as_u64).unwrap_or(0),
                        created_at,
                        modified_at,
                        kind: infer_kind(&path, &summary, ""),
                        category_id,
                        parent_category_id,
                        category_path,
                        representation,
                        risk: "medium".to_string(),
                    },
                );
            }
        }

        if map.is_empty() {
            for item in collect_inventory_from_fs(root_path, &overrides) {
                map.insert(normalize_path_key(&item.path), item);
            }
        }

        let mut rows = map.into_values().collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            right
                .size
                .cmp(&left.size)
                .then_with(|| left.path.cmp(&right.path))
        });
        rows
    }
}
