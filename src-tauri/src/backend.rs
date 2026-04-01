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

const CREDENTIAL_SERVICE: &str = "aicleaner";
const SEARCH_SECRET_KEY: &str = "search:tavily:apiKey";

#[derive(Clone)]
pub struct AppState {
    pub settings_path: PathBuf,
    pub db_path: PathBuf,
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

trait CredentialStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, value: &str) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;

    fn get_many(&self, accounts: &[String]) -> Result<HashMap<String, String>, String> {
        let mut values = HashMap::with_capacity(accounts.len());
        for account in accounts {
            if let Some(value) = self.get(account)? {
                values.insert(account.clone(), value);
            }
        }
        Ok(values)
    }

    fn set_many(&self, entries: &[(String, String)]) -> Result<(), String> {
        for (account, value) in entries {
            if value.trim().is_empty() {
                self.delete(account)?;
            } else {
                self.set(account, value)?;
            }
        }
        Ok(())
    }
}

struct WindowsCredentialStore {
    service: String,
}

impl WindowsCredentialStore {
    fn new(service: &str) -> Self {
        Self {
            service: service.to_string(),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(&self.service, account).map_err(|e| e.to_string())
    }
}

impl CredentialStore for WindowsCredentialStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        match self.entry(account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.to_string()),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        self.entry(account)?
            .set_password(value)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct InMemoryCredentialStore {
    values: Mutex<HashMap<String, String>>,
}

#[cfg(test)]
impl CredentialStore for InMemoryCredentialStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        Ok(self.values.lock().get(account).cloned())
    }

    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        self.values
            .lock()
            .insert(account.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        self.values.lock().remove(account);
        Ok(())
    }
}

fn provider_secret_key(endpoint: &str) -> String {
    format!("provider:{}:apiKey", endpoint.trim())
}

fn default_model_for_endpoint(endpoint: &str) -> &'static str {
    match endpoint.trim() {
        "https://api.deepseek.com" => "deepseek-chat",
        "https://generativelanguage.googleapis.com/v1beta/openai/" => "gemini-2.5-flash",
        "https://dashscope.aliyuncs.com/compatible-mode/v1" => "qwen-plus",
        "https://open.bigmodel.cn/api/paas/v4" => "glm-4-flash",
        "https://api.moonshot.cn/v1" => "moonshot-v1-8k",
        _ => "gpt-4o-mini",
    }
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
            credential_store: Arc::new(WindowsCredentialStore::new(CREDENTIAL_SERVICE)),
            settings_path,
            db_path,
            scan_tasks: Arc::new(Mutex::new(HashMap::new())),
            organize_tasks: Arc::new(Mutex::new(HashMap::new())),
            tika_process: Arc::new(Mutex::new(None)),
        })
    }

    #[cfg(test)]
    fn with_store(settings_path: PathBuf, credential_store: Arc<dyn CredentialStore>) -> Self {
        Self {
            settings_path,
            db_path: std::env::temp_dir().join("aicleaner-test.sqlite"),
            scan_tasks: Arc::new(Mutex::new(HashMap::new())),
            organize_tasks: Arc::new(Mutex::new(HashMap::new())),
            tika_process: Arc::new(Mutex::new(None)),
            credential_store,
        }
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

fn default_settings() -> Value {
    json!({
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
        "maxDepthUnlimited": false,
        "scanIgnorePaths": [],
        "lastScanTime": null,
        "searchApi": {
            "provider": "tavily",
            "enabled": false,
            "apiKey": "",
            "scopes": { "scan": false, "classify": false, "organizer": false }
        },
        "contentExtraction": {
            "tika": {
                "enabled": true,
                "url": "http://127.0.0.1:9998",
                "autoStart": true,
                "jarPath": ""
            }
        },
        "credentialsMeta": {
            "providers": {},
            "searchApi": false
        }
    })
}

fn value_as_non_empty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn legacy_provider_api_key_from_settings(settings: &Value, endpoint: &str) -> Option<String> {
    value_as_non_empty_string(
        settings
            .get("providerConfigs")
            .and_then(|configs| configs.get(endpoint))
            .and_then(|config| config.get("apiKey")),
    )
}

fn legacy_search_api_key_from_settings(settings: &Value) -> Option<String> {
    value_as_non_empty_string(settings.pointer("/searchApi/apiKey"))
        .or_else(|| value_as_non_empty_string(settings.get("tavilyApiKey")))
}

fn migrate_legacy_settings_fields(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };

    let legacy_endpoint = value_as_non_empty_string(obj.get("apiEndpoint"));
    let legacy_model = value_as_non_empty_string(obj.get("model"));
    let legacy_scan_enabled = obj.get("enableWebSearch").and_then(Value::as_bool);
    let legacy_classify_enabled = obj.get("enableWebSearchClassify").and_then(Value::as_bool);
    let legacy_organizer_enabled = obj.get("enableWebSearchOrganizer").and_then(Value::as_bool);
    let legacy_search_api_key = value_as_non_empty_string(obj.get("tavilyApiKey")).or_else(|| {
        value_as_non_empty_string(obj.get("searchApi").and_then(|search| search.get("apiKey")))
    });

    if let Some(endpoint) = legacy_endpoint.clone() {
        if value_as_non_empty_string(obj.get("defaultProviderEndpoint")).is_none() {
            obj.insert(
                "defaultProviderEndpoint".to_string(),
                Value::String(endpoint.clone()),
            );
        }

        let provider_configs = obj
            .entry("providerConfigs".to_string())
            .or_insert_with(|| json!({}));
        if !provider_configs.is_object() {
            *provider_configs = json!({});
        }
        if let Some(configs) = provider_configs.as_object_mut() {
            let entry = configs.entry(endpoint.clone()).or_insert_with(|| json!({}));
            if !entry.is_object() {
                *entry = json!({});
            }
            if let Some(config) = entry.as_object_mut() {
                if value_as_non_empty_string(config.get("endpoint")).is_none() {
                    config.insert("endpoint".to_string(), Value::String(endpoint.clone()));
                }
                if value_as_non_empty_string(config.get("name")).is_none() {
                    config.insert("name".to_string(), Value::String(endpoint.clone()));
                }
                if value_as_non_empty_string(config.get("model")).is_none() {
                    config.insert(
                        "model".to_string(),
                        Value::String(
                            legacy_model.clone().unwrap_or_else(|| {
                                default_model_for_endpoint(&endpoint).to_string()
                            }),
                        ),
                    );
                }
            }
        }
    }

    let has_legacy_search_settings = legacy_scan_enabled.is_some()
        || legacy_classify_enabled.is_some()
        || legacy_organizer_enabled.is_some()
        || legacy_search_api_key.is_some();
    if has_legacy_search_settings {
        let search_api = obj
            .entry("searchApi".to_string())
            .or_insert_with(|| json!({}));
        if !search_api.is_object() {
            *search_api = json!({});
        }
        if let Some(search) = search_api.as_object_mut() {
            if value_as_non_empty_string(search.get("provider")).is_none() {
                search.insert("provider".to_string(), Value::String("tavily".to_string()));
            }
            if !search.contains_key("enabled") {
                let enabled = legacy_scan_enabled.unwrap_or(false)
                    || legacy_classify_enabled.unwrap_or(false)
                    || legacy_organizer_enabled.unwrap_or(false);
                search.insert("enabled".to_string(), Value::Bool(enabled));
            }
            if value_as_non_empty_string(search.get("apiKey")).is_none() {
                if let Some(api_key) = legacy_search_api_key {
                    search.insert("apiKey".to_string(), Value::String(api_key));
                }
            }

            let scopes = search
                .entry("scopes".to_string())
                .or_insert_with(|| json!({}));
            if !scopes.is_object() {
                *scopes = json!({});
            }
            if let Some(scopes_obj) = scopes.as_object_mut() {
                if !scopes_obj.contains_key("scan") {
                    scopes_obj.insert(
                        "scan".to_string(),
                        Value::Bool(legacy_scan_enabled.unwrap_or(false)),
                    );
                }
                if !scopes_obj.contains_key("classify") {
                    scopes_obj.insert(
                        "classify".to_string(),
                        Value::Bool(
                            legacy_classify_enabled
                                .unwrap_or(legacy_organizer_enabled.unwrap_or(false)),
                        ),
                    );
                }
                if !scopes_obj.contains_key("organizer") {
                    scopes_obj.insert(
                        "organizer".to_string(),
                        Value::Bool(
                            legacy_organizer_enabled
                                .unwrap_or(legacy_classify_enabled.unwrap_or(false)),
                        ),
                    );
                }
            }
        }
    }
}

fn normalize_settings_shape(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        *value = default_settings();
        return;
    };

    let mut default_provider_endpoint = obj
        .get("defaultProviderEndpoint")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    if let Some(configs) = obj
        .get_mut("providerConfigs")
        .and_then(Value::as_object_mut)
    {
        let mut first_endpoint = String::new();
        for (key, config) in configs.iter_mut() {
            let endpoint = config
                .get("endpoint")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .trim()
                .to_string();
            if endpoint.is_empty() {
                continue;
            }
            if first_endpoint.is_empty() {
                first_endpoint = endpoint.clone();
            }
            if let Some(config_obj) = config.as_object_mut() {
                config_obj.insert("endpoint".to_string(), Value::String(endpoint.clone()));
                let current_model = config_obj
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if current_model.is_empty() {
                    config_obj.insert(
                        "model".to_string(),
                        Value::String(default_model_for_endpoint(&endpoint).to_string()),
                    );
                }
                if !config_obj.contains_key("name") {
                    config_obj.insert("name".to_string(), Value::String(endpoint.clone()));
                }
            }
        }
        if default_provider_endpoint.is_empty() {
            default_provider_endpoint = first_endpoint;
        }
        if default_provider_endpoint.is_empty() {
            default_provider_endpoint = "https://api.openai.com/v1".to_string();
        }
        let default_provider_endpoint_for_insert = default_provider_endpoint.clone();
        configs
            .entry(default_provider_endpoint.clone())
            .or_insert_with(|| {
                json!({
                    "name": default_provider_endpoint_for_insert.clone(),
                    "endpoint": default_provider_endpoint_for_insert.clone(),
                    "apiKey": "",
                    "model": default_model_for_endpoint(&default_provider_endpoint_for_insert).to_string()
                })
            });
    }
    obj.insert(
        "defaultProviderEndpoint".to_string(),
        Value::String(default_provider_endpoint.clone()),
    );

    let search_api = obj
        .get("searchApi")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let search_scopes = search_api
        .get("scopes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let scan_enabled = search_scopes
        .get("scan")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let classify_enabled = search_scopes
        .get("classify")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let organizer_enabled = search_scopes
        .get("organizer")
        .and_then(Value::as_bool)
        .unwrap_or(classify_enabled);
    let search_enabled = search_api
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(scan_enabled || classify_enabled || organizer_enabled);
    obj.insert(
        "searchApi".to_string(),
        json!({
            "provider": "tavily",
            "enabled": search_enabled,
            "apiKey": search_api
                .get("apiKey")
                .and_then(Value::as_str)
                .unwrap_or(""),
            "scopes": {
                "scan": scan_enabled,
                "classify": classify_enabled,
                "organizer": organizer_enabled
            }
        }),
    );
    let tika = obj
        .get("contentExtraction")
        .and_then(|value| value.get("tika"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let tika_enabled = tika.get("enabled").and_then(Value::as_bool);
    let tika_auto_start = tika.get("autoStart").and_then(Value::as_bool);
    let tika_url = tika
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("http://127.0.0.1:9998")
        .trim()
        .trim_end_matches('/')
        .to_string();
    let tika_jar_path = tika
        .get("jarPath")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    // Keep settings UI aligned with organizer runtime's legacy fallback behavior.
    let legacy_tika_defaults = !tika_enabled.unwrap_or(false)
        && !tika_auto_start.unwrap_or(false)
        && tika_url == "http://127.0.0.1:9998"
        && tika_jar_path.is_empty();
    obj.insert(
        "contentExtraction".to_string(),
        json!({
            "tika": {
                "enabled": if legacy_tika_defaults { true } else { tika_enabled.unwrap_or(true) },
                "url": tika_url,
                "autoStart": if legacy_tika_defaults { true } else { tika_auto_start.unwrap_or(true) },
                "jarPath": tika_jar_path
            }
        }),
    );
    obj.remove("apiEndpoint");
    obj.remove("model");
    obj.remove("enableWebSearch");
    obj.remove("enableWebSearchClassify");
    obj.remove("enableWebSearchOrganizer");
    obj.remove("tavilyApiKey");
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
    if let Some(mut parsed) = fs::read_to_string(path)
        .ok()
        .and_then(|x| serde_json::from_str::<Value>(&x).ok())
    {
        migrate_legacy_settings_fields(&mut parsed);
        merge_json(&mut merged, &parsed);
    }
    normalize_settings_shape(&mut merged);
    merged
}

pub(crate) fn normalize_scan_ignore_paths(value: Option<&Value>) -> Vec<String> {
    let mut seen = HashSet::new();
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return None;
            }
            let path = PathBuf::from(trimmed).to_string_lossy().to_string();
            let key = path.trim().to_lowercase();
            if key.is_empty() || !seen.insert(key) {
                return None;
            }
            Some(path)
        })
        .collect()
}

pub(crate) fn read_scan_ignore_paths(path: &Path) -> Vec<String> {
    let settings = read_settings(path);
    normalize_scan_ignore_paths(settings.get("scanIgnorePaths"))
}

fn strip_secret_fields(v: &mut Value) {
    if let Some(obj) = v.as_object_mut() {
        obj.remove("secretStatus");
        obj.remove("credentialsStatus");
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
    let normalized_ignore_paths = normalize_scan_ignore_paths(merged.get("scanIgnorePaths"));
    if let Some(obj) = merged.as_object_mut() {
        obj.insert(
            "scanIgnorePaths".to_string(),
            Value::Array(
                normalized_ignore_paths
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    normalize_settings_shape(&mut merged);
    strip_secret_fields(&mut merged);
    fs::write(
        path,
        serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
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
    let mut settings = read_settings(&state.settings_path);
    apply_credentials_meta(&mut settings, provider_meta, search_has_api_key);
    write_settings(&state.settings_path, &settings)?;
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
    let settings = read_settings(&state.settings_path);
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
    legacy_provider_api_key_from_settings(&read_settings(&state.settings_path), endpoint)
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
    legacy_search_api_key_from_settings(&read_settings(&state.settings_path))
        .ok_or_else(|| "Credential not found.".to_string())
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
pub async fn credentials_get(state: State<'_, AppState>) -> Result<Value, String> {
    let settings = read_settings(&state.settings_path);
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
    let settings = read_settings(&state.settings_path);
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
        let _ = crate::persist::delete_scan_data_for_paths(&state.db_path, task_id, &cleaned);
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
    pub baseline_task_id: Option<String>,
    pub scan_mode: Option<String>,
    pub auto_analyze: Option<bool>,
    pub api_endpoint: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub use_web_search: Option<bool>,
    pub search_api_key: Option<String>,
    pub response_language: Option<String>,
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
    if input.use_web_search.is_none() {
        let settings = read_settings(&state.settings_path);
        let scan_enabled = settings
            .pointer("/searchApi/scopes/scan")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let classify_enabled = settings
            .pointer("/searchApi/scopes/classify")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        input.use_web_search = Some(scan_enabled || classify_enabled);
    }
    if input.use_web_search.unwrap_or(false) && input.search_api_key.is_none() {
        input.search_api_key = Some(resolve_search_api_key(state.inner()).unwrap_or_default());
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
pub async fn scan_find_latest_for_path(
    state: State<'_, AppState>,
    path: String,
) -> Result<Value, String> {
    crate::scan_runtime::scan_find_latest_for_path(state, path).await
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
        let raw = read_settings(&state.settings_path);
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

        let settings = read_settings(&state.settings_path);
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

        let status = build_credentials_status_value(&state, &read_settings(&state.settings_path));

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

        let saved = read_settings(&state.settings_path);
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

        let saved = read_settings(&state.settings_path);
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
}
