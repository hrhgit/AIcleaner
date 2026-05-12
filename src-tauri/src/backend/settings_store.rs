use crate::backend::{
    normalize_provider_api_format, normalize_provider_endpoint, normalize_provider_thinking_level,
    preset_provider_configs_json,
};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

pub(crate) fn default_settings() -> Value {
    json!({
        "defaultProviderEndpoint": "",
        "providerConfigs": preset_provider_configs_json(),
        "searchApi": {
            "provider": "tavily",
            "enabled": false,
            "apiKey": "",
            "scopes": {
                "classify": false,
                "organizer": false
            }
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

fn ensure_provider_defaults(config: &mut serde_json::Map<String, Value>, fallback_name: &str) {
    if value_as_non_empty_string(config.get("name")).is_none() {
        config.insert("name".to_string(), Value::String(fallback_name.to_string()));
    }
    if !config.contains_key("model") {
        config.insert("model".to_string(), Value::String(String::new()));
    }
    let thinking = config
        .entry("thinking".to_string())
        .or_insert_with(|| json!({}));
    if !thinking.is_object() {
        *thinking = json!({});
    }
    if let Some(thinking_obj) = thinking.as_object_mut() {
        let enabled = thinking_obj
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        thinking_obj.insert("enabled".to_string(), Value::Bool(enabled));
        thinking_obj.insert(
            "level".to_string(),
            Value::String(
                normalize_provider_thinking_level(
                    thinking_obj.get("level").and_then(Value::as_str),
                )
                .to_string(),
            ),
        );
    }
}

fn normalize_provider_configs(obj: &mut serde_json::Map<String, Value>) -> Result<(), String> {
    let provider_configs = obj
        .entry("providerConfigs".to_string())
        .or_insert_with(|| json!({}));
    if !provider_configs.is_object() {
        *provider_configs = json!({});
    }

    let existing = provider_configs
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect::<Vec<_>>();

    let mut normalized = serde_json::Map::new();

    for (key, raw_config) in existing {
        let mut config_obj = raw_config.as_object().cloned().unwrap_or_default();
        let raw_endpoint = config_obj
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or(&key)
            .trim()
            .to_string();
        let api_format = normalize_provider_api_format(
            &raw_endpoint,
            config_obj.get("apiFormat").and_then(Value::as_str),
        );
        let endpoint = normalize_provider_endpoint(&raw_endpoint, api_format)?;
        config_obj.insert("endpoint".to_string(), Value::String(endpoint.clone()));
        config_obj.insert(
            "apiFormat".to_string(),
            Value::String(api_format.as_str().to_string()),
        );
        ensure_provider_defaults(&mut config_obj, &endpoint);
        normalized.insert(endpoint, Value::Object(config_obj));
    }

    *provider_configs = Value::Object(normalized);
    Ok(())
}

fn migrate_legacy_settings_fields(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };

    let legacy_endpoint = value_as_non_empty_string(obj.get("apiEndpoint"));
    let legacy_model = value_as_non_empty_string(obj.get("model"));
    let legacy_search_enabled = obj.get("enableWebSearch").and_then(Value::as_bool);
    let legacy_classify_enabled = obj.get("enableWebSearchClassify").and_then(Value::as_bool);
    let legacy_organizer_enabled = obj.get("enableWebSearchOrganizer").and_then(Value::as_bool);
    let legacy_search_api_key = value_as_non_empty_string(obj.get("tavilyApiKey")).or_else(|| {
        value_as_non_empty_string(obj.get("searchApi").and_then(|search| search.get("apiKey")))
    });

    if let Some(endpoint) = legacy_endpoint.clone() {
        let api_format = normalize_provider_api_format(&endpoint, None);
        let normalized_endpoint =
            normalize_provider_endpoint(&endpoint, api_format).unwrap_or(endpoint.clone());
        if value_as_non_empty_string(obj.get("defaultProviderEndpoint")).is_none() {
            obj.insert(
                "defaultProviderEndpoint".to_string(),
                Value::String(normalized_endpoint.clone()),
            );
        }

        let provider_configs = obj
            .entry("providerConfigs".to_string())
            .or_insert_with(|| json!({}));
        if !provider_configs.is_object() {
            *provider_configs = json!({});
        }
        if let Some(configs) = provider_configs.as_object_mut() {
            let entry = configs
                .entry(normalized_endpoint.clone())
                .or_insert_with(|| json!({}));
            if !entry.is_object() {
                *entry = json!({});
            }
            if let Some(config) = entry.as_object_mut() {
                if value_as_non_empty_string(config.get("endpoint")).is_none() {
                    config.insert(
                        "endpoint".to_string(),
                        Value::String(normalized_endpoint.clone()),
                    );
                }
                if value_as_non_empty_string(config.get("name")).is_none() {
                    config.insert("name".to_string(), Value::String(normalized_endpoint.clone()));
                }
                if value_as_non_empty_string(config.get("apiFormat")).is_none() {
                    config.insert(
                        "apiFormat".to_string(),
                        Value::String(api_format.as_str().to_string()),
                    );
                }
                if !config.contains_key("model") {
                    config.insert(
                        "model".to_string(),
                        Value::String(legacy_model.clone().unwrap_or_default()),
                    );
                }
            }
        }
    }

    let has_legacy_search_settings = legacy_search_enabled.is_some()
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
                let enabled = legacy_search_enabled.unwrap_or(false)
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
                if !scopes_obj.contains_key("classify") {
                    scopes_obj.insert(
                        "classify".to_string(),
                        Value::Bool(
                            legacy_classify_enabled
                                .unwrap_or(legacy_search_enabled.unwrap_or(false)),
                        ),
                    );
                }
                if !scopes_obj.contains_key("organizer") {
                    scopes_obj.insert(
                        "organizer".to_string(),
                        Value::Bool(
                            legacy_organizer_enabled.unwrap_or(
                                legacy_classify_enabled
                                    .unwrap_or(legacy_search_enabled.unwrap_or(false)),
                            ),
                        ),
                    );
                }
            }
        }
    }
}

fn normalize_settings_shape(value: &mut Value) -> Result<(), String> {
    let Some(obj) = value.as_object_mut() else {
        *value = default_settings();
        return Ok(());
    };

    normalize_provider_configs(obj)?;

    let provider_configs = obj
        .get("providerConfigs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut default_provider_endpoint = obj
        .get("defaultProviderEndpoint")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    if !default_provider_endpoint.is_empty() {
        let api_format = normalize_provider_api_format(&default_provider_endpoint, None);
        default_provider_endpoint =
            normalize_provider_endpoint(&default_provider_endpoint, api_format)?;
    }

    if !provider_configs.contains_key(&default_provider_endpoint) {
        default_provider_endpoint = provider_configs
            .keys()
            .next()
            .cloned()
            .unwrap_or_default();
    }

    obj.insert(
        "defaultProviderEndpoint".to_string(),
        Value::String(default_provider_endpoint),
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
    let legacy_scan_enabled = search_scopes
        .get("scan")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let classify_enabled = search_scopes
        .get("classify")
        .and_then(Value::as_bool)
        .unwrap_or(legacy_scan_enabled);
    let organizer_enabled = search_scopes
        .get("organizer")
        .and_then(Value::as_bool)
        .unwrap_or(classify_enabled);
    let workflow_search_enabled = classify_enabled || organizer_enabled;
    let search_enabled = search_api
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(workflow_search_enabled);
    obj.insert(
        "searchApi".to_string(),
        json!({
            "provider": "tavily",
            "enabled": search_enabled || workflow_search_enabled,
            "apiKey": search_api.get("apiKey").and_then(Value::as_str).unwrap_or(""),
            "scopes": {
                "classify": workflow_search_enabled,
                "organizer": workflow_search_enabled
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
    obj.remove("scanPath");
    obj.remove("maxDepth");
    obj.remove("maxDepthUnlimited");
    obj.remove("scanIgnorePaths");
    obj.remove("lastScanTime");
    obj.remove("scanPersistentRules");
    Ok(())
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
    if let Err(err) = normalize_settings_shape(&mut merged) {
        log::warn!("Failed to normalize settings shape: {err}");
        let mut fallback = default_settings();
        merge_json(&mut fallback, &merged);
        normalize_settings_shape(&mut fallback).ok();
        merged = fallback;
    }
    merged
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
    normalize_settings_shape(&mut merged)?;
    strip_secret_fields(&mut merged);
    fs::write(
        path,
        serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_settings_shape_unifies_workflow_search_scopes() {
        let mut value = json!({
            "searchApi": {
                "enabled": false,
                "scopes": {
                    "classify": true,
                    "organizer": false
                }
            }
        });

        normalize_settings_shape(&mut value).expect("normalize settings");

        assert_eq!(value["searchApi"]["enabled"], Value::Bool(true));
        assert_eq!(value["searchApi"]["scopes"]["classify"], Value::Bool(true));
        assert_eq!(value["searchApi"]["scopes"]["organizer"], Value::Bool(true));
    }
}
