impl<'a> ToolService<'a> {
    pub(crate) fn apply_reclassification(
        &self,
        session: &mut Value,
        selection_id: &str,
        category_path: &[String],
    ) -> Result<Value, String> {
        let selection = persist::load_advisor_selection(&self.state.db_path(), selection_id)?
            .ok_or_else(|| "selection not found".to_string())?;
        ensure_session_owned(
            &selection,
            session
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "selection",
        )?;
        let items = selection
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let rollback_rows = apply_reclassification_selection(session, &items, category_path)?;
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
        Ok(json!({
            "jobId": Uuid::new_v4().to_string(),
            "sessionId": session.get("sessionId").cloned().unwrap_or(Value::Null),
            "status": "applied",
            "request": {
                "selectionId": selection_id,
                "categoryPath": category_path,
            },
            "result": {
                "Summary": local_text(
                    session.get("responseLanguage").and_then(Value::as_str).unwrap_or("zh"),
                    "分类修正已写入当前会话树。旧选择和旧预览已失效。",
                    "The reclassification has been applied to the session tree. Previous selections and previews are now invalid.",
                ),
                "tree": overview.derived_tree,
            },
            "rollback": {
                "entries": rollback_rows,
            },
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
        }))
    }

    pub(crate) fn apply_reclassification_request(
        &self,
        session: &mut Value,
        request: &Value,
        apply_preference_capture: bool,
    ) -> Result<Value, String> {
        self.apply_reclassification_request_impl(session, request, apply_preference_capture)
    }

    pub(crate) fn rollback_reclassification(
        &self,
        session: &mut Value,
        job_id: &str,
    ) -> Result<(Value, Value, Option<Value>), String> {
        let mut job = persist::load_advisor_reclass_job(&self.state.db_path(), job_id)?
            .ok_or_else(|| "reclass job not found".to_string())?;
        ensure_session_owned(
            &job,
            session
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "reclassification job",
        )?;
        let rows = job
            .pointer("/rollback/entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        rollback_reclassification_rows(session, &rows)?;
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
        if let Some(obj) = job.as_object_mut() {
            obj.insert(
                "status".to_string(),
                Value::String("rolled_back".to_string()),
            );
            obj.insert(
                "rollbackResult".to_string(),
                json!({
                    "tree": overview.derived_tree,
                }),
            );
            obj.insert("updatedAt".to_string(), Value::String(now_iso()));
        }
        persist::save_advisor_reclass_job(&self.state.db_path(), &job)?;
        Ok((
            job,
            json!({
                "message": local_text(
                    session.get("responseLanguage").and_then(Value::as_str).unwrap_or("zh"),
                    "已回滚最近一次分类修正。",
                    "Rolled back the latest reclassification."
                ),
                "updatedTreeText": build_tree_text(&overview.derived_tree.clone().unwrap_or(Value::Null)),
                "rolledBack": true,
                "invalidated": ["selection", "preview"],
            }),
            overview.derived_tree,
        ))
    }

    fn apply_reclassification_request_impl(
        &self,
        session: &mut Value,
        request: &Value,
        apply_preference_capture: bool,
    ) -> Result<Value, String> {
        let changes = collect_reclassification_changes(request)?;
        let session_backup = session.clone();
        let mut category_aliases: HashMap<String, Option<Vec<String>>> = HashMap::new();
        let mut rollback_entries = Vec::new();
        let apply_result = (|| {
            let db_path = self.state.db_path();
            for change in &changes {
                let rows = apply_reclassification_change(
                    db_path.as_path(),
                    session,
                    change,
                    &mut category_aliases,
                )?;
                rollback_entries.extend(rows);
            }
            Ok::<(), String>(())
        })();
        if let Err(err) = apply_result {
            *session = session_backup;
            return Err(err);
        }
        if let Some(obj) = session.as_object_mut() {
            obj.insert("activeSelectionId".to_string(), Value::Null);
        }
        if apply_preference_capture {
            persist::save_advisor_memory(
                &self.state.db_path(),
                &json!({
                    "memoryId": Uuid::new_v4().to_string(),
                    "sessionId": session.get("sessionId").cloned().unwrap_or(Value::Null),
                    "scope": "session",
                    "enabled": true,
                    "text": request.get("intentSummary").and_then(Value::as_str).unwrap_or_default(),
                    "createdAt": now_iso(),
                    "updatedAt": now_iso(),
                }),
            )?;
        }
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
        Ok(json!({
            "jobId": Uuid::new_v4().to_string(),
            "sessionId": session.get("sessionId").cloned().unwrap_or(Value::Null),
            "status": "applied",
            "request": request,
            "result": {
                "message": local_text(
                    session.get("responseLanguage").and_then(Value::as_str).unwrap_or("zh"),
                    "已应用归类修订。",
                    "Reclassification applied."
                ),
                "changeSummary": build_reclassification_change_summary(&changes, &rollback_entries),
                "updatedTreeText": build_tree_text(&overview.derived_tree.clone().unwrap_or(Value::Null)),
                "tree": overview.derived_tree,
                "invalidated": ["selection", "preview"],
            },
            "rollback": {
                "entries": rollback_entries,
            },
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
        }))
    }
}
