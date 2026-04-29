pub async fn organize_get_capability(state: State<'_, AppState>) -> Result<Value, String> {
    let settings = crate::backend::read_settings(&state.settings_path());
    let (endpoint, model) =
        crate::backend::resolve_provider_endpoint_and_model(state.inner(), None, None);
    Ok(json!({
        "selectedModel": model,
        "selectedModels": { "text": model, "image": model, "video": model, "audio": model },
        "selectedProviders": { "text": endpoint, "image": endpoint, "video": endpoint, "audio": endpoint },
        "supportsMultimodal": supports_multimodal(&model, &endpoint),
        "useWebSearch": settings.pointer("/searchApi/scopes/organizer").and_then(Value::as_bool).unwrap_or(false),
        "webSearchEnabled": settings.pointer("/searchApi/enabled").and_then(Value::as_bool).unwrap_or(false),
    }))
}

fn record_organizer_state_event(
    state: &AppState,
    level: &str,
    event: &str,
    message: &str,
    details: Value,
) {
    crate::diagnostics::record_state_event(
        state,
        level,
        "organizer",
        event,
        None,
        message,
        details,
        None,
        None,
    );
}

pub async fn organize_start<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    input: OrganizeStartInput,
    operation_id: String,
) -> Result<Value, String> {
    if input.root_path.trim().is_empty() {
        return Err("rootPath is required".to_string());
    }
    let task_id = format!("org_{}", Uuid::new_v4().simple());
    let settings = crate::backend::read_settings(&state.settings_path());
    let mut extraction_tool = extraction_tool_config_from_settings(&settings);
    let normalized_summary_strategy = normalize_summary_mode(input.summary_strategy.as_deref());
    if normalized_summary_strategy != SUMMARY_MODE_FILENAME_ONLY {
        force_enable_tika_for_summary_mode(&mut extraction_tool);
        ensure_tika_server_running(state.inner(), &mut extraction_tool).await;
    }
    let routes = parse_routes(&input.model_routing);
    let text_route = routes.get("text").cloned().unwrap_or(RouteConfig {
        endpoint: "https://api.openai.com/v1".to_string(),
        api_key: String::new(),
        model: "gpt-4o-mini".to_string(),
    });
    let (tree, tree_version) = (category_tree_to_value(&default_tree()), 0);
    let snapshot = OrganizeSnapshot {
        id: task_id.clone(),
        status: "idle".to_string(),
        error: None,
        root_path: input.root_path.clone(),
        recursive: true,
        excluded_patterns: normalize_excluded(input.excluded_patterns.clone()),
        batch_size: normalize_batch_size(input.batch_size),
        summary_strategy: normalized_summary_strategy,
        use_web_search: input.use_web_search.unwrap_or(false),
        web_search_enabled: input.use_web_search.unwrap_or(false)
            && input.search_api_key.as_deref().unwrap_or("").trim().len() > 0,
        selected_model: text_route.model.clone(),
        selected_models: json!({
            "text": routes.get("text").map(|x| x.model.clone()).unwrap_or_else(|| text_route.model.clone()),
            "image": routes.get("image").map(|x| x.model.clone()).unwrap_or_else(|| text_route.model.clone()),
            "video": routes.get("video").map(|x| x.model.clone()).unwrap_or_else(|| text_route.model.clone()),
            "audio": routes.get("audio").map(|x| x.model.clone()).unwrap_or_else(|| text_route.model.clone()),
        }),
        selected_providers: json!({
            "text": routes.get("text").map(|x| x.endpoint.clone()).unwrap_or_else(|| text_route.endpoint.clone()),
            "image": routes.get("image").map(|x| x.endpoint.clone()).unwrap_or_else(|| text_route.endpoint.clone()),
            "video": routes.get("video").map(|x| x.endpoint.clone()).unwrap_or_else(|| text_route.endpoint.clone()),
            "audio": routes.get("audio").map(|x| x.endpoint.clone()).unwrap_or_else(|| text_route.endpoint.clone()),
        }),
        supports_multimodal: supports_multimodal(&text_route.model, &text_route.endpoint),
        tree: tree.clone(),
        tree_version,
        initial_tree: Value::Null,
        base_tree_version: tree_version,
        batch_outputs: Vec::new(),
        tree_proposals: Vec::new(),
        draft_tree: Value::Null,
        proposal_mappings: Vec::new(),
        review_issues: Vec::new(),
        final_tree: Value::Null,
        final_assignments: Vec::new(),
        classification_errors: Vec::new(),
        total_files: 0,
        processed_files: 0,
        total_batches: 0,
        processed_batches: 0,
        progress: organize_progress(
            "idle",
            "Idle",
            Some("Waiting to start organize task.".to_string()),
            None,
            None,
            None,
            true,
        ),
        token_usage: TokenUsage::default(),
        results: Vec::new(),
        preview: Vec::new(),
        created_at: now_iso(),
        completed_at: None,
        job_id: None,
    };
    persist::init_organize_task(&state.db_path(), &snapshot)?;
    let task = Arc::new(OrganizeTaskRuntime {
        stop: AtomicBool::new(false),
        snapshot: Mutex::new(snapshot.clone()),
        routes,
        search_api_key: input.search_api_key.unwrap_or_default(),
        response_language: input.response_language.unwrap_or_else(|| "zh".to_string()),
        extraction_tool,
        diagnostics: OrganizerDiagnostics {
            data_dir: state.data_dir(),
            operation_id,
            task_id: task_id.clone(),
        },
        job: Mutex::new(None),
    });
    state
        .organize_tasks
        .lock()
        .insert(task_id.clone(), task.clone());
    let state_clone = state.inner().clone();
    let task_id_clone = task_id.clone();
    let app_clone = app.clone();
    let runtime = task.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let result = run_organize_task(&app_clone, &state_clone, &runtime).await;
        if runtime.stop.load(Ordering::Relaxed) {
            let mut snap = runtime.snapshot.lock();
            snap.status = "stopped".to_string();
            set_organize_progress(
                &mut snap,
                "stopped",
                "Stopped",
                Some("Organize task stopped by request.".to_string()),
                None,
                None,
                None,
                true,
            );
            snap.completed_at = Some(now_iso());
            let _ = persist::save_organize_snapshot(&state_clone.db_path(), &snap);
            let payload = serde_json::to_value(&*snap).unwrap_or_else(|_| json!({}));
            drop(snap);
            runtime
                .diagnostics
                .task_stopped("organize task stopped", payload.clone(), None);
            let _ = app_clone.emit("organize_stopped", payload);
        } else if let Err(err) = result {
            let mut snap = runtime.snapshot.lock();
            snap.status = "error".to_string();
            snap.error = Some(err.clone());
            set_organize_progress(
                &mut snap,
                "error",
                "Error",
                Some(err.clone()),
                None,
                None,
                None,
                true,
            );
            snap.completed_at = Some(now_iso());
            let _ = persist::save_organize_snapshot(&state_clone.db_path(), &snap);
            let payload = json!({ "taskId": task_id_clone, "message": err, "snapshot": &*snap });
            drop(snap);
            runtime.diagnostics.task_failed(payload.clone());
            let _ = app_clone.emit("organize_error", payload);
        }
        state_clone.organize_tasks.lock().remove(&task_id_clone);
    });
    *task.job.lock() = Some(handle);
    Ok(json!({
        "taskId": task_id,
        "summaryStrategy": snapshot.summary_strategy,
        "selectedModel": snapshot.selected_model,
        "selectedModels": snapshot.selected_models,
        "selectedProviders": snapshot.selected_providers,
        "supportsMultimodal": snapshot.supports_multimodal
    }))
}

pub async fn organize_stop<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Value, String> {
    let task = state
        .organize_tasks
        .lock()
        .get(&task_id)
        .cloned()
        .ok_or_else(|| "Task not found".to_string())?;
    task.stop.store(true, Ordering::Relaxed);
    {
        let mut snapshot = task.snapshot.lock();
        if matches!(snapshot.status.as_str(), "collecting" | "classifying") {
            snapshot.status = "stopping".to_string();
        }
        set_organize_progress(
            &mut snapshot,
            "stopped",
            "Stopping",
            Some("Stop requested. Waiting for the current stage to exit.".to_string()),
            None,
            None,
            None,
            true,
        );
    }
    emit_snapshot(&app, state.inner(), &task).await?;
    Ok(json!({ "success": true }))
}

pub async fn organize_get_result(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Value, String> {
    if let Some(task) = state.organize_tasks.lock().get(&task_id).cloned() {
        let snap = task.snapshot.lock().clone();
        return serde_json::to_value(snap).map_err(|e| e.to_string());
    }
    persist::prepare_organizer_module_access(&state.db_path())?;
    let snapshot = persist::load_organize_snapshot(&state.db_path(), &task_id)?
        .ok_or_else(|| "Task not found".to_string())?;
    serde_json::to_value(planner::hydrate_loaded_snapshot(snapshot)).map_err(|e| e.to_string())
}

pub async fn organize_get_latest_result(
    state: State<'_, AppState>,
    root_path: String,
) -> Result<Value, String> {
    let root_path = root_path.trim().to_string();
    if root_path.is_empty() {
        return Ok(Value::Null);
    }
    persist::prepare_organizer_module_access(&state.db_path())?;
    let Some(task_id) =
        persist::find_latest_organize_task_id_for_root(&state.db_path(), &root_path)?
    else {
        return Ok(Value::Null);
    };
    organize_get_result(state, task_id).await
}

pub async fn organize_apply(state: State<'_, AppState>, task_id: String) -> Result<Value, String> {
    let mut snapshot = planner::hydrate_loaded_snapshot(
        persist::load_organize_snapshot(&state.db_path(), &task_id)?
            .ok_or_else(|| "Task not found".to_string())?,
    );
    if snapshot.status != "completed" && snapshot.status != "done" {
        return Err(format!(
            "task status is {}, cannot apply move",
            snapshot.status
        ));
    }
    let plan = planner::build_apply_plan(&snapshot);
    snapshot.status = "moving".to_string();
    set_organize_progress(
        &mut snapshot,
        "moving",
        "Applying",
        Some("Moving files according to the generated plan.".to_string()),
        Some(0),
        Some(plan.len() as u64),
        Some("files"),
        plan.is_empty(),
    );
    persist::save_organize_snapshot(&state.db_path(), &snapshot)?;

    let mut entries = Vec::new();
    for row in &plan {
        let source = PathBuf::from(row.get("sourcePath").and_then(Value::as_str).unwrap_or(""));
        let item_type = row
            .get("itemType")
            .and_then(Value::as_str)
            .unwrap_or("file")
            .to_string();
        let category = sanitize_category_name(
            row.get("category")
                .and_then(Value::as_str)
                .unwrap_or(CATEGORY_OTHER_PENDING),
        );
        let planned_target =
            PathBuf::from(row.get("targetPath").and_then(Value::as_str).unwrap_or(""));
        let target_base = if planned_target.as_os_str().is_empty() {
            let fallback_name = source
                .file_name()
                .and_then(|x| x.to_str())
                .unwrap_or("item");
            PathBuf::from(&snapshot.root_path)
                .join(&category)
                .join(fallback_name)
        } else {
            planned_target
        };
        let target_dir = target_base
            .parent()
            .unwrap_or_else(|| Path::new(&snapshot.root_path))
            .to_path_buf();
        if !source.exists() {
            entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target_base.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "failed",
                "error": "source_not_found"
            }));
            continue;
        }
        if let Err(err) = fs::create_dir_all(&target_dir) {
            entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target_base.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "failed",
                "error": err.to_string()
            }));
            continue;
        }
        let target = planner::resolve_apply_target_path(&source, &target_base);
        if planner::normalize_path_key(&source) == planner::normalize_path_key(&target) {
            entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "skipped",
                "error": Value::Null
            }));
            continue;
        }
        match fs::rename(&source, &target) {
            Ok(_) => entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "moved",
                "error": Value::Null
            })),
            Err(err) => entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "category": category,
                "status": "failed",
                "error": err.to_string()
            })),
        }
    }
    let moved = entries
        .iter()
        .filter(|x| x.get("status").and_then(Value::as_str) == Some("moved"))
        .count();
    let skipped = entries
        .iter()
        .filter(|x| x.get("status").and_then(Value::as_str) == Some("skipped"))
        .count();
    let failed = entries
        .iter()
        .filter(|x| x.get("status").and_then(Value::as_str) == Some("failed"))
        .count();
    let job_id = format!("job_{}", Uuid::new_v4().simple());
    let total_entries = entries.len() as u64;
    let manifest = json!({
        "jobId": job_id,
        "taskId": task_id,
        "rootPath": snapshot.root_path,
        "createdAt": now_iso(),
        "batchSize": snapshot.batch_size,
        "recursive": snapshot.recursive,
        "entries": entries,
        "summary": {
            "moved": moved,
            "skipped": skipped,
            "failed": failed,
            "total": total_entries
        }
    });
    record_organizer_state_event(
        state.inner(),
        if failed > 0 { "warn" } else { "info" },
        "organize_apply_manifest",
        "organize apply move results",
        json!({
            "taskId": manifest.get("taskId").cloned().unwrap_or(Value::Null),
            "jobId": manifest.get("jobId").cloned().unwrap_or(Value::Null),
            "manifest": manifest.clone(),
        }),
    );
    persist::save_organize_manifest(&state.db_path(), &manifest)?;
    snapshot.status = "done".to_string();
    set_organize_progress(
        &mut snapshot,
        "completed",
        "Completed",
        Some("File move stage finished.".to_string()),
        Some(total_entries),
        Some(total_entries),
        Some("files"),
        false,
    );
    snapshot.job_id = manifest
        .get("jobId")
        .and_then(Value::as_str)
        .map(|x| x.to_string());
    persist::save_organize_snapshot(&state.db_path(), &snapshot)?;
    if let Some(task) = state.organize_tasks.lock().get(&task_id).cloned() {
        *task.snapshot.lock() = snapshot;
    }
    Ok(json!({ "success": true, "manifest": manifest }))
}

pub async fn organize_rollback(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<Value, String> {
    let manifest = persist::load_organize_job(&state.db_path(), &job_id)?
        .ok_or_else(|| "job manifest not found".to_string())?;
    let task_id = manifest
        .get("taskId")
        .and_then(Value::as_str)
        .map(|value| value.to_string());
    let root_path = PathBuf::from(
        manifest
            .get("rootPath")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let mut entries = persist::load_organize_job_entries(&state.db_path(), &job_id)?;
    entries.reverse();
    let mut rollback_entries = Vec::new();
    for entry in entries {
        let item_type = entry
            .get("itemType")
            .and_then(Value::as_str)
            .unwrap_or("file")
            .to_string();
        let source = PathBuf::from(
            entry
                .get("sourcePath")
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        let target = PathBuf::from(
            entry
                .get("targetPath")
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        if entry.get("status").and_then(Value::as_str) != Some("moved") {
            rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "status": "skipped",
                "error": "not_moved_in_apply"
            }));
            continue;
        }
        if !target.exists() {
            rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "status": "failed",
                "error": "target_not_found"
            }));
            continue;
        }
        if source.exists() {
            rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "status": "failed",
                "error": "source_already_exists"
            }));
            continue;
        }
        if let Some(parent) = source.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match fs::rename(&target, &source) {
            Ok(_) => {
                if let Some(target_parent) = target.parent() {
                    planner::prune_empty_dirs_upward(target_parent, &root_path);
                }
                rollback_entries.push(json!({
                    "sourcePath": source.to_string_lossy().to_string(),
                    "targetPath": target.to_string_lossy().to_string(),
                    "itemType": item_type,
                    "status": "rolled_back",
                    "error": Value::Null
                }))
            }
            Err(err) => rollback_entries.push(json!({
                "sourcePath": source.to_string_lossy().to_string(),
                "targetPath": target.to_string_lossy().to_string(),
                "itemType": item_type,
                "status": "failed",
                "error": err.to_string()
            })),
        }
    }
    let rollback = json!({
        "at": now_iso(),
        "entries": rollback_entries,
        "summary": {
            "rolledBack": rollback_entries.iter().filter(|x| x.get("status").and_then(Value::as_str) == Some("rolled_back")).count(),
            "failed": rollback_entries.iter().filter(|x| x.get("status").and_then(Value::as_str) == Some("failed")).count(),
            "skipped": rollback_entries.iter().filter(|x| x.get("status").and_then(Value::as_str) == Some("skipped")).count(),
            "total": rollback_entries.len()
        }
    });
    let rollback_failed = rollback
        .get("summary")
        .and_then(|summary| summary.get("failed"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    record_organizer_state_event(
        state.inner(),
        if rollback_failed > 0 { "warn" } else { "info" },
        "organize_rollback_result",
        "organize rollback results",
        json!({
            "jobId": job_id.clone(),
            "taskId": task_id.clone(),
            "rollback": rollback.clone(),
        }),
    );
    persist::save_organize_rollback(&state.db_path(), &job_id, &rollback)?;

    let failed = rollback
        .get("summary")
        .and_then(|summary| summary.get("failed"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if failed == 0 {
        if let Some(task_id) = task_id {
            if let Some(mut snapshot) = persist::load_organize_snapshot(&state.db_path(), &task_id)?
            {
                snapshot = planner::hydrate_loaded_snapshot(snapshot);
                snapshot.status = "completed".to_string();
                let processed_batches = snapshot.processed_batches;
                let total_batches = snapshot.total_batches;
                set_organize_progress(
                    &mut snapshot,
                    "completed",
                    "Completed",
                    Some("Rollback finished. Organize result is available again.".to_string()),
                    Some(processed_batches),
                    Some(total_batches),
                    Some("batches"),
                    false,
                );
                snapshot.job_id = None;
                persist::save_organize_snapshot(&state.db_path(), &snapshot)?;
                if let Some(task) = state.organize_tasks.lock().get(&task_id).cloned() {
                    *task.snapshot.lock() = snapshot;
                }
            }
        }
    }

    Ok(json!({
        "success": true,
        "jobId": manifest.get("jobId").and_then(Value::as_str).unwrap_or(&job_id),
        "rollback": rollback
    }))
}
