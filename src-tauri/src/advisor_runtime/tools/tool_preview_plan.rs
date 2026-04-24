impl<'a> ToolService<'a> {
    pub(crate) fn preview_plan_from_value(
        &self,
        session: &Value,
        plan: &Value,
    ) -> Result<Value, String> {
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let lang = session
            .get("responseLanguage")
            .and_then(Value::as_str)
            .unwrap_or("zh");
        let targets = plan
            .get("targets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if targets.is_empty() {
            return Err(
                "当前计划缺少 selectionId，请先调用 find_files 生成筛选结果，再继续生成预览。"
                    .to_string(),
            );
        }

        let mut all_entries = Vec::new();
        let mut preview_selection_id = None::<String>;
        for target in targets {
            let selection_id = target
                .get("selectionId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    "当前计划缺少 selectionId，请先调用 find_files 生成筛选结果，再继续生成预览。"
                        .to_string()
                })?;
            let action = target
                .get("action")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("review");
            let selection = persist::load_advisor_selection(&self.state.db_path(), selection_id)?
                .ok_or_else(|| {
                "当前筛选结果已失效，请先重新调用 find_files 生成新的 selection，再继续生成预览。"
                    .to_string()
            })?;
            ensure_session_owned(&selection, session_id, "selection")?;
            let items = selection
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let inventory = items
                .iter()
                .map(value_to_inventory_item)
                .collect::<Vec<_>>();
            let label = action_label(action, lang);
            let preview = build_plan_preview_for_matches(
                session
                    .get("rootPath")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                session_id,
                selection_id,
                plan.get("intentSummary")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                normalize_plan_action(action),
                &label,
                &inventory,
                lang,
            );
            if preview_selection_id.is_none() {
                preview_selection_id = Some(selection_id.to_string());
            }
            all_entries.extend(
                preview
                    .get("entries")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
        }

        let can_execute = all_entries
            .iter()
            .filter(|row| {
                row.get("canExecute")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .count();
        let preview_id = Uuid::new_v4().to_string();
        let preview = json!({
            "previewId": preview_id,
            "planId": preview_id,
            "intentSummary": plan.get("intentSummary").cloned().unwrap_or(Value::String(String::new())),
            "selectionId": preview_selection_id.clone(),
            "summary": {
                "total": all_entries.len(),
                "canExecute": can_execute,
                "blocked": all_entries.len().saturating_sub(can_execute),
            },
            "entries": all_entries,
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
        });
        let job = json!({
            "jobId": preview_id,
            "sessionId": session_id,
            "selectionId": preview_selection_id,
            "status": "preview_ready",
            "preview": preview,
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
        });
        persist::save_advisor_plan_job(&self.state.db_path(), &job)?;
        Ok(job)
    }
}
