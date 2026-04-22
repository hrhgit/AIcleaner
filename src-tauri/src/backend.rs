mod credential_store;
mod provider_registry;
mod settings_store;

use parking_lot::Mutex;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tauri::{Runtime, State};

use credential_store::{CredentialStore, WindowsCredentialStore};
#[cfg(test)]
use credential_store::InMemoryCredentialStore;
pub(crate) use provider_registry::{default_model_for_endpoint, provider_secret_key};
pub(crate) use settings_store::{
    default_settings, legacy_provider_api_key_from_settings, legacy_search_api_key_from_settings,
    read_scan_ignore_paths, read_settings, strip_secret_fields, write_settings,
};
#[cfg(test)]
pub(crate) use settings_store::read_scan_persistent_rules_with_cleanup;

const CREDENTIAL_SERVICE: &str = "aicleaner";
const SEARCH_SECRET_KEY: &str = "search:tavily:apiKey";
const STORAGE_LOCATION_FILE: &str = "storage-location.json";

#[derive(Clone, Debug)]
struct AppPaths {
    data_dir: PathBuf,
    settings_path: PathBuf,
    db_path: PathBuf,
}

impl AppPaths {
    fn from_data_dir(data_dir: PathBuf) -> Self {
        Self {
            settings_path: data_dir.join("settings.json"),
            db_path: data_dir.join("scan-cache.sqlite"),
            data_dir,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StorageLocationConfig {
    data_dir: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub base_data_dir: PathBuf,
    pub bootstrap_path: PathBuf,
    paths: Arc<Mutex<AppPaths>>,
    pub(crate) scan_tasks: Arc<Mutex<HashMap<String, Arc<crate::scan_runtime::ScanTaskRuntime>>>>,
    pub(crate) organize_tasks:
        Arc<Mutex<HashMap<String, Arc<crate::organizer_runtime::OrganizeTaskRuntime>>>>,
    pub(crate) tika_process: Arc<Mutex<Option<ManagedTikaProcess>>>,
    credential_store: Arc<dyn CredentialStore>,
}

pub(crate) struct ManagedTikaProcess {
    pub url: String,
    pub child: Child,
}

impl AppState {
    pub fn bootstrap(base_data_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&base_data_dir).map_err(|e| e.to_string())?;
        let bootstrap_path = base_data_dir.join(STORAGE_LOCATION_FILE);
        let resolved_data_dir = resolve_storage_data_dir(&base_data_dir, &bootstrap_path);
        fs::create_dir_all(&resolved_data_dir).map_err(|e| e.to_string())?;
        let paths = AppPaths::from_data_dir(resolved_data_dir);
        if !paths.settings_path.exists() {
            fs::write(
                &paths.settings_path,
                serde_json::to_vec_pretty(&default_settings()).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(Self {
            credential_store: Arc::new(WindowsCredentialStore::new(CREDENTIAL_SERVICE)),
            base_data_dir,
            bootstrap_path,
            paths: Arc::new(Mutex::new(paths)),
            scan_tasks: Arc::new(Mutex::new(HashMap::new())),
            organize_tasks: Arc::new(Mutex::new(HashMap::new())),
            tika_process: Arc::new(Mutex::new(None)),
        })
    }

    #[cfg(test)]
    fn with_store(settings_path: PathBuf, credential_store: Arc<dyn CredentialStore>) -> Self {
        let data_dir = settings_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Self {
            base_data_dir: data_dir.clone(),
            bootstrap_path: data_dir.join(STORAGE_LOCATION_FILE),
            paths: Arc::new(Mutex::new(AppPaths {
                data_dir,
                settings_path,
                db_path: std::env::temp_dir().join("aicleaner-test.sqlite"),
            })),
            scan_tasks: Arc::new(Mutex::new(HashMap::new())),
            organize_tasks: Arc::new(Mutex::new(HashMap::new())),
            tika_process: Arc::new(Mutex::new(None)),
            credential_store,
        }
    }

    pub fn data_dir(&self) -> PathBuf {
        self.paths.lock().data_dir.clone()
    }

    pub fn settings_path(&self) -> PathBuf {
        self.paths.lock().settings_path.clone()
    }

    pub fn db_path(&self) -> PathBuf {
        self.paths.lock().db_path.clone()
    }

    pub fn uses_custom_data_dir(&self) -> bool {
        !same_path(&self.data_dir(), &self.base_data_dir)
    }

    fn set_data_dir(&self, data_dir: PathBuf) {
        *self.paths.lock() = AppPaths::from_data_dir(data_dir);
    }
}

fn resolve_storage_data_dir(base_data_dir: &Path, bootstrap_path: &Path) -> PathBuf {
    fs::read_to_string(bootstrap_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<StorageLocationConfig>(&raw).ok())
        .and_then(|config| config.data_dir)
        .map(|value| PathBuf::from(value.trim()))
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| base_data_dir.to_path_buf())
}

fn write_storage_location_config(
    base_data_dir: &Path,
    bootstrap_path: &Path,
    active_data_dir: &Path,
) -> Result<(), String> {
    fs::create_dir_all(base_data_dir).map_err(|e| e.to_string())?;
    if same_path(active_data_dir, base_data_dir) {
        if bootstrap_path.exists() {
            fs::remove_file(bootstrap_path).map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    let payload = StorageLocationConfig {
        data_dir: Some(active_data_dir.to_string_lossy().to_string()),
    };
    fs::write(
        bootstrap_path,
        serde_json::to_vec_pretty(&payload).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn same_path(left: &Path, right: &Path) -> bool {
    crate::persist::normalize_root_path(&left.to_string_lossy())
        == crate::persist::normalize_root_path(&right.to_string_lossy())
}

fn is_same_or_descendant_path(candidate: &Path, parent: &Path) -> bool {
    let candidate = crate::persist::normalize_root_path(&candidate.to_string_lossy());
    let parent = crate::persist::normalize_root_path(&parent.to_string_lossy());
    if candidate == parent {
        return true;
    }
    let Some(stripped) = candidate.strip_prefix(&parent) else {
        return false;
    };
    stripped.starts_with('\\')
}

fn validate_data_dir_target(current_data_dir: &Path, target_data_dir: &Path) -> Result<(), String> {
    if target_data_dir.as_os_str().is_empty() {
        return Err("Missing target data directory.".to_string());
    }
    if same_path(current_data_dir, target_data_dir) {
        return Err("The selected directory is already the current cache location.".to_string());
    }
    if is_same_or_descendant_path(target_data_dir, current_data_dir)
        || is_same_or_descendant_path(current_data_dir, target_data_dir)
    {
        return Err(
            "The new cache directory cannot be the current directory or its parent/child."
                .to_string(),
        );
    }
    Ok(())
}

fn copy_dir_contents_recursive(
    source_dir: &Path,
    target_dir: &Path,
    skip_paths: &HashSet<String>,
) -> Result<(), String> {
    fs::create_dir_all(target_dir).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let source_path = entry.path();
        let source_key = crate::persist::normalize_root_path(&source_path.to_string_lossy());
        if skip_paths.contains(&source_key) {
            continue;
        }
        let target_path = target_dir.join(entry.file_name());
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            copy_dir_contents_recursive(&source_path, &target_path, skip_paths)?;
        } else if file_type.is_file() {
            if target_path.exists() {
                return Err(format!(
                    "Target already contains {}.",
                    target_path.to_string_lossy()
                ));
            }
            fs::copy(&source_path, &target_path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn remove_dir_contents_recursive(
    source_dir: &Path,
    skip_paths: &HashSet<String>,
) -> Result<(), String> {
    if !source_dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(source_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let source_path = entry.path();
        let source_key = crate::persist::normalize_root_path(&source_path.to_string_lossy());
        if skip_paths.contains(&source_key) {
            continue;
        }
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            remove_dir_contents_recursive(&source_path, skip_paths)?;
            let _ = fs::remove_dir(&source_path);
        } else if file_type.is_file() {
            fs::remove_file(&source_path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
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
    pub scan_mode: String,
    pub baseline_task_id: Option<String>,
    pub visible_latest: bool,
    pub root_path_key: String,
    pub target_path: String,
    pub auto_analyze: bool,
    pub root_node_id: String,
    pub configured_max_depth: Option<u32>,
    pub max_scanned_depth: u32,
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
    pub excluded_patterns: Vec<String>,
    pub batch_size: u32,
    #[serde(default = "default_organize_summary_mode")]
    pub summary_mode: String,
    pub max_cluster_depth: Option<u32>,
    pub use_web_search: bool,
    pub web_search_enabled: bool,
    pub selected_model: String,
    pub selected_models: Value,
    pub selected_providers: Value,
    pub supports_multimodal: bool,
    pub tree: Value,
    pub tree_version: u64,
    pub total_files: u64,
    pub processed_files: u64,
    pub total_batches: u64,
    pub processed_batches: u64,
    pub token_usage: TokenUsage,
    #[serde(default)]
    pub results: Vec<Value>,
    #[serde(default)]
    pub preview: Vec<Value>,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub job_id: Option<String>,
}

pub fn default_organize_summary_mode() -> String {
    "filename_only".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsSaveInput {
    provider_secrets: Option<HashMap<String, String>>,
    search_api_key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ScanRuleTopChild {
    pub name: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ScanPersistentRuleRecord {
    pub path: String,
    pub node_type: String,
    pub classification: String,
    pub reason: String,
    pub risk: String,
    pub source: String,
    pub size: u64,
    pub self_size: u64,
    pub child_count: u64,
    #[serde(default)]
    pub name_tags: Vec<String>,
    #[serde(default)]
    pub top_children: Vec<ScanRuleTopChild>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ScanPersistentRules {
    #[serde(default)]
    pub keep_exact: Vec<ScanPersistentRuleRecord>,
    #[serde(default)]
    pub safe_delete_exact: Vec<ScanPersistentRuleRecord>,
}

fn apply_credentials_meta(
    settings: &mut Value,
    provider_meta: &HashMap<String, bool>,
    search_has_api_key: bool,
) {
    if let Some(obj) = settings.as_object_mut() {
        obj.remove("secretMeta");
        obj.insert(
            "credentialsMeta".to_string(),
            json!({
                "providers": provider_meta,
                "searchApi": search_has_api_key
            }),
        );
    }
}

fn collect_provider_endpoints(settings: &Value) -> Vec<String> {
    let mut endpoints = Vec::new();
    if let Some(configs) = settings.get("providerConfigs").and_then(Value::as_object) {
        for (key, config) in configs {
            let endpoint = config
                .get("endpoint")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .trim()
                .to_string();
            if !endpoint.is_empty() && !endpoints.contains(&endpoint) {
                endpoints.push(endpoint);
            }
        }
    }
    let endpoint = settings
        .get("defaultProviderEndpoint")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if !endpoint.is_empty() && !endpoints.contains(&endpoint) {
        endpoints.push(endpoint);
    }
    endpoints
}

fn build_credentials_status_value(state: &AppState, settings: &Value) -> Value {
    let endpoints = collect_provider_endpoints(settings);
    let mut accounts = endpoints
        .iter()
        .map(|endpoint| provider_secret_key(endpoint))
        .collect::<Vec<_>>();
    accounts.push(SEARCH_SECRET_KEY.to_string());
    let values = state
        .credential_store
        .get_many(&accounts)
        .unwrap_or_default();
    let mut provider_meta = HashMap::new();
    for endpoint in endpoints {
        let account = provider_secret_key(&endpoint);
        provider_meta.insert(
            endpoint.clone(),
            values
                .get(&account)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
                || legacy_provider_api_key_from_settings(settings, &endpoint).is_some(),
        );
    }
    json!({
        "providerHasApiKey": provider_meta,
        "searchApiHasKey": values
            .get(SEARCH_SECRET_KEY)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
            || legacy_search_api_key_from_settings(settings).is_some()
    })
}

fn redact_settings_for_client(state: &AppState, raw_settings: &Value) -> Value {
    let mut sanitized = raw_settings.clone();
    strip_secret_fields(&mut sanitized);
    if let Some(obj) = sanitized.as_object_mut() {
        obj.insert(
            "storage".to_string(),
            json!({
                "dataDir": state.data_dir().to_string_lossy().to_string(),
                "defaultDataDir": state.base_data_dir.to_string_lossy().to_string(),
                "customized": state.uses_custom_data_dir(),
            }),
        );
        obj.insert(
            "credentialsStatus".to_string(),
            build_credentials_status_value(state, raw_settings),
        );
    }
    sanitized
}

fn update_credentials_meta_in_settings(
    state: &AppState,
    provider_meta: &HashMap<String, bool>,
    search_has_api_key: bool,
) -> Result<Value, String> {
    let settings_path = state.settings_path();
    let mut settings = read_settings(&settings_path);
    apply_credentials_meta(&mut settings, provider_meta, search_has_api_key);
    write_settings(&settings_path, &settings)?;
    Ok(settings)
}

fn extract_credentials_meta(status: &Value) -> (HashMap<String, bool>, bool) {
    let provider_meta = status
        .get("providerHasApiKey")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(endpoint, value)| (endpoint.clone(), value.as_bool().unwrap_or(false)))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let search_has_api_key = status
        .get("searchApiHasKey")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    (provider_meta, search_has_api_key)
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
    let settings = read_settings(&state.settings_path());
    let endpoint = endpoint_hint
        .unwrap_or("")
        .trim()
        .to_string()
        .if_empty_then(|| {
            settings
                .get("defaultProviderEndpoint")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .if_empty_then(|| {
            settings
                .get("providerConfigs")
                .and_then(Value::as_object)
                .and_then(|configs| configs.iter().next())
                .map(|(key, config)| {
                    config
                        .get("endpoint")
                        .and_then(Value::as_str)
                        .unwrap_or(key)
                        .trim()
                        .to_string()
                })
                .unwrap_or_default()
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
                .get("providerConfigs")
                .and_then(|v| {
                    v.get(
                        settings
                            .get("defaultProviderEndpoint")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                    )
                })
                .and_then(|v| v.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .if_empty_then(|| default_model_for_endpoint(&endpoint).to_string());
    (endpoint, model)
}

pub(crate) fn resolve_provider_api_key(state: &AppState, endpoint: &str) -> Result<String, String> {
    if let Some(api_key) = state
        .credential_store
        .get(&provider_secret_key(endpoint))?
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(api_key);
    }
    legacy_provider_api_key_from_settings(&read_settings(&state.settings_path()), endpoint)
        .ok_or_else(|| "Credential not found.".to_string())
}

pub(crate) fn resolve_search_api_key(state: &AppState) -> Result<String, String> {
    if let Some(api_key) = state
        .credential_store
        .get(SEARCH_SECRET_KEY)?
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(api_key);
    }
    legacy_search_api_key_from_settings(&read_settings(&state.settings_path()))
        .ok_or_else(|| "Credential not found.".to_string())
}

#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>) -> Result<Value, String> {
    let settings_path = state.settings_path();
    let raw = read_settings(&settings_path);
    Ok(redact_settings_for_client(state.inner(), &raw))
}

#[tauri::command]
pub async fn settings_save(state: State<'_, AppState>, data: Value) -> Result<Value, String> {
    let settings_path = state.settings_path();
    write_settings(&settings_path, &data)?;
    let raw = read_settings(&settings_path);
    Ok(json!({
        "success": true,
        "settings": redact_settings_for_client(state.inner(), &raw)
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDataDirInput {
    path: String,
}

fn migrate_data_dir(state: &AppState, target_path: &str) -> Result<Value, String> {
    if !state.scan_tasks.lock().is_empty() || !state.organize_tasks.lock().is_empty() {
        return Err(
            "Please stop running scan or organize tasks before moving the cache directory."
                .to_string(),
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
    migrate_data_dir(state.inner(), &data.path)
}

#[tauri::command]
pub async fn credentials_get(state: State<'_, AppState>) -> Result<Value, String> {
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
}

#[tauri::command]
pub async fn credentials_save(
    state: State<'_, AppState>,
    data: CredentialsSaveInput,
) -> Result<Value, String> {
    save_credentials_internal(state.inner(), data)
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
pub struct OpenExternalUrlInput {
    url: String,
}

#[tauri::command]
pub async fn system_open_external_url(data: OpenExternalUrlInput) -> Result<Value, String> {
    let parsed = Url::parse(data.url.trim()).map_err(|e| e.to_string())?;
    match parsed.scheme() {
        "http" | "https" => {}
        _ => return Err("Only http(s) URLs are allowed.".to_string()),
    }
    tauri_plugin_opener::open_url(parsed.as_str(), None::<&str>).map_err(|e| e.to_string())?;
    Ok(json!({ "success": true, "url": parsed.as_str() }))
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanStartInput {
    pub target_path: String,
    pub max_depth: Option<u32>,
    pub baseline_task_id: Option<String>,
    pub scan_mode: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeStartInput {
    pub root_path: String,
    pub excluded_patterns: Option<Vec<String>>,
    pub batch_size: Option<u32>,
    pub summary_mode: Option<String>,
    pub max_cluster_depth: Option<u32>,
    pub use_web_search: Option<bool>,
    pub model_routing: Option<Value>,
    pub search_api_key: Option<String>,
    pub response_language: Option<String>,
}

pub use crate::advisor_runtime::{
    AdvisorCardActionInput, AdvisorMessageSendInput, AdvisorSessionStartInput,
};

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
    input: ScanStartInput,
) -> Result<Value, String> {
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
pub async fn organize_stop<R: Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Value, String> {
    crate::organizer_runtime::organize_stop(app, state, task_id).await
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

#[tauri::command]
pub async fn advisor_session_start(
    state: State<'_, AppState>,
    input: AdvisorSessionStartInput,
) -> Result<Value, String> {
    crate::advisor_runtime::advisor_session_start(state, input).await
}

#[tauri::command]
pub async fn advisor_session_get(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Value, String> {
    crate::advisor_runtime::advisor_session_get(state, session_id).await
}

#[tauri::command]
pub async fn advisor_message_send(
    state: State<'_, AppState>,
    input: AdvisorMessageSendInput,
) -> Result<Value, String> {
    crate::advisor_runtime::advisor_message_send(state, input).await
}

#[tauri::command]
pub async fn advisor_card_action(
    state: State<'_, AppState>,
    input: AdvisorCardActionInput,
) -> Result<Value, String> {
    crate::advisor_runtime::advisor_card_action(state, input).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_settings_path() -> PathBuf {
        std::env::temp_dir().join(format!("aicleaner-settings-{}.json", Uuid::new_v4()))
    }

    #[test]
    fn partial_save_preserves_credentials_meta() {
        let path = temp_settings_path();
        let seeded = json!({
            "credentialsMeta": {
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
            saved["credentialsMeta"]["providers"]["https://api.deepseek.com"],
            Value::Bool(true)
        );
        assert_eq!(saved["credentialsMeta"]["searchApi"], Value::Bool(true));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn read_settings_normalizes_scan_persistent_rules_shape() {
        let path = temp_settings_path();
        let seeded = json!({
            "scanPersistentRules": {
                "keepExact": [
                    {
                        "path": "C:\\Users\\tester\\Documents",
                        "nodeType": "directory",
                        "reason": "keep this",
                        "risk": "high",
                        "source": "ai_promoted",
                        "size": 10,
                        "selfSize": 0,
                        "childCount": 2,
                        "nameTags": ["user_content", "user_content"],
                        "topChildren": [
                            { "name": "Docs", "type": "directory", "size": 10 }
                        ]
                    }
                ],
                "safeDeleteExact": [
                    {
                        "path": "C:\\Users\\tester\\AppData\\Local\\Temp",
                        "nodeType": "directory",
                        "classification": "safe_to_delete",
                        "reason": "temp",
                        "risk": "low",
                        "source": "local_rule",
                        "size": 1,
                        "selfSize": 0,
                        "childCount": 1,
                        "nameTags": ["temp"],
                        "topChildren": [
                            { "name": "foo.tmp", "type": "file", "size": 1 }
                        ]
                    }
                ]
            }
        });
        write_settings(&path, &seeded).expect("write settings");

        let saved = read_settings(&path);
        assert_eq!(
            saved["scanPersistentRules"]["keepExact"][0]["classification"],
            Value::String("keep".to_string())
        );
        assert_eq!(
            saved["scanPersistentRules"]["keepExact"][0]["nameTags"],
            json!(["user_content"])
        );
        assert_eq!(
            saved["scanPersistentRules"]["safeDeleteExact"][0]["classification"],
            Value::String("safe_to_delete".to_string())
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn read_scan_persistent_rules_with_cleanup_filters_local_rule_records() {
        let path = temp_settings_path();
        let seeded = json!({
            "scanPersistentRules": {
                "keepExact": [
                    {
                        "path": "C:\\Users\\tester\\Documents",
                        "nodeType": "directory",
                        "classification": "keep",
                        "reason": "keep this",
                        "risk": "high",
                        "source": "ai_promoted",
                        "size": 10,
                        "selfSize": 0,
                        "childCount": 2,
                        "nameTags": ["user_content"],
                        "topChildren": []
                    }
                ],
                "safeDeleteExact": [
                    {
                        "path": "C:\\Users\\tester\\AppData\\Local\\Temp",
                        "nodeType": "directory",
                        "classification": "safe_to_delete",
                        "reason": "temp",
                        "risk": "low",
                        "source": "local_rule",
                        "size": 1,
                        "selfSize": 0,
                        "childCount": 1,
                        "nameTags": ["temp"],
                        "topChildren": []
                    },
                    {
                        "path": "C:\\Users\\tester\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\Cache",
                        "nodeType": "directory",
                        "classification": "safe_to_delete",
                        "reason": "browser cache",
                        "risk": "low",
                        "source": "ai_promoted",
                        "size": 2,
                        "selfSize": 0,
                        "childCount": 1,
                        "nameTags": ["cache"],
                        "topChildren": []
                    }
                ]
            }
        });
        write_settings(&path, &seeded).expect("write settings");

        let (rules, cleaned) = read_scan_persistent_rules_with_cleanup(&path);
        assert!(cleaned);
        assert_eq!(rules.keep_exact.len(), 1);
        assert_eq!(rules.safe_delete_exact.len(), 1);
        assert_eq!(rules.safe_delete_exact[0].source, "ai_promoted");

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

    #[test]
    fn read_settings_migrates_legacy_provider_and_search_fields() {
        let path = temp_settings_path();
        let legacy = json!({
            "apiEndpoint": "https://api.deepseek.com",
            "model": "deepseek-reasoner",
            "enableWebSearch": true,
            "enableWebSearchOrganizer": true,
            "providerConfigs": {
                "https://api.deepseek.com": {
                    "apiKey": "sk-legacy-inline"
                }
            },
            "tavilyApiKey": "tvly-legacy-inline"
        });
        fs::write(
            &path,
            serde_json::to_string_pretty(&legacy).expect("serialize legacy settings"),
        )
        .expect("write legacy settings");

        let saved = read_settings(&path);

        assert_eq!(
            saved["defaultProviderEndpoint"],
            Value::String("https://api.deepseek.com".to_string())
        );
        assert_eq!(
            saved["providerConfigs"]["https://api.deepseek.com"]["model"],
            Value::String("deepseek-reasoner".to_string())
        );
        assert_eq!(saved["searchApi"]["enabled"], Value::Bool(true));
        assert_eq!(saved["searchApi"]["scopes"]["scan"], Value::Bool(true));
        assert_eq!(saved["searchApi"]["scopes"]["classify"], Value::Bool(true));
        assert_eq!(saved["searchApi"]["scopes"]["organizer"], Value::Bool(true));
        assert!(saved.get("apiEndpoint").is_none());
        assert!(saved.get("model").is_none());
        assert!(saved.get("enableWebSearch").is_none());
        assert!(saved.get("enableWebSearchOrganizer").is_none());
        assert!(saved.get("tavilyApiKey").is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn read_settings_upgrades_legacy_tika_defaults_for_ui_consistency() {
        let path = temp_settings_path();
        let legacy = json!({
            "contentExtraction": {
                "tika": {
                    "enabled": false,
                    "autoStart": false,
                    "url": "http://127.0.0.1:9998",
                    "jarPath": ""
                }
            }
        });
        fs::write(
            &path,
            serde_json::to_string_pretty(&legacy).expect("serialize legacy tika settings"),
        )
        .expect("write legacy tika settings");

        let saved = read_settings(&path);

        assert_eq!(
            saved["contentExtraction"]["tika"]["enabled"],
            Value::Bool(true)
        );
        assert_eq!(
            saved["contentExtraction"]["tika"]["autoStart"],
            Value::Bool(true)
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn organize_snapshot_defaults_summary_mode_when_missing() {
        let snapshot: OrganizeSnapshot = serde_json::from_value(json!({
            "id": "task_legacy",
            "status": "completed",
            "error": null,
            "rootPath": "C:\\root",
            "recursive": true,
            "excludedPatterns": [],
            "batchSize": 20,
            "maxClusterDepth": null,
            "useWebSearch": false,
            "webSearchEnabled": false,
            "selectedModel": "deepseek-chat",
            "selectedModels": {},
            "selectedProviders": {},
            "supportsMultimodal": false,
            "tree": {},
            "treeVersion": 0,
            "totalFiles": 0,
            "processedFiles": 0,
            "totalBatches": 0,
            "processedBatches": 0,
            "tokenUsage": { "prompt": 0, "completion": 0, "total": 0 },
            "results": [],
            "preview": [],
            "createdAt": "2026-03-28T00:00:00Z",
            "completedAt": null,
            "jobId": null
        }))
        .expect("deserialize legacy snapshot");

        assert_eq!(snapshot.summary_mode, default_organize_summary_mode());
    }

    #[test]
    fn resolve_credentials_fall_back_to_legacy_inline_values() {
        let path = temp_settings_path();
        let settings = json!({
            "defaultProviderEndpoint": "https://api.deepseek.com",
            "providerConfigs": {
                "https://api.deepseek.com": {
                    "endpoint": "https://api.deepseek.com",
                    "model": "deepseek-chat",
                    "apiKey": "sk-inline"
                }
            },
            "searchApi": {
                "provider": "tavily",
                "enabled": true,
                "apiKey": "tvly-inline",
                "scopes": {
                    "scan": true,
                    "classify": true,
                    "organizer": true
                }
            }
        });
        fs::write(
            &path,
            serde_json::to_string_pretty(&settings).expect("serialize settings"),
        )
        .expect("write settings");

        let state =
            AppState::with_store(path.clone(), Arc::new(InMemoryCredentialStore::default()));
        let raw = read_settings(&state.settings_path());
        let status = build_credentials_status_value(&state, &raw);

        assert_eq!(
            resolve_provider_api_key(&state, "https://api.deepseek.com")
                .expect("fallback provider secret"),
            "sk-inline".to_string()
        );
        assert_eq!(
            resolve_search_api_key(&state).expect("fallback search secret"),
            "tvly-inline".to_string()
        );
        assert_eq!(
            status["providerHasApiKey"]["https://api.deepseek.com"],
            Value::Bool(true)
        );
        assert_eq!(status["searchApiHasKey"], Value::Bool(true));

        let _ = fs::remove_file(path);
    }

    fn temp_app_state() -> (AppState, PathBuf) {
        let path = temp_settings_path();
        write_settings(&path, &default_settings()).expect("seed default settings");
        let state =
            AppState::with_store(path.clone(), Arc::new(InMemoryCredentialStore::default()));
        (state, path)
    }

    #[test]
    fn credentials_meta_reflects_saved_values() {
        let (state, path) = temp_app_state();
        let payload = CredentialsSaveInput {
            provider_secrets: Some(HashMap::from([(
                "https://api.openai.com/v1".to_string(),
                "sk-test".to_string(),
            )])),
            search_api_key: Some("tvly-test".to_string()),
        };

        let settings = read_settings(&state.settings_path());
        let mut provider_meta = HashMap::new();
        provider_meta.insert("https://api.openai.com/v1".to_string(), true);
        state
            .credential_store
            .set_many(&[
                (
                    provider_secret_key("https://api.openai.com/v1"),
                    "sk-test".to_string(),
                ),
                (SEARCH_SECRET_KEY.to_string(), "tvly-test".to_string()),
            ])
            .expect("save credentials");
        let saved = update_credentials_meta_in_settings(&state, &provider_meta, true)
            .expect("update credentials meta");
        let status = build_credentials_status_value(&state, &settings);

        assert_eq!(
            saved["credentialsMeta"]["providers"]["https://api.openai.com/v1"],
            Value::Bool(true)
        );
        assert_eq!(saved["credentialsMeta"]["searchApi"], Value::Bool(true));
        assert_eq!(
            status["providerHasApiKey"]["https://api.openai.com/v1"],
            Value::Bool(true)
        );
        assert_eq!(status["searchApiHasKey"], Value::Bool(true));
        assert_eq!(payload.search_api_key, Some("tvly-test".to_string()));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn credentials_meta_clears_when_empty_values_saved() {
        let (state, path) = temp_app_state();
        state
            .credential_store
            .set_many(&[
                (
                    provider_secret_key("https://api.openai.com/v1"),
                    "sk-test".to_string(),
                ),
                (SEARCH_SECRET_KEY.to_string(), "tvly-test".to_string()),
            ])
            .expect("seed credentials");

        let provider_meta = HashMap::from([("https://api.openai.com/v1".to_string(), false)]);
        let saved = update_credentials_meta_in_settings(&state, &provider_meta, false)
            .expect("update credentials meta");
        state
            .credential_store
            .set_many(&[
                (
                    provider_secret_key("https://api.openai.com/v1"),
                    String::new(),
                ),
                (SEARCH_SECRET_KEY.to_string(), String::new()),
            ])
            .expect("clear credentials");

        let status = build_credentials_status_value(&state, &read_settings(&state.settings_path()));

        assert_eq!(
            saved["credentialsMeta"]["providers"]["https://api.openai.com/v1"],
            Value::Bool(false)
        );
        assert_eq!(saved["credentialsMeta"]["searchApi"], Value::Bool(false));
        assert_eq!(
            status["providerHasApiKey"]["https://api.openai.com/v1"],
            Value::Bool(false)
        );
        assert_eq!(status["searchApiHasKey"], Value::Bool(false));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn credentials_save_preserves_existing_values_for_omitted_fields() {
        let (state, path) = temp_app_state();
        state
            .credential_store
            .set_many(&[
                (
                    provider_secret_key("https://api.openai.com/v1"),
                    "sk-existing".to_string(),
                ),
                (SEARCH_SECRET_KEY.to_string(), "tvly-existing".to_string()),
            ])
            .expect("seed credentials");

        save_credentials_internal(
            &state,
            CredentialsSaveInput {
                provider_secrets: Some(HashMap::new()),
                search_api_key: None,
            },
        )
        .expect("save without touching secrets");

        assert_eq!(
            state
                .credential_store
                .get(&provider_secret_key("https://api.openai.com/v1"))
                .expect("read provider credential"),
            Some("sk-existing".to_string())
        );
        assert_eq!(
            state
                .credential_store
                .get(SEARCH_SECRET_KEY)
                .expect("read search credential"),
            Some("tvly-existing".to_string())
        );

        let saved = read_settings(&state.settings_path());
        assert_eq!(
            saved["credentialsMeta"]["providers"]["https://api.openai.com/v1"],
            Value::Bool(true)
        );
        assert_eq!(saved["credentialsMeta"]["searchApi"], Value::Bool(true));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn credentials_save_updates_only_explicitly_touched_fields() {
        let (state, path) = temp_app_state();
        state
            .credential_store
            .set_many(&[
                (
                    provider_secret_key("https://api.openai.com/v1"),
                    "sk-openai".to_string(),
                ),
                (
                    provider_secret_key("https://api.deepseek.com"),
                    "sk-deepseek".to_string(),
                ),
                (SEARCH_SECRET_KEY.to_string(), "tvly-existing".to_string()),
            ])
            .expect("seed credentials");

        save_credentials_internal(
            &state,
            CredentialsSaveInput {
                provider_secrets: Some(HashMap::from([(
                    "https://api.openai.com/v1".to_string(),
                    String::new(),
                )])),
                search_api_key: Some(String::new()),
            },
        )
        .expect("save touched secrets");

        assert_eq!(
            state
                .credential_store
                .get(&provider_secret_key("https://api.openai.com/v1"))
                .expect("read cleared provider credential"),
            None
        );
        assert_eq!(
            state
                .credential_store
                .get(&provider_secret_key("https://api.deepseek.com"))
                .expect("read untouched provider credential"),
            Some("sk-deepseek".to_string())
        );
        assert_eq!(
            state
                .credential_store
                .get(SEARCH_SECRET_KEY)
                .expect("read cleared search credential"),
            None
        );

        let saved = read_settings(&state.settings_path());
        assert_eq!(
            saved["credentialsMeta"]["providers"]["https://api.openai.com/v1"],
            Value::Bool(false)
        );
        assert_eq!(
            saved["credentialsMeta"]["providers"]["https://api.deepseek.com"],
            Value::Bool(true)
        );
        assert_eq!(saved["credentialsMeta"]["searchApi"], Value::Bool(false));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn migrate_data_dir_moves_settings_and_database() {
        let base_dir = std::env::temp_dir().join(format!("aicleaner-data-root-{}", Uuid::new_v4()));
        let target_dir =
            std::env::temp_dir().join(format!("aicleaner-data-target-{}", Uuid::new_v4()));
        fs::create_dir_all(&base_dir).expect("create base dir");

        let state = AppState::bootstrap(base_dir.clone()).expect("bootstrap state");
        write_settings(
            &state.settings_path(),
            &json!({
                "scanPath": "D:\\data",
                "targetSizeGB": 2
            }),
        )
        .expect("write settings");
        fs::write(state.data_dir().join("marker.txt"), b"marker").expect("write marker");

        let response =
            migrate_data_dir(&state, &target_dir.to_string_lossy()).expect("migrate data dir");

        assert!(same_path(&state.data_dir(), &target_dir));
        assert!(state.settings_path().exists());
        assert!(state.db_path().exists());
        assert!(state.data_dir().join("marker.txt").exists());
        assert_eq!(
            response["settings"]["storage"]["dataDir"],
            Value::String(target_dir.to_string_lossy().to_string())
        );
        assert_eq!(
            resolve_storage_data_dir(&base_dir, &state.bootstrap_path),
            target_dir
        );
        assert!(!base_dir.join("settings.json").exists());
        assert!(!base_dir.join("scan-cache.sqlite").exists());

        let _ = fs::remove_dir_all(&target_dir);
        let _ = fs::remove_file(&state.bootstrap_path);
        let _ = fs::remove_dir_all(&base_dir);
    }
}
