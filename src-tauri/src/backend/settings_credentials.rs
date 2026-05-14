#[derive(Clone, Debug)]
pub(crate) struct ProviderRouteConfig {
    pub endpoint: String,
    pub model: String,
    pub api_format: ApiFormat,
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

fn build_credentials_status_from_meta(settings: &Value) -> Value {
    let endpoints = collect_provider_endpoints(settings);
    let saved_provider_meta = settings
        .get("credentialsMeta")
        .and_then(|meta| meta.get("providers"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut provider_meta = HashMap::new();
    for endpoint in endpoints {
        let saved = saved_provider_meta
            .get(&endpoint)
            .and_then(Value::as_bool)
            .unwrap_or(false);
        provider_meta.insert(
            endpoint.clone(),
            saved || legacy_provider_api_key_from_settings(settings, &endpoint).is_some(),
        );
    }
    let saved_search_has_key = settings
        .get("credentialsMeta")
        .and_then(|meta| meta.get("searchApi"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    json!({
        "providerHasApiKey": provider_meta,
        "searchApiHasKey": saved_search_has_key
            || legacy_search_api_key_from_settings(settings).is_some()
    })
}

fn build_credentials_status_value(state: &AppState, settings: &Value) -> Value {
    let endpoints = collect_provider_endpoints(settings);
    let mut accounts = Vec::new();
    let mut endpoint_accounts = Vec::new();
    for endpoint in &endpoints {
        let api_format = settings
            .get("providerConfigs")
            .and_then(|configs| configs.get(endpoint))
            .and_then(|config| config.get("apiFormat"))
            .and_then(Value::as_str)
            .map(|raw| normalize_provider_api_format(endpoint, Some(raw)))
            .unwrap_or_else(|| normalize_provider_api_format(endpoint, None));
        let aliases = provider_secret_key_aliases(endpoint, api_format);
        for alias in &aliases {
            if !accounts.contains(alias) {
                accounts.push(alias.clone());
            }
        }
        endpoint_accounts.push((endpoint.clone(), aliases));
    }
    accounts.push(SEARCH_SECRET_KEY.to_string());
    let values = state
        .credential_store
        .get_many(&accounts)
        .inspect_err(|err| {
            log::warn!(
                "Failed to read credential store in status builder ({} accounts): {err}",
                accounts.len()
            );
        })
        .unwrap_or_default();
    let mut provider_meta = HashMap::new();
    for (endpoint, aliases) in endpoint_accounts {
        let has_value = aliases.iter().any(|account| {
            values
                .get(account)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
        });
        provider_meta.insert(
            endpoint.clone(),
            has_value || legacy_provider_api_key_from_settings(settings, &endpoint).is_some(),
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
            build_credentials_status_from_meta(raw_settings),
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
) -> ProviderRouteConfig {
    let settings = read_settings(&state.settings_path());
    let provider_configs = settings
        .get("providerConfigs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
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
            provider_configs
                .iter()
                .next()
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

    let provider = provider_configs.get(&endpoint);
    let api_format = normalize_provider_api_format(
        &endpoint,
        provider
            .and_then(|config| config.get("apiFormat"))
            .and_then(Value::as_str),
    );
    let model = model_hint
        .unwrap_or("")
        .trim()
        .to_string()
        .if_empty_then(|| {
            provider
                .and_then(|config| config.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        });
    ProviderRouteConfig {
        endpoint,
        model,
        api_format,
    }
}

pub(crate) fn validate_default_provider_ready(state: &AppState) -> Result<ProviderRouteConfig, String> {
    let route = resolve_provider_endpoint_and_model(state, None, None);
    if route.model.trim().is_empty() {
        return Err(
            "Default provider model is empty. Please open Provider API and select or enter a model."
                .to_string(),
        );
    }
    Ok(route)
}

pub(crate) fn resolve_provider_api_key(state: &AppState, endpoint: &str) -> Result<String, String> {
    let route = resolve_provider_endpoint_and_model(state, Some(endpoint), None);
    for account in provider_secret_key_aliases(&route.endpoint, route.api_format) {
        if let Some(api_key) = state
            .credential_store
            .get(&account)?
            .filter(|value| !value.trim().is_empty())
        {
            return Ok(api_key);
        }
    }
    legacy_provider_api_key_from_settings(&read_settings(&state.settings_path()), &route.endpoint)
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
