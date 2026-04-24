impl<'a> ToolService<'a> {
    pub(crate) fn find_files_by_args(
        &self,
        session: &Value,
        args: &Value,
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
        let matches = filter_inventory_by_args(&overview.inventory, args);
        let query_summary = summarize_find_query(args);
        let selection = create_selection(
            &self.state.db_path(),
            session
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            &query_summary,
            &matches,
        )?;
        Ok(json!({
            "message": local_text(
                session.get("responseLanguage").and_then(Value::as_str).unwrap_or("zh"),
                &format!("已找到 {} 个候选文件。", matches.len()),
                &format!("Found {} candidate files.", matches.len())
            ),
            "total": selection.get("total").cloned().unwrap_or(Value::from(0)),
            "selectionId": selection.get("selectionId").cloned().unwrap_or(Value::Null),
            "querySummary": selection.get("querySummary").cloned().unwrap_or(Value::String(query_summary)),
            "sortBy": args.get("sortBy").cloned().unwrap_or(Value::String("size".to_string())),
            "sortOrder": args.get("sortOrder").cloned().unwrap_or(Value::String("desc".to_string())),
            "files": selection.get("items").cloned().unwrap_or_else(|| json!([])),
        }))
    }
}
