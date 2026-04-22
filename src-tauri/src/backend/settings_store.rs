use super::{
    ScanPersistentRuleRecord, ScanPersistentRules, ScanRuleTopChild,
};
use crate::backend::provider_registry::default_model_for_endpoint;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn default_settings() -> Value {
    json!({
        "defaultProviderEndpoint": "https://api.openai.com/v1",
        "providerConfigs": {
            "https://api.openai.com/v1": { "name": "OpenAI", "endpoint": "https://api.openai.com/v1", "apiKey": "", "model": "gpt-4o-mini" },
            "https://api.deepseek.com": { "name": "DeepSeek", "endpoint": "https://api.deepseek.com", "apiKey": "", "model": "deepseek-chat" },
            "https://generativelanguage.googleapis.com/v1beta/openai/": { "name": "Google Gemini", "endpoint": "https://generativelanguage.googleapis.com/v1beta/openai/", "apiKey": "", "model": "gemini-2.5-flash" },
            "https://dashscope.aliyuncs.com/compatible-mode/v1": { "name": "Qwen", "endpoint": "https://dashscope.aliyuncs.com/compatible-mode/v1", "apiKey": "", "model": "qwen-plus" },
            "https://open.bigmodel.cn/api/paas/v4": { "name": "GLM", "endpoint": "https://open.bigmodel.cn/api/paas/v4", "apiKey": "", "model": "glm-4-flash" },
            "https://api.moonshot.cn/v1": { "name": "Moonshot", "endpoint": "https://api.moonshot.cn/v1", "apiKey": "", "model": "moonshot-v1-8k" },
            "https://api.minimax.io/anthropic/v1": { "name": "MiniMax (Anthropic)", "endpoint": "https://api.minimax.io/anthropic/v1", "apiKey": "", "model": "MiniMax-M2.7" }
        },
        "scanPath": "",
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
        "scanPersistentRules": {
            "keepExact": [],
            "safeDeleteExact": []
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

pub(crate) fn legacy_provider_api_key_from_settings(
    settings: &Value,
    endpoint: &str,
) -> Option<String> {
    value_as_non_empty_string(
        settings
            .get("providerConfigs")
            .and_then(|configs| configs.get(endpoint))
            .and_then(|config| config.get("apiKey")),
    )
}

pub(crate) fn legacy_search_api_key_from_settings(settings: &Value) -> Option<String> {
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
                            legacy_model
                                .clone()
                                .unwrap_or_else(|| default_model_for_endpoint(&endpoint).to_string()),
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

    if let Some(configs) = obj.get_mut("providerConfigs").and_then(Value::as_object_mut) {
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
        configs.entry(default_provider_endpoint.clone()).or_insert_with(|| {
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
        .unwrap_or_else(|| search_api.get("enabled").and_then(Value::as_bool).unwrap_or(false));
    let classify_enabled = search_scopes
        .get("classify")
        .and_then(Value::as_bool)
        .unwrap_or(scan_enabled);
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
            "apiKey": search_api.get("apiKey").and_then(Value::as_str).unwrap_or(""),
            "scopes": {
                "scan": scan_enabled,
                "classify": classify_enabled,
                "organizer": organizer_enabled
            }
        }),
    );
    obj.insert(
        "scanPersistentRules".to_string(),
        serde_json::to_value(normalize_scan_persistent_rules(obj.get("scanPersistentRules")))
            .unwrap_or_else(|_| json!({
                "keepExact": [],
                "safeDeleteExact": []
            })),
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

fn normalize_scan_rule_top_children(records: &[ScanRuleTopChild]) -> Vec<ScanRuleTopChild> {
    records
        .iter()
        .filter_map(|record| {
            let name = record.name.trim();
            if name.is_empty() {
                return None;
            }
            let item_type = match record.item_type.trim() {
                "directory" => "directory",
                "file" => "file",
                _ => return None,
            };
            Some(ScanRuleTopChild {
                name: name.to_string(),
                item_type: item_type.to_string(),
                size: record.size,
            })
        })
        .take(5)
        .collect()
}

fn normalize_scan_rule_name_tags(tags: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    tags.iter()
        .filter_map(|tag| {
            let normalized = tag.trim().to_ascii_lowercase();
            if normalized.is_empty() || !seen.insert(normalized.clone()) {
                return None;
            }
            Some(normalized)
        })
        .collect()
}

fn normalize_scan_persistent_rule_record(
    raw: ScanPersistentRuleRecord,
    fallback_classification: &str,
) -> Option<ScanPersistentRuleRecord> {
    let path = PathBuf::from(raw.path.trim()).to_string_lossy().to_string();
    if path.trim().is_empty() {
        return None;
    }
    let node_type = match raw.node_type.trim() {
        "directory" => "directory",
        "file" => "file",
        _ => return None,
    }
    .to_string();
    let classification = match raw.classification.trim() {
        "keep" => "keep",
        "safe_to_delete" => "safe_to_delete",
        _ => fallback_classification,
    }
    .to_string();
    let source = if raw.source.trim().is_empty() {
        "ai_promoted".to_string()
    } else {
        raw.source.trim().to_string()
    };
    let risk = if raw.risk.trim().is_empty() {
        if classification == "keep" {
            "high".to_string()
        } else {
            "low".to_string()
        }
    } else {
        raw.risk.trim().to_string()
    };

    Some(ScanPersistentRuleRecord {
        path,
        node_type,
        classification,
        reason: raw.reason.trim().to_string(),
        risk,
        source,
        size: raw.size,
        self_size: raw.self_size,
        child_count: raw.child_count,
        name_tags: normalize_scan_rule_name_tags(&raw.name_tags),
        top_children: normalize_scan_rule_top_children(&raw.top_children),
    })
}

fn normalize_scan_persistent_rule_records(
    value: Option<&Value>,
    fallback_classification: &str,
) -> Vec<ScanPersistentRuleRecord> {
    let mut seen = HashSet::new();
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| serde_json::from_value::<ScanPersistentRuleRecord>(item.clone()).ok())
        .filter_map(|item| normalize_scan_persistent_rule_record(item, fallback_classification))
        .filter(|record| seen.insert(record.path.to_lowercase()))
        .collect()
}

#[cfg(test)]
fn remove_local_rule_records(rules: &mut ScanPersistentRules) -> bool {
    let keep_before = rules.keep_exact.len();
    let safe_before = rules.safe_delete_exact.len();
    rules
        .keep_exact
        .retain(|record| !record.source.eq_ignore_ascii_case("local_rule"));
    rules
        .safe_delete_exact
        .retain(|record| !record.source.eq_ignore_ascii_case("local_rule"));
    keep_before != rules.keep_exact.len() || safe_before != rules.safe_delete_exact.len()
}

pub(crate) fn normalize_scan_persistent_rules(value: Option<&Value>) -> ScanPersistentRules {
    ScanPersistentRules {
        keep_exact: normalize_scan_persistent_rule_records(
            value.and_then(|rules| rules.get("keepExact")),
            "keep",
        ),
        safe_delete_exact: normalize_scan_persistent_rule_records(
            value.and_then(|rules| rules.get("safeDeleteExact")),
            "safe_to_delete",
        ),
    }
}

pub(crate) fn read_scan_ignore_paths(path: &Path) -> Vec<String> {
    let settings = read_settings(path);
    normalize_scan_ignore_paths(settings.get("scanIgnorePaths"))
}

#[cfg(test)]
pub(crate) fn read_scan_persistent_rules_with_cleanup(path: &Path) -> (ScanPersistentRules, bool) {
    let settings = read_settings(path);
    let mut rules = normalize_scan_persistent_rules(settings.get("scanPersistentRules"));
    let cleaned = remove_local_rule_records(&mut rules);
    (rules, cleaned)
}

pub(crate) fn strip_secret_fields(v: &mut Value) {
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

pub(crate) fn write_settings(path: &Path, value: &Value) -> Result<(), String> {
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
