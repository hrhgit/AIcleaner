impl<'a> ToolService<'a> {
    pub(crate) async fn summarize_files_tool(
        &self,
        session: &Value,
        args: &Value,
    ) -> Result<Value, String> {
        self.summarize_files_tool_impl(session, args).await
    }

    pub(crate) fn read_only_file_summaries_tool(
        &self,
        session: &Value,
        args: &Value,
    ) -> Result<Value, String> {
        self.read_only_file_summaries_tool_impl(session, args)
    }

    async fn summarize_files_tool_impl(
        &self,
        session: &Value,
        args: &Value,
    ) -> Result<Value, String> {
        let lang = session
            .get("responseLanguage")
            .and_then(Value::as_str)
            .unwrap_or("zh");
        let root_path = session
            .get("rootPath")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let overview = self.get_directory_overview(
            root_path,
            session_organize_task_id(session),
            Some(session),
            lang,
        )?;
        let level =
            RepresentationLevel::parse(args.get("representationLevel").and_then(Value::as_str));
        let selected = filter_inventory_by_args(&overview.inventory, args);
        let total = selected.len();
        let missing_only = args
            .get("missingOnly")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if level == RepresentationLevel::Metadata {
            let mut items = Vec::new();
            for item in selected {
                if missing_only {
                    if let Some(row) =
                        load_summary_row(&self.state.db_path(), root_path, &item.path, level, true)?
                    {
                        items.push(summary_row_to_tool_item(
                            &row,
                            RepresentationLevel::Metadata,
                        ));
                        continue;
                    }
                }
                let row = self.persist_summary(root_path, &item, level, None)?;
                items.push(summary_row_to_tool_item(&row, level));
            }
            return Ok(json!({
                "status": "ok",
                "message": local_text(
                    lang,
                    &format!("已完成 {} 条文件摘要。", items.len()),
                    &format!("Completed {} file summaries.", items.len())
                ),
                "representationLevel": level.as_str(),
                "total": total,
                "completed": items.len(),
                "failed": 0,
                "items": items,
                "errors": [],
                "scheduler": {
                    "initialConcurrency": 1,
                    "finalConcurrency": 1,
                    "retryRounds": 0,
                    "degradedConcurrency": false
                },
            }));
        }

        let mut cached_rows = Vec::new();
        let mut generation_items = Vec::new();
        for item in selected {
            if missing_only {
                if let Some(row) =
                    load_summary_row(&self.state.db_path(), root_path, &item.path, level, true)?
                {
                    cached_rows.push(row);
                    continue;
                }
            }
            generation_items.push(item);
        }

        let batch_size = args.get("batchSize").and_then(Value::as_u64).unwrap_or(8) as usize;
        let max_concurrency = args
            .get("maxConcurrency")
            .and_then(Value::as_u64)
            .unwrap_or(2) as usize;
        let route = AdvisorLlm::new(self.state).resolve_route(None, None)?;
        let mut rows = cached_rows;
        let (generated_rows, errors, scheduler) = run_summary_scheduler(
            &route,
            root_path,
            &generation_items,
            level,
            lang,
            batch_size.max(1),
            max_concurrency.max(1),
        )
        .await?;
        rows.extend(generated_rows);
        for row in &rows {
            persist::save_advisor_file_summary(&self.state.db_path(), row)?;
        }
        let items = rows
            .iter()
            .map(|row| summary_row_to_tool_item(row, level))
            .collect::<Vec<_>>();
        Ok(json!({
            "status": if errors.is_empty() { "ok" } else { "error" },
            "message": if errors.is_empty() {
                Value::String(local_text(
                    lang,
                    &format!("已完成 {} 条文件摘要。", items.len()),
                    &format!("Completed {} file summaries.", items.len())
                ).to_string())
            } else {
                Value::String(local_text(
                    lang,
                    "部分摘要生成失败，请检查 errors 并重试。",
                    "Some summaries failed. Check errors and retry."
                ).to_string())
            },
            "representationLevel": level.as_str(),
            "total": total,
            "completed": items.len(),
            "failed": errors.len(),
            "items": items,
            "errors": errors.into_iter().map(|error| json!({
                "path": error.path,
                "reason": error.reason,
            })).collect::<Vec<_>>(),
            "scheduler": {
                "initialConcurrency": scheduler.initial_concurrency,
                "finalConcurrency": scheduler.final_concurrency,
                "retryRounds": scheduler.retry_rounds,
                "degradedConcurrency": scheduler.degraded_concurrency,
            },
        }))
    }

    fn read_only_file_summaries_tool_impl(
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
        let level =
            RepresentationLevel::parse(args.get("representationLevel").and_then(Value::as_str));
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(24) as usize;
        let root_path = session
            .get("rootPath")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let items = filter_inventory_by_args(&overview.inventory, args)
            .into_iter()
            .take(limit)
            .filter_map(|item| {
                read_existing_summary_item(&self.state.db_path(), root_path, &item, level)
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({
            "message": local_text(
                session.get("responseLanguage").and_then(Value::as_str).unwrap_or("zh"),
                &format!("已读取 {} 条已有摘要。", items.len()),
                &format!("Read {} existing summaries.", items.len())
            ),
            "total": items.len(),
            "items": items,
        }))
    }

    fn persist_summary(
        &self,
        root_path: &str,
        item: &InventoryItem,
        level: RepresentationLevel,
        representation: Option<FileRepresentation>,
    ) -> Result<Value, String> {
        let representation = representation
            .unwrap_or_else(|| metadata_representation(item))
            .prune_to_level(level);
        let row = json!({
            "rootPathKey": persist::create_root_path_key(root_path),
            "pathKey": persist::create_root_path_key(&item.path),
            "path": item.path,
            "name": item.name,
            "representation": representation.to_value(),
            "summaryShort": representation.short.clone(),
            "summaryNormal": representation.long.clone(),
            "source": representation.source,
            "representationLevel": level.as_str(),
            "updatedAt": now_iso(),
        });
        persist::save_advisor_file_summary(&self.state.db_path(), &row)?;
        Ok(row)
    }
}
