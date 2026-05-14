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
        let started_at = Instant::now();
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
        let summary_strategy = summary_strategy_from_args(args);
        let selected = filter_inventory_by_args(&overview.inventory, args);
        let total = selected.len();
        let missing_only = args
            .get("missingOnly")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if summary_strategy == ADVISOR_SUMMARY_MODE_FILENAME_ONLY {
            let mut items = Vec::new();
            for item in selected {
                if missing_only {
                    if let Some(row) =
                        load_summary_row(
                            &self.state.db_path(),
                            root_path,
                            &item.path,
                            &summary_strategy,
                            true,
                        )?
                    {
                        items.push(summary_row_to_tool_item(&row));
                        continue;
                    }
                }
                let row = self.persist_summary(root_path, &item, &summary_strategy, None)?;
                items.push(summary_row_to_tool_item(&row));
            }
            return Ok(json!({
                "status": "ok",
                "message": local_text(
                    lang,
                    &format!("已完成 {} 条文件摘要。", items.len()),
                    &format!("Completed {} file summaries.", items.len())
                ),
                "summaryStrategy": summary_strategy,
                "total": total,
                "completed": items.len(),
                "failed": 0,
                "items": items,
                "errors": [],
                "durationMs": started_at.elapsed().as_millis() as u64,
                "tokenUsage": { "prompt": 0, "completion": 0, "total": 0 },
                "requestCount": 0,
                "errorCount": 0,
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
        let mut unsupported_errors = Vec::new();
        for item in selected {
            if missing_only {
                if let Some(row) =
                    load_summary_row(
                        &self.state.db_path(),
                        root_path,
                        &item.path,
                        &summary_strategy,
                        true,
                    )?
                {
                    cached_rows.push(row);
                    continue;
                }
            }
            if !supports_content_summarization(&item.path) {
                unsupported_errors.push(json!({
                    "path": item.path,
                    "reason": local_text(
                        lang,
                        "此文件类型不支持内容摘要，仅支持文档和纯文本文件。",
                        "This file type does not support content summarization. Only document and plain text files are supported."
                    ),
                }));
                continue;
            }
            generation_items.push(item);
        }

        let batch_size = args.get("batchSize").and_then(Value::as_u64).unwrap_or(8) as usize;
        let max_concurrency = args
            .get("maxConcurrency")
            .and_then(Value::as_u64)
            .unwrap_or(2) as usize;
        let route = AdvisorLlm::new(self.state).resolve_route(None, None)?;

        let settings = crate::backend::read_settings(&self.state.settings_path());
        let mut extraction_config = extraction_tool_config_from_settings(&settings);
        if summary_strategy_uses_content(&summary_strategy) {
            force_enable_tika_for_summary_mode(&mut extraction_config);
            ensure_tika_server_running(self.state, &mut extraction_config).await;
        }

        let mut rows = cached_rows;
        let (generated_rows, scheduler_errors, scheduler, usage) =
            if summary_strategy == ADVISOR_SUMMARY_MODE_AGENT_SUMMARY {
                run_summary_scheduler(
                    &route,
                    root_path,
                    &generation_items,
                    &summary_strategy,
                    lang,
                    batch_size.max(1),
                    max_concurrency.max(1),
                    Some(extraction_config),
                )
                .await?
            } else {
                let mut generated_rows = Vec::new();
                let mut scheduler_errors = Vec::new();
                for item in &generation_items {
                    match summarize_item_with_local_strategy(
                        root_path,
                        item,
                        &summary_strategy,
                        lang,
                        Some(&extraction_config),
                    )
                    .await
                    {
                        Ok(row) => generated_rows.push(row),
                        Err(error) => scheduler_errors.push(error),
                    }
                }
                (
                    generated_rows,
                    scheduler_errors,
                    SummarySchedulerStats {
                        initial_concurrency: 1,
                        final_concurrency: 1,
                        retry_rounds: 0,
                        degraded_concurrency: false,
                    },
                    TokenUsage::default(),
                )
            };
        rows.extend(generated_rows);
        for row in &rows {
            persist::save_advisor_file_summary(&self.state.db_path(), row)?;
        }
        let items = rows
            .iter()
            .map(summary_row_to_tool_item)
            .collect::<Vec<_>>();
        let mut all_errors: Vec<Value> = scheduler_errors.into_iter().map(|error| json!({
            "path": error.path,
            "reason": error.reason,
        })).collect();
        all_errors.extend(unsupported_errors);
        let total_error_count = all_errors.len();
        Ok(json!({
            "status": if all_errors.is_empty() { "ok" } else { "error" },
            "message": if all_errors.is_empty() {
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
            "summaryStrategy": summary_strategy,
            "total": total,
            "completed": items.len(),
            "failed": total_error_count,
            "durationMs": started_at.elapsed().as_millis() as u64,
            "tokenUsage": {
                "prompt": usage.prompt,
                "completion": usage.completion,
                "total": usage.total,
            },
            "requestCount": generation_items.len(),
            "errorCount": total_error_count,
            "items": items,
            "errors": all_errors,
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
        let summary_strategy = summary_strategy_from_args(args);
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(24) as usize;
        let root_path = session
            .get("rootPath")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let items = filter_inventory_by_args(&overview.inventory, args)
            .into_iter()
            .take(limit)
            .filter_map(|item| {
                read_existing_summary_item(
                    &self.state.db_path(),
                    root_path,
                    &item,
                    &summary_strategy,
                )
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({
            "summaryStrategy": summary_strategy,
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
        summary_strategy: &str,
        representation: Option<FileRepresentation>,
    ) -> Result<Value, String> {
        let representation = representation.unwrap_or_else(|| metadata_representation(item));
        let row = json!({
            "rootPathKey": persist::create_root_path_key(root_path),
            "pathKey": persist::create_root_path_key(&item.path),
            "path": item.path,
            "name": item.name,
            "representation": representation.to_value(),
            "summaryShort": representation.short.clone(),
            "summaryNormal": representation.long.clone(),
            "source": representation.source,
            "summaryStrategy": summary_strategy,
            "updatedAt": now_iso(),
        });
        persist::save_advisor_file_summary(&self.state.db_path(), &row)?;
        Ok(row)
    }
}
