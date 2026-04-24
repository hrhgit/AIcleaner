#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>) -> Result<Value, String> {
    let (operation_id, started_at) =
        command_log_start(state.inner(), "settings", "settings_get", json!({}));
    let result = {
        let settings_path = state.settings_path();
        let raw = read_settings(&settings_path);
        Ok(redact_settings_for_client(state.inner(), &raw))
    };
    command_log_finish(
        state.inner(),
        "settings",
        "settings_get",
        &operation_id,
        started_at,
        &result,
        json!({}),
    );
    result
}

#[tauri::command]
pub async fn settings_save(state: State<'_, AppState>, data: Value) -> Result<Value, String> {
    let (operation_id, started_at) = command_log_start(
        state.inner(),
        "settings",
        "settings_save",
        json!({ "settings": data.clone() }),
    );
    let result = (|| -> Result<Value, String> {
        let settings_path = state.settings_path();
        write_settings(&settings_path, &data)?;
        let raw = read_settings(&settings_path);
        Ok(json!({
            "success": true,
            "settings": redact_settings_for_client(state.inner(), &raw)
        }))
    })();
    command_log_finish(
        state.inner(),
        "settings",
        "settings_save",
        &operation_id,
        started_at,
        &result,
        json!({ "settings": data }),
    );
    result
}

fn command_log_start(
    state: &AppState,
    module: &str,
    event: &str,
    details: Value,
) -> (String, Instant) {
    let operation_id = crate::diagnostics::new_operation_id();
    crate::diagnostics::command_start(state, module, event, &operation_id, details);
    (operation_id, Instant::now())
}

fn command_log_finish(
    state: &AppState,
    module: &str,
    event: &str,
    operation_id: &str,
    started_at: Instant,
    result: &Result<Value, String>,
    details: Value,
) {
    crate::diagnostics::command_finish(
        state,
        module,
        event,
        operation_id,
        started_at,
        result,
        details,
    );
}

fn credentials_save_log_details(data: &CredentialsSaveInput) -> Value {
    json!({
        "providerSecretEndpoints": data
            .provider_secrets
            .as_ref()
            .map(|providers| providers.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default(),
        "searchApiKeyTouched": data.search_api_key.is_some(),
    })
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDataDirInput {
    path: String,
}

fn migrate_data_dir(state: &AppState, target_path: &str) -> Result<Value, String> {
    if !state.organize_tasks.lock().is_empty() {
        return Err(
            "Please stop running organize tasks before moving the cache directory.".to_string(),
        );
    }

    let current_data_dir = state.data_dir();
    let target_data_dir = PathBuf::from(target_path.trim());
    validate_data_dir_target(&current_data_dir, &target_data_dir)?;
    fs::create_dir_all(&target_data_dir).map_err(|e| e.to_string())?;

    let bootstrap_key =
        crate::persist::normalize_root_path(&state.bootstrap_path.to_string_lossy());
    let mut skip_paths = HashSet::new();
    skip_paths.insert(bootstrap_key);

    copy_dir_contents_recursive(&current_data_dir, &target_data_dir, &skip_paths)?;

    let target_paths = AppPaths::from_data_dir(target_data_dir.clone());
    if !target_paths.settings_path.exists() {
        fs::write(
            &target_paths.settings_path,
            serde_json::to_vec_pretty(&default_settings()).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    crate::persist::init_db(&target_paths.db_path)?;
    crate::persist::mark_stale_tasks(&target_paths.db_path)?;

    write_storage_location_config(
        &state.base_data_dir,
        &state.bootstrap_path,
        &target_data_dir,
    )?;
    state.set_data_dir(target_data_dir.clone());

    let cleanup_warning = remove_dir_contents_recursive(&current_data_dir, &skip_paths).err();
    let settings = read_settings(&target_paths.settings_path);
    Ok(json!({
        "success": true,
        "dataDir": target_data_dir.to_string_lossy().to_string(),
        "cleanupWarning": cleanup_warning,
        "settings": redact_settings_for_client(state, &settings)
    }))
}

#[tauri::command]
pub async fn settings_move_data_dir(
    state: State<'_, AppState>,
    data: SettingsDataDirInput,
) -> Result<Value, String> {
    let details = json!({ "targetPath": data.path.clone() });
    let (operation_id, started_at) = command_log_start(
        state.inner(),
        "settings",
        "settings_move_data_dir",
        details.clone(),
    );
    let result = migrate_data_dir(state.inner(), &data.path);
    command_log_finish(
        state.inner(),
        "settings",
        "settings_move_data_dir",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}

#[tauri::command]
pub async fn credentials_get(state: State<'_, AppState>) -> Result<Value, String> {
    let (operation_id, started_at) =
        command_log_start(state.inner(), "settings", "credentials_get", json!({}));
    let result = (|| -> Result<Value, String> {
        let settings_path = state.settings_path();
        let settings = read_settings(&settings_path);
        let mut provider_key_map = HashMap::new();
        let mut secret_keys = Vec::new();
        if let Some(configs) = settings.get("providerConfigs").and_then(Value::as_object) {
            for (key, config) in configs {
                let endpoint = config
                    .get("endpoint")
                    .and_then(Value::as_str)
                    .unwrap_or(key)
                    .trim()
                    .to_string();
                if endpoint.is_empty() {
                    continue;
                }
                let secret_key = provider_secret_key(&endpoint);
                provider_key_map.insert(endpoint, secret_key.clone());
                secret_keys.push(secret_key);
            }
        }
        secret_keys.push(SEARCH_SECRET_KEY.to_string());
        let secret_values = state.credential_store.get_many(&secret_keys)?;
        let mut providers = HashMap::new();
        for (endpoint, secret_key) in provider_key_map {
            providers.insert(
                endpoint,
                secret_values.get(&secret_key).cloned().unwrap_or_default(),
            );
        }
        let search_api_key = secret_values
            .get(SEARCH_SECRET_KEY)
            .cloned()
            .unwrap_or_default();
        Ok(json!({
            "providerSecrets": providers,
            "searchApiKey": search_api_key
        }))
    })();
    command_log_finish(
        state.inner(),
        "settings",
        "credentials_get",
        &operation_id,
        started_at,
        &result,
        json!({}),
    );
    result
}

#[tauri::command]
pub async fn credentials_save(
    state: State<'_, AppState>,
    data: CredentialsSaveInput,
) -> Result<Value, String> {
    let details = credentials_save_log_details(&data);
    let (operation_id, started_at) = command_log_start(
        state.inner(),
        "settings",
        "credentials_save",
        details.clone(),
    );
    let result = save_credentials_internal(state.inner(), data);
    command_log_finish(
        state.inner(),
        "settings",
        "credentials_save",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}

fn save_credentials_internal(
    state: &AppState,
    data: CredentialsSaveInput,
) -> Result<Value, String> {
    let started_at = Instant::now();
    let settings_path = state.settings_path();
    let settings = read_settings(&settings_path);
    let provider_secrets = data.provider_secrets.unwrap_or_default();
    let mut entries = Vec::new();
    if let Some(configs) = settings.get("providerConfigs").and_then(Value::as_object) {
        for (key, config) in configs {
            let endpoint = config
                .get("endpoint")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .trim()
                .to_string();
            if let Some(raw) = provider_secrets.get(&endpoint) {
                entries.push((provider_secret_key(&endpoint), raw.trim().to_string()));
            }
        }
    }
    if let Some(search_api_key) = data.search_api_key {
        entries.push((
            SEARCH_SECRET_KEY.to_string(),
            search_api_key.trim().to_string(),
        ));
    }
    if !entries.is_empty() {
        state.credential_store.set_many(&entries)?;
    }
    let latest_status = build_credentials_status_value(state, &settings);
    let (provider_meta, search_has_api_key) = extract_credentials_meta(&latest_status);
    let next_settings =
        update_credentials_meta_in_settings(state, &provider_meta, search_has_api_key)?;
    log::info!(
        "credentials_save completed in {} ms (entries={})",
        started_at.elapsed().as_millis(),
        entries.len()
    );
    Ok(json!({
        "success": true,
        "credentialsStatus": build_credentials_status_value(state, &next_settings)
    }))
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelsInput {
    endpoint: String,
    api_key: Option<String>,
}

#[tauri::command]
pub async fn settings_get_provider_models(
    state: State<'_, AppState>,
    data: ProviderModelsInput,
) -> Result<Value, String> {
    let details = json!({
        "endpoint": data.endpoint.clone(),
        "apiKeyProvided": data.api_key.is_some(),
    });
    let (operation_id, started_at) = command_log_start(
        state.inner(),
        "settings",
        "settings_get_provider_models",
        details.clone(),
    );
    let result: Result<Value, String> = async {
        let endpoint = data.endpoint.trim();
        if endpoint.is_empty() {
            return Err("Missing endpoint".to_string());
        }
        let api_key = if let Some(raw) = data.api_key {
            raw.trim().to_string()
        } else {
            resolve_provider_api_key(state.inner(), endpoint).unwrap_or_default()
        };
        let url = format!("{}/models", endpoint.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        let mut req = client.get(url.clone()).header("Accept", "application/json");
        if !api_key.is_empty() {
            req = req
                .header("Authorization", format!("Bearer {}", api_key))
                .header("x-api-key", api_key.clone())
                .header("api-key", api_key.clone());
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = resp.status();
        let raw_body = resp.text().await.map_err(|e| e.to_string())?;
        crate::diagnostics::record_state_event(
            state.inner(),
            if status.is_success() { "info" } else { "error" },
            "settings",
            "settings_get_provider_models_http",
            Some(&operation_id),
            "provider models HTTP response",
            json!({
                "endpoint": endpoint,
                "url": url,
                "status": status.as_u16(),
                "rawBody": raw_body.clone(),
            }),
            None,
            None,
        );
        if !status.is_success() {
            return Err(format!("Failed to fetch models ({})", status.as_u16()));
        }
        let payload: Value = serde_json::from_str(&raw_body).map_err(|e| e.to_string())?;
        let raw = payload
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| payload.get("models").and_then(Value::as_array).cloned())
            .unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        let mut models = Vec::new();
        for item in raw {
            let id = if item.is_string() {
                item.as_str().unwrap_or("").trim().to_string()
            } else {
                item.get("id")
                    .or_else(|| item.get("name"))
                    .or_else(|| item.get("model"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };
            if !id.is_empty() && seen.insert(id.clone()) {
                models.push(json!({ "value": id, "label": id }));
            }
        }
        Ok(json!({ "success": true, "models": models }))
    }
    .await;
    command_log_finish(
        state.inner(),
        "settings",
        "settings_get_provider_models",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}

#[tauri::command]
pub async fn settings_browse_folder(state: State<'_, AppState>) -> Result<Value, String> {
    let (operation_id, started_at) = command_log_start(
        state.inner(),
        "settings",
        "settings_browse_folder",
        json!({}),
    );
    let result: Result<Value, String> = async {
        let picked = tauri::async_runtime::spawn_blocking(|| rfd::FileDialog::new().pick_folder())
            .await
            .map_err(|e| e.to_string())?;
        if let Some(path) = picked {
            Ok(json!({ "success": true, "cancelled": false, "path": path.to_string_lossy().to_string() }))
        } else {
            Ok(json!({ "success": true, "cancelled": true }))
        }
    }
    .await;
    command_log_finish(
        state.inner(),
        "settings",
        "settings_browse_folder",
        &operation_id,
        started_at,
        &result,
        json!({}),
    );
    result
}

#[tauri::command]
pub async fn system_get_privilege(state: State<'_, AppState>) -> Result<Value, String> {
    let (operation_id, started_at) =
        command_log_start(state.inner(), "system", "system_get_privilege", json!({}));
    let result = Ok(json!({
        "platform": if cfg!(windows) { "win32" } else { std::env::consts::OS },
        "isAdmin": if cfg!(windows) { check_elevation::is_elevated().unwrap_or(false) } else { false }
    }));
    command_log_finish(
        state.inner(),
        "system",
        "system_get_privilege",
        &operation_id,
        started_at,
        &result,
        json!({}),
    );
    result
}

#[tauri::command]
pub async fn system_request_elevation(state: State<'_, AppState>) -> Result<Value, String> {
    let (operation_id, started_at) = command_log_start(
        state.inner(),
        "system",
        "system_request_elevation",
        json!({}),
    );
    let result = (|| -> Result<Value, String> {
        if !cfg!(windows) {
            return Err("Elevation is only supported on Windows.".to_string());
        }
        if check_elevation::is_elevated().unwrap_or(false) {
            return Ok(json!({ "success": true, "alreadyAdmin": true }));
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe_str = exe.to_string_lossy().replace('\'', "''");
        let work_dir = exe
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_string_lossy()
            .replace('\'', "''");
        let ps = format!(
            "Start-Process -Verb RunAs -FilePath '{}' -WorkingDirectory '{}'",
            exe_str, work_dir
        );
        let status = Command::new("powershell.exe")
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-Command")
            .arg(ps)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Failed to request elevation.".to_string());
        }
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(250));
            std::process::exit(0);
        });
        Ok(json!({ "success": true, "restarting": true, "reloadRecommended": true }))
    })();
    command_log_finish(
        state.inner(),
        "system",
        "system_request_elevation",
        &operation_id,
        started_at,
        &result,
        json!({}),
    );
    result
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OpenExternalUrlInput {
    url: String,
}

#[tauri::command]
pub async fn system_open_external_url(
    state: State<'_, AppState>,
    data: OpenExternalUrlInput,
) -> Result<Value, String> {
    let details = json!({ "url": data.url.clone() });
    let (operation_id, started_at) = command_log_start(
        state.inner(),
        "system",
        "system_open_external_url",
        details.clone(),
    );
    let result = (|| -> Result<Value, String> {
        let parsed = Url::parse(data.url.trim()).map_err(|e| e.to_string())?;
        match parsed.scheme() {
            "http" | "https" => {}
            _ => return Err("Only http(s) URLs are allowed.".to_string()),
        }
        tauri_plugin_opener::open_url(parsed.as_str(), None::<&str>).map_err(|e| e.to_string())?;
        Ok(json!({ "success": true, "url": parsed.as_str() }))
    })();
    command_log_finish(
        state.inner(),
        "system",
        "system_open_external_url",
        &operation_id,
        started_at,
        &result,
        details,
    );
    result
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeStartInput {
    pub root_path: String,
    pub excluded_patterns: Option<Vec<String>>,
    pub batch_size: Option<u32>,
    #[serde(alias = "summaryMode")]
    pub summary_strategy: Option<String>,
    pub max_cluster_depth: Option<u32>,
    pub use_web_search: Option<bool>,
    pub model_routing: Option<Value>,
    pub search_api_key: Option<String>,
    pub response_language: Option<String>,
}

pub use crate::advisor_runtime::{
    AdvisorCardActionInput, AdvisorMessageSendInput, AdvisorSessionStartInput,
};

