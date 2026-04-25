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

fn rename_category(session: &mut Value, change: &Value) -> Result<Vec<Value>, String> {
    let source_category_id = required_change_str(change, "sourceCategoryId")?;
    let new_category_name = required_change_str(change, "newCategoryName")?;
    let source_path =
        category_path_from_category_id(session, source_category_id)?.ok_or_else(|| {
            "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
        })?;
    let mut target_path = source_path.clone();
    if let Some(last) = target_path.last_mut() {
        *last = new_category_name.to_string();
    }
    reclass_by_category_path(session, &source_path, &target_path)
}

fn merge_category_into_category(session: &mut Value, change: &Value) -> Result<Vec<Value>, String> {
    let source_category_id = required_change_str(change, "sourceCategoryId")?;
    let target_category_id = required_change_str(change, "targetCategoryId")?;
    let source_path =
        category_path_from_category_id(session, source_category_id)?.ok_or_else(|| {
            "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
        })?;
    let target_path =
        category_path_from_category_id(session, target_category_id)?.ok_or_else(|| {
            "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
        })?;
    reclass_by_category_path(session, &source_path, &target_path)
}

fn delete_empty_category(session: &mut Value, change: &Value) -> Result<Vec<Value>, String> {
    let source_category_id = required_change_str(change, "sourceCategoryId")?;
    let source_path =
        category_path_from_category_id(session, source_category_id)?.ok_or_else(|| {
            "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
        })?;
    let overrides = super::types::inventory_overrides(session);
    if overrides.values().any(|path| path == &source_path) {
        return Err("当前分类下仍有文件，不能删除非空分类。".to_string());
    }
    Ok(Vec::new())
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

fn build_reclassification_change_summary(change: &Value, rollback_entries: &[Value]) -> String {
    let change_type = change
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let file_count = rollback_entries.len();
    format!("{change_type}: {file_count} items updated")
}

async fn summarize_batch_with_model(
    client: &reqwest::Client,
    route: &super::llm::AdvisorModelRoute,
    root_path: &str,
    batch: &[InventoryItem],
    level: RepresentationLevel,
    lang: &str,
) -> (Vec<Value>, Vec<SummaryFailedItem>) {
    let mut output = Vec::new();
    let mut errors = Vec::new();
    for item in batch {
        match summarize_item_with_model(client, route, root_path, item, level, lang).await {
            Ok(row) => output.push(row),
            Err(error) => errors.push(SummaryFailedItem {
                item: item.clone(),
                error,
            }),
        }
    }
    (output, errors)
}

async fn summarize_item_with_model(
    client: &reqwest::Client,
    route: &super::llm::AdvisorModelRoute,
    root_path: &str,
    item: &InventoryItem,
    level: RepresentationLevel,
    lang: &str,
) -> Result<Value, SummaryErrorRow> {
    let prompt = build_summary_prompt(item, level, lang);
    let api_format = detect_api_format(&route.endpoint);
    let request = build_completion_payload(
        api_format,
        &route.model,
        &[
            json!({ "role": "system", "content": summary_system_prompt(lang, level) }),
            json!({ "role": "user", "content": prompt }),
        ],
        None,
        0.0,
        DEFAULT_MAX_TOKENS,
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
                let assistant_text = parse_completion_response(api_format, status, &body)
                    .map(|parsed| parsed.assistant_text)
                    .unwrap_or_else(|_| body.clone());
                let Some((short, long)) = parse_summary_response(&assistant_text) else {
                    last_error = Some(SummaryErrorRow {
                        path: item.path.clone(),
                        reason: "summary_response_parse_failed".to_string(),
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
                    short: Some(short),
                    long: Some(long),
                    source: "model".to_string(),
                    degraded: false,
                    confidence: item.representation.confidence.clone(),
                    keywords: item.representation.keywords.clone(),
                };
                return Ok(build_summary_row(root_path, item, level, representation));
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
) -> Result<(Vec<Value>, Vec<SummaryFailedItem>), String> {
    if items.is_empty() {
        return Ok((Vec::new(), Vec::new()));
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
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            summarize_batch_with_model(&client, &route, &root_path, &batch, level, &lang).await
        }));
    }
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for handle in handles {
        let (mut batch_rows, mut batch_failures) = handle
            .await
            .map_err(|e| format!("summary_batch_join_failed:{e}"))?;
        rows.append(&mut batch_rows);
        failures.append(&mut batch_failures);
    }
    Ok((rows, failures))
}

async fn run_summary_scheduler(
    route: &super::llm::AdvisorModelRoute,
    root_path: &str,
    items: &[InventoryItem],
    level: RepresentationLevel,
    lang: &str,
    batch_size: usize,
    max_concurrency: usize,
) -> Result<(Vec<Value>, Vec<SummaryErrorRow>, SummarySchedulerStats), String> {
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
        ));
    }

    let mut completed_rows = Vec::new();
    let mut final_errors = Vec::new();
    let mut pending = items.to_vec();
    let mut current_concurrency = initial_concurrency;
    let mut retry_rounds = 0usize;
    let mut degraded_concurrency = false;
    let backoffs_ms = [500u64, 1000u64, 2000u64];

    loop {
        let (mut rows, failures) = run_summary_round(
            route,
            root_path,
            &pending,
            level,
            lang,
            batch_size,
            current_concurrency,
        )
        .await?;
        completed_rows.append(&mut rows);

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
    ))
}

fn summary_system_prompt(lang: &str, level: RepresentationLevel) -> String {
    let is_zh = lang.trim().to_ascii_lowercase().starts_with("zh");
    if is_zh {
        if level == RepresentationLevel::Short {
            r#"你负责为文件整理系统生成 summaryText。

summaryText 用于为后续文件归类提供内容证据。
你的任务是概括当前输入中可读或可解析的信息，不是最终归类，也不是生成分类树。

请简洁总结：
1. 文件或目录的主要内容、主题、用途、文档类型、数据类型或主要对象。
2. 对分类有帮助的类型、命名或路径线索。
3. 如果信息不足，请说明具体不确定点。

不要编造未提供的内容。

输出语言使用中文。

只返回 JSON：{"summaryShort":"...","summaryLong":"..."}"#.to_string()
        } else {
            r#"你负责为文件整理系统生成 summaryText。

summaryText 用于为后续文件归类提供内容证据。
你的任务是概括当前输入中可读或可解析的信息，不是最终归类，也不是生成分类树。

请简洁总结：
1. 文件或目录的主要内容、主题、用途、文档类型、数据类型或主要对象。
2. 对分类有帮助的类型、命名或路径线索。
3. 如果信息不足，请说明具体不确定点。

不要编造未提供的内容。

输出语言使用中文。

只返回 JSON：{"summaryShort":"...","summaryLong":"..."}"#.to_string()
        }
    } else {
        if level == RepresentationLevel::Short {
            r#"You are responsible for generating summaryText for a file organization system.

summaryText provides content evidence for subsequent file classification.
Your task is to summarize readable or parseable information from the current input, not to perform final classification or generate a category tree.

Please concisely summarize:
1. The main content, topic, purpose, document type, data type, or primary object of the file or directory.
2. Type, naming, or path clues that help with classification.
3. If information is insufficient, specify the exact uncertainty.

Do not fabricate content not provided.

Write summaries in English only.

Return JSON only: {"summaryShort":"...","summaryLong":"..."}"#.to_string()
        } else {
            r#"You are responsible for generating summaryText for a file organization system.

summaryText provides content evidence for subsequent file classification.
Your task is to summarize readable or parseable information from the current input, not to perform final classification or generate a category tree.

Please concisely summarize:
1. The main content, topic, purpose, document type, data type, or primary object of the file or directory.
2. Type, naming, or path clues that help with classification.
3. If information is insufficient, specify the exact uncertainty.

Do not fabricate content not provided.

Write summaries in English only.

Return JSON only: {"summaryShort":"...","summaryLong":"..."}"#.to_string()
        }
    }
}

fn build_summary_prompt(item: &InventoryItem, _level: RepresentationLevel, _lang: &str) -> String {
    format!(
        "path: {}\nname: {}\ncategory: {}\nsize: {}\nexisting metadata: {}\nexisting short summary: {}\nexisting long summary: {}",
        item.path,
        item.name,
        item.category_path.join(" / "),
        format_size_text(item.size),
        item.representation.metadata.clone().unwrap_or_default(),
        item.representation.short.clone().unwrap_or_default(),
        item.representation.long.clone().unwrap_or_default()
    )
}

fn parse_summary_response(body: &str) -> Option<(String, String)> {
    let parsed = serde_json::from_str::<Value>(body).ok().or_else(|| {
        let start = body.find('{')?;
        let end = body.rfind('}')?;
        serde_json::from_str::<Value>(&body[start..=end]).ok()
    })?;
    let content = parsed
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or(body);
    let payload = serde_json::from_str::<Value>(content).ok().or_else(|| {
        let start = content.find('{')?;
        let end = content.rfind('}')?;
        serde_json::from_str::<Value>(&content[start..=end]).ok()
    })?;
    Some((
        payload.get("summaryShort")?.as_str()?.trim().to_string(),
        payload.get("summaryLong")?.as_str()?.trim().to_string(),
    ))
}

