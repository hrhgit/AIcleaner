struct PreparedSummaryBatch {
    batch_idx: usize,
    batch_rows: Vec<Value>,
    summary_usage: TokenUsage,
}

struct PreparedLocalSummaryUnit {
    unit: OrganizeUnit,
    route: RouteConfig,
    extraction: Option<SummaryExtraction>,
    local_result: SummaryBuildResult,
}

#[derive(Clone, Copy)]
struct SummaryAgentBatchConfig {
    max_chars: usize,
    max_items: usize,
    flush_ms: u64,
    max_in_flight: usize,
}

impl Default for SummaryAgentBatchConfig {
    fn default() -> Self {
        Self {
            max_chars: SUMMARY_AGENT_BATCH_MAX_CHARS,
            max_items: SUMMARY_AGENT_BATCH_MAX_ITEMS,
            flush_ms: SUMMARY_AGENT_BATCH_FLUSH_MS,
            max_in_flight: SUMMARY_AGENT_MAX_IN_FLIGHT,
        }
    }
}

#[derive(Default)]
struct SummaryAgentSchedulerStats {
    agent_batch_count: usize,
    total_chars: usize,
    max_batch_chars: usize,
    failed_batches: usize,
    degraded_items: usize,
}

struct SummaryAgentPendingItem {
    unit_idx: usize,
    row: Value,
    local_result: SummaryBuildResult,
}

#[derive(Default)]
struct SummaryAgentMicroBatch {
    items: Vec<SummaryAgentPendingItem>,
    char_count: usize,
}

struct SummaryAgentMicroBatchRun {
    batch_idx: usize,
    rows: Vec<(usize, Value)>,
    usage: TokenUsage,
    char_count: usize,
    item_count: usize,
    failed: bool,
    error: Option<String>,
}

#[derive(Clone, Copy)]
struct ExtractionProfile {
    label: &'static str,
    global_cost: u32,
    uses_tika: bool,
    uses_heavy_doc: bool,
}

#[derive(Default)]
struct ExtractionSchedulerStats {
    prepared_units: usize,
    text_units: usize,
    tika_units: usize,
    heavy_doc_units: usize,
    fallback_units: usize,
    total_cost: u64,
}

struct ExtractionPermitSet {
    _global: tokio::sync::OwnedSemaphorePermit,
    _tika: Option<tokio::sync::OwnedSemaphorePermit>,
    _heavy_doc: Option<tokio::sync::OwnedSemaphorePermit>,
}

#[derive(Clone)]
struct ExtractionGates {
    global: Arc<tokio::sync::Semaphore>,
    tika: Arc<tokio::sync::Semaphore>,
    heavy_doc: Arc<tokio::sync::Semaphore>,
}

struct ClassificationBatchRun {
    prepared: PreparedClassificationBatch,
    output: ClassifyOrganizeBatchOutput,
    attempt_errors: Vec<String>,
}

fn abort_initial_tree_task(handle: &mut Option<JoinHandle<Result<InitialTreeOutput, String>>>) {
    if let Some(handle) = handle.take() {
        handle.abort();
    }
}

fn is_retryable_classification_error(error: &str) -> bool {
    let lower = error.trim().to_ascii_lowercase();
    if lower.is_empty() || lower == "stop_requested" {
        return false;
    }
    [
        "http 408", "http 429", "http 500", "http 502", "http 503", "http 504",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || lower.contains("request_timeout")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connect")
        || lower.contains("connection reset")
        || lower.contains("connection closed")
        || lower.contains("connection aborted")
}

fn classification_retry_error_summary(attempt_errors: &[String]) -> String {
    attempt_errors
        .iter()
        .map(|error| error.trim())
        .filter(|error| !error.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

async fn run_classification_round(
    task: &Arc<OrganizeTaskRuntime>,
    text_route: &RouteConfig,
    base_tree: &CategoryTreeNode,
    base_tree_version: u64,
    pending_batches: Vec<(PreparedClassificationBatch, Vec<String>)>,
    reference_structure: Option<String>,
    use_web_search: bool,
    shared_search_calls: Arc<AtomicUsize>,
    shared_search_gate: Arc<tokio::sync::Semaphore>,
    concurrency: usize,
    attempt: usize,
) -> Result<Vec<ClassificationBatchRun>, String> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
    let mut handles: Vec<JoinHandle<ClassificationBatchRun>> = Vec::new();
    for (prepared, attempt_errors) in pending_batches.into_iter() {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| format!("classification_concurrency_closed:{e}"))?;
        let task = task.clone();
        let text_route = text_route.clone();
        let base_tree = base_tree.clone();
        let shared_search_calls = shared_search_calls.clone();
        let shared_search_gate = shared_search_gate.clone();
        let reference_structure = reference_structure.clone();
        let stage = if attempt == 1 {
            format!("classification_batch_{}", prepared.batch_idx + 1)
        } else {
            format!(
                "classification_batch_{}_retry_{}_concurrency_{}",
                prepared.batch_idx + 1,
                attempt,
                concurrency.max(1)
            )
        };
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
            ClassificationBatchRun {
                prepared,
                output,
                attempt_errors,
            }
        }));
    }

    let mut runs = Vec::new();
    for handle in handles {
        runs.push(
            handle
                .await
                .map_err(|e| format!("classification_join_failed:{e}"))?,
        );
    }
    Ok(runs)
}

async fn run_classification_scheduler(
    app: &AppHandle<impl Runtime>,
    state: &AppState,
    task: &Arc<OrganizeTaskRuntime>,
    text_route: &RouteConfig,
    base_tree: &CategoryTreeNode,
    base_tree_version: u64,
    prepared_batches: Vec<PreparedClassificationBatch>,
    reference_structure: Option<String>,
    use_web_search: bool,
    shared_search_calls: Arc<AtomicUsize>,
    shared_search_gate: Arc<tokio::sync::Semaphore>,
    initial_concurrency: usize,
) -> Result<(Vec<ClassificationBatchRun>, usize, usize, u64), String> {
    let mut pending = prepared_batches
        .into_iter()
        .map(|batch| (batch, Vec::new()))
        .collect::<Vec<_>>();
    let mut completed = Vec::new();
    let mut concurrency = initial_concurrency.max(1);
    let mut attempt = 1usize;
    let mut retry_rounds = 0usize;
    let total_batches = pending.len() as u64;
    let mut completed_batches = 0_u64;
    let mut model_request_count = 0_u64;

    loop {
        let runs = run_classification_round(
            task,
            text_route,
            base_tree,
            base_tree_version,
            pending,
            reference_structure.clone(),
            use_web_search,
            shared_search_calls.clone(),
            shared_search_gate.clone(),
            concurrency,
            attempt,
        )
        .await?;
        if !text_route.api_key.trim().is_empty() {
            model_request_count = model_request_count.saturating_add(runs.len() as u64);
        }
        let mut retryable = Vec::new();
        for mut run in runs {
            if let Some(error) = run.output.error.as_deref() {
                let retryable_error = is_retryable_classification_error(error);
                run.attempt_errors
                    .push(format!("concurrency={concurrency}: {error}"));
                if retryable_error && concurrency > 1 && !task.stop.load(Ordering::Relaxed) {
                    retryable.push((run.prepared, run.attempt_errors));
                    continue;
                }
                if retryable_error && !run.attempt_errors.is_empty() {
                    let summary = classification_retry_error_summary(&run.attempt_errors);
                    run.output.error = Some(format!(
                        "classification_retry_exhausted after concurrency degraded to {concurrency}: {error}; attempts: {summary}"
                    ));
                }
            }
            completed_batches = completed_batches.saturating_add(1).min(total_batches);
            {
                let mut snap = task.snapshot.lock();
                set_organize_progress(
                    &mut snap,
                    "classification",
                    "Classifying batches",
                    Some(format!(
                        "Completed classification batch {completed_batches} of {total_batches}."
                    )),
                    Some(completed_batches),
                    Some(total_batches),
                    Some("batches"),
                    total_batches == 0,
                );
            }
            emit_snapshot(app, state, task).await?;
            completed.push(run);
        }

        if retryable.is_empty() {
            break;
        }

        retry_rounds = retry_rounds.saturating_add(1);
        let next_concurrency = (concurrency / 2).max(1);
        task.diagnostics.stage_completed(
            "classification_retry",
            json!({
                "attempt": attempt,
                "retryableBatches": retryable.len(),
                "previousConcurrency": concurrency,
                "nextConcurrency": next_concurrency,
            }),
            Duration::from_millis(0),
        );
        {
            let mut snap = task.snapshot.lock();
            set_organize_progress(
                &mut snap,
                "classification",
                "Classifying batches",
                Some(format!(
                    "Retrying {} batch(es) at concurrency {next_concurrency}.",
                    retryable.len()
                )),
                Some(completed_batches),
                Some(total_batches),
                Some("batches"),
                false,
            );
        }
        emit_snapshot(app, state, task).await?;
        let delay_ms = match retry_rounds {
            1 => 500,
            2 => 1000,
            _ => 2000,
        };
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        pending = retryable;
        concurrency = next_concurrency;
        attempt = attempt.saturating_add(1);
    }

    completed.sort_by_key(|run| run.prepared.batch_idx);
    Ok((completed, concurrency, retry_rounds, model_request_count))
}

fn add_token_usage(total: &mut TokenUsage, usage: &TokenUsage) {
    total.prompt = total.prompt.saturating_add(usage.prompt);
    total.completion = total.completion.saturating_add(usage.completion);
    total.total = total.total.saturating_add(usage.total);
}

fn token_usage_json(usage: &TokenUsage) -> Value {
    json!({
        "prompt": usage.prompt,
        "completion": usage.completion,
        "total": usage.total,
    })
}

fn token_usage_by_stage_json(
    summary: &TokenUsage,
    initial_tree: &TokenUsage,
    classification: &TokenUsage,
    reconcile: &TokenUsage,
) -> Value {
    json!({
        "summaryPreparation": token_usage_json(summary),
        "initialTree": token_usage_json(initial_tree),
        "classification": token_usage_json(classification),
        "reconcile": token_usage_json(reconcile),
    })
}

fn pending_reconcile_input(
    batch_index: usize,
    base_tree_version: u64,
    parsed: &Value,
    batch_error: Option<&str>,
) -> Option<Value> {
    let tree_proposals = compact_reconcile_tree_proposals(
        parsed
            .get("treeProposals")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
    );
    let deferred_assignments = compact_reconcile_deferred_assignments(
        parsed
            .get("deferredAssignments")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
    );
    let error = batch_error.unwrap_or("").trim();

    if tree_proposals.is_empty() && deferred_assignments.is_empty() && error.is_empty() {
        return None;
    }

    Some(json!({
        "batchIndex": batch_index,
        "baseTreeVersion": base_tree_version,
        "treeProposals": tree_proposals,
        "deferredAssignments": deferred_assignments,
        "error": error,
    }))
}

fn compact_reconcile_tree_proposals(values: &[Value]) -> Vec<Value> {
    values
        .iter()
        .filter_map(|value| {
            let proposal_id = value.get("proposalId").and_then(Value::as_str)?.trim();
            if proposal_id.is_empty() {
                return None;
            }
            let mut out = json!({ "proposalId": proposal_id });
            for key in [
                "operation",
                "targetNodeId",
                "sourceNodeIds",
                "suggestedName",
                "suggestedPath",
                "evidenceItemIds",
            ] {
                if let Some(field) = value.get(key) {
                    if !field.is_null() {
                        out[key] = field.clone();
                    }
                }
            }
            Some(out)
        })
        .collect()
}

fn compact_reconcile_deferred_assignments(values: &[Value]) -> Vec<Value> {
    values
        .iter()
        .filter_map(|value| {
            let item_id = value.get("itemId").and_then(Value::as_str)?.trim();
            if item_id.is_empty() {
                return None;
            }
            let mut out = json!({ "itemId": item_id });
            for key in ["proposalId", "suggestedPath", "confidence"] {
                if let Some(field) = value.get(key) {
                    if !field.is_null() {
                        out[key] = field.clone();
                    }
                }
            }
            Some(out)
        })
        .collect()
}
fn refresh_assignment_paths(
    assignments: &mut HashMap<String, (String, Vec<String>, String)>,
    tree: &CategoryTreeNode,
) -> Result<(), String> {
    for (item_id, (leaf_node_id, category_path, _reason)) in assignments.iter_mut() {
        let Some(path) = category_path_for_id(tree, leaf_node_id) else {
            return Err(format!(
                "reconcile_failed:existing assignment for {item_id} references missing leafNodeId:{leaf_node_id}"
            ));
        };
        *category_path = path;
    }
    Ok(())
}

fn route_for_unit(task: &OrganizeTaskRuntime, unit: &OrganizeUnit) -> RouteConfig {
    let route_key = if unit.item_type == "directory" {
        "text"
    } else {
        unit.modality.as_str()
    };
    task.routes
        .get(route_key)
        .or_else(|| task.routes.get("text"))
        .cloned()
        .unwrap_or(RouteConfig {
            endpoint: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            model: "gpt-4o-mini".to_string(),
        })
}

fn extraction_profile(summary_strategy: &str, unit: &OrganizeUnit, task: &OrganizeTaskRuntime) -> ExtractionProfile {
    if summary_strategy == SUMMARY_MODE_FILENAME_ONLY {
        return ExtractionProfile {
            label: "metadata_only",
            global_cost: 1,
            uses_tika: false,
            uses_heavy_doc: false,
        };
    }
    if unit.item_type == "directory" {
        return ExtractionProfile {
            label: "directory",
            global_cost: 1,
            uses_tika: false,
            uses_heavy_doc: false,
        };
    }

    let ext = extension_key(Path::new(&unit.path));
    let can_call_tika = task.extraction_tool.tika_ready
        && unit.size <= TIKA_MAX_UPLOAD_BYTES
        && summary::supports_tika_extraction(unit);
    if can_call_tika {
        let is_spreadsheet = matches!(ext.as_str(), ".xls" | ".xlsx" | ".ods");
        if is_spreadsheet {
            return ExtractionProfile {
                label: "heavy_spreadsheet",
                global_cost: 16,
                uses_tika: true,
                uses_heavy_doc: true,
            };
        }
        if unit.size >= 8 * 1024 * 1024 {
            return ExtractionProfile {
                label: "heavy_document",
                global_cost: 8,
                uses_tika: true,
                uses_heavy_doc: true,
            };
        }
        return ExtractionProfile {
            label: "tika_document",
            global_cost: 4,
            uses_tika: true,
            uses_heavy_doc: false,
        };
    }

    ExtractionProfile {
        label: if summary::supports_tika_extraction(unit) {
            "fallback_document"
        } else {
            "plain_text"
        },
        global_cost: 1,
        uses_tika: false,
        uses_heavy_doc: false,
    }
}

async fn acquire_extraction_permits(
    profile: ExtractionProfile,
    gates: &ExtractionGates,
) -> Result<ExtractionPermitSet, String> {
    let heavy_doc = if profile.uses_heavy_doc {
        Some(
            gates
                .heavy_doc
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| format!("extraction_heavy_doc_gate_closed:{e}"))?,
        )
    } else {
        None
    };
    let tika = if profile.uses_tika {
        Some(
            gates
                .tika
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| format!("extraction_tika_gate_closed:{e}"))?,
        )
    } else {
        None
    };
    let global = gates
        .global
        .clone()
        .acquire_many_owned(profile.global_cost.max(1))
        .await
        .map_err(|e| format!("extraction_global_budget_closed:{e}"))?;
    Ok(ExtractionPermitSet {
        _global: global,
        _tika: tika,
        _heavy_doc: heavy_doc,
    })
}

fn filename_only_summary(unit: &OrganizeUnit) -> SummaryBuildResult {
    SummaryBuildResult {
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
    }
}

async fn prepare_summary_unit(
    task: Arc<OrganizeTaskRuntime>,
    summary_strategy: String,
    unit: OrganizeUnit,
) -> Result<PreparedLocalSummaryUnit, String> {
    if task.stop.load(Ordering::Relaxed) {
        return Err("stop_requested".to_string());
    }
    let route = route_for_unit(&task, &unit);
    let extracted = match summary_strategy.as_str() {
        SUMMARY_MODE_FILENAME_ONLY => None,
        _ => Some(
            summary::extract_unit_content_for_summary_with_tools(
                &unit,
                &task.response_language,
                &task.stop,
                &task.extraction_tool,
            )
            .await,
        ),
    };
    if task.stop.load(Ordering::Relaxed) {
        return Err("stop_requested".to_string());
    }
    let local_result = match summary_strategy.as_str() {
        SUMMARY_MODE_FILENAME_ONLY => filename_only_summary(&unit),
        _ => summary::build_local_summary(
            &unit,
            extracted.as_ref().unwrap_or(&SummaryExtraction::default()),
        ),
    };
    Ok(PreparedLocalSummaryUnit {
        unit,
        route,
        extraction: extracted,
        local_result,
    })
}

async fn prepare_summary_units_weighted(
    task: Arc<OrganizeTaskRuntime>,
    summary_strategy: &str,
    units: &[OrganizeUnit],
) -> Result<(Vec<PreparedLocalSummaryUnit>, ExtractionSchedulerStats), String> {
    let mut stats = ExtractionSchedulerStats::default();
    if units.is_empty() {
        return Ok((Vec::new(), stats));
    }

    let profiles = units
        .iter()
        .map(|unit| {
            let profile = extraction_profile(summary_strategy, unit, &task);
            stats.prepared_units += 1;
            stats.total_cost += profile.global_cost as u64;
            if profile.uses_tika {
                stats.tika_units += 1;
            } else if profile.label == "fallback_document" {
                stats.fallback_units += 1;
            } else {
                stats.text_units += 1;
            }
            if profile.uses_heavy_doc {
                stats.heavy_doc_units += 1;
            }
            profile
        })
        .collect::<Vec<_>>();
    let mut order = (0..units.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| {
        profiles[*right]
            .global_cost
            .cmp(&profiles[*left].global_cost)
            .then_with(|| units[*right].size.cmp(&units[*left].size))
            .then_with(|| left.cmp(right))
    });

    let gates = ExtractionGates {
        global: Arc::new(tokio::sync::Semaphore::new(EXTRACTION_GLOBAL_BUDGET as usize)),
        tika: Arc::new(tokio::sync::Semaphore::new(EXTRACTION_TIKA_HARD_CAP)),
        heavy_doc: Arc::new(tokio::sync::Semaphore::new(EXTRACTION_HEAVY_DOC_HARD_CAP)),
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(units.len());
    let mut handles: Vec<JoinHandle<()>> = Vec::new();
    for unit_idx in order {
        let profile = profiles[unit_idx];
        let task = task.clone();
        let tx = tx.clone();
        let gates = gates.clone();
        let summary_strategy = summary_strategy.to_string();
        let unit = units[unit_idx].clone();
        handles.push(tauri::async_runtime::spawn(async move {
            let result = async {
                if task.stop.load(Ordering::Relaxed) {
                    return Err("stop_requested".to_string());
                }
                let permits = acquire_extraction_permits(profile, &gates).await?;
                let _permits = permits;
                prepare_summary_unit(task, summary_strategy, unit).await
            }
            .await;
            let _ = tx.send((unit_idx, result)).await;
        }));
    }
    drop(tx);

    let mut prepared = (0..units.len()).map(|_| None).collect::<Vec<_>>();
    let mut completed = 0usize;
    while let Some((unit_idx, result)) = rx.recv().await {
        match result {
            Ok(unit) => {
                prepared[unit_idx] = Some(unit);
                completed += 1;
            }
            Err(err) => {
                for handle in &handles {
                    handle.abort();
                }
                return Err(err);
            }
        }
    }
    if completed != units.len() {
        for handle in &handles {
            handle.abort();
        }
        return Err(format!(
            "summary_extraction_incomplete:completed={},expected={}",
            completed,
            units.len()
        ));
    }
    for handle in handles {
        handle
            .await
            .map_err(|e| format!("summary_extraction_join_failed:{e}"))?;
    }

    Ok((
        prepared
            .into_iter()
            .map(|unit| unit.ok_or_else(|| "summary_extraction_missing_unit".to_string()))
            .collect::<Result<Vec<_>, _>>()?,
        stats,
    ))
}

fn extraction_json(extracted: Option<&SummaryExtraction>) -> Value {
    extracted
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
        .unwrap_or(Value::Null)
}

fn basic_batch_row(batch_idx: usize, offset: usize, unit: &OrganizeUnit) -> Value {
    json!({
        "itemId": format!("batch{}_{}", batch_idx + 1, offset + 1),
        "name": unit.name.clone(),
        "path": unit.path.clone(),
        "relativePath": unit.relative_path.clone(),
        "size": unit.size,
        "createdAt": unit.created_at.clone(),
        "modifiedAt": unit.modified_at.clone(),
        "itemType": unit.item_type.clone(),
        "modality": unit.modality.clone(),
    })
}

fn build_basic_batch_rows(units: &[OrganizeUnit], batch_size: u32) -> Vec<Vec<Value>> {
    let batch_size = (batch_size as usize).max(1);
    units
        .chunks(batch_size)
        .enumerate()
        .map(|(batch_idx, batch)| {
            batch
                .iter()
                .enumerate()
                .map(|(offset, unit)| basic_batch_row(batch_idx, offset, unit))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn summary_batch_row(
    batch_idx: usize,
    offset: usize,
    base_row: &Value,
    prepared: &PreparedLocalSummaryUnit,
    summary_strategy: &str,
) -> Result<Value, String> {
    let mut row = base_row.clone();
    if !row.is_object() {
        return Err(format!(
            "summary_batch_base_row_invalid:batch={},offset={}",
            batch_idx + 1,
            offset + 1
        ));
    }
    row["summaryStrategy"] = Value::String(summary_strategy.to_string());
    row["representation"] = prepared.local_result.representation.to_value();
    row["summaryDegraded"] = Value::Bool(prepared.local_result.representation.degraded);
    row["summaryWarnings"] = Value::Array(
        prepared
            .local_result
            .warnings
            .iter()
            .cloned()
            .map(Value::String)
            .collect::<Vec<_>>(),
    );
    row["localExtraction"] = extraction_json(prepared.extraction.as_ref());
    row["provider"] = Value::String(prepared.route.endpoint.clone());
    row["model"] = Value::String(prepared.route.model.clone());
    Ok(row)
}

fn apply_summary_agent_output_to_rows(
    batch_rows: &mut [Value],
    local_results: &[SummaryBuildResult],
    output: &SummaryAgentBatchOutput,
) -> usize {
    let batch_failed_warning = output
        .error
        .clone()
        .unwrap_or_else(|| "summary_agent_missing_items".to_string());
    let mut degraded_items = 0usize;
    for (idx, row) in batch_rows.iter_mut().enumerate() {
        let item_id = row.get("itemId").and_then(Value::as_str).unwrap_or("");
        let local_result = local_results.get(idx).cloned().unwrap_or_default();

        if row
            .get("summaryDegraded")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
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
            degraded_items = degraded_items.saturating_add(1);
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
    degraded_items
}

async fn prepare_summary_batch_from_units(
    task: Arc<OrganizeTaskRuntime>,
    text_route: RouteConfig,
    summary_strategy: String,
    batch_idx: usize,
    prepared_units: Vec<PreparedLocalSummaryUnit>,
    base_rows: Vec<Value>,
) -> Result<PreparedSummaryBatch, String> {
    if prepared_units.len() != base_rows.len() {
        return Err(format!(
            "summary_batch_base_row_count_mismatch:batch={},prepared={},base={}",
            batch_idx + 1,
            prepared_units.len(),
            base_rows.len()
        ));
    }
    let mut batch_rows = Vec::new();
    let mut local_results = Vec::new();
    for (offset, prepared) in prepared_units.iter().enumerate() {
        if task.stop.load(Ordering::Relaxed) {
            return Err("stop_requested".to_string());
        }
        batch_rows.push(summary_batch_row(
            batch_idx,
            offset,
            &base_rows[offset],
            prepared,
            &summary_strategy,
        )?);
        local_results.push(prepared.local_result.clone());
    }

    let mut summary_usage = TokenUsage::default();
    if summary_strategy == SUMMARY_MODE_AGENT_SUMMARY {
        let batch_rows_for_agent: Vec<Value> = batch_rows
            .iter()
            .filter(|row| {
                !row.get("summaryDegraded")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
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
        summary_usage = output.usage.clone();
        apply_summary_agent_output_to_rows(&mut batch_rows, &local_results, &output);
    }

    Ok(PreparedSummaryBatch {
        batch_idx,
        batch_rows,
        summary_usage,
    })
}

#[cfg(test)]
async fn prepare_summary_batch(
    task: Arc<OrganizeTaskRuntime>,
    text_route: RouteConfig,
    summary_strategy: String,
    batch_idx: usize,
    batch: Vec<OrganizeUnit>,
) -> Result<PreparedSummaryBatch, String> {
    let (prepared_units, _) =
        prepare_summary_units_weighted(task.clone(), &summary_strategy, &batch).await?;
    let base_rows = batch
        .iter()
        .enumerate()
        .map(|(offset, unit)| basic_batch_row(batch_idx, offset, unit))
        .collect::<Vec<_>>();
    prepare_summary_batch_from_units(
        task,
        text_route,
        summary_strategy,
        batch_idx,
        prepared_units,
        base_rows,
    )
    .await
}

fn summary_agent_payload_char_count(batch_rows: &[Value], response_language: &str) -> usize {
    let payload = json!({
        "mode": SUMMARY_MODE_AGENT_SUMMARY,
        "outputLanguage": localized_language_name(response_language, response_language),
        "fileIndex": batch_rows.iter().map(|row| json!({
            "itemId": row.get("itemId").and_then(Value::as_str).unwrap_or(""),
            "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
            "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
            "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
            "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
        })).collect::<Vec<_>>(),
        "items": batch_rows.iter().map(|row| json!({
            "itemId": row.get("itemId").and_then(Value::as_str).unwrap_or(""),
            "name": row.get("name").and_then(Value::as_str).unwrap_or(""),
            "relativePath": row.get("relativePath").and_then(Value::as_str).unwrap_or(""),
            "itemType": row.get("itemType").and_then(Value::as_str).unwrap_or("file"),
            "modality": row.get("modality").and_then(Value::as_str).unwrap_or("text"),
            "representation": row.get("representation").cloned().unwrap_or_else(|| FileRepresentation::default().to_value()),
            "summaryWarnings": row.get("summaryWarnings").cloned().unwrap_or(Value::Array(Vec::new())),
            "localExtraction": row.get("localExtraction").cloned().unwrap_or(Value::Null),
        })).collect::<Vec<_>>(),
    });
    payload.to_string().chars().count()
}

fn summary_agent_microbatch_char_count(
    items: &[SummaryAgentPendingItem],
    response_language: &str,
) -> usize {
    let rows = items
        .iter()
        .map(|item| item.row.clone())
        .collect::<Vec<_>>();
    summary_agent_payload_char_count(&rows, response_language)
}

fn candidate_summary_agent_char_count(
    current: &[SummaryAgentPendingItem],
    row: &Value,
    response_language: &str,
) -> usize {
    let mut rows = current
        .iter()
        .map(|item| item.row.clone())
        .collect::<Vec<_>>();
    rows.push(row.clone());
    summary_agent_payload_char_count(&rows, response_language)
}

async fn spawn_summary_agent_microbatch(
    handles: &mut Vec<JoinHandle<SummaryAgentMicroBatchRun>>,
    task: Arc<OrganizeTaskRuntime>,
    text_route: RouteConfig,
    in_flight: Arc<tokio::sync::Semaphore>,
    batch: SummaryAgentMicroBatch,
    agent_batch_idx: usize,
) -> Result<(), String> {
    if batch.items.is_empty() {
        return Ok(());
    }
    let permit = in_flight
        .acquire_owned()
        .await
        .map_err(|e| format!("summary_agent_in_flight_closed:{e}"))?;
    handles.push(tauri::async_runtime::spawn(async move {
        let _permit = permit;
        let rows = batch
            .items
            .iter()
            .map(|item| item.row.clone())
            .collect::<Vec<_>>();
        let local_results = batch
            .items
            .iter()
            .map(|item| item.local_result.clone())
            .collect::<Vec<_>>();
        let unit_indices = batch
            .items
            .iter()
            .map(|item| item.unit_idx)
            .collect::<Vec<_>>();
        let output = summary::summarize_batch_with_agent(
            &text_route,
            &task.response_language,
            &task.stop,
            &rows,
            Some(&task.diagnostics),
            &format!("summary_agent_batch_{}", agent_batch_idx + 1),
        )
        .await;
        let failed = output.error.is_some();
        let error = output.error.clone();
        let usage = output.usage.clone();
        let mut resolved_rows = rows;
        apply_summary_agent_output_to_rows(&mut resolved_rows, &local_results, &output);
        SummaryAgentMicroBatchRun {
            batch_idx: agent_batch_idx,
            rows: unit_indices
                .into_iter()
                .zip(resolved_rows.into_iter())
                .collect::<Vec<_>>(),
            usage,
            char_count: batch.char_count,
            item_count: local_results.len(),
            failed,
            error,
        }
    }));
    Ok(())
}

async fn prepare_agent_summary_batches_microbatched(
    task: Arc<OrganizeTaskRuntime>,
    text_route: RouteConfig,
    summary_strategy: String,
    units: &[OrganizeUnit],
    batch_size: u32,
    basic_batch_rows: &[Vec<Value>],
    config: SummaryAgentBatchConfig,
) -> Result<
    (
        Vec<PreparedClassificationBatch>,
        ExtractionSchedulerStats,
        SummaryAgentSchedulerStats,
        TokenUsage,
    ),
    String,
> {
    let mut extraction_stats = ExtractionSchedulerStats::default();
    let mut agent_stats = SummaryAgentSchedulerStats::default();
    let mut summary_usage = TokenUsage::default();
    if units.is_empty() {
        return Ok((
            Vec::new(),
            extraction_stats,
            agent_stats,
            summary_usage,
        ));
    }

    let batch_size = (batch_size as usize).max(1);
    let expected_rows = basic_batch_rows.iter().map(Vec::len).sum::<usize>();
    if expected_rows != units.len() {
        return Err(format!(
            "summary_agent_base_row_count_mismatch:rows={},units={}",
            expected_rows,
            units.len()
        ));
    }

    let profiles = units
        .iter()
        .map(|unit| {
            let profile = extraction_profile(&summary_strategy, unit, &task);
            extraction_stats.prepared_units += 1;
            extraction_stats.total_cost += profile.global_cost as u64;
            if profile.uses_tika {
                extraction_stats.tika_units += 1;
            } else if profile.label == "fallback_document" {
                extraction_stats.fallback_units += 1;
            } else {
                extraction_stats.text_units += 1;
            }
            if profile.uses_heavy_doc {
                extraction_stats.heavy_doc_units += 1;
            }
            profile
        })
        .collect::<Vec<_>>();
    let mut order = (0..units.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| {
        profiles[*right]
            .global_cost
            .cmp(&profiles[*left].global_cost)
            .then_with(|| units[*right].size.cmp(&units[*left].size))
            .then_with(|| left.cmp(right))
    });

    let gates = ExtractionGates {
        global: Arc::new(tokio::sync::Semaphore::new(EXTRACTION_GLOBAL_BUDGET as usize)),
        tika: Arc::new(tokio::sync::Semaphore::new(EXTRACTION_TIKA_HARD_CAP)),
        heavy_doc: Arc::new(tokio::sync::Semaphore::new(EXTRACTION_HEAVY_DOC_HARD_CAP)),
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(units.len());
    let mut extraction_handles: Vec<JoinHandle<()>> = Vec::new();
    for unit_idx in order {
        let profile = profiles[unit_idx];
        let task = task.clone();
        let tx = tx.clone();
        let gates = gates.clone();
        let summary_strategy = summary_strategy.clone();
        let unit = units[unit_idx].clone();
        extraction_handles.push(tauri::async_runtime::spawn(async move {
            let result = async {
                if task.stop.load(Ordering::Relaxed) {
                    return Err("stop_requested".to_string());
                }
                let permits = acquire_extraction_permits(profile, &gates).await?;
                let _permits = permits;
                prepare_summary_unit(task, summary_strategy, unit).await
            }
            .await;
            let _ = tx.send((unit_idx, result)).await;
        }));
    }
    drop(tx);

    let mut final_rows = (0..units.len()).map(|_| None).collect::<Vec<_>>();
    let mut current_batch = SummaryAgentMicroBatch::default();
    let mut agent_handles = Vec::new();
    let in_flight = Arc::new(tokio::sync::Semaphore::new(config.max_in_flight.max(1)));
    let mut next_agent_batch_idx = 0usize;
    let mut completed_extractions = 0usize;

    loop {
        let received = if current_batch.items.is_empty() {
            rx.recv().await
        } else if config.flush_ms == 0 {
            spawn_summary_agent_microbatch(
                &mut agent_handles,
                task.clone(),
                text_route.clone(),
                in_flight.clone(),
                std::mem::take(&mut current_batch),
                next_agent_batch_idx,
            )
            .await?;
            next_agent_batch_idx = next_agent_batch_idx.saturating_add(1);
            continue;
        } else {
            match tokio::time::timeout(Duration::from_millis(config.flush_ms), rx.recv()).await {
                Ok(value) => value,
                Err(_) => {
                    spawn_summary_agent_microbatch(
                        &mut agent_handles,
                        task.clone(),
                        text_route.clone(),
                        in_flight.clone(),
                        std::mem::take(&mut current_batch),
                        next_agent_batch_idx,
                    )
                    .await?;
                    next_agent_batch_idx = next_agent_batch_idx.saturating_add(1);
                    continue;
                }
            }
        };

        let Some((unit_idx, result)) = received else {
            break;
        };
        completed_extractions = completed_extractions.saturating_add(1);
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(err) => {
                for handle in &extraction_handles {
                    handle.abort();
                }
                for handle in &agent_handles {
                    handle.abort();
                }
                return Err(err);
            }
        };
        let batch_idx = unit_idx / batch_size;
        let offset = unit_idx % batch_size;
        let base_row = basic_batch_rows
            .get(batch_idx)
            .and_then(|batch| batch.get(offset))
            .ok_or_else(|| {
                format!(
                    "summary_agent_base_row_missing:unit={},batch={},offset={}",
                    unit_idx,
                    batch_idx + 1,
                    offset + 1
                )
            })?;
        let row = summary_batch_row(batch_idx, offset, base_row, &prepared, &summary_strategy)?;
        if row
            .get("summaryDegraded")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            agent_stats.degraded_items = agent_stats.degraded_items.saturating_add(1);
            final_rows[unit_idx] = Some(row);
            continue;
        }

        let candidate_chars =
            candidate_summary_agent_char_count(&current_batch.items, &row, &task.response_language);
        if !current_batch.items.is_empty()
            && (current_batch.items.len() >= config.max_items.max(1)
                || candidate_chars > config.max_chars.max(1))
        {
            spawn_summary_agent_microbatch(
                &mut agent_handles,
                task.clone(),
                text_route.clone(),
                in_flight.clone(),
                std::mem::take(&mut current_batch),
                next_agent_batch_idx,
            )
            .await?;
            next_agent_batch_idx = next_agent_batch_idx.saturating_add(1);
        }

        current_batch.items.push(SummaryAgentPendingItem {
            unit_idx,
            row,
            local_result: prepared.local_result,
        });
        current_batch.char_count =
            summary_agent_microbatch_char_count(&current_batch.items, &task.response_language);
        if current_batch.items.len() >= config.max_items.max(1)
            || current_batch.char_count >= config.max_chars.max(1)
        {
            spawn_summary_agent_microbatch(
                &mut agent_handles,
                task.clone(),
                text_route.clone(),
                in_flight.clone(),
                std::mem::take(&mut current_batch),
                next_agent_batch_idx,
            )
            .await?;
            next_agent_batch_idx = next_agent_batch_idx.saturating_add(1);
        }
    }

    if !current_batch.items.is_empty() {
        spawn_summary_agent_microbatch(
            &mut agent_handles,
            task.clone(),
            text_route,
            in_flight,
            std::mem::take(&mut current_batch),
            next_agent_batch_idx,
        )
        .await?;
    }
    if completed_extractions != units.len() {
        for handle in &extraction_handles {
            handle.abort();
        }
        for handle in &agent_handles {
            handle.abort();
        }
        return Err(format!(
            "summary_extraction_incomplete:completed={},expected={}",
            completed_extractions,
            units.len()
        ));
    }
    for handle in extraction_handles {
        handle
            .await
            .map_err(|e| format!("summary_extraction_join_failed:{e}"))?;
    }

    for handle in agent_handles {
        let run = handle
            .await
            .map_err(|e| format!("summary_agent_batch_join_failed:{e}"))?;
        agent_stats.agent_batch_count = agent_stats.agent_batch_count.saturating_add(1);
        agent_stats.total_chars = agent_stats.total_chars.saturating_add(run.char_count);
        agent_stats.max_batch_chars = agent_stats.max_batch_chars.max(run.char_count);
        if run.failed {
            agent_stats.failed_batches = agent_stats.failed_batches.saturating_add(1);
        }
        let degraded_in_run = run
            .rows
            .iter()
            .filter(|(_, row)| {
                row.get("summaryDegraded")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .count();
        agent_stats.degraded_items = agent_stats.degraded_items.saturating_add(degraded_in_run);
        if let Some(error) = run.error.filter(|value| !value.trim().is_empty()) {
            task.diagnostics.stage_completed(
                "summary_agent_batch_failed",
                json!({
                    "agentBatchIndex": run.batch_idx + 1,
                    "itemCount": run.item_count,
                    "charCount": run.char_count,
                    "error": error,
                }),
                Duration::from_millis(0),
            );
        }
        add_token_usage(&mut summary_usage, &run.usage);
        for (unit_idx, row) in run.rows {
            final_rows[unit_idx] = Some(row);
        }
    }

    let mut ordered_rows = Vec::with_capacity(units.len());
    for (unit_idx, row) in final_rows.into_iter().enumerate() {
        ordered_rows.push(row.ok_or_else(|| {
            format!(
                "summary_agent_missing_final_row:unit={},name={}",
                unit_idx,
                units
                    .get(unit_idx)
                    .map(|unit| unit.name.as_str())
                    .unwrap_or("")
            )
        })?);
    }
    let prepared_batches = ordered_rows
        .chunks(batch_size)
        .enumerate()
        .map(|(batch_idx, batch_rows)| PreparedClassificationBatch {
            batch_idx,
            batch_rows: batch_rows.to_vec(),
        })
        .collect::<Vec<_>>();

    Ok((
        prepared_batches,
        extraction_stats,
        agent_stats,
        summary_usage,
    ))
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

fn build_organize_file_done_payload(task_id: &str, row: &Value) -> Value {
    let mut payload = row.clone();
    if !payload.is_object() {
        return json!({
            "taskId": task_id,
            "row": payload,
        });
    }
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("taskId".to_string(), Value::String(task_id.to_string()));
    }
    payload
}

async fn run_organize_task<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task: &Arc<OrganizeTaskRuntime>,
) -> Result<(), String> {
    let task_started_at = Instant::now();
    let (root_path, recursive, excluded, batch_size, summary_strategy, use_web_search) = {
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
        "summaryExtractionGlobalBudget": EXTRACTION_GLOBAL_BUDGET,
        "summaryExtractionTikaHardCap": EXTRACTION_TIKA_HARD_CAP,
        "summaryExtractionHeavyDocHardCap": EXTRACTION_HEAVY_DOC_HARD_CAP,
        "summaryAgentBatchMaxChars": SUMMARY_AGENT_BATCH_MAX_CHARS,
        "summaryAgentBatchMaxItems": SUMMARY_AGENT_BATCH_MAX_ITEMS,
        "summaryAgentBatchFlushMs": SUMMARY_AGENT_BATCH_FLUSH_MS,
        "summaryAgentMaxInFlight": SUMMARY_AGENT_MAX_IN_FLIGHT,
        "classificationBatchConcurrency": CLASSIFICATION_BATCH_CONCURRENCY,
        "summaryStrategy": summary_strategy.clone(),
        "useWebSearch": use_web_search,
    }));
    {
        let mut snap = task.snapshot.lock();
        snap.status = "collecting".to_string();
        set_organize_progress(
            &mut snap,
            "collecting",
            "Collecting files",
            Some("Scanning the selected directory.".to_string()),
            None,
            None,
            None,
            true,
        );
    }
    let collection_started_at = Instant::now();
    task.diagnostics.collection_started(&root_path);
    emit_snapshot(app, state, task).await?;

    let collection = collect_units(Path::new(&root_path), recursive, &excluded, &task.stop);
    let units = collection.units;
    let collection_elapsed = collection_started_at.elapsed();
    task.diagnostics.collection_completed(
        &root_path,
        units.len(),
        &collection.report,
        collection_elapsed,
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
        set_organize_progress(
            &mut snap,
            "summary",
            "Preparing summaries",
            Some(format!(
                "Preparing {total_batches} batch(es) for classification."
            )),
            Some(0),
            Some(total_batches),
            Some("batches"),
            total_batches == 0,
        );
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
    let basic_batch_rows = build_basic_batch_rows(&units, batch_size);
    let initial_tree_rows = basic_batch_rows
        .iter()
        .flat_map(|batch| batch.iter().cloned())
        .collect::<Vec<_>>();
    let mut tree = deterministic_initial_tree(&initial_tree_rows);
    let initial_tree_started_at = Instant::now();
    let mut initial_tree_token_usage = TokenUsage::default();
    let base_tree_version = {
        let snap = task.snapshot.lock();
        snap.tree_version.saturating_add(1)
    };
    let mut initial_tree_handle = if !text_route.api_key.trim().is_empty()
        && !initial_tree_rows.is_empty()
    {
        let text_route = text_route.clone();
        let response_language = task.response_language.clone();
        let task = task.clone();
        let diagnostics = task.diagnostics.clone();
        let rows = initial_tree_rows.clone();
        Some(tauri::async_runtime::spawn(async move {
            summary::generate_initial_tree(
                &text_route,
                &response_language,
                &task.stop,
                &rows,
                Some(&diagnostics),
            )
            .await
        }))
    } else {
        None
    };

    let summary_started_at = Instant::now();
    let mut summary_token_usage = TokenUsage::default();
    let mut agent_summary_stats: Option<SummaryAgentSchedulerStats> = None;
    let (prepared_batches, extraction_stats) = if summary_strategy == SUMMARY_MODE_AGENT_SUMMARY {
        let (prepared_batches, extraction_stats, agent_stats, usage) =
            match prepare_agent_summary_batches_microbatched(
                task.clone(),
                text_route.clone(),
                summary_strategy.clone(),
                &units,
                batch_size,
                &basic_batch_rows,
                SummaryAgentBatchConfig::default(),
            )
            .await
            {
                Ok(value) => value,
                Err(err) if task.stop.load(Ordering::Relaxed) => {
                    abort_initial_tree_task(&mut initial_tree_handle);
                    task.diagnostics.task_stopped(
                        "organize task stopped during summary preparation",
                        json!({ "stage": "summary_preparation", "error": err }),
                        Some(task_started_at.elapsed()),
                    );
                    return Ok(());
                }
                Err(err) => {
                    abort_initial_tree_task(&mut initial_tree_handle);
                    return Err(err);
                }
            };
        add_token_usage(&mut summary_token_usage, &usage);
        {
            let mut snap = task.snapshot.lock();
            add_token_usage(&mut snap.token_usage, &usage);
        }
        agent_summary_stats = Some(agent_stats);
        (prepared_batches, extraction_stats)
    } else {
        let (prepared_units, extraction_stats) =
            match prepare_summary_units_weighted(task.clone(), &summary_strategy, &units).await {
                Ok(value) => value,
                Err(err) if task.stop.load(Ordering::Relaxed) => {
                    abort_initial_tree_task(&mut initial_tree_handle);
                    task.diagnostics.task_stopped(
                        "organize task stopped during summary extraction",
                        json!({ "stage": "summary_extraction", "error": err }),
                        Some(task_started_at.elapsed()),
                    );
                    return Ok(());
                }
                Err(err) => {
                    abort_initial_tree_task(&mut initial_tree_handle);
                    return Err(err);
                }
            };
        if task.stop.load(Ordering::Relaxed) {
            abort_initial_tree_task(&mut initial_tree_handle);
            task.diagnostics.task_stopped(
                "organize task stopped after summary extraction",
                json!({ "stage": "summary_extraction" }),
                Some(task_started_at.elapsed()),
            );
            return Ok(());
        }

        let mut prepared_unit_batches: Vec<Vec<PreparedLocalSummaryUnit>> = Vec::new();
        let mut current_batch = Vec::new();
        for unit in prepared_units {
            current_batch.push(unit);
            if current_batch.len() >= batch_size as usize {
                prepared_unit_batches.push(current_batch);
                current_batch = Vec::new();
            }
        }
        if !current_batch.is_empty() {
            prepared_unit_batches.push(current_batch);
        }

        let mut prepared_batches = Vec::with_capacity(prepared_unit_batches.len());
        for (batch_idx, batch) in prepared_unit_batches.into_iter().enumerate() {
            let batch_started_at = Instant::now();
            let batch_len = batch.len();
            task.diagnostics
                .batch_started(batch_idx + 1, batch_len, total_batches);
            if task.stop.load(Ordering::Relaxed) {
                abort_initial_tree_task(&mut initial_tree_handle);
                task.diagnostics.task_stopped(
                    "organize task stopped before batch",
                    json!({ "stage": "batch_start", "batchIndex": batch_idx + 1 }),
                    Some(task_started_at.elapsed()),
                );
                return Ok(());
            }

            let prepared = match prepare_summary_batch_from_units(
                task.clone(),
                text_route.clone(),
                summary_strategy.clone(),
                batch_idx,
                batch,
                basic_batch_rows
                    .get(batch_idx)
                    .cloned()
                    .unwrap_or_default(),
            )
            .await
            {
                Ok(prepared) => prepared,
                Err(err) if task.stop.load(Ordering::Relaxed) => {
                    abort_initial_tree_task(&mut initial_tree_handle);
                    task.diagnostics.task_stopped(
                        "organize task stopped during summary batch preparation",
                        json!({ "stage": "summary_batch", "batchIndex": batch_idx + 1, "error": err }),
                        Some(task_started_at.elapsed()),
                    );
                    return Ok(());
                }
                Err(err) => {
                    abort_initial_tree_task(&mut initial_tree_handle);
                    return Err(err);
                }
            };
            if prepared.batch_idx != batch_idx {
                abort_initial_tree_task(&mut initial_tree_handle);
                return Err(format!(
                    "summary_batch_order_mismatch:expected={},actual={}",
                    batch_idx + 1,
                    prepared.batch_idx + 1
                ));
            }
            let summary_usage = prepared.summary_usage;
            add_token_usage(&mut summary_token_usage, &summary_usage);
            {
                let mut snap = task.snapshot.lock();
                add_token_usage(&mut snap.token_usage, &summary_usage);
            }
            task.diagnostics.batch_completed(
                false,
                json!({
                    "batchIndex": batch_idx + 1,
                    "summaryPreparedRows": prepared.batch_rows.len(),
                    "summaryUsage": token_usage_json(&summary_usage),
                }),
                batch_started_at.elapsed(),
            );
            prepared_batches.push(PreparedClassificationBatch {
                batch_idx,
                batch_rows: prepared.batch_rows,
            });
        }
        (prepared_batches, extraction_stats)
    };

    if task.stop.load(Ordering::Relaxed) {
        abort_initial_tree_task(&mut initial_tree_handle);
        task.diagnostics.task_stopped(
            "organize task stopped after summary preparation",
            json!({ "stage": "summary_preparation" }),
            Some(task_started_at.elapsed()),
        );
        return Ok(());
    }

    for prepared in &prepared_batches {
        let batch_idx = prepared.batch_idx;
        if summary_strategy == SUMMARY_MODE_AGENT_SUMMARY {
            task.diagnostics
                .batch_started(batch_idx + 1, prepared.batch_rows.len(), total_batches);
        }
        if summary_strategy == SUMMARY_MODE_LOCAL_SUMMARY
            || summary_strategy == SUMMARY_MODE_AGENT_SUMMARY
        {
            for row in &prepared.batch_rows {
                summary::emit_organize_summary_ready(app, &task_id, (batch_idx + 1) as u64, row);
            }
        }
        {
            let mut snap = task.snapshot.lock();
            snap.processed_batches = (batch_idx + 1) as u64;
            set_organize_progress(
                &mut snap,
                "summary",
                "Preparing summaries",
                Some(format!(
                    "Prepared summary batch {} of {total_batches}.",
                    batch_idx + 1
                )),
                Some((batch_idx + 1) as u64),
                Some(total_batches),
                Some("batches"),
                total_batches == 0,
            );
        }
        if let Err(err) = emit_snapshot(app, state, task).await {
            abort_initial_tree_task(&mut initial_tree_handle);
            return Err(err);
        }
        if summary_strategy == SUMMARY_MODE_AGENT_SUMMARY {
            task.diagnostics.batch_completed(
                false,
                json!({
                    "batchIndex": batch_idx + 1,
                    "summaryPreparedRows": prepared.batch_rows.len(),
                    "summaryUsage": token_usage_json(&TokenUsage::default()),
                }),
                Duration::from_millis(0),
            );
        }
    }
    let summary_elapsed = summary_started_at.elapsed();
    let agent_summary_scheduler = agent_summary_stats
        .as_ref()
        .map(|stats| {
            json!({
                "maxChars": SUMMARY_AGENT_BATCH_MAX_CHARS,
                "maxItems": SUMMARY_AGENT_BATCH_MAX_ITEMS,
                "flushMs": SUMMARY_AGENT_BATCH_FLUSH_MS,
                "maxInFlight": SUMMARY_AGENT_MAX_IN_FLIGHT,
                "agentBatchCount": stats.agent_batch_count,
                "agentBatchChars": {
                    "total": stats.total_chars,
                    "max": stats.max_batch_chars,
                },
                "failedBatches": stats.failed_batches,
                "degradedItems": stats.degraded_items,
            })
        })
        .unwrap_or(Value::Null);
    task.diagnostics.stage_completed(
        "summary_preparation",
        json!({
            "summaryStrategy": summary_strategy.clone(),
            "totalBatches": total_batches,
            "preparedBatches": prepared_batches.len(),
            "preparedRows": prepared_batches.iter().map(|batch| batch.batch_rows.len()).sum::<usize>(),
            "extractionScheduler": {
                "globalBudget": EXTRACTION_GLOBAL_BUDGET,
                "tikaHardCap": EXTRACTION_TIKA_HARD_CAP,
                "heavyDocHardCap": EXTRACTION_HEAVY_DOC_HARD_CAP,
                "preparedUnits": extraction_stats.prepared_units,
                "textUnits": extraction_stats.text_units,
                "tikaUnits": extraction_stats.tika_units,
                "heavyDocUnits": extraction_stats.heavy_doc_units,
                "fallbackUnits": extraction_stats.fallback_units,
                "totalCost": extraction_stats.total_cost,
            },
            "agentSummaryScheduler": agent_summary_scheduler,
            "tokenUsage": token_usage_json(&summary_token_usage),
        }),
        summary_elapsed,
    );

    {
        let mut snap = task.snapshot.lock();
        set_organize_progress(
            &mut snap,
            "initial_tree",
            "Building category tree",
            Some("Generating the initial category tree.".to_string()),
            None,
            None,
            None,
            true,
        );
    }
    if let Err(err) = emit_snapshot(app, state, task).await {
        abort_initial_tree_task(&mut initial_tree_handle);
        return Err(err);
    }
    if let Some(handle) = initial_tree_handle.take() {
        match handle
            .await
            .map_err(|e| format!("initial_tree_join_failed:{e}"))?
        {
            Ok(output) => {
                {
                    let mut snap = task.snapshot.lock();
                    add_token_usage(&mut snap.token_usage, &output.usage);
                }
                add_token_usage(&mut initial_tree_token_usage, &output.usage);
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
    let initial_tree_elapsed = initial_tree_started_at.elapsed();
    task.diagnostics.stage_completed(
        "initial_tree",
        json!({
            "usedModel": !text_route.api_key.trim().is_empty() && !initial_tree_rows.is_empty(),
            "rowCount": initial_tree_rows.len(),
            "baseTreeVersion": base_tree_version,
            "tokenUsage": token_usage_json(&initial_tree_token_usage),
        }),
        initial_tree_elapsed,
    );
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
    let classification_started_at = Instant::now();
    let mut classification_token_usage = TokenUsage::default();
    {
        let mut snap = task.snapshot.lock();
        set_organize_progress(
            &mut snap,
            "classification",
            "Classifying batches",
            Some(format!("Classifying {total_batches} prepared batch(es).")),
            Some(0),
            Some(total_batches),
            Some("batches"),
            total_batches == 0,
        );
    }
    emit_snapshot(app, state, task).await?;
    let (
        classification_runs,
        final_classification_concurrency,
        classification_retry_rounds,
        classification_request_count,
    ) =
        run_classification_scheduler(
            app,
            state,
            task,
            &text_route,
            &tree,
            base_tree_version,
            prepared_batches,
            reference_structure,
            use_web_search,
            shared_search_calls.clone(),
            shared_search_gate.clone(),
            CLASSIFICATION_BATCH_CONCURRENCY,
        )
        .await?;

    let mut batch_outputs = Vec::new();
    let mut reconcile_inputs = Vec::new();
    let mut proposal_map: HashMap<String, (String, Vec<String>)> = HashMap::new();
    let mut deferred = Vec::new();
    let mut final_assignment_inputs: HashMap<String, (String, Vec<String>, String)> =
        HashMap::new();
    let mut rows_by_id: HashMap<String, (usize, Value)> = HashMap::new();
    let mut classification_errors = Vec::new();
    let mut result_rows = Vec::new();

    for ClassificationBatchRun {
        prepared,
        output,
        attempt_errors,
    } in classification_runs
    {
        {
            let mut snap = task.snapshot.lock();
            add_token_usage(&mut snap.token_usage, &output.usage);
        }
        add_token_usage(&mut classification_token_usage, &output.usage);
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
            "attemptErrors": attempt_errors,
            "searchCalls": output.search_calls,
            "modelRawOutput": output.raw_output,
        });
        batch_outputs.push(batch_record);
        if let Some(input) = pending_reconcile_input(
            prepared.batch_idx + 1,
            base_tree_version,
            &parsed,
            batch_error.as_deref(),
        ) {
            reconcile_inputs.push(input);
        }
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
                    if row_offset == 0 {
                        &output.raw_output
                    } else {
                        ""
                    },
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
    let classification_elapsed = classification_started_at.elapsed();
    task.diagnostics.stage_completed(
        "classification",
        json!({
            "batchCount": total_batches,
            "resultRows": result_rows.len(),
            "classificationErrorCount": classification_errors.len(),
            "searchCalls": shared_search_calls.load(Ordering::Relaxed),
            "initialConcurrency": CLASSIFICATION_BATCH_CONCURRENCY,
            "finalConcurrency": final_classification_concurrency,
            "retryRounds": classification_retry_rounds,
            "degradedConcurrency": final_classification_concurrency < CLASSIFICATION_BATCH_CONCURRENCY,
            "tokenUsage": token_usage_json(&classification_token_usage),
        }),
        classification_elapsed,
    );

    let reconcile_started_at = Instant::now();
    let mut reconcile_token_usage = TokenUsage::default();
    {
        let mut snap = task.snapshot.lock();
        set_organize_progress(
            &mut snap,
            "reconcile",
            "Reconciling tree",
            Some("Merging batch results into the final category tree.".to_string()),
            None,
            None,
            None,
            true,
        );
    }
    emit_snapshot(app, state, task).await?;
    let reconcile_used_model = !text_route.api_key.trim().is_empty() && !reconcile_inputs.is_empty();
    if reconcile_used_model {
        match summary::reconcile_organize_batches(
            &text_route,
            &task.response_language,
            &task.stop,
            &category_tree_to_value(&tree),
            &reconcile_inputs,
            Some(&task.diagnostics),
        )
        .await
        {
            Ok(output) => {
                {
                    let mut snap = task.snapshot.lock();
                    add_token_usage(&mut snap.token_usage, &output.usage);
                }
                add_token_usage(&mut reconcile_token_usage, &output.usage);
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
                        let fallback_path =
                            category_path_from_value(assignment.get("categoryPath"));
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
                    refresh_assignment_paths(&mut final_assignment_inputs, &tree)?;
                    final_assignment_inputs.extend(reconciled_assignments);
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
                        let path = category_path_for_id(&tree, &leaf).unwrap_or_else(|| {
                            category_path_from_value(mapping.get("categoryPath"))
                        });
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
    let reconcile_elapsed = reconcile_started_at.elapsed();
    task.diagnostics.stage_completed(
        "reconcile",
        json!({
            "usedModel": reconcile_used_model,
            "rowCount": rows_by_id.len(),
            "pendingBatchCount": reconcile_inputs.len(),
            "tokenUsage": token_usage_json(&reconcile_token_usage),
        }),
        reconcile_elapsed,
    );

    let finalize_started_at = Instant::now();
    {
        let mut snap = task.snapshot.lock();
        set_organize_progress(
            &mut snap,
            "finalize",
            "Finalizing results",
            Some("Writing organize rows, preview, and category tree.".to_string()),
            None,
            None,
            None,
            true,
        );
    }
    emit_snapshot(app, state, task).await?;
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
        app.emit(
            "organize_file_done",
            build_organize_file_done_payload(&task_id, row),
        )
            .map_err(|e| e.to_string())?;
    }
    let classification_error_count = classification_errors.len();

    {
        let mut snap = task.snapshot.lock();
        snap.processed_files = result_rows.len() as u64;
        snap.processed_batches = total_batches;
        set_organize_progress(
            &mut snap,
            "finalize",
            "Finalizing results",
            Some(format!("Prepared {} result row(s).", result_rows.len())),
            Some(result_rows.len() as u64),
            Some(units.len() as u64),
            Some("files"),
            units.is_empty(),
        );
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

    let finalize_elapsed = finalize_started_at.elapsed();
    let timing_ms = json!({
        "collection": collection_elapsed.as_millis() as u64,
        "summaryPreparation": summary_elapsed.as_millis() as u64,
        "initialTree": initial_tree_elapsed.as_millis() as u64,
        "classification": classification_elapsed.as_millis() as u64,
        "reconcile": reconcile_elapsed.as_millis() as u64,
        "finalize": finalize_elapsed.as_millis() as u64,
        "total": task_started_at.elapsed().as_millis() as u64,
    });
    let token_usage_by_stage = token_usage_by_stage_json(
        &summary_token_usage,
        &initial_tree_token_usage,
        &classification_token_usage,
        &reconcile_token_usage,
    );
    let request_count = agent_summary_stats
        .as_ref()
        .map(|stats| stats.agent_batch_count as u64)
        .unwrap_or(0)
        .saturating_add(
            if !text_route.api_key.trim().is_empty() && !initial_tree_rows.is_empty() {
                1
            } else {
                0
            },
        )
        .saturating_add(classification_request_count)
        .saturating_add(if reconcile_used_model { 1 } else { 0 });
    let error_count = agent_summary_stats
        .as_ref()
        .map(|stats| stats.failed_batches as u64)
        .unwrap_or(0)
        .saturating_add(classification_error_count as u64);

    let final_snapshot = {
        let mut snap = task.snapshot.lock();
        snap.results
            .sort_by_key(|x| x.get("index").and_then(Value::as_u64).unwrap_or(0));
        snap.preview = planner::build_preview(&snap.root_path, &snap.results);
        snap.tree = snap.final_tree.clone();
        snap.status = "completed".to_string();
        let processed_batches = snap.processed_batches;
        let total_batches = snap.total_batches;
        set_organize_progress(
            &mut snap,
            "completed",
            "Completed",
            Some("Organize results are ready.".to_string()),
            Some(processed_batches),
            Some(total_batches),
            Some("batches"),
            false,
        );
        snap.completed_at = Some(now_iso());
        snap.duration_ms = Some(task_started_at.elapsed().as_millis() as u64);
        snap.timing_ms = timing_ms.clone();
        snap.token_usage_by_stage = token_usage_by_stage.clone();
        snap.request_count = Some(request_count);
        snap.error_count = Some(error_count);
        snap.clone()
    };
    persist::save_organize_snapshot(&state.db_path(), &final_snapshot)?;
    persist::save_latest_organize_tree(
        &state.db_path(),
        &final_snapshot.root_path,
        &final_snapshot.tree,
        final_snapshot.tree_version,
    )?;
    task.diagnostics.stage_completed(
        "finalize",
        json!({
            "resultRows": final_snapshot.results.len(),
            "previewCount": final_snapshot.preview.len(),
            "classificationErrorCount": final_snapshot.classification_errors.len(),
        }),
        finalize_elapsed,
    );
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
            "durationMs": final_snapshot.duration_ms,
            "timingMs": timing_ms,
            "tokenUsage": token_usage_json(&final_snapshot.token_usage),
            "tokenUsageByStage": token_usage_by_stage,
            "requestCount": request_count,
            "errorCount": error_count,
        }),
        task_started_at.elapsed(),
    );
    Ok(())
}
