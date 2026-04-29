async fn run_organize_task<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<OrganizeTaskRuntime>,
) -> Result<(), String> {
    let task_started_at = Instant::now();
    let (
        root_path,
        recursive,
        excluded,
        batch_size,
        summary_strategy,
        use_web_search,
    ) = {
        let snap = task.snapshot.lock();
        (
            snap.root_path.clone(),
            snap.recursive,
            snap.excluded_patterns.clone(),
            snap.batch_size,
            snap.summary_strategy.clone(),
            snap.use_web_search,
        )
    };
    task.diagnostics.task_started(json!({
            "rootPath": root_path.clone(),
            "recursive": recursive,
            "excludedPatterns": excluded.clone(),
            "batchSize": batch_size,
            "summaryStrategy": summary_strategy.clone(),
            "useWebSearch": use_web_search,
        }));
    {
        let mut snap = task.snapshot.lock();
        snap.status = "collecting".to_string();
    }
    let collection_started_at = Instant::now();
    task.diagnostics.collection_started(&root_path);
    emit_snapshot(app, state, task).await?;

    let collection = collect_units(Path::new(&root_path), recursive, &excluded, &task.stop);
    let units = collection.units;
    task.diagnostics.collection_completed(
        &root_path,
        units.len(),
        &collection.report,
        collection_started_at.elapsed(),
    );
    if task.stop.load(Ordering::Relaxed) {
        task.diagnostics.task_stopped(
            "organize task stopped after directory collection",
            json!({ "stage": "directory_collection" }),
            Some(task_started_at.elapsed()),
        );
        return Ok(());
    }
    let reference_structure = None;
    if task.stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let total_batches = if units.is_empty() {
        0
    } else {
        ((units.len() as u64) + batch_size as u64 - 1) / batch_size as u64
    };
    {
        let mut snap = task.snapshot.lock();
        snap.status = "classifying".to_string();
        snap.total_files = units.len() as u64;
        snap.total_batches = total_batches;
        snap.processed_files = 0;
        snap.processed_batches = 0;
        snap.results.clear();
        snap.preview.clear();
    }
    emit_snapshot(app, state, task).await?;

    let mut tree = {
        let snap = task.snapshot.lock();
        tree_from_value(&snap.tree)
    };
    let text_route = task.routes.get("text").cloned().unwrap_or(RouteConfig {
        endpoint: "https://api.openai.com/v1".to_string(),
        api_key: String::new(),
        model: "gpt-4o-mini".to_string(),
    });
    let task_id = task.snapshot.lock().id.clone();

    for (batch_idx, batch) in units.chunks(batch_size as usize).enumerate() {
        let batch_started_at = Instant::now();
        task.diagnostics
            .batch_started(batch_idx + 1, batch.len(), total_batches);
        if task.stop.load(Ordering::Relaxed) {
            task.diagnostics.task_stopped(
                "organize task stopped before batch",
                json!({ "stage": "batch_start", "batchIndex": batch_idx + 1 }),
                Some(task_started_at.elapsed()),
            );
            return Ok(());
        }

        let mut batch_rows = Vec::new();
        let mut local_results = Vec::new();
        for (offset, unit) in batch.iter().enumerate() {
            if task.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let route_key = if unit.item_type == "directory" {
                "text"
            } else {
                unit.modality.as_str()
            };
            let route = task
                .routes
                .get(route_key)
                .or_else(|| task.routes.get("text"))
                .cloned()
                .unwrap_or(RouteConfig {
                    endpoint: "https://api.openai.com/v1".to_string(),
                    api_key: String::new(),
                    model: "gpt-4o-mini".to_string(),
                });
            let extracted = match summary_strategy.as_str() {
                SUMMARY_MODE_FILENAME_ONLY => None,
                _ => Some(
                    summary::extract_unit_content_for_summary_with_tools(
                        unit,
                        &task.response_language,
                        &task.stop,
                        &task.extraction_tool,
                    )
                    .await,
                ),
            };
            let local_result = match summary_strategy.as_str() {
                SUMMARY_MODE_FILENAME_ONLY => SummaryBuildResult {
                    representation: FileRepresentation {
                        metadata: Some(summary::build_representation_metadata(
                            unit,
                            &SummaryExtraction {
                                parser: SUMMARY_SOURCE_FILENAME_ONLY.to_string(),
                                ..SummaryExtraction::default()
                            },
                        )),
                        short: None,
                        long: None,
                        source: SUMMARY_SOURCE_FILENAME_ONLY.to_string(),
                        degraded: false,
                        confidence: None,
                        keywords: Vec::new(),
                    },
                    warnings: Vec::new(),
                },
                _ => summary::build_local_summary(
                    unit,
                    extracted.as_ref().unwrap_or(&SummaryExtraction::default()),
                ),
            };
            let extraction_json = extracted
                .as_ref()
                .map(|value| {
                    json!({
                        "parser": value.parser,
                        "title": value.title,
                        "excerpt": value.excerpt,
                        "keywords": value.keywords,
                        "metadata": value.metadata_lines,
                        "warnings": value.warnings,
                    })
                })
                .unwrap_or(Value::Null);
            batch_rows.push(json!({
                "itemId": format!("batch{}_{}", batch_idx + 1, offset + 1),
                "name": unit.name,
                "path": unit.path,
                "relativePath": unit.relative_path,
                "size": unit.size,
                "createdAt": unit.created_at,
                "modifiedAt": unit.modified_at,
                "itemType": unit.item_type,
                "modality": unit.modality,
                "summaryStrategy": summary_strategy.clone(),
                "representation": local_result.representation.to_value(),
                "summaryDegraded": local_result.representation.degraded,
                "summaryWarnings": local_result.warnings,
                "localExtraction": extraction_json,
                "provider": route.endpoint,
                "model": route.model,
            }));
            if summary_strategy == SUMMARY_MODE_LOCAL_SUMMARY {
                if let Some(row) = batch_rows.last() {
                    summary::emit_organize_summary_ready(
                        &app,
                        &task_id,
                        (batch_idx + 1) as u64,
                        row,
                    );
                }
            }
            local_results.push(local_result);
        }

        let mut summary_usage = TokenUsage::default();
        if summary_strategy == SUMMARY_MODE_AGENT_SUMMARY {
            // Filter to only non-degraded items for agent summary
            let batch_rows_for_agent: Vec<Value> = batch_rows
                .iter()
                .filter(|row| !row.get("summaryDegraded").and_then(Value::as_bool).unwrap_or(false))
                .cloned()
                .collect();

            let output = summary::summarize_batch_with_agent(
                &text_route,
                &task.response_language,
                &task.stop,
                &batch_rows_for_agent,
                Some(&task.diagnostics),
                "summary_agent",
            )
            .await;
            summary_usage = output.usage;
            let batch_failed_warning = output
                .error
                .clone()
                .unwrap_or_else(|| "summary_agent_missing_items".to_string());
            for (idx, row) in batch_rows.iter_mut().enumerate() {
                let item_id = row.get("itemId").and_then(Value::as_str).unwrap_or("");
                let local_result = local_results.get(idx).cloned().unwrap_or_default();

                // If degraded, keep local result as-is without warnings
                if row.get("summaryDegraded").and_then(Value::as_bool).unwrap_or(false) {
                    continue;
                }

                let mut warnings = row
                    .get("summaryWarnings")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|value| value.as_str().map(|item| item.to_string()))
                    .collect::<Vec<_>>();
                let fallback_source =
                    if local_result.representation.source == SUMMARY_SOURCE_FILENAME_ONLY {
                        SUMMARY_SOURCE_FILENAME_ONLY
                    } else {
                        SUMMARY_SOURCE_AGENT_FALLBACK_LOCAL
                    };

                if let Some(agent_item) = output
                    .error
                    .is_none()
                    .then(|| output.items.get(item_id))
                    .flatten()
                    .filter(|item| {
                        !item.summary_long.trim().is_empty()
                            || !item.summary_short.trim().is_empty()
                    })
                {
                    warnings.extend(agent_item.warnings.clone());
                    let mut representation = local_result.representation.clone();
                    representation.short = Some(agent_item.summary_short.clone());
                    representation.long = Some(agent_item.summary_long.clone());
                    representation.source = SUMMARY_SOURCE_AGENT_SUMMARY.to_string();
                    representation.confidence = agent_item.confidence.clone();
                    representation.keywords = agent_item.keywords.clone();
                    row["representation"] = representation.to_value();
                    row["summaryDegraded"] = Value::Bool(local_result.representation.degraded);
                    row["summaryWarnings"] =
                        Value::Array(warnings.into_iter().map(Value::String).collect::<Vec<_>>());
                } else {
                    warnings.push(batch_failed_warning.clone());
                    let mut representation = local_result.representation.clone();
                    representation.source = fallback_source.to_string();
                    representation.degraded = true;
                    representation.confidence = None;
                    row["representation"] = representation.to_value();
                    row["summaryDegraded"] = Value::Bool(true);
                    row["summaryWarnings"] =
                        Value::Array(warnings.into_iter().map(Value::String).collect::<Vec<_>>());
                }
            }
            for row in &batch_rows {
                summary::emit_organize_summary_ready(&app, &task_id, (batch_idx + 1) as u64, row);
            }
        }

        let mut cluster_usage = TokenUsage::default();
        let mut cluster_failed = false;
        let mut cluster_raw_output = String::new();
        let mut cluster_error = String::new();
        let mut assignment_map: HashMap<String, (String, Vec<String>, String)> = HashMap::new();

        if !text_route.api_key.trim().is_empty() {
            let category_inventory = {
                let snap = task.snapshot.lock();
                summary::build_category_inventory(
                    &tree,
                    &snap.results,
                    CATEGORY_INVENTORY_FILES_PER_CATEGORY,
                )
            };
            match summary::classify_organize_batch(
                &text_route,
                &task.response_language,
                &task.stop,
                &tree,
                &batch_rows,
                &category_inventory,
                reference_structure.as_ref(),
                use_web_search,
                &task.search_api_key,
                Some(&task.diagnostics),
                &format!("classification_batch_{}", batch_idx + 1),
            )
            .await
            {
                Ok(output) => {
                    cluster_usage = output.usage;
                    cluster_raw_output = output.raw_output;
                    if let Some(search_error) = output.error {
                        cluster_failed = true;
                        cluster_error = search_error;
                    }
                    if let Some(parsed) = output.parsed {
                        tree = normalize_ai_tree(&parsed, &tree);
                        ensure_uncategorized_leaf(&mut tree);
                        for assignment in parsed
                            .get("assignments")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                        {
                            let Some(item_id) = assignment.get("itemId").and_then(Value::as_str)
                            else {
                                continue;
                            };
                            let mut category_path = category_path_from_value(
                                assignment
                                    .get("categoryPath")
                                    .or_else(|| assignment.get("leafPath")),
                            );
                            let leaf_node_id = if let Some(node_id) =
                                assignment.get("leafNodeId").and_then(Value::as_str)
                            {
                                if let Some(path) = category_path_for_id(&tree, node_id) {
                                    category_path = path;
                                    node_id.to_string()
                                } else if !category_path.is_empty() {
                                    ensure_path(&mut tree, &category_path)
                                } else {
                                    ensure_uncategorized_leaf(&mut tree)
                                }
                            } else if !category_path.is_empty() {
                                ensure_path(&mut tree, &category_path)
                            } else {
                                ensure_uncategorized_leaf(&mut tree)
                            };
                            if category_path.is_empty() {
                                category_path = category_path_for_id(&tree, &leaf_node_id)
                                    .unwrap_or_else(|| vec![UNCATEGORIZED_NODE_NAME.to_string()]);
                            }
                            assignment_map.insert(
                                item_id.to_string(),
                                (
                                    leaf_node_id,
                                    category_path,
                                    assignment
                                        .get("reason")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string(),
                                ),
                            );
                        }
                        if assignment_map.len() < batch_rows.len() {
                            cluster_failed = true;
                            if cluster_error.is_empty() {
                                cluster_error = format!(
                                    "classification response missing assignments for {} item(s)",
                                    batch_rows.len().saturating_sub(assignment_map.len())
                                );
                            }
                            assignment_map.clear();
                        }
                    }
                }
                Err(err) => {
                    cluster_failed = true;
                    cluster_error = err;
                }
            }
        }
        if task.stop.load(Ordering::Relaxed) {
            task.diagnostics.task_stopped(
                "organize task stopped after classification",
                json!({ "stage": "classification", "batchIndex": batch_idx + 1 }),
                Some(task_started_at.elapsed()),
            );
            return Ok(());
        }

        let batch_base_index = {
            let snap = task.snapshot.lock();
            snap.processed_files
        };
        let mut persisted_rows = Vec::with_capacity(batch_rows.len());
        for (row_offset, row) in batch_rows.into_iter().enumerate() {
            if task.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let item_id = row.get("itemId").and_then(Value::as_str).unwrap_or("");
            let (leaf_node_id, category_path, category, reason) = if cluster_failed {
                (
                    String::new(),
                    Vec::new(),
                    CATEGORY_CLASSIFICATION_ERROR.to_string(),
                    RESULT_REASON_CLASSIFICATION_ERROR.to_string(),
                )
            } else {
                let (leaf_node_id, category_path, reason) =
                    assignment_map.get(item_id).cloned().unwrap_or_else(|| {
                        let leaf = ensure_uncategorized_leaf(&mut tree);
                        let path = category_path_for_id(&tree, &leaf)
                            .unwrap_or_else(|| vec![UNCATEGORIZED_NODE_NAME.to_string()]);
                        (leaf, path, "fallback_uncategorized".to_string())
                    });
                (
                    leaf_node_id,
                    category_path.clone(),
                    category_path_display(&category_path),
                    reason,
                )
            };
            let warnings = row
                .get("summaryWarnings")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let result_row = json!({
                "taskId": task_id.clone(),
                "index": batch_base_index + row_offset as u64 + 1,
                "batchIndex": (batch_idx + 1) as u64,
                "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
                "path": row.get("path").and_then(Value::as_str).unwrap_or(""),
                "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
                "size": row.get("size").and_then(Value::as_u64).unwrap_or(0),
                "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
                "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
                "summaryStrategy": row.get("summaryStrategy").cloned().unwrap_or(Value::String(SUMMARY_MODE_FILENAME_ONLY.to_string())),
                "representation": row.get("representation").cloned().unwrap_or_else(|| FileRepresentation::default().to_value()),
                "localExtraction": row.get("localExtraction").cloned().unwrap_or(Value::Null),
                "leafNodeId": leaf_node_id,
                "categoryPath": category_path,
                "category": category,
                "reason": reason,
                "degraded": cluster_failed || row.get("summaryDegraded").and_then(Value::as_bool).unwrap_or(false),
                "warnings": warnings,
                "provider": row.get("provider").and_then(Value::as_str).unwrap_or(""),
                "model": row.get("model").and_then(Value::as_str).unwrap_or(""),
                "classificationError": if row_offset == 0 { cluster_error.clone() } else { String::new() },
                "modelRawOutput": if row_offset == 0 { cluster_raw_output.clone() } else { String::new() },
            });
            persisted_rows.push(result_row.clone());
        }
        persist::upsert_organize_results(&state.db_path(), &task_id, &persisted_rows)?;
        {
            let mut snap = task.snapshot.lock();
            snap.processed_files = snap
                .processed_files
                .saturating_add(persisted_rows.len() as u64);
            snap.results.extend(persisted_rows.iter().cloned());
        }
        let persisted_row_count = persisted_rows.len();
        for row in persisted_rows {
            app.emit("organize_file_done", row)
                .map_err(|e| e.to_string())?;
        }

        {
            let mut snap = task.snapshot.lock();
            snap.tree = category_tree_to_value(&tree);
            snap.tree_version = snap.tree_version.saturating_add(1);
            snap.processed_batches = (batch_idx + 1) as u64;
            snap.token_usage.prompt = snap
                .token_usage
                .prompt
                .saturating_add(summary_usage.prompt)
                .saturating_add(cluster_usage.prompt);
            snap.token_usage.completion = snap
                .token_usage
                .completion
                .saturating_add(summary_usage.completion)
                .saturating_add(cluster_usage.completion);
            snap.token_usage.total = snap
                .token_usage
                .total
                .saturating_add(summary_usage.total)
                .saturating_add(cluster_usage.total);
        }
        emit_snapshot(app, state, task).await?;
        task.diagnostics.batch_completed(
            cluster_failed,
            json!({
                "batchIndex": batch_idx + 1,
                "persistedRows": persisted_row_count,
                "clusterFailed": cluster_failed,
                "clusterError": cluster_error.clone(),
                "summaryUsage": {
                    "prompt": summary_usage.prompt,
                    "completion": summary_usage.completion,
                    "total": summary_usage.total,
                },
                "clusterUsage": {
                    "prompt": cluster_usage.prompt,
                    "completion": cluster_usage.completion,
                    "total": cluster_usage.total,
                },
            }),
            batch_started_at.elapsed(),
        );
    }

    let final_snapshot = {
        let mut snap = task.snapshot.lock();
        snap.results
            .sort_by_key(|x| x.get("index").and_then(Value::as_u64).unwrap_or(0));
        snap.preview = planner::build_preview(&snap.root_path, &snap.results);
        snap.tree = category_tree_to_value(&tree);
        snap.status = "completed".to_string();
        snap.completed_at = Some(now_iso());
        snap.clone()
    };
    persist::save_organize_snapshot(&state.db_path(), &final_snapshot)?;
    persist::save_latest_organize_tree(
        &state.db_path(),
        &final_snapshot.root_path,
        &final_snapshot.tree,
        final_snapshot.tree_version,
    )?;
    app.emit(
        "organize_done",
        serde_json::to_value(final_snapshot.clone()).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    task.diagnostics.task_completed(
        json!({
            "totalFiles": final_snapshot.total_files,
            "processedFiles": final_snapshot.processed_files,
            "totalBatches": final_snapshot.total_batches,
            "processedBatches": final_snapshot.processed_batches,
            "previewCount": final_snapshot.preview.len(),
            "tokenUsage": {
                "prompt": final_snapshot.token_usage.prompt,
                "completion": final_snapshot.token_usage.completion,
                "total": final_snapshot.token_usage.total,
            },
        }),
        task_started_at.elapsed(),
    );
    Ok(())
}

