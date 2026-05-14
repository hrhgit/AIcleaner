fn infer_inventory_timestamps(
    path: &str,
    created_at: Option<&Value>,
    modified_at: Option<&Value>,
) -> (Option<String>, Option<String>) {
    let created = created_at.and_then(Value::as_str).map(str::to_string);
    let modified = modified_at.and_then(Value::as_str).map(str::to_string);
    if created.is_some() || modified.is_some() {
        return (created, modified);
    }
    let meta = fs::metadata(path).ok();
    let created = meta
        .as_ref()
        .and_then(|value| value.created().ok())
        .map(system_time_to_iso);
    let modified = meta
        .as_ref()
        .and_then(|value| value.modified().ok())
        .map(system_time_to_iso);
    (created, modified)
}

fn system_time_to_iso(value: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(value)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn metadata_summary_short(item: &InventoryItem) -> String {
    format!(
        "{}，{}，{}",
        item.name,
        item.kind,
        format_size_text(item.size)
    )
}

fn metadata_representation(item: &InventoryItem) -> FileRepresentation {
    FileRepresentation {
        metadata: Some(metadata_summary_short(item)),
        short: None,
        long: None,
        source: "metadata".to_string(),
        degraded: false,
        confidence: None,
        keywords: Vec::new(),
    }
}

fn representation_from_organize_row(row: &Value) -> FileRepresentation {
    if let Some(value) = row.get("representation") {
        let parsed = FileRepresentation::from_value(value);
        if parsed.has_level(RepresentationLevel::Metadata)
            || parsed.has_level(RepresentationLevel::Short)
            || parsed.has_level(RepresentationLevel::Long)
        {
            return parsed;
        }
    }
    let summary = row
        .get("summary")
        .or_else(|| row.get("reason"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    FileRepresentation {
        metadata: summary.clone(),
        short: summary.clone(),
        long: summary,
        source: row
            .get("summarySource")
            .and_then(Value::as_str)
            .unwrap_or("organize")
            .to_string(),
        degraded: row
            .get("summaryDegraded")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        confidence: row
            .get("summaryConfidence")
            .and_then(Value::as_str)
            .map(str::to_string),
        keywords: row
            .get("summaryKeywords")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect(),
    }
}

fn collect_inventory_from_fs(
    root_path: &str,
    overrides: &HashMap<String, Vec<String>>,
) -> Vec<InventoryItem> {
    if root_path.trim().is_empty() {
        return Vec::new();
    }

    let mut output = Vec::new();
    let mut visited_dirs = 0usize;
    let mut stack = vec![(PathBuf::from(root_path), 0usize)];
    while let Some((current, depth)) = stack.pop() {
        visited_dirs += 1;
        if visited_dirs > FS_FALLBACK_MAX_DIRS {
            break;
        }
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                if depth < FS_FALLBACK_MAX_DEPTH && !should_skip_fallback_dir(&path) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            if output.len() >= FS_FALLBACK_MAX_FILES {
                return output;
            }
            let path_string = path.to_string_lossy().to_string();
            let key = normalize_path_key(&path_string);
            let mut category_path = overrides.get(&key).cloned().unwrap_or_default();
            if category_path.is_empty() {
                category_path.push("其他待定".to_string());
            }
            let category_id = category_id_from_path(&category_path);
            let parent_category_id = parent_category_id_from_path(&category_path);
            let mut item = InventoryItem {
                path: path_string.clone(),
                name: basename(&path_string),
                size: metadata.len(),
                created_at: metadata.created().ok().map(system_time_to_iso),
                modified_at: metadata.modified().ok().map(system_time_to_iso),
                kind: infer_kind(&path_string, "", ""),
                category_id,
                parent_category_id,
                category_path,
                representation: FileRepresentation::default(),
                summary_representation: None,
                risk: "unknown".to_string(),
            };
            item.representation = metadata_representation(&item);
            output.push(item);
        }
    }
    output
}

fn should_skip_fallback_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| {
            FS_FALLBACK_SKIP_DIRS
                .iter()
                .any(|blocked| name.eq_ignore_ascii_case(blocked))
        })
}

fn modified_age_text(item: &InventoryItem) -> String {
    let Some(modified_at) = item.modified_at.as_deref() else {
        return String::new();
    };
    let Ok(modified) = chrono::DateTime::parse_from_rfc3339(modified_at) else {
        return String::new();
    };
    let age = chrono::Utc::now().signed_duration_since(modified.with_timezone(&chrono::Utc));
    if age.num_days() > 0 {
        format!("modified {} days ago", age.num_days())
    } else if age.num_hours() > 0 {
        format!("modified {} hours ago", age.num_hours())
    } else if age.num_minutes() > 0 {
        format!("modified {} minutes ago", age.num_minutes())
    } else {
        "modified just now".to_string()
    }
}

fn load_summary_row(
    db_path: &Path,
    root_path: &str,
    path: &str,
    level: RepresentationLevel,
    missing_only: bool,
) -> Result<Option<Value>, String> {
    let row = persist::load_advisor_file_summary(
        db_path,
        &persist::create_root_path_key(root_path),
        &persist::create_root_path_key(path),
    )?;
    if !missing_only {
        return Ok(row);
    }
    Ok(row.filter(|value| {
        FileRepresentation::from_value(value.get("representation").unwrap_or(&Value::Null))
            .has_level(level)
    }))
}

fn read_existing_summary_item(
    db_path: &Path,
    root_path: &str,
    item: &InventoryItem,
    level: RepresentationLevel,
) -> Result<Option<Value>, String> {
    if let Some(row) = persist::load_advisor_file_summary(
        db_path,
        &persist::create_root_path_key(root_path),
        &persist::create_root_path_key(&item.path),
    )? {
        let representation =
            FileRepresentation::from_value(row.get("representation").unwrap_or(&Value::Null));
        if representation.has_level(level) {
            return Ok(Some(summary_row_to_read_item(&row, level)));
        }
    }
    if item.representation.has_level(level) {
        return Ok(Some(json!({
            "path": item.path,
            "name": item.name,
            "representation": item.representation.prune_to_level(level).to_value(),
            "warning": Value::Null,
        })));
    }
    Ok(None)
}

fn summary_row_to_tool_item(row: &Value, level: RepresentationLevel) -> Value {
    json!({
        "path": row.get("path").cloned().unwrap_or(Value::Null),
        "name": row.get("name").cloned().unwrap_or(Value::Null),
        "representation": FileRepresentation::from_value(
            row.get("representation").unwrap_or(&Value::Null),
        ).prune_to_level(level).to_value(),
        "warning": Value::Null,
    })
}

fn summary_row_to_read_item(row: &Value, level: RepresentationLevel) -> Value {
    json!({
        "path": row.get("path").cloned().unwrap_or(Value::Null),
        "name": row.get("name").cloned().unwrap_or(Value::Null),
        "representation": FileRepresentation::from_value(
            row.get("representation").unwrap_or(&Value::Null),
        ).prune_to_level(level).to_value(),
    })
}

fn collect_reclassification_changes(request: &Value) -> Result<Vec<Value>, String> {
    let changes = request
        .get("changes")
        .and_then(Value::as_array)
        .ok_or_else(|| "当前归类修改缺少必填字段，请提供非空 changes 数组后重试。".to_string())?;
    if changes.is_empty() {
        return Err("当前归类修改缺少必填字段，请提供非空 changes 数组后重试。".to_string());
    }
    Ok(changes.clone())
}

fn apply_reclassification_change(
    db_path: &Path,
    session: &mut Value,
    change: &Value,
    category_aliases: &mut HashMap<String, Option<Vec<String>>>,
) -> Result<Vec<Value>, String> {
    let change_type = change
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
        })?;
    match change_type {
        "move_selection_to_category" => {
            let selection_id = required_change_str(change, "selectionId")?;
            let selection = persist::load_advisor_selection(db_path, selection_id)?
                .ok_or_else(|| "当前筛选结果已失效，请先重新调用 find_files 生成新的 selection，再继续分类修正。".to_string())?;
            let items = selection
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let category_path = resolve_reclassification_category_path(
                session,
                required_change_str(change, "targetCategoryId")?,
                category_aliases,
            )?
                .ok_or_else(|| {
                    "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
                })?;
            apply_reclassification_selection(session, &items, &category_path)
        }
        "split_selection_to_new_category" => {
            let selection_id = required_change_str(change, "selectionId")?;
            let new_category_name = required_change_str(change, "newCategoryName")?;
            let selection = persist::load_advisor_selection(db_path, selection_id)?
                .ok_or_else(|| "当前筛选结果已失效，请先重新调用 find_files 生成新的 selection，再继续分类修正。".to_string())?;
            let items = selection
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut category_path = resolve_reclassification_category_path(
                session,
                required_change_str(change, "sourceCategoryId")?,
                category_aliases,
            )?
            .unwrap_or_default();
            category_path.push(new_category_name.to_string());
            apply_reclassification_selection(session, &items, &category_path)
        }
        "rename_category" => rename_category(session, change, category_aliases),
        "merge_category_into_category" => {
            merge_category_into_category(session, change, category_aliases)
        }
        "delete_empty_category" => delete_empty_category(session, change, category_aliases),
        _ => Err("当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()),
    }
}

fn rename_category(
    session: &mut Value,
    change: &Value,
    category_aliases: &mut HashMap<String, Option<Vec<String>>>,
) -> Result<Vec<Value>, String> {
    let source_input_id = required_change_str(change, "sourceCategoryId")?;
    let new_category_name = required_change_str(change, "newCategoryName")?;
    let source_path = resolve_reclassification_category_path(session, source_input_id, category_aliases)?
        .ok_or_else(|| {
            "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
        })?;
    let mut target_path = source_path.clone();
    if let Some(last) = target_path.last_mut() {
        *last = new_category_name.to_string();
    }
    let result = reclass_by_category_path(session, &source_path, &target_path)?;
    category_aliases.insert(source_input_id.to_string(), Some(target_path));
    Ok(result)
}

fn merge_category_into_category(
    session: &mut Value,
    change: &Value,
    category_aliases: &mut HashMap<String, Option<Vec<String>>>,
) -> Result<Vec<Value>, String> {
    let source_input_id = required_change_str(change, "sourceCategoryId")?;
    let target_input_id = required_change_str(change, "targetCategoryId")?;
    let source_path = resolve_reclassification_category_path(session, source_input_id, category_aliases)?
        .ok_or_else(|| {
            "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
        })?;
    let target_path = resolve_reclassification_category_path(session, target_input_id, category_aliases)?
        .ok_or_else(|| {
            "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
        })?;
    let result = reclass_by_category_path(session, &source_path, &target_path)?;
    category_aliases.insert(source_input_id.to_string(), Some(target_path));
    Ok(result)
}

fn delete_empty_category(
    session: &mut Value,
    change: &Value,
    category_aliases: &mut HashMap<String, Option<Vec<String>>>,
) -> Result<Vec<Value>, String> {
    let source_input_id = required_change_str(change, "sourceCategoryId")?;
    let source_path = resolve_reclassification_category_path(session, source_input_id, category_aliases)?
        .ok_or_else(|| {
            "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
        })?;
    let overrides = super::types::inventory_overrides(session);
    if overrides.values().any(|path| path == &source_path) {
        return Err("当前分类下仍有文件，不能删除非空分类。".to_string());
    }
    category_aliases.insert(source_input_id.to_string(), None);
    Ok(Vec::new())
}

fn resolve_reclassification_category_path(
    session: &Value,
    category_id: &str,
    category_aliases: &HashMap<String, Option<Vec<String>>>,
) -> Result<Option<Vec<String>>, String> {
    if let Some(path) = category_aliases.get(category_id) {
        return Ok(path.clone());
    }
    category_path_from_category_id(session, category_id)
}

fn reclass_by_category_path(
    session: &mut Value,
    source_path: &[String],
    target_path: &[String],
) -> Result<Vec<Value>, String> {
    let overrides = super::types::inventory_overrides(session);
    let matched = overrides
        .iter()
        .filter(|(_, path)| path.as_slice() == source_path)
        .map(|(path, _)| json!({ "path": path }))
        .collect::<Vec<_>>();
    apply_reclassification_selection(session, &matched, target_path)
}

fn build_reclassification_change_summary(changes: &[Value], rollback_entries: &[Value]) -> String {
    let file_count = rollback_entries.len();
    match changes.len() {
        0 => format!("unknown: {file_count} items updated"),
        1 => format!(
            "{}: {file_count} items updated",
            changes[0]
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ),
        count => format!("{count} changes applied: {file_count} items updated"),
    }
}

fn supports_content_summarization(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_ascii_lowercase()))
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        // Document files (Tika)
        ".pdf" | ".doc" | ".docx" | ".xls" | ".xlsx" | ".ppt" | ".pptx"
        | ".rtf" | ".odt" | ".ods" | ".odp" | ".epub"
        // Plain text / code files
        | ".txt" | ".md" | ".csv" | ".json" | ".yaml" | ".yml" | ".toml"
        | ".xml" | ".log" | ".ini" | ".cfg" | ".conf"
        | ".js" | ".ts" | ".jsx" | ".tsx" | ".rs" | ".py" | ".java"
        | ".go" | ".c" | ".cpp" | ".h" | ".hpp" | ".css" | ".html"
        | ".sql" | ".sh" | ".bat" | ".ps1"
    )
}

fn inventory_to_organize_unit(item: &InventoryItem) -> OrganizeUnit {
    OrganizeUnit {
        name: item.name.clone(),
        path: item.path.clone(),
        relative_path: item.path.clone(),
        size: item.size,
        created_at: item.created_at.clone(),
        modified_at: item.modified_at.clone(),
        item_type: "file".to_string(),
        modality: String::new(),
        directory_assessment: None,
    }
}

async fn summarize_batch_with_model(
    client: &reqwest::Client,
    route: &super::llm::AdvisorModelRoute,
    root_path: &str,
    batch: &[InventoryItem],
    level: RepresentationLevel,
    lang: &str,
    extraction_config: Option<&ExtractionToolConfig>,
) -> (Vec<Value>, Vec<SummaryFailedItem>, TokenUsage) {
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut usage = TokenUsage::default();
    for item in batch {
        match summarize_item_with_model(client, route, root_path, item, level, lang, extraction_config).await {
            Ok((row, item_usage)) => {
                add_token_usage(&mut usage, &item_usage);
                output.push(row);
            }
            Err(error) => errors.push(SummaryFailedItem {
                item: item.clone(),
                error,
            }),
        }
    }
    (output, errors, usage)
}

async fn summarize_item_with_model(
    client: &reqwest::Client,
    route: &super::llm::AdvisorModelRoute,
    root_path: &str,
    item: &InventoryItem,
    level: RepresentationLevel,
    lang: &str,
    extraction_config: Option<&ExtractionToolConfig>,
) -> Result<(Value, TokenUsage), SummaryErrorRow> {
    let extracted = if let Some(config) = extraction_config.filter(|c| c.tika_ready) {
        let unit = inventory_to_organize_unit(item);
        let stop = std::sync::atomic::AtomicBool::new(false);
        let extraction = extract_unit_content_for_summary_with_tools(&unit, lang, &stop, config).await;
        if extraction.excerpt.trim().is_empty() { None } else { Some(extraction.excerpt) }
    } else {
        None
    };
    let prompt = build_summary_prompt(item, extracted.as_deref(), level, lang);
    let api_format = route.api_format;
    let tool_registry = ToolRegistry::new();
    let tool_ctx = ToolContext {
        workflow: ToolWorkflow::Advisor,
        stage: "advisor_summary_generation",
        session: None,
        bootstrap_turn: false,
        response_language: lang,
        web_search_allowed: false,
        search_remaining: 0,
    };
    let tools = tool_registry.definitions(&tool_ctx);
    let request = build_completion_payload(
        api_format,
        &route.model,
        &[
            json!({ "role": "system", "content": summary_system_prompt(lang, level) }),
            json!({ "role": "user", "content": prompt }),
        ],
        Some(&tools),
        0.0,
        DEFAULT_MAX_TOKENS,
        {
            let thinking = reasoning_policy::advisor_summary_tool(&route.thinking_level);
            ThinkingConfig {
                enabled: thinking.enabled,
                level: thinking.level,
            }
        },
    )
    .map_err(|reason| SummaryErrorRow {
        path: item.path.clone(),
        reason,
        retryable: false,
    })?;

    let mut last_error = None;
    for _attempt in 0..2 {
        let req = client
            .post(build_messages_url(&route.endpoint, api_format))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&request);
        let req = apply_auth_headers(req, api_format, &route.api_key);
        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                let body = match resp.text().await {
                    Ok(body) => body,
                    Err(err) => {
                        last_error = Some(SummaryErrorRow {
                            path: item.path.clone(),
                            reason: format!("summary_body_read_failed:{err}"),
                            retryable: false,
                        });
                        continue;
                    }
                };
                if !status.is_success() {
                    last_error = Some(SummaryErrorRow {
                        path: item.path.clone(),
                        reason: format_http_summary_error(status, &body),
                        retryable: is_retryable_status(status),
                    });
                    continue;
                }
                let parsed = match parse_completion_response(api_format, status, &body) {
                    Ok(parsed) => parsed,
                    Err(_) => {
                        last_error = Some(SummaryErrorRow {
                            path: item.path.clone(),
                            reason: "summary_response_parse_failed".to_string(),
                            retryable: false,
                        });
                        continue;
                    }
                };
                let usage = parsed.usage.clone();
                let Some(tool_call) = parsed
                    .tool_calls
                    .into_iter()
                    .find(|call| {
                        tool_registry.id_for_name(&call.name) == Some(ToolId::SubmitFileSummaries)
                    })
                else {
                    last_error = Some(SummaryErrorRow {
                        path: item.path.clone(),
                        reason: "summary response did not call submit_file_summaries".to_string(),
                        retryable: false,
                    });
                    continue;
                };
                let mut exec_ctx = ToolExecutionContext {
                    workflow: ToolWorkflow::Advisor,
                    stage: "advisor_summary_generation",
                    session: None,
                    bootstrap_turn: false,
                    response_language: lang,
                    web_search_allowed: false,
                    search_remaining: 0,
                    state: None,
                    search_api_key: None,
                    diagnostics: None,
                    organizer_search_counter: None,
                    organizer_search_gate: None,
                };
                let tool_result = match tool_registry.dispatch(&mut exec_ctx, &tool_call).await {
                    Ok(result) => result.result,
                    Err(message) => {
                        last_error = Some(SummaryErrorRow {
                            path: item.path.clone(),
                            reason: message,
                            retryable: false,
                        });
                        continue;
                    }
                };
                let Some(payload_item) = tool_result
                    .get("items")
                    .and_then(Value::as_array)
                    .and_then(|items| items.first())
                else {
                    last_error = Some(SummaryErrorRow {
                        path: item.path.clone(),
                        reason: "submit_file_summaries returned no items".to_string(),
                        retryable: false,
                    });
                    continue;
                };
                let Some(short) = payload_item
                    .get("summaryShort")
                    .and_then(Value::as_str)
                    .map(str::trim)
                else {
                    last_error = Some(SummaryErrorRow {
                        path: item.path.clone(),
                        reason: "submit_file_summaries missing summaryShort".to_string(),
                        retryable: false,
                    });
                    continue;
                };
                let Some(long) = payload_item
                    .get("summaryLong")
                    .and_then(Value::as_str)
                    .map(str::trim)
                else {
                    last_error = Some(SummaryErrorRow {
                        path: item.path.clone(),
                        reason: "submit_file_summaries missing summaryLong".to_string(),
                        retryable: false,
                    });
                    continue;
                };
                let metadata = item
                    .representation
                    .metadata
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .or_else(|| Some(metadata_summary_short(item)));
                let representation = FileRepresentation {
                    metadata,
                    short: Some(short.to_string()),
                    long: Some(long.to_string()),
                    source: "model".to_string(),
                    degraded: false,
                    confidence: item.representation.confidence.clone(),
                    keywords: item.representation.keywords.clone(),
                };
                return Ok((build_summary_row(root_path, item, level, representation), usage));
            }
            Err(err) => {
                last_error = Some(SummaryErrorRow {
                    path: item.path.clone(),
                    reason: format_transport_summary_error(&err),
                    retryable: is_retryable_transport_error(&err),
                });
            }
        }
    }
    Err(last_error.unwrap_or_else(|| SummaryErrorRow {
        path: item.path.clone(),
        reason: "summary_generation_failed".to_string(),
        retryable: false,
    }))
}

fn build_summary_row(
    root_path: &str,
    item: &InventoryItem,
    level: RepresentationLevel,
    representation: FileRepresentation,
) -> Value {
    let representation = representation.prune_to_level(level);
    json!({
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
    })
}

fn is_retryable_transport_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect()
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn format_transport_summary_error(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        format!("summary_request_timeout:{err}")
    } else if err.is_connect() {
        format!("summary_request_connect_failed:{err}")
    } else {
        format!("summary_request_failed:{err}")
    }
}

fn format_http_summary_error(status: StatusCode, body: &str) -> String {
    let snippet = body.trim().chars().take(240).collect::<String>();
    if snippet.is_empty() {
        format!("summary_http_{}", status.as_u16())
    } else {
        format!("summary_http_{}:{}", status.as_u16(), snippet)
    }
}

async fn run_summary_round(
    route: &super::llm::AdvisorModelRoute,
    root_path: &str,
    items: &[InventoryItem],
    level: RepresentationLevel,
    lang: &str,
    batch_size: usize,
    max_concurrency: usize,
    extraction_config: Option<ExtractionToolConfig>,
) -> Result<(Vec<Value>, Vec<SummaryFailedItem>, TokenUsage), String> {
    if items.is_empty() {
        return Ok((Vec::new(), Vec::new(), TokenUsage::default()));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())?;
    let semaphore = Arc::new(Semaphore::new(max_concurrency.max(1)));
    let mut handles = Vec::new();
    for chunk in items.chunks(batch_size.max(1)) {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| e.to_string())?;
        let client = client.clone();
        let route = route.clone();
        let root_path = root_path.to_string();
        let lang = lang.to_string();
        let batch = chunk.to_vec();
        let ext_config = extraction_config.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            summarize_batch_with_model(&client, &route, &root_path, &batch, level, &lang, ext_config.as_ref()).await
        }));
    }
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    let mut usage = TokenUsage::default();
    for handle in handles {
        let (mut batch_rows, mut batch_failures, batch_usage) = handle
            .await
            .map_err(|e| format!("summary_batch_join_failed:{e}"))?;
        rows.append(&mut batch_rows);
        failures.append(&mut batch_failures);
        add_token_usage(&mut usage, &batch_usage);
    }
    Ok((rows, failures, usage))
}

async fn run_summary_scheduler(
    route: &super::llm::AdvisorModelRoute,
    root_path: &str,
    items: &[InventoryItem],
    level: RepresentationLevel,
    lang: &str,
    batch_size: usize,
    max_concurrency: usize,
    extraction_config: Option<ExtractionToolConfig>,
) -> Result<(Vec<Value>, Vec<SummaryErrorRow>, SummarySchedulerStats, TokenUsage), String> {
    let initial_concurrency = max_concurrency.max(1);
    if items.is_empty() {
        return Ok((
            Vec::new(),
            Vec::new(),
            SummarySchedulerStats {
                initial_concurrency,
                final_concurrency: initial_concurrency,
                retry_rounds: 0,
                degraded_concurrency: false,
            },
            TokenUsage::default(),
        ));
    }

    let mut completed_rows = Vec::new();
    let mut final_errors = Vec::new();
    let mut total_usage = TokenUsage::default();
    let mut pending = items.to_vec();
    let mut current_concurrency = initial_concurrency;
    let mut retry_rounds = 0usize;
    let mut degraded_concurrency = false;
    let backoffs_ms = [500u64, 1000u64, 2000u64];

    loop {
        let (mut rows, failures, usage) = run_summary_round(
            route,
            root_path,
            &pending,
            level,
            lang,
            batch_size,
            current_concurrency,
            extraction_config.clone(),
        )
        .await?;
        completed_rows.append(&mut rows);
        add_token_usage(&mut total_usage, &usage);

        let mut retryable_items = Vec::new();
        for failure in failures {
            if failure.error.retryable {
                retryable_items.push(failure);
            } else {
                final_errors.push(failure.error);
            }
        }

        if retryable_items.is_empty() {
            break;
        }
        if current_concurrency == 1 {
            final_errors.extend(retryable_items.into_iter().map(|failure| failure.error));
            break;
        }

        retry_rounds += 1;
        let next_concurrency = (current_concurrency / 2).max(1);
        if next_concurrency < current_concurrency {
            degraded_concurrency = true;
        }
        let delay_ms = backoffs_ms
            .get(retry_rounds.saturating_sub(1))
            .copied()
            .unwrap_or(2000);
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        current_concurrency = next_concurrency;
        pending = retryable_items
            .into_iter()
            .map(|failure| failure.item)
            .collect();
    }

    Ok((
        completed_rows,
        final_errors,
        SummarySchedulerStats {
            initial_concurrency,
            final_concurrency: current_concurrency,
            retry_rounds,
            degraded_concurrency,
        },
        total_usage,
    ))
}

fn add_token_usage(total: &mut TokenUsage, usage: &TokenUsage) {
    total.prompt = total.prompt.saturating_add(usage.prompt);
    total.completion = total.completion.saturating_add(usage.completion);
    total.total = total.total.saturating_add(usage.total);
}

fn summary_system_prompt(lang: &str, level: RepresentationLevel) -> String {
    let is_zh = lang.trim().to_ascii_lowercase().starts_with("zh");
    if is_zh {
        if level == RepresentationLevel::Short {
            r#"你负责为文件整理系统生成 summaryText。

summaryText 用于为后续文件归类提供内容证据。
你的任务是根据提供的文件内容推断文件的用途和内容特征，不是最终归类，也不是生成分类树。

请简洁总结：
1. 文件或目录的主要内容、主题、用途、文档类型、数据类型或主要对象。
2. 对分类有帮助的类型、命名或路径线索。
3. 如果信息不足，请说明具体不确定点。

不要编造未提供的内容。

输出语言使用中文。

你必须使用原生 tool calling。
不要输出普通文本结果，不要手写 JSON。
当你准备好时，调用 submit_file_summaries。"#.to_string()
        } else {
            r#"你负责为文件整理系统生成 summaryText。

summaryText 用于为后续文件归类提供内容证据。
你的任务是根据提供的文件内容推断文件的用途和内容特征，不是最终归类，也不是生成分类树。

请简洁总结：
1. 文件或目录的主要内容、主题、用途、文档类型、数据类型或主要对象。
2. 对分类有帮助的类型、命名或路径线索。
3. 如果信息不足，请说明具体不确定点。

不要编造未提供的内容。

输出语言使用中文。

你必须使用原生 tool calling。
不要输出普通文本结果，不要手写 JSON。
当你准备好时，调用 submit_file_summaries。"#.to_string()
        }
    } else {
        if level == RepresentationLevel::Short {
            r#"You are responsible for generating summaryText for a file organization system.

summaryText provides content evidence for subsequent file classification.
Your task is to infer the file's purpose and content characteristics from the provided file content, not to perform final classification or generate a category tree.

Please concisely summarize:
1. The main content, topic, purpose, document type, data type, or primary object of the file or directory.
2. Type, naming, or path clues that help with classification.
3. If information is insufficient, specify the exact uncertainty.

Do not fabricate content not provided.

Write summaries in English only.

You must use native tool calling.
Do not return plain text results and do not hand-write JSON.
When ready, call submit_file_summaries."#.to_string()
        } else {
            r#"You are responsible for generating summaryText for a file organization system.

summaryText provides content evidence for subsequent file classification.
Your task is to infer the file's purpose and content characteristics from the provided file content, not to perform final classification or generate a category tree.

Please concisely summarize:
1. The main content, topic, purpose, document type, data type, or primary object of the file or directory.
2. Type, naming, or path clues that help with classification.
3. If information is insufficient, specify the exact uncertainty.

Do not fabricate content not provided.

Write summaries in English only.

You must use native tool calling.
Do not return plain text results and do not hand-write JSON.
When ready, call submit_file_summaries."#.to_string()
        }
    }
}

fn build_summary_prompt(item: &InventoryItem, extracted: Option<&str>, _level: RepresentationLevel, _lang: &str) -> String {
    let mut prompt = format!(
        "path: {}\nname: {}\ncategory: {}\nsize: {}\nexisting metadata: {}\nexisting short summary: {}\nexisting long summary: {}",
        item.path,
        item.name,
        item.category_path.join(" / "),
        format_size_text(item.size),
        item.representation.metadata.clone().unwrap_or_default(),
        item.representation.short.clone().unwrap_or_default(),
        item.representation.long.clone().unwrap_or_default()
    );
    if let Some(content) = extracted.filter(|c| !c.trim().is_empty()) {
        prompt.push_str("\nextracted content:\n");
        prompt.push_str(content);
    }
    prompt
}

