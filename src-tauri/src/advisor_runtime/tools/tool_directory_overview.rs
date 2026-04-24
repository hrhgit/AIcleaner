impl<'a> ToolService<'a> {
    pub(crate) fn get_directory_overview(
        &self,
        root_path: &str,
        organize_task_id: Option<&str>,
        session: Option<&Value>,
        response_language: &str,
    ) -> Result<DirectoryOverview, String> {
        let assets = self.load_assets(root_path, organize_task_id)?;
        let inventory = self.build_inventory(root_path, &assets, session);
        let preferences = self.list_preferences(
            session
                .and_then(|value| value.get("sessionId"))
                .and_then(Value::as_str),
        )?;
        let derived_tree =
            build_derived_tree(root_path, &inventory).or_else(|| assets.latest_tree.clone());
        let context_bar = build_context_bar(
            root_path,
            session,
            &assets,
            &inventory,
            &preferences,
            response_language,
            assets.organize_snapshot.is_some() || assets.latest_tree.is_some(),
        );
        Ok(DirectoryOverview {
            assets,
            inventory,
            derived_tree,
            context_bar,
        })
    }

    pub(crate) fn get_directory_overview_tool(
        &self,
        session: &Value,
        view_type: Option<&str>,
        _root_category_id: Option<&str>,
        _max_depth: Option<u64>,
    ) -> Result<Value, String> {
        let overview = self.get_directory_overview(
            session
                .get("rootPath")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            session_organize_task_id(session),
            Some(session),
            session
                .get("responseLanguage")
                .and_then(Value::as_str)
                .unwrap_or("zh"),
        )?;
        let tree = overview.derived_tree.unwrap_or(Value::Null);
        let view_type = normalize_view_type(view_type.unwrap_or("summaryTree"));
        Ok(json!({
            "message": local_text(
                session.get("responseLanguage").and_then(Value::as_str).unwrap_or("zh"),
                "已返回当前目录概览。",
                "Returned the current directory overview."
            ),
            "viewType": view_type,
            "treeText": build_tree_text(&tree),
            "tree": tree
        }))
    }
}
