fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn system_time_to_iso(value: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(value)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn format_relative_age(duration: chrono::Duration) -> String {
    let seconds = duration.num_seconds().max(0);
    if seconds < 60 {
        "lt1m".to_string()
    } else if seconds < 60 * 60 {
        format!("{}m", (seconds / 60).max(1))
    } else if seconds < 60 * 60 * 24 {
        format!("{}h", (seconds / (60 * 60)).max(1))
    } else if seconds < 60 * 60 * 24 * 7 {
        format!("{}d", (seconds / (60 * 60 * 24)).max(1))
    } else if seconds < 60 * 60 * 24 * 30 {
        format!("{}w", (seconds / (60 * 60 * 24 * 7)).max(1))
    } else if seconds < 60 * 60 * 24 * 365 {
        format!("{}mo", (seconds / (60 * 60 * 24 * 30)).max(1))
    } else {
        format!("{}y", (seconds / (60 * 60 * 24 * 365)).max(1))
    }
}

fn compute_relative_age_at(
    value: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    let parsed = value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|value| value.with_timezone(&chrono::Utc))?;
    Some(format_relative_age(now.signed_duration_since(parsed)))
}

fn compute_relative_age(value: Option<&str>) -> Option<String> {
    compute_relative_age_at(value, chrono::Utc::now())
}

fn is_zh_language(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized == "zh" || normalized.starts_with("zh-") || normalized.starts_with("zh_")
}

fn localized_language_name(prompt_language: &str, output_language: &str) -> &'static str {
    if is_zh_language(prompt_language) {
        if is_zh_language(output_language) {
            "简体中文"
        } else {
            "英文"
        }
    } else if is_zh_language(output_language) {
        "Simplified Chinese"
    } else {
        "English"
    }
}

fn organizer_unknown_label(value: &str) -> &'static str {
    if is_zh_language(value) {
        "锛堟湭鐭ワ級"
    } else {
        "(unknown)"
    }
}

fn organizer_none_label(value: &str) -> &'static str {
    if is_zh_language(value) {
        "（无）"
    } else {
        "(none)"
    }
}

fn normalize_summary_mode(value: Option<&str>) -> String {
    match value.unwrap_or("").trim() {
        SUMMARY_MODE_LOCAL_SUMMARY => SUMMARY_MODE_LOCAL_SUMMARY.to_string(),
        SUMMARY_MODE_AGENT_SUMMARY => SUMMARY_MODE_AGENT_SUMMARY.to_string(),
        _ => SUMMARY_MODE_FILENAME_ONLY.to_string(),
    }
}

