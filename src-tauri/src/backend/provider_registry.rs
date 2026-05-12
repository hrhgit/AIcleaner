use crate::llm_protocol::ApiFormat;
use reqwest::Url;
use serde_json::Value;

pub(crate) fn provider_secret_key(endpoint: &str) -> String {
    format!("provider:{}:apiKey", endpoint.trim())
}

pub(crate) fn preset_provider_configs_json() -> Value {
    Value::Object(serde_json::Map::new())
}

pub(crate) fn normalize_provider_api_format(
    endpoint: &str,
    raw: Option<&str>,
) -> ApiFormat {
    raw.and_then(ApiFormat::from_str)
        .unwrap_or_else(|| crate::llm_protocol::detect_api_format(endpoint))
}

pub(crate) fn normalize_provider_thinking_level(raw: Option<&str>) -> &'static str {
    match raw.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "low" => "low",
        "high" => "high",
        _ => "medium",
    }
}

pub(crate) fn normalize_provider_endpoint(
    endpoint: &str,
    api_format: ApiFormat,
) -> Result<String, String> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return Err("Provider endpoint is required.".to_string());
    }

    let mut url = Url::parse(trimmed).map_err(|err| format!("Invalid provider endpoint: {err}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Provider endpoint must start with http:// or https://".to_string());
    }

    let mut segments = url
        .path_segments()
        .map(|parts| {
            parts
                .filter(|segment| !segment.trim().is_empty())
                .map(|segment| segment.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    match api_format {
        ApiFormat::OpenAi => {
            if matches!(
                segments.as_slice(),
                [.., chat, completions] if chat == "chat" && completions == "completions"
            ) {
                segments.truncate(segments.len().saturating_sub(2));
            }
        }
        ApiFormat::Anthropic => {
            if segments.last().is_some_and(|segment| segment == "messages") {
                segments.pop();
            }
            if !segments.last().is_some_and(|segment| segment == "v1") {
                segments.push("v1".to_string());
            }
        }
    }

    match url.path_segments_mut() {
        Ok(mut path) => {
            path.clear();
            for segment in &segments {
                path.push(segment);
            }
        }
        Err(_) => return Err("Provider endpoint path cannot be normalized.".to_string()),
    }

    url.set_query(None);
    url.set_fragment(None);
    let normalized = url.to_string().trim_end_matches('/').to_string();
    if normalized.is_empty() {
        return Err("Provider endpoint is required.".to_string());
    }
    Ok(normalized)
}

pub(crate) fn provider_secret_key_aliases(endpoint: &str, api_format: ApiFormat) -> Vec<String> {
    let normalized = endpoint.trim().trim_end_matches('/').to_string();
    let mut aliases = Vec::new();
    let mut push_alias = |candidate: String| {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            return;
        }
        let key = provider_secret_key(trimmed);
        if !aliases.contains(&key) {
            aliases.push(key);
        }
    };

    push_alias(normalized.clone());
    push_alias(format!("{}/", normalized));

    match api_format {
        ApiFormat::OpenAi => {
            push_alias(format!("{}/chat/completions", normalized));
        }
        ApiFormat::Anthropic => {
            push_alias(format!("{}/messages", normalized));
            if normalized.ends_with("/v1") {
                let base = normalized.trim_end_matches("/v1").to_string();
                if !base.is_empty() {
                    push_alias(base.clone());
                    push_alias(format!("{base}/messages"));
                }
            } else {
                push_alias(format!("{}/v1", normalized));
                push_alias(format!("{}/v1/messages", normalized));
            }
        }
    }

    aliases
}
