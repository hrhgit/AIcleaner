use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::Digest;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Runtime, State};
use tauri_plugin_stronghold::stronghold::Stronghold as SecureStronghold;

const STRONGHOLD_FILE: &str = "secrets.hold";
const STRONGHOLD_CLIENT: &[u8] = b"aicleaner";
const SEARCH_SECRET_KEY: &str = "search:tavily:apiKey";

#[derive(Clone)]
pub struct AppState {
    pub data_dir: PathBuf,
    pub settings_path: PathBuf,
    pub db_path: PathBuf,
    pub(crate) scan_tasks: Arc<Mutex<HashMap<String, Arc<crate::scan_runtime::ScanTaskRuntime>>>>,
    pub(crate) organize_tasks:
        Arc<Mutex<HashMap<String, Arc<crate::organizer_runtime::OrganizeTaskRuntime>>>>,
    secret_vault: Arc<Mutex<SecretVaultState>>,
}

struct SecretVaultState {
    snapshot_path: PathBuf,
    session: Option<SecureStronghold>,
}

impl SecretVaultState {
    fn new(snapshot_path: PathBuf) -> Self {
        Self {
            snapshot_path,
            session: None,
        }
    }

    fn is_initialized(&self) -> bool {
        self.snapshot_path.exists()
    }

    fn is_unlocked(&self) -> bool {
        self.session.is_some()
    }

    fn setup(&mut self, password: &str) -> Result<(), String> {
        if self.is_initialized() {
            return Err("Secret vault is already initialized.".to_string());
        }
        let stronghold = open_stronghold(&self.snapshot_path, password)?;
        let _ = ensure_client(&stronghold)?;
        stronghold.save().map_err(|e| e.to_string())?;
        self.session = Some(stronghold);
        Ok(())
    }

    fn unlock(&mut self, password: &str) -> Result<(), String> {
        if !self.is_initialized() {
            return Err("Secret vault is not initialized.".to_string());
        }
        let stronghold = open_stronghold(&self.snapshot_path, password)?;
        let _ = ensure_client(&stronghold)?;
        self.session = Some(stronghold);
        Ok(())
    }

    fn lock(&mut self) {
        self.session = None;
    }

    fn reset(&mut self) -> Result<(), String> {
        self.session = None;
        if self.snapshot_path.exists() {
            fs::remove_file(&self.snapshot_path).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn read(&self, key: &str) -> Result<String, String> {
        let stronghold = self
            .session
            .as_ref()
            .ok_or_else(|| "Secret vault is locked.".to_string())?;
        let client = ensure_client(stronghold)?;
        let raw = client
            .store()
            .get(key.as_bytes())
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        String::from_utf8(raw).map_err(|e| e.to_string())
    }

    fn write(&self, key: &str, value: &str) -> Result<(), String> {
        let stronghold = self
            .session
            .as_ref()
            .ok_or_else(|| "Secret vault is locked.".to_string())?;
        let client = ensure_client(stronghold)?;
        if value.trim().is_empty() {
            let _ = client
                .store()
                .delete(key.as_bytes())
                .map_err(|e| e.to_string())?;
        } else {
            let _ = client
                .store()
                .insert(key.as_bytes().to_vec(), value.as_bytes().to_vec(), None)
                .map_err(|e| e.to_string())?;
        }
        stronghold.save().map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn hash_password(password: &str) -> Vec<u8> {
    let mut hasher = sha1::Sha1::new();
    hasher.update(password.as_bytes());
    hasher.finalize().to_vec()
}

fn open_stronghold(path: &Path, password: &str) -> Result<SecureStronghold, String> {
    SecureStronghold::new(path, hash_password(password)).map_err(|e| e.to_string())
}

fn ensure_client(stronghold: &SecureStronghold) -> Result<iota_stronghold::Client, String> {
    if let Ok(client) = stronghold.get_client(STRONGHOLD_CLIENT) {
        return Ok(client);
    }
    if let Ok(client) = stronghold.load_client(STRONGHOLD_CLIENT) {
        return Ok(client);
    }
    stronghold
        .create_client(STRONGHOLD_CLIENT)
        .map_err(|e| e.to_string())
}

fn provider_secret_key(endpoint: &str) -> String {
    format!("provider:{}:apiKey", endpoint.trim())
}

impl AppState {
    pub fn bootstrap(data_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
        let settings_path = data_dir.join("settings.json");
        let db_path = data_dir.join("scan-cache.sqlite");
        if !settings_path.exists() {
            fs::write(
                &settings_path,
                serde_json::to_vec_pretty(&default_settings()).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        }
        crate::persist::init_db(&db_path)?;
        crate::persist::mark_stale_tasks(&db_path)?;
        Ok(Self {
            secret_vault: Arc::new(Mutex::new(SecretVaultState::new(
                data_dir.join(STRONGHOLD_FILE),
            ))),
            data_dir,
            settings_path,
            db_path,
            scan_tasks: Arc::new(Mutex::new(HashMap::new())),
            organize_tasks: Arc::new(Mutex::new(HashMap::new())),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub prompt: u64,
    pub completion: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScanResultItem {
    pub name: String,
    pub path: String,
    pub size: u64,
    #[serde(rename = "type")]
    pub item_type: String,
    pub purpose: String,
    pub reason: String,
    pub risk: String,
    pub classification: String,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSnapshot {
    pub id: String,
    pub status: String,
    pub target_path: String,
    pub auto_analyze: bool,
    pub root_node_id: String,
    pub current_path: String,
    pub current_depth: u32,
    pub scanned_count: u64,
    pub total_entries: u64,
    pub processed_entries: u64,
    pub deletable_count: u64,
    pub total_cleanable: u64,
    pub target_size: u64,
    pub token_usage: TokenUsage,
    pub deletable: Vec<ScanResultItem>,
    pub permission_denied_count: u64,
    pub permission_denied_paths: Vec<String>,
    pub error_message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeSnapshot {
    pub id: String,
    pub status: String,
    pub error: Option<String>,
    pub root_path: String,
    pub recursive: bool,
    pub mode: String,
    pub categories: Vec<String>,
    pub allow_new_categories: bool,
    pub excluded_patterns: Vec<String>,
    pub parallelism: u32,
    pub use_web_search: bool,
    pub web_search_enabled: bool,
    pub selected_model: String,
    pub selected_models: Value,
    pub selected_providers: Value,
    pub supports_multimodal: bool,
    pub total_files: u64,
    pub processed_files: u64,
    pub token_usage: TokenUsage,
    pub results: Vec<Value>,
    pub preview: Vec<Value>,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub job_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretPasswordInput {
    password: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretResetInput {
    confirmed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretSaveInput {
    provider_secrets: Option<HashMap<String, String>>,
    search_api_key: Option<String>,
}

fn default_settings() -> Value {
    json!({
        "apiEndpoint": "https://api.openai.com/v1",
        "apiKey": "",
        "model": "gpt-4o-mini",
        "defaultProviderEndpoint": "https://api.openai.com/v1",
        "providerConfigs": {
            "https://api.openai.com/v1": { "name": "OpenAI", "endpoint": "https://api.openai.com/v1", "apiKey": "", "model": "gpt-4o-mini" },
            "https://api.deepseek.com": { "name": "DeepSeek", "endpoint": "https://api.deepseek.com", "apiKey": "", "model": "deepseek-chat" },
            "https://generativelanguage.googleapis.com/v1beta/openai/": { "name": "Google Gemini", "endpoint": "https://generativelanguage.googleapis.com/v1beta/openai/", "apiKey": "", "model": "gemini-2.5-flash" },
            "https://dashscope.aliyuncs.com/compatible-mode/v1": { "name": "Qwen", "endpoint": "https://dashscope.aliyuncs.com/compatible-mode/v1", "apiKey": "", "model": "qwen-plus" },
            "https://open.bigmodel.cn/api/paas/v4": { "name": "GLM", "endpoint": "https://open.bigmodel.cn/api/paas/v4", "apiKey": "", "model": "glm-4-flash" },
            "https://api.moonshot.cn/v1": { "name": "Moonshot", "endpoint": "https://api.moonshot.cn/v1", "apiKey": "", "model": "moonshot-v1-8k" }
        },
        "scanPath": "",
        "targetSizeGB": 1,
        "maxDepth": 5,
        "lastScanTime": null,
        "enableWebSearch": false,
        "enableWebSearchClassify": false,
        "enableWebSearchOrganizer": false,
        "tavilyApiKey": "",
        "searchApi": {
            "provider": "tavily",
            "enabled": false,
            "apiKey": "",
            "scopes": { "scan": false, "classify": false, "organizer": false }
        },
        "secretMeta": {
            "providers": {},
            "searchApi": false
        }
    })
}

fn merge_json(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(base_map), Value::Object(patch_map)) => {
            for (k, v) in patch_map {
                match base_map.get_mut(k) {
                    Some(existing) => merge_json(existing, v),
                    None => {
                        base_map.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (base_val, patch_val) => *base_val = patch_val.clone(),
    }
}

pub(crate) fn read_settings(path: &Path) -> Value {
    let mut merged = default_settings();
    if let Some(parsed) = fs::read_to_string(path)
        .ok()
        .and_then(|x| serde_json::from_str::<Value>(&x).ok())
    {
        merge_json(&mut merged, &parsed);
    }
    merged
}

fn strip_secret_fields(v: &mut Value) {
    if let Some(obj) = v.as_object_mut() {
        obj.remove("secretStatus");
        obj.insert("apiKey".to_string(), Value::String(String::new()));
        obj.insert("tavilyApiKey".to_string(), Value::String(String::new()));
        if let Some(search) = obj.get_mut("searchApi").and_then(Value::as_object_mut) {
            search.insert("apiKey".to_string(), Value::String(String::new()));
        }
        if let Some(provider_configs) = obj
            .get_mut("providerConfigs")
            .and_then(Value::as_object_mut)
        {
            for (_, item) in provider_configs.iter_mut() {
                if let Some(config) = item.as_object_mut() {
                    config.insert("apiKey".to_string(), Value::String(String::new()));
                }
            }
        }
    }
}

fn write_settings(path: &Path, value: &Value) -> Result<(), String> {
    let mut merged = if path.exists() {
        read_settings(path)
    } else {
        default_settings()
    };
    merge_json(&mut merged, value);
    strip_secret_fields(&mut merged);
    fs::write(
        path,
        serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn read_secret_meta(settings: &Value) -> (HashMap<String, bool>, bool) {
    let provider_meta = settings
        .get("secretMeta")
        .and_then(|v| v.get("providers"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut providers = HashMap::new();
    for (endpoint, raw) in provider_meta {
        providers.insert(endpoint, raw.as_bool().unwrap_or(false));
    }
    let search = settings
        .get("secretMeta")
        .and_then(|v| v.get("searchApi"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    (providers, search)
}

fn apply_secret_meta(
    settings: &mut Value,
    provider_meta: &HashMap<String, bool>,
    search_has_api_key: bool,
) {
    if let Some(obj) = settings.as_object_mut() {
        obj.insert(
            "secretMeta".to_string(),
            json!({
                "providers": provider_meta,
                "searchApi": search_has_api_key
            }),
        );
    }
}

fn has_plaintext_secrets(settings: &Value) -> bool {
    let top_level_api_key = settings
        .get("apiKey")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !top_level_api_key.is_empty() {
        return true;
    }
    let tavily_api_key = settings
        .get("tavilyApiKey")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !tavily_api_key.is_empty() {
        return true;
    }
    let search_api_key = settings
        .get("searchApi")
        .and_then(|v| v.get("apiKey"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !search_api_key.is_empty() {
        return true;
    }
    settings
        .get("providerConfigs")
        .and_then(Value::as_object)
        .map(|configs| {
            configs.values().any(|config| {
                !config
                    .get("apiKey")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .is_empty()
            })
        })
        .unwrap_or(false)
}

fn extract_plaintext_provider_secrets(settings: &Value) -> HashMap<String, String> {
    let mut providers = HashMap::new();
    if let Some(configs) = settings.get("providerConfigs").and_then(Value::as_object) {
        for (key, config) in configs {
            let endpoint = config
                .get("endpoint")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .trim()
                .to_string();
            let api_key = config
                .get("apiKey")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if !endpoint.is_empty() && !api_key.is_empty() {
                providers.insert(endpoint, api_key);
            }
        }
    }
    let endpoint = settings
        .get("apiEndpoint")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let api_key = settings
        .get("apiKey")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if !endpoint.is_empty() && !api_key.is_empty() {
        providers.insert(endpoint, api_key);
    }
    providers
}

fn extract_plaintext_search_secret(settings: &Value) -> String {
    let search_api_key = settings
        .get("searchApi")
        .and_then(|v| v.get("apiKey"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !search_api_key.is_empty() {
        return search_api_key.to_string();
    }
    settings
        .get("tavilyApiKey")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn build_secret_status_value(state: &AppState, settings: &Value) -> Value {
    let (provider_meta, search_has_api_key) = read_secret_meta(settings);
    let vault = state.secret_vault.lock().unwrap();
    json!({
        "initialized": vault.is_initialized(),
        "unlocked": vault.is_unlocked(),
        "needsMigration": has_plaintext_secrets(settings),
        "providerHasApiKey": provider_meta,
        "searchApiHasKey": search_has_api_key
    })
}

fn redact_settings_for_client(state: &AppState, raw_settings: &Value) -> Value {
    let mut sanitized = raw_settings.clone();
    strip_secret_fields(&mut sanitized);
    if let Some(obj) = sanitized.as_object_mut() {
        obj.insert(
            "secretStatus".to_string(),
            build_secret_status_value(state, raw_settings),
        );
    }
    sanitized
}

fn update_secret_meta_in_settings(
    state: &AppState,
    provider_meta: &HashMap<String, bool>,
    search_has_api_key: bool,
) -> Result<Value, String> {
    let mut settings = read_settings(&state.settings_path);
    apply_secret_meta(&mut settings, provider_meta, search_has_api_key);
    write_settings(&state.settings_path, &settings)?;
    Ok(settings)
}

fn migrate_plaintext_secrets_if_needed(state: &AppState) -> Result<bool, String> {
    let settings = read_settings(&state.settings_path);
    if !has_plaintext_secrets(&settings) {
        return Ok(false);
    }
    let provider_secrets = extract_plaintext_provider_secrets(&settings);
    let search_api_key = extract_plaintext_search_secret(&settings);
    {
        let vault = state.secret_vault.lock().unwrap();
        for (endpoint, api_key) in &provider_secrets {
            vault.write(&provider_secret_key(endpoint), api_key)?;
        }
        vault.write(SEARCH_SECRET_KEY, &search_api_key)?;
    }
    let mut provider_meta = HashMap::new();
    if let Some(configs) = settings.get("providerConfigs").and_then(Value::as_object) {
        for (key, config) in configs {
            let endpoint = config
                .get("endpoint")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .trim()
                .to_string();
            provider_meta.insert(
                endpoint.clone(),
                provider_secrets
                    .get(&endpoint)
                    .map(|value| !value.trim().is_empty())
                    .unwrap_or(false),
            );
        }
    }
    let mut next_settings = settings.clone();
    apply_secret_meta(&mut next_settings, &provider_meta, !search_api_key.trim().is_empty());
    write_settings(&state.settings_path, &next_settings)?;
    Ok(true)
}

trait StringExt {
    fn if_empty_then<F: FnOnce() -> String>(self, fallback: F) -> String;
}

impl StringExt for String {
    fn if_empty_then<F: FnOnce() -> String>(self, fallback: F) -> String {
        if self.trim().is_empty() {
            fallback()
        } else {
            self
        }
    }
}

pub(crate) fn resolve_provider_endpoint_and_model(
    state: &AppState,
    endpoint_hint: Option<&str>,
    model_hint: Option<&str>,
) -> (String, String) {
    let settings = read_settings(&state.settings_path);
    let endpoint = endpoint_hint
        .unwrap_or("")
        .trim()
        .to_string()
        .if_empty_then(|| {
            settings
                .get("apiEndpoint")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .if_empty_then(|| {
            settings
                .get("defaultProviderEndpoint")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .if_empty_then(|| "https://api.openai.com/v1".to_string());
    let model = model_hint
        .unwrap_or("")
        .trim()
        .to_string()
        .if_empty_then(|| {
            settings
                .get("providerConfigs")
                .and_then(|v| v.get(&endpoint))
                .and_then(|v| v.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .if_empty_then(|| {
            settings
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .if_empty_then(|| "gpt-4o-mini".to_string());
    (endpoint, model)
}

pub(crate) fn resolve_provider_api_key(state: &AppState, endpoint: &str) -> Result<String, String> {
    state
        .secret_vault
        .lock()
        .unwrap()
        .read(&provider_secret_key(endpoint))
}

pub(crate) fn resolve_search_api_key(state: &AppState) -> Result<String, String> {
    state.secret_vault.lock().unwrap().read(SEARCH_SECRET_KEY)
}

#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>) -> Result<Value, String> {
    let raw = read_settings(&state.settings_path);
    Ok(redact_settings_for_client(state.inner(), &raw))
}

#[tauri::command]
pub async fn settings_save(state: State<'_, AppState>, data: Value) -> Result<Value, String> {
    write_settings(&state.settings_path, &data)?;
    let raw = read_settings(&state.settings_path);
    Ok(json!({
        "success": true,
        "settings": redact_settings_for_client(state.inner(), &raw)
    }))
}

#[tauri::command]
pub async fn secret_status(state: State<'_, AppState>) -> Result<Value, String> {
    let raw = read_settings(&state.settings_path);
    Ok(build_secret_status_value(state.inner(), &raw))
}

#[tauri::command]
pub async fn secret_setup(
    state: State<'_, AppState>,
    data: SecretPasswordInput,
) -> Result<Value, String> {
    if data.password.trim().is_empty() {
        return Err("Password is required.".to_string());
    }
    {
        let mut vault = state.secret_vault.lock().unwrap();
        vault.setup(&data.password)?;
    }
    let migrated = migrate_plaintext_secrets_if_needed(state.inner())?;
    let raw = read_settings(&state.settings_path);
    Ok(json!({
        "success": true,
        "migrated": migrated,
        "secretStatus": build_secret_status_value(state.inner(), &raw)
    }))
}

#[tauri::command]
pub async fn secret_unlock(
    state: State<'_, AppState>,
    data: SecretPasswordInput,
) -> Result<Value, String> {
    if data.password.trim().is_empty() {
        return Err("Password is required.".to_string());
    }
    {
        let mut vault = state.secret_vault.lock().unwrap();
        vault.unlock(&data.password)?;
    }
    let migrated = migrate_plaintext_secrets_if_needed(state.inner())?;
    let raw = read_settings(&state.settings_path);
    Ok(json!({
        "success": true,
        "migrated": migrated,
        "secretStatus": build_secret_status_value(state.inner(), &raw)
    }))
}

#[tauri::command]
pub async fn secret_lock(state: State<'_, AppState>) -> Result<Value, String> {
    state.secret_vault.lock().unwrap().lock();
    let raw = read_settings(&state.settings_path);
    Ok(json!({
        "success": true,
        "secretStatus": build_secret_status_value(state.inner(), &raw)
    }))
}

#[tauri::command]
pub async fn secret_reset(
    state: State<'_, AppState>,
    data: SecretResetInput,
) -> Result<Value, String> {
    if !data.confirmed {
        return Err("Reset confirmation is required.".to_string());
    }
    state.secret_vault.lock().unwrap().reset()?;
    let settings = update_secret_meta_in_settings(state.inner(), &HashMap::new(), false)?;
    Ok(json!({
        "success": true,
        "secretStatus": build_secret_status_value(state.inner(), &settings)
    }))
}

#[tauri::command]
pub async fn secret_get_editable(state: State<'_, AppState>) -> Result<Value, String> {
    let settings = read_settings(&state.settings_path);
    let mut providers = HashMap::new();
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
            let value = resolve_provider_api_key(state.inner(), &endpoint)?;
            providers.insert(endpoint, value);
        }
    }
    let search_api_key = resolve_search_api_key(state.inner()).unwrap_or_default();
    Ok(json!({
        "providerSecrets": providers,
        "searchApiKey": search_api_key
    }))
}

#[tauri::command]
pub async fn secret_save(
    state: State<'_, AppState>,
    data: SecretSaveInput,
) -> Result<Value, String> {
    let settings = read_settings(&state.settings_path);
    let mut provider_meta = HashMap::new();
    let provider_secrets = data.provider_secrets.unwrap_or_default();
    if let Some(configs) = settings.get("providerConfigs").and_then(Value::as_object) {
        for (key, config) in configs {
            let endpoint = config
                .get("endpoint")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .trim()
                .to_string();
            let value = provider_secrets
                .get(&endpoint)
                .map(|raw| raw.trim().to_string())
                .unwrap_or_default();
            state
                .secret_vault
                .lock()
                .unwrap()
                .write(&provider_secret_key(&endpoint), &value)?;
            provider_meta.insert(endpoint, !value.is_empty());
        }
    }
    let search_api_key = data.search_api_key.unwrap_or_default().trim().to_string();
    state
        .secret_vault
        .lock()
        .unwrap()
        .write(SEARCH_SECRET_KEY, &search_api_key)?;
    let next_settings =
        update_secret_meta_in_settings(state.inner(), &provider_meta, !search_api_key.is_empty())?;
    Ok(json!({
        "success": true,
        "secretStatus": build_secret_status_value(state.inner(), &next_settings)
    }))
}

#[derive(Debug, Deserialize)]
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
    let mut req = client.get(url).header("Accept", "application/json");
    if !api_key.is_empty() {
        req = req
            .header("Authorization", format!("Bearer {}", api_key))
            .header("x-api-key", api_key.clone())
            .header("api-key", api_key.clone());
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!(
            "Failed to fetch models ({})",
            resp.status().as_u16()
        ));
    }
    let payload: Value = resp.json().await.map_err(|e| e.to_string())?;
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

#[tauri::command]
pub async fn settings_browse_folder() -> Result<Value, String> {
    let picked = tauri::async_runtime::spawn_blocking(|| rfd::FileDialog::new().pick_folder())
        .await
        .map_err(|e| e.to_string())?;
    if let Some(path) = picked {
        Ok(
            json!({ "success": true, "cancelled": false, "path": path.to_string_lossy().to_string() }),
        )
    } else {
        Ok(json!({ "success": true, "cancelled": true }))
    }
}

#[tauri::command]
pub async fn system_get_privilege() -> Result<Value, String> {
    Ok(json!({
        "platform": if cfg!(windows) { "win32" } else { std::env::consts::OS },
        "isAdmin": if cfg!(windows) { check_elevation::is_elevated().unwrap_or(false) } else { false }
    }))
}

#[tauri::command]
pub async fn system_request_elevation() -> Result<Value, String> {
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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenLocationInput {
    path: String,
}

#[tauri::command]
pub async fn files_open_location(data: OpenLocationInput) -> Result<Value, String> {
    let path = PathBuf::from(data.path);
    if !path.exists() {
        return Err("File or directory does not exist".to_string());
    }
    if cfg!(windows) {
        let _ = Command::new("explorer.exe")
            .arg(format!("/select,{}", path.to_string_lossy()))
            .spawn();
    }
    Ok(json!({ "success": true }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanFilesInput {
    paths: Vec<String>,
    scan_task_id: Option<String>,
}

fn clean_one_target(path: &Path) -> Result<(bool, bool), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
            let child = entry.map_err(|e| e.to_string())?.path();
            let _ = clean_one_target(&child)?;
        }
        Ok((true, false))
    } else {
        fs::remove_file(path).map_err(|e| e.to_string())?;
        Ok((true, true))
    }
}

#[tauri::command]
pub async fn files_clean(
    state: State<'_, AppState>,
    data: CleanFilesInput,
) -> Result<Value, String> {
    if data.paths.is_empty() {
        return Err("Paths array is required".to_string());
    }
    let mut cleaned = Vec::new();
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for raw in &data.paths {
        let path = PathBuf::from(raw);
        if !path.exists() {
            failed.push(json!({"path": raw, "error": "Not found", "code": "ENOENT", "skipped": false, "permissionDenied": false, "requiresElevation": false}));
            continue;
        }
        match clean_one_target(&path) {
            Ok((ok, removed_self)) => {
                if ok {
                    cleaned.push(path.to_string_lossy().to_string());
                }
                if removed_self {
                    removed.push(path.to_string_lossy().to_string());
                }
            }
            Err(err) => failed.push(json!({
                "path": path.to_string_lossy().to_string(),
                "error": err,
                "code": "UNKNOWN",
                "skipped": false,
                "permissionDenied": true,
                "requiresElevation": cfg!(windows) && !check_elevation::is_elevated().unwrap_or(false)
            })),
        }
    }
    let mut scan_snapshot = Value::Null;
    if let Some(task_id) = data.scan_task_id.as_deref() {
        let _ = crate::persist::delete_scan_findings_by_paths(&state.db_path, task_id, &cleaned);
        if let Ok(Some(snapshot)) = crate::persist::load_scan_snapshot(&state.db_path, task_id) {
            scan_snapshot = serde_json::to_value(snapshot).unwrap_or(Value::Null);
        }
    }
    Ok(
        json!({ "success": true, "results": { "cleaned": cleaned, "removed": removed, "failed": failed }, "scanSnapshot": scan_snapshot }),
    )
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanStartInput {
    pub target_path: String,
    pub target_size_gb: Option<f64>,
    pub max_depth: Option<u32>,
    pub auto_analyze: Option<bool>,
    pub api_endpoint: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeStartInput {
    pub root_path: String,
    pub recursive: Option<bool>,
    pub mode: Option<String>,
    pub categories: Option<Vec<String>>,
    pub allow_new_categories: Option<bool>,
    pub excluded_patterns: Option<Vec<String>>,
    pub parallelism: Option<u32>,
    pub use_web_search: Option<bool>,
    pub model_routing: Option<Value>,
    pub search_api_key: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeSuggestInput {
    pub root_path: String,
    pub recursive: Option<bool>,
    pub excluded_patterns: Option<Vec<String>>,
    pub manual_categories: Option<Vec<String>>,
    pub model_routing: Option<Value>,
    pub use_web_search: Option<bool>,
    pub search_api_key: Option<String>,
}

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
pub async fn scan_start<R: Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    mut input: ScanStartInput,
) -> Result<Value, String> {
    let (endpoint, model) = resolve_provider_endpoint_and_model(
        state.inner(),
        input.api_endpoint.as_deref(),
        input.model.as_deref(),
    );
    input.api_endpoint = Some(endpoint.clone());
    input.model = Some(model);
    if input.auto_analyze.unwrap_or(true) {
        input.api_key = Some(resolve_provider_api_key(state.inner(), &endpoint)?);
    } else if input.api_key.is_none() {
        input.api_key = Some(String::new());
    }
    crate::scan_runtime::scan_start(app, state, input).await
}

#[tauri::command]
pub async fn scan_get_active(state: State<'_, AppState>) -> Result<Vec<Value>, String> {
    crate::scan_runtime::scan_get_active(state).await
}

#[tauri::command]
pub async fn scan_list_history(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<Value>, String> {
    crate::scan_runtime::scan_list_history(state, limit).await
}

#[tauri::command]
pub async fn scan_delete_history(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Value, String> {
    crate::scan_runtime::scan_delete_history(state, task_id).await
}

#[tauri::command]
pub async fn scan_stop(state: State<'_, AppState>, task_id: String) -> Result<Value, String> {
    crate::scan_runtime::scan_stop(state, task_id).await
}

#[tauri::command]
pub async fn scan_get_result(state: State<'_, AppState>, task_id: String) -> Result<Value, String> {
    crate::scan_runtime::scan_get_result(state, task_id).await
}

#[tauri::command]
pub async fn organize_get_capability(state: State<'_, AppState>) -> Result<Value, String> {
    crate::organizer_runtime::organize_get_capability(state).await
}

#[tauri::command]
pub async fn organize_suggest_categories(
    state: State<'_, AppState>,
    mut input: OrganizeSuggestInput,
) -> Result<Value, String> {
    input.model_routing = hydrate_model_routing_with_secrets(state.inner(), &input.model_routing);
    if input.use_web_search.unwrap_or(false) && input.search_api_key.is_none() {
        input.search_api_key = Some(resolve_search_api_key(state.inner()).unwrap_or_default());
    }
    crate::organizer_runtime::organize_suggest_categories(input).await
}

#[tauri::command]
pub async fn organize_start<R: Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    mut input: OrganizeStartInput,
) -> Result<Value, String> {
    input.model_routing = hydrate_model_routing_with_secrets(state.inner(), &input.model_routing);
    if input.use_web_search.unwrap_or(false) && input.search_api_key.is_none() {
        input.search_api_key = Some(resolve_search_api_key(state.inner()).unwrap_or_default());
    }
    crate::organizer_runtime::organize_start(app, state, input).await
}

#[tauri::command]
pub async fn organize_stop(state: State<'_, AppState>, task_id: String) -> Result<Value, String> {
    crate::organizer_runtime::organize_stop(state, task_id).await
}

#[tauri::command]
pub async fn organize_get_result(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Value, String> {
    crate::organizer_runtime::organize_get_result(state, task_id).await
}

#[tauri::command]
pub async fn organize_apply(state: State<'_, AppState>, task_id: String) -> Result<Value, String> {
    crate::organizer_runtime::organize_apply(state, task_id).await
}

#[tauri::command]
pub async fn organize_rollback(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<Value, String> {
    crate::organizer_runtime::organize_rollback(state, job_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_settings_path() -> PathBuf {
        std::env::temp_dir().join(format!("aicleaner-settings-{}.json", Uuid::new_v4()))
    }

    #[test]
    fn partial_save_preserves_secret_meta() {
        let path = temp_settings_path();
        let seeded = json!({
            "secretMeta": {
                "providers": {
                    "https://api.deepseek.com": true
                },
                "searchApi": true
            }
        });
        write_settings(&path, &seeded).expect("seed settings");

        let patch = json!({
            "scanPath": "C:\\Users\\tester\\Downloads",
            "targetSizeGB": 2
        });
        write_settings(&path, &patch).expect("partial save");

        let saved = read_settings(&path);
        assert_eq!(
            saved["secretMeta"]["providers"]["https://api.deepseek.com"],
            Value::Bool(true)
        );
        assert_eq!(saved["secretMeta"]["searchApi"], Value::Bool(true));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn partial_save_preserves_custom_provider_configs() {
        let path = temp_settings_path();
        let custom_endpoint = "https://example.com/openai";
        let seeded = json!({
            "defaultProviderEndpoint": custom_endpoint,
            "providerConfigs": {
                custom_endpoint: {
                    "name": "Custom",
                    "endpoint": custom_endpoint,
                    "model": "custom-model"
                }
            }
        });
        write_settings(&path, &seeded).expect("seed provider config");

        let patch = json!({
            "scanPath": "C:\\temp",
            "maxDepth": 4
        });
        write_settings(&path, &patch).expect("partial save");

        let saved = read_settings(&path);
        assert_eq!(
            saved["defaultProviderEndpoint"],
            Value::String(custom_endpoint.to_string())
        );
        assert_eq!(
            saved["providerConfigs"][custom_endpoint]["model"],
            Value::String("custom-model".to_string())
        );

        let _ = fs::remove_file(path);
    }
}
