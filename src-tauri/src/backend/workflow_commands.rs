fn hydrate_model_routing_with_secrets(state: &AppState, input: &Option<Value>) -> Option<Value> {
    let mut routing = input.clone().unwrap_or_else(|| json!({}));
    if let Some(obj) = routing.as_object_mut() {
        for modality in ["text", "image", "video", "audio"] {
            let entry = obj.entry(modality.to_string()).or_insert_with(|| json!({}));
            if !entry.is_object() {
                *entry = json!({});
            }
            let endpoint = entry
                .get("endpoint")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let model = entry
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let (resolved_endpoint, resolved_model) =
                resolve_provider_endpoint_and_model(state, Some(&endpoint), Some(&model));
            let api_key = resolve_provider_api_key(state, &resolved_endpoint).unwrap_or_default();
            if let Some(map) = entry.as_object_mut() {
                map.insert("endpoint".to_string(), Value::String(resolved_endpoint));
                map.insert("model".to_string(), Value::String(resolved_model));
                map.insert("apiKey".to_string(), Value::String(api_key));
            }
        }
    }
    Some(routing)
}

#[tauri::command]
pub async fn organize_get_capability(state: State<'_, AppState>) -> Result<Value, String> {
    let state_for_log = state.inner().clone();
    let (operation_id, started_at) = command_log_start(
        &state_for_log,
        "organizer",
        "organize_get_capability",
        json!({}),
    );
    let result = crate::organizer_runtime::organize_get_capability(state).await;
    command_log_finish(
        &state_for_log,
        "organizer",
        "organize_get_capability",
        &operation_id,
        started_at,
        &result,
        json!({}),
    );
    result
}

#[tauri::command]
pub async fn organize_start<R: Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    mut input: OrganizeStartInput,
) -> Result<Value, String> {
    let state_for_log = state.inner().clone();
    let details = json!({
        "rootPath": input.root_path.clone(),
        "excludedPatterns": input.excluded_patterns.clone(),
        "batchSize": input.batch_size,
        "summaryStrategy": input.summary_strategy.clone(),
        "maxClusterDepth": input.max_cluster_depth,
        "useWebSearch": input.use_web_search,
        "modelRouting": input.model_routing.clone(),
        "searchApiKeyProvided": input.search_api_key.is_some(),
        "responseLanguage": input.response_language.clone(),
    });
    let (operation_id, started_at) = command_log_start(
        &state_for_log,
        "organizer",
        "organize_start",
        details.clone(),
    );
    input.model_routing = hydrate_model_routing_with_secrets(state.inner(), &input.model_routing);
    if input.use_web_search.unwrap_or(false) && input.search_api_key.is_none() {
        input.search_api_key = Some(resolve_search_api_key(state.inner()).unwrap_or_default());
    }
    let result =
        crate::organizer_runtime::organize_start(app, state, input, operation_id.clone()).await;
    command_log_finish(
        &state_for_log,
        "organizer",
        "organize_start",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}

#[tauri::command]
pub async fn organize_stop<R: Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Value, String> {
    let state_for_log = state.inner().clone();
    let details = json!({ "taskId": task_id.clone() });
    let (operation_id, started_at) = command_log_start(
        &state_for_log,
        "organizer",
        "organize_stop",
        details.clone(),
    );
    let result = crate::organizer_runtime::organize_stop(app, state, task_id).await;
    command_log_finish(
        &state_for_log,
        "organizer",
        "organize_stop",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}

#[tauri::command]
pub async fn organize_get_result(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Value, String> {
    let state_for_log = state.inner().clone();
    let details = json!({ "taskId": task_id.clone() });
    let (operation_id, started_at) = command_log_start(
        &state_for_log,
        "organizer",
        "organize_get_result",
        details.clone(),
    );
    let result = crate::organizer_runtime::organize_get_result(state, task_id).await;
    command_log_finish(
        &state_for_log,
        "organizer",
        "organize_get_result",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}

#[tauri::command]
pub async fn organize_get_latest_result(
    state: State<'_, AppState>,
    root_path: String,
) -> Result<Value, String> {
    let state_for_log = state.inner().clone();
    let details = json!({ "rootPath": root_path.clone() });
    let (operation_id, started_at) = command_log_start(
        &state_for_log,
        "organizer",
        "organize_get_latest_result",
        details.clone(),
    );
    let result = crate::organizer_runtime::organize_get_latest_result(state, root_path).await;
    command_log_finish(
        &state_for_log,
        "organizer",
        "organize_get_latest_result",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}

#[tauri::command]
pub async fn organize_apply(state: State<'_, AppState>, task_id: String) -> Result<Value, String> {
    let state_for_log = state.inner().clone();
    let details = json!({ "taskId": task_id.clone() });
    let (operation_id, started_at) = command_log_start(
        &state_for_log,
        "organizer",
        "organize_apply",
        details.clone(),
    );
    let result = crate::organizer_runtime::organize_apply(state, task_id).await;
    command_log_finish(
        &state_for_log,
        "organizer",
        "organize_apply",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}

#[tauri::command]
pub async fn organize_rollback(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<Value, String> {
    let state_for_log = state.inner().clone();
    let details = json!({ "jobId": job_id.clone() });
    let (operation_id, started_at) = command_log_start(
        &state_for_log,
        "organizer",
        "organize_rollback",
        details.clone(),
    );
    let result = crate::organizer_runtime::organize_rollback(state, job_id).await;
    command_log_finish(
        &state_for_log,
        "organizer",
        "organize_rollback",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}

#[tauri::command]
pub async fn advisor_session_start(
    state: State<'_, AppState>,
    input: AdvisorSessionStartInput,
) -> Result<Value, String> {
    let state_for_log = state.inner().clone();
    let details = json!({
        "rootPath": input.root_path.clone(),
        "mode": input.mode.clone(),
        "responseLanguage": input.response_language.clone(),
    });
    let (operation_id, started_at) = command_log_start(
        &state_for_log,
        "advisor",
        "advisor_session_start",
        details.clone(),
    );
    let result = crate::advisor_runtime::advisor_session_start(state, input).await;
    command_log_finish(
        &state_for_log,
        "advisor",
        "advisor_session_start",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}

#[tauri::command]
pub async fn advisor_session_get(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Value, String> {
    let state_for_log = state.inner().clone();
    let details = json!({ "sessionId": session_id.clone() });
    let (operation_id, started_at) = command_log_start(
        &state_for_log,
        "advisor",
        "advisor_session_get",
        details.clone(),
    );
    let result = crate::advisor_runtime::advisor_session_get(state, session_id).await;
    command_log_finish(
        &state_for_log,
        "advisor",
        "advisor_session_get",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}

#[tauri::command]
pub async fn advisor_message_send(
    state: State<'_, AppState>,
    input: AdvisorMessageSendInput,
) -> Result<Value, String> {
    let state_for_log = state.inner().clone();
    let details = json!({
        "sessionId": input.session_id.clone(),
        "message": input.message.clone(),
    });
    let (operation_id, started_at) = command_log_start(
        &state_for_log,
        "advisor",
        "advisor_message_send",
        details.clone(),
    );
    let result = crate::advisor_runtime::advisor_message_send(state, input).await;
    command_log_finish(
        &state_for_log,
        "advisor",
        "advisor_message_send",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}

#[tauri::command]
pub async fn advisor_card_action(
    state: State<'_, AppState>,
    input: AdvisorCardActionInput,
) -> Result<Value, String> {
    let state_for_log = state.inner().clone();
    let details = json!({
        "sessionId": input.session_id.clone(),
        "cardId": input.card_id.clone(),
        "action": input.action.clone(),
        "payload": input.payload.clone(),
    });
    let (operation_id, started_at) = command_log_start(
        &state_for_log,
        "advisor",
        "advisor_card_action",
        details.clone(),
    );
    let result = crate::advisor_runtime::advisor_card_action(state, input).await;
    command_log_finish(
        &state_for_log,
        "advisor",
        "advisor_card_action",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}
