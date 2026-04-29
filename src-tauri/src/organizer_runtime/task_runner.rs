struct PreparedSummaryBatch {
    batch_idx: usize,
    batch_rows: Vec<Value>,
    summary_usage: TokenUsage,
}

type SummaryPrefetchHandle = JoinHandle<Result<PreparedSummaryBatch, String>>;

fn spawn_summary_prefetch(
    task: &Arc<OrganizeTaskRuntime>,
    text_route: &RouteConfig,
    summary_strategy: &str,
    batch_idx: usize,
    batch: &[OrganizeUnit],
) -> SummaryPrefetchHandle {
    let task = task.clone();
    let text_route = text_route.clone();
    let summary_strategy = summary_strategy.to_string();
    let batch = batch.to_vec();
    tauri::async_runtime::spawn(async move {
        prepare_summary_batch(task, text_route, summary_strategy, batch_idx, batch).await
    })
}

fn abort_pending_summary_prefetches(handles: &mut [Option<SummaryPrefetchHandle>]) {
    for handle in handles.iter_mut().filter_map(Option::take) {
        handle.abort();
    }
}

async fn await_summary_prefetch(
    handles: &mut [Option<SummaryPrefetchHandle>],
    batch_idx: usize,
) -> Result<PreparedSummaryBatch, String> {
    let handle = handles
        .get_mut(batch_idx)
        .and_then(Option::take)
        .ok_or_else(|| format!("summary_prefetch_missing_handle:batch{}", batch_idx + 1))?;
    match handle.await {
        Ok(result) => result,
        Err(err) => Err(format!(
            "summary_prefetch_join_failed:batch{}:{err}",
            batch_idx + 1
        )),
    }
}

async fn prepare_summary_batch(
    task: Arc<OrganizeTaskRuntime>,
    text_route: RouteConfig,
    summary_strategy: String,
    batch_idx: usize,
    batch: Vec<OrganizeUnit>,
) -> Result<PreparedSummaryBatch, String> {
    let mut batch_rows = Vec::new();
    let mut local_results = Vec::new();
    for (offset, unit) in batch.iter().enumerate() {
        if task.stop.load(Ordering::Relaxed) {
            return Err("stop_requested".to_string());
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
        local_results.push(local_result);
    }

    let mut summary_usage = TokenUsage::default();
    if summary_strategy == SUMMARY_MODE_AGENT_SUMMARY {
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
            &format!("summary_agent_batch_{}", batch_idx + 1),
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
            let fallback_source = if local_result.representation.source == SUMMARY_SOURCE_FILENAME_ONLY
            {
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
                    !item.summary_long.trim().is_empty() || !item.summary_short.trim().is_empty()
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
    }

    Ok(PreparedSummaryBatch {
        batch_idx,
        batch_rows,
        summary_usage,
    })
}

fn deterministic_initial_tree(rows: &[Value]) -> CategoryTreeNode {
    let mut tree = default_tree();
    for row in rows {
        let name = row.get("name").and_then(Value::as_str).unwrap_or("");
        let family = if row.get("itemType").and_then(Value::as_str) == Some("directory") {
            "application".to_string()
        } else {
            classify_extension_family(&extension_key(Path::new(name))).to_string()
        };
        let label = match family.as_str() {
            "app" => "应用程序",
            "config" => "配置文件",
            "document" => "文档",
            "image" => "图片",
            "video" => "视频",
            "audio" => "音频",
            "archive" => "压缩包",
            "font" => "字体",
            "runtime" => "运行时数据",
            "script" => "脚本",
            "code" => "代码",
            _ => UNCATEGORIZED_NODE_NAME,
        };
        ensure_path(&mut tree, &[label.to_string()]);
    }
    ensure_uncategorized_leaf(&mut tree);
    tree
}

fn result_row_from_assignment(
    task_id: &str,
    index: u64,
    batch_idx: usize,
    row: &Value,
    leaf_node_id: &str,
    category_path: Vec<String>,
    reason: &str,
    cluster_raw_output: &str,
    cluster_error: &str,
) -> Value {
    let warnings = row
        .get("summaryWarnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    json!({
        "taskId": task_id,
        "index": index,
        "batchIndex": (batch_idx + 1) as u64,
        "itemId": row.get("itemId").and_then(Value::as_str).unwrap_or(""),
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
        "categoryPath": category_path.clone(),
        "category": category_path_display(&category_path),
        "reason": reason,
        "degraded": row.get("summaryDegraded").and_then(Value::as_bool).unwrap_or(false),
        "warnings": warnings,
        "provider": row.get("provider").and_then(Value::as_str).unwrap_or(""),
        "model": row.get("model").and_then(Value::as_str).unwrap_or(""),
        "classificationError": cluster_error,
        "modelRawOutput": cluster_raw_output,
    })
}

fn classification_error_row(
    task_id: &str,
    index: u64,
    batch_idx: usize,
    row: &Value,
    error: &str,
    raw_output: &str,
) -> Value {
    let mut result = result_row_from_assignment(
        task_id,
        index,
        batch_idx,
        row,
        "",
        Vec::new(),
        RESULT_REASON_CLASSIFICATION_ERROR,
        raw_output,
        error,
    );
    result["category"] = Value::String(CATEGORY_CLASSIFICATION_ERROR.to_string());
    result["degraded"] = Value::Bool(true);
    result
}

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

    let text_route = task.routes.get("text").cloned().unwrap_or(RouteConfig {
        endpoint: "https://api.openai.com/v1".to_string(),
        api_key: String::new(),
        model: "gpt-4o-mini".to_string(),
    });
    let task_id = task.snapshot.lock().id.clone();

    let batches = units
        .chunks(batch_size as usize)
        .map(|chunk| chunk.to_vec())
        .collect::<Vec<_>>();
    let mut prefetch_handles = (0..batches.len()).map(|_| None).collect::<Vec<_>>();
    let mut next_prefetch_idx = 0usize;
    while next_prefetch_idx < batches.len() && next_prefetch_idx < SUMMARY_PREFETCH_BATCHES {
        prefetch_handles[next_prefetch_idx] = Some(spawn_summary_prefetch(
            task,
            &text_route,
            &summary_strategy,
            next_prefetch_idx,
            &batches[next_prefetch_idx],
        ));
        next_prefetch_idx += 1;
    }

    let mut prepared_batches = Vec::with_capacity(batches.len());
    for batch_idx in 0..batches.len() {
        let batch = &batches[batch_idx];
        let batch_started_at = Instant::now();
        task.diagnostics
            .batch_started(batch_idx + 1, batch.len(), total_batches);
        if task.stop.load(Ordering::Relaxed) {
            abort_pending_summary_prefetches(&mut prefetch_handles);
            task.diagnostics.task_stopped(
                "organize task stopped before batch",
                json!({ "stage": "batch_start", "batchIndex": batch_idx + 1 }),
                Some(task_started_at.elapsed()),
            );
            return Ok(());
        }

        let prepared = match await_summary_prefetch(&mut prefetch_handles, batch_idx).await {
            Ok(prepared) => prepared,
            Err(err) if task.stop.load(Ordering::Relaxed) => {
                abort_pending_summary_prefetches(&mut prefetch_handles);
                task.diagnostics.task_stopped(
                    "organize task stopped during summary prefetch",
                    json!({ "stage": "summary_prefetch", "batchIndex": batch_idx + 1, "error": err }),
                    Some(task_started_at.elapsed()),
                );
                return Ok(());
            }
            Err(err) => {
                abort_pending_summary_prefetches(&mut prefetch_handles);
                return Err(err);
            }
        };
        if prepared.batch_idx != batch_idx {
            abort_pending_summary_prefetches(&mut prefetch_handles);
            return Err(format!(
                "summary_prefetch_batch_order_mismatch:expected={},actual={}",
                batch_idx + 1,
                prepared.batch_idx + 1
            ));
        }
        if task.stop.load(Ordering::Relaxed) {
            abort_pending_summary_prefetches(&mut prefetch_handles);
            task.diagnostics.task_stopped(
                "organize task stopped after summary prefetch",
                json!({ "stage": "summary_prefetch", "batchIndex": batch_idx + 1 }),
                Some(task_started_at.elapsed()),
            );
            return Ok(());
        }

        if next_prefetch_idx < batches.len() {
            prefetch_handles[next_prefetch_idx] = Some(spawn_summary_prefetch(
                task,
                &text_route,
                &summary_strategy,
                next_prefetch_idx,
                &batches[next_prefetch_idx],
            ));
            next_prefetch_idx += 1;
        }

        let batch_rows = prepared.batch_rows;
        let summary_usage = prepared.summary_usage;
        if summary_strategy == SUMMARY_MODE_LOCAL_SUMMARY
            || summary_strategy == SUMMARY_MODE_AGENT_SUMMARY
        {
            for row in &batch_rows {
                summary::emit_organize_summary_ready(&app, &task_id, (batch_idx + 1) as u64, row);
            }
        }
        {
            let mut snap = task.snapshot.lock();
            snap.processed_batches = (batch_idx + 1) as u64;
            snap.token_usage.prompt = snap
                .token_usage
                .prompt
                .saturating_add(summary_usage.prompt);
            snap.token_usage.completion = snap
                .token_usage
                .completion
                .saturating_add(summary_usage.completion);
            snap.token_usage.total = snap
                .token_usage
                .total
                .saturating_add(summary_usage.total);
        }
        prepared_batches.push(PreparedClassificationBatch {
            batch_idx,
            batch_rows,
        });
        emit_snapshot(app, state, task).await?;
        task.diagnostics.batch_completed(
            false,
            json!({
                "batchIndex": batch_idx + 1,
                "summaryPreparedRows": prepared_batches.last().map(|batch| batch.batch_rows.len()).unwrap_or(0),
                "summaryUsage": {
                    "prompt": summary_usage.prompt,
                    "completion": summary_usage.completion,
                    "total": summary_usage.total,
                },
            }),
            batch_started_at.elapsed(),
        );
    }

    let all_batch_rows = prepared_batches
        .iter()
        .flat_map(|batch| batch.batch_rows.iter().cloned())
        .collect::<Vec<_>>();
    let mut tree = deterministic_initial_tree(&all_batch_rows);
    let base_tree_version = {
        let snap = task.snapshot.lock();
        snap.tree_version.saturating_add(1)
    };
    if !text_route.api_key.trim().is_empty() && !all_batch_rows.is_empty() {
        match summary::generate_initial_tree(
            &text_route,
            &task.response_language,
            &task.stop,
            &all_batch_rows,
            Some(&task.diagnostics),
        )
        .await
        {
            Ok(output) => {
                {
                    let mut snap = task.snapshot.lock();
                    snap.token_usage.prompt =
                        snap.token_usage.prompt.saturating_add(output.usage.prompt);
                    snap.token_usage.completion = snap
                        .token_usage
                        .completion
                        .saturating_add(output.usage.completion);
                    snap.token_usage.total =
                        snap.token_usage.total.saturating_add(output.usage.total);
                }
                if let Some(err) = output.error {
                    return Err(format!("initial_tree_failed:{err}"));
                }
                if let Some(parsed) = output.parsed {
                    tree = normalize_ai_tree(&parsed, &tree);
                    ensure_uncategorized_leaf(&mut tree);
                }
                if !output.raw_output.trim().is_empty() {
                    let mut snap = task.snapshot.lock();
                    snap.batch_outputs.push(json!({
                        "stage": "initial_tree",
                        "modelRawOutput": output.raw_output,
                    }));
                }
            }
            Err(err) => return Err(format!("initial_tree_failed:{err}")),
        }
    }
    {
        let mut snap = task.snapshot.lock();
        snap.initial_tree = category_tree_to_value(&tree);
        snap.draft_tree = snap.initial_tree.clone();
        snap.tree = snap.initial_tree.clone();
        snap.tree_version = base_tree_version;
        snap.base_tree_version = base_tree_version;
    }
    emit_snapshot(app, state, task).await?;

    let shared_search_calls = Arc::new(AtomicUsize::new(0));
    let shared_search_gate = Arc::new(tokio::sync::Semaphore::new(ORGANIZER_SEARCH_CONCURRENCY));
    let semaphore = Arc::new(tokio::sync::Semaphore::new(CLASSIFICATION_BATCH_CONCURRENCY));
    let mut handles = Vec::new();
    for prepared in prepared_batches.into_iter() {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| format!("classification_concurrency_closed:{e}"))?;
        let task = task.clone();
        let text_route = text_route.clone();
        let base_tree = tree.clone();
        let shared_search_calls = shared_search_calls.clone();
        let shared_search_gate = shared_search_gate.clone();
        let reference_structure = reference_structure.clone();
        let stage = format!("classification_batch_{}", prepared.batch_idx + 1);
        handles.push(tauri::async_runtime::spawn(async move {
            let _permit = permit;
            let output = if text_route.api_key.trim().is_empty() {
                ClassifyOrganizeBatchOutput {
                    parsed: None,
                    usage: TokenUsage::default(),
                    raw_output: String::new(),
                    error: Some("classification_missing_api_key".to_string()),
                    search_calls: 0,
                }
            } else {
                let category_inventory = Vec::new();
                summary::classify_organize_batch(
                    &text_route,
                    &task.response_language,
                    &task.stop,
                    &base_tree,
                    base_tree_version,
                    &prepared.batch_rows,
                    &category_inventory,
                    reference_structure.as_ref(),
                    use_web_search,
                    &task.search_api_key,
                    shared_search_calls,
                    shared_search_gate,
                    Some(&task.diagnostics),
                    &stage,
                )
                .await
                .unwrap_or_else(|err| ClassifyOrganizeBatchOutput {
                    parsed: None,
                    usage: TokenUsage::default(),
                    raw_output: String::new(),
                    error: Some(err),
                    search_calls: 0,
                })
            };
            (prepared, output)
        }));
    }

    let mut batch_outputs = Vec::new();
    let mut proposal_map: HashMap<String, (String, Vec<String>)> = HashMap::new();
    let mut deferred = Vec::new();
    let mut final_assignment_inputs: HashMap<String, (String, Vec<String>, String)> =
        HashMap::new();
    let mut rows_by_id: HashMap<String, (usize, Value)> = HashMap::new();
    let mut classification_errors = Vec::new();
    let mut result_rows = Vec::new();

    for handle in handles {
        let (prepared, output) = handle
            .await
            .map_err(|e| format!("classification_join_failed:{e}"))?;
        {
            let mut snap = task.snapshot.lock();
            snap.token_usage.prompt = snap.token_usage.prompt.saturating_add(output.usage.prompt);
            snap.token_usage.completion = snap
                .token_usage
                .completion
                .saturating_add(output.usage.completion);
            snap.token_usage.total = snap.token_usage.total.saturating_add(output.usage.total);
        }
        for row in &prepared.batch_rows {
            if let Some(item_id) = row.get("itemId").and_then(Value::as_str) {
                rows_by_id.insert(item_id.to_string(), (prepared.batch_idx, row.clone()));
            }
        }

        let mut batch_error = output.error.clone();
        let parsed = output.parsed.clone().unwrap_or_else(|| json!({}));
        if batch_error.is_none()
            && parsed.get("baseTreeVersion").and_then(Value::as_u64) != Some(base_tree_version)
        {
            batch_error = Some(format!(
                "classification_batch_base_tree_version_mismatch:expected={base_tree_version},actual={}",
                parsed
                    .get("baseTreeVersion")
                    .and_then(Value::as_u64)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "missing".to_string())
            ));
        }
        let assignment_count = parsed
            .get("assignments")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0)
            + parsed
                .get("deferredAssignments")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
        if batch_error.is_none() && assignment_count < prepared.batch_rows.len() {
            batch_error = Some(format!(
                "classification response missing assignments for {} item(s)",
                prepared.batch_rows.len().saturating_sub(assignment_count)
            ));
        }
        let batch_record = json!({
            "batchIndex": prepared.batch_idx + 1,
            "baseTreeVersion": base_tree_version,
            "output": parsed,
            "error": batch_error.clone().unwrap_or_default(),
            "searchCalls": output.search_calls,
            "modelRawOutput": output.raw_output,
        });
        batch_outputs.push(batch_record);
        if let Some(err) = batch_error {
            classification_errors.push(json!({
                "batchIndex": prepared.batch_idx + 1,
                "kind": "classification_error",
                "message": err,
            }));
            for (row_offset, row) in prepared.batch_rows.iter().enumerate() {
                result_rows.push(classification_error_row(
                    &task_id,
                    result_rows.len() as u64 + 1,
                    prepared.batch_idx,
                    row,
                    classification_errors
                        .last()
                        .and_then(|value| value.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("classification_error"),
                    if row_offset == 0 { &output.raw_output } else { "" },
                ));
            }
            continue;
        }

        for proposal in parsed
            .get("treeProposals")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let proposal_id = proposal
                .get("proposalId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if proposal_id.is_empty() {
                continue;
            }
            let path = category_path_from_value(proposal.get("suggestedPath"));
            let leaf = if !path.is_empty() {
                ensure_path(&mut tree, &path)
            } else if let Some(target_id) = proposal.get("targetNodeId").and_then(Value::as_str) {
                target_id.to_string()
            } else {
                ensure_uncategorized_leaf(&mut tree)
            };
            let resolved_path = category_path_for_id(&tree, &leaf)
                .unwrap_or_else(|| vec![UNCATEGORIZED_NODE_NAME.to_string()]);
            proposal_map.insert(proposal_id, (leaf, resolved_path));
        }

        for assignment in parsed
            .get("assignments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let Some(item_id) = assignment.get("itemId").and_then(Value::as_str) else {
                continue;
            };
            let Some(leaf_node_id) = assignment.get("leafNodeId").and_then(Value::as_str) else {
                continue;
            };
            let Some(path) = category_path_for_id(&tree, leaf_node_id) else {
                classification_errors.push(json!({
                    "batchIndex": prepared.batch_idx + 1,
                    "kind": "validation_error",
                    "message": format!("assignment references missing leafNodeId:{leaf_node_id}"),
                    "itemId": item_id,
                }));
                continue;
            };
            final_assignment_inputs.insert(
                item_id.to_string(),
                (
                    leaf_node_id.to_string(),
                    path,
                    assignment
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ),
            );
        }
        deferred.extend(
            parsed
                .get("deferredAssignments")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
    }

    for assignment in deferred {
        let Some(item_id) = assignment.get("itemId").and_then(Value::as_str) else {
            continue;
        };
        let Some(proposal_id) = assignment.get("proposalId").and_then(Value::as_str) else {
            continue;
        };
        if let Some((leaf, path)) = proposal_map.get(proposal_id).cloned() {
            final_assignment_inputs.insert(
                item_id.to_string(),
                (
                    leaf,
                    path,
                    assignment
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("resolved_deferred_assignment")
                        .to_string(),
                ),
            );
        } else {
            classification_errors.push(json!({
                "kind": "validation_error",
                "message": format!("deferred assignment references unresolved proposalId:{proposal_id}"),
                "itemId": item_id,
            }));
        }
    }

    if !text_route.api_key.trim().is_empty() && !rows_by_id.is_empty() {
        match summary::reconcile_organize_batches(
            &text_route,
            &task.response_language,
            &task.stop,
            &category_tree_to_value(&tree),
            &batch_outputs,
            &all_batch_rows,
            Some(&task.diagnostics),
        )
        .await
        {
            Ok(output) => {
                {
                    let mut snap = task.snapshot.lock();
                    snap.token_usage.prompt =
                        snap.token_usage.prompt.saturating_add(output.usage.prompt);
                    snap.token_usage.completion = snap
                        .token_usage
                        .completion
                        .saturating_add(output.usage.completion);
                    snap.token_usage.total =
                        snap.token_usage.total.saturating_add(output.usage.total);
                }
                if let Some(err) = output.error {
                    return Err(format!("reconcile_failed:{err}"));
                }
                if let Some(parsed) = output.parsed {
                    let final_tree = parsed
                        .get("finalTree")
                        .cloned()
                        .ok_or_else(|| "reconcile_failed:missing finalTree".to_string())?;
                    let reconciled_tree = tree_from_value(&final_tree);
                    let mut reconciled_assignments = HashMap::new();
                    for assignment in parsed
                        .get("finalAssignments")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                    {
                        let Some(item_id) = assignment.get("itemId").and_then(Value::as_str) else {
                            continue;
                        };
                        let Some(leaf_node_id) =
                            assignment.get("leafNodeId").and_then(Value::as_str)
                        else {
                            continue;
                        };
                        let fallback_path = category_path_from_value(assignment.get("categoryPath"));
                        let path = category_path_for_id(&reconciled_tree, leaf_node_id)
                            .or_else(|| {
                                if fallback_path.is_empty() {
                                    None
                                } else {
                                    Some(fallback_path)
                                }
                            })
                            .ok_or_else(|| {
                                format!(
                                    "reconcile_failed:final assignment references missing leafNodeId:{leaf_node_id}"
                                )
                            })?;
                        reconciled_assignments.insert(
                            item_id.to_string(),
                            (
                                leaf_node_id.to_string(),
                                path,
                                assignment
                                    .get("reason")
                                    .and_then(Value::as_str)
                                    .unwrap_or("reconciled_assignment")
                                    .to_string(),
                            ),
                        );
                    }
                    tree = reconciled_tree;
                    final_assignment_inputs = reconciled_assignments;
                    proposal_map.clear();
                    for mapping in parsed
                        .get("proposalMappings")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                    {
                        let Some(proposal_id) = mapping.get("proposalId").and_then(Value::as_str)
                        else {
                            continue;
                        };
                        let leaf = mapping
                            .get("leafNodeId")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let path = category_path_for_id(&tree, &leaf)
                            .unwrap_or_else(|| category_path_from_value(mapping.get("categoryPath")));
                        proposal_map.insert(proposal_id.to_string(), (leaf, path));
                    }
                }
                if !output.raw_output.trim().is_empty() {
                    batch_outputs.push(json!({
                        "stage": "reconcile",
                        "modelRawOutput": output.raw_output,
                    }));
                }
            }
            Err(err) => return Err(format!("reconcile_failed:{err}")),
        }
    }

    for (item_id, (batch_idx, row)) in rows_by_id.iter() {
        if result_rows.iter().any(|result| {
            result.get("itemId").and_then(Value::as_str) == Some(item_id.as_str())
                && row_has_classification_error(result)
        }) {
            continue;
        }
        if let Some((leaf, path, reason)) = final_assignment_inputs.get(item_id) {
            result_rows.push(result_row_from_assignment(
                &task_id,
                result_rows.len() as u64 + 1,
                *batch_idx,
                row,
                leaf,
                path.clone(),
                reason,
                "",
                "",
            ));
        } else {
            result_rows.push(classification_error_row(
                &task_id,
                result_rows.len() as u64 + 1,
                *batch_idx,
                row,
                "final_assignment_missing_after_reconciliation",
                "",
            ));
        }
    }

    result_rows.sort_by_key(|row| row.get("index").and_then(Value::as_u64).unwrap_or(0));
    persist::upsert_organize_results(&state.db_path(), &task_id, &result_rows)?;
    for row in &result_rows {
        app.emit("organize_file_done", row.clone())
            .map_err(|e| e.to_string())?;
    }
    {
        let mut snap = task.snapshot.lock();
        snap.processed_files = result_rows.len() as u64;
        snap.processed_batches = total_batches;
        snap.results = result_rows.clone();
        snap.batch_outputs = batch_outputs;
        snap.tree_proposals = proposal_map
            .iter()
            .map(|(proposal_id, (leaf_node_id, category_path))| {
                json!({
                    "proposalId": proposal_id,
                    "status": "accepted",
                    "leafNodeId": leaf_node_id,
                    "categoryPath": category_path,
                })
            })
            .collect();
        snap.proposal_mappings = snap.tree_proposals.clone();
        snap.classification_errors = classification_errors;
        snap.final_tree = category_tree_to_value(&tree);
        snap.draft_tree = snap.final_tree.clone();
        snap.tree = snap.final_tree.clone();
        snap.final_assignments = final_assignment_inputs
            .iter()
            .map(|(item_id, (leaf_node_id, category_path, reason))| {
                json!({
                    "itemId": item_id,
                    "leafNodeId": leaf_node_id,
                    "categoryPath": category_path,
                    "reason": reason,
                })
            })
            .collect();
        snap.review_issues = Vec::new();
    }
    emit_snapshot(app, state, task).await?;

    let final_snapshot = {
        let mut snap = task.snapshot.lock();
        snap.results
            .sort_by_key(|x| x.get("index").and_then(Value::as_u64).unwrap_or(0));
        snap.preview = planner::build_preview(&snap.root_path, &snap.results);
        snap.tree = snap.final_tree.clone();
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

