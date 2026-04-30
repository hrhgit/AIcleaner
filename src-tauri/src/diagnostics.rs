use crate::backend::AppState;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

const REDACTED: &str = "[REDACTED]";
static LOG_FILE_NAMES: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn new_operation_id() -> String {
    format!("op_{}_{}", sortable_utc_stamp(), Uuid::new_v4().simple())
}

pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut redacted = Map::new();
            for (key, value) in map {
                if is_sensitive_key(key) {
                    redacted.insert(key.clone(), Value::String(REDACTED.to_string()));
                } else {
                    redacted.insert(key.clone(), redact_value(value));
                }
            }
            Value::Object(redacted)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        _ => value.clone(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    if is_usage_metric_key(&normalized) {
        return false;
    }
    normalized.contains("apikey")
        || normalized.contains("authorization")
        || normalized == "apikey"
        || normalized == "key"
        || normalized.contains("token")
        || normalized.contains("password")
        || normalized.contains("secret")
        || normalized.contains("credential")
}

fn is_usage_metric_key(normalized: &str) -> bool {
    matches!(
        normalized,
        "usage"
            | "tokenusage"
            | "tokenusagebystage"
            | "summaryusage"
            | "prompt"
            | "completion"
            | "total"
    )
}

pub fn record_event(
    data_dir: &Path,
    level: &str,
    module: &str,
    event: &str,
    operation_id: Option<&str>,
    message: &str,
    details: Value,
    error: Option<Value>,
    duration: Option<Duration>,
) {
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let logs_dir = data_dir.join("logs");
    let path = logs_dir.join(diagnostic_log_file_name(operation_id, event, &details));
    let line = json!({
        "timestamp": timestamp,
        "level": level,
        "event": event,
        "module": module,
        "operationId": operation_id.unwrap_or(""),
        "taskId": extract_text(&details, "taskId"),
        "sessionId": extract_text(&details, "sessionId"),
        "jobId": extract_text(&details, "jobId"),
        "message": message,
        "details": redact_value(&details),
        "error": error.map(|value| redact_value(&value)).unwrap_or(Value::Null),
        "durationMs": duration.map(|value| value.as_millis() as u64),
    });

    if let Err(err) = fs::create_dir_all(&logs_dir)
        .and_then(|_| OpenOptions::new().create(true).append(true).open(&path))
        .and_then(|mut file| {
            let encoded = serde_json::to_string(&line)
                .unwrap_or_else(|_| "{\"error\":\"diagnostic_encode_failed\"}".to_string());
            writeln!(file, "{encoded}")
        })
    {
        log::error!(
            "failed to write diagnostic log {}: {}",
            path.to_string_lossy(),
            err
        );
    }
}

fn diagnostic_log_file_name(operation_id: Option<&str>, event: &str, details: &Value) -> String {
    let task_id = extract_string(details, "taskId");
    let session_id = extract_string(details, "sessionId");
    let job_id = extract_string(details, "jobId");
    let correlation = task_id
        .as_deref()
        .map(|value| ("task", value))
        .or_else(|| session_id.as_deref().map(|value| ("session", value)))
        .or_else(|| job_id.as_deref().map(|value| ("job", value)))
        .or_else(|| operation_id.map(|value| ("operation", value)));

    let Some((kind, value)) = correlation else {
        return format!(
            "aicleaner-diagnostics-{}-{}.jsonl",
            sortable_utc_stamp(),
            sanitize_log_component(event, "event")
        );
    };

    let key = format!("{kind}:{value}");
    if let Ok(mut names) = LOG_FILE_NAMES.lock() {
        return names
            .entry(key)
            .or_insert_with(|| {
                format!(
                    "aicleaner-diagnostics-{}-{}.jsonl",
                    sortable_utc_stamp(),
                    sanitize_log_component(value, kind)
                )
            })
            .clone();
    }

    format!(
        "aicleaner-diagnostics-{}-{}.jsonl",
        sortable_utc_stamp(),
        sanitize_log_component(value, kind)
    )
}

fn sortable_utc_stamp() -> String {
    let now = chrono::Utc::now();
    format!(
        "{}-{:03}Z",
        now.format("%Y%m%d-%H%M%S"),
        now.timestamp_subsec_millis()
    )
}

fn sanitize_log_component(value: &str, fallback: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .chars()
        .take(96)
        .collect::<String>();
    if sanitized.is_empty() {
        fallback.to_string()
    } else {
        sanitized
    }
}

pub fn record_state_event(
    state: &AppState,
    level: &str,
    module: &str,
    event: &str,
    operation_id: Option<&str>,
    message: &str,
    details: Value,
    error: Option<Value>,
    duration: Option<Duration>,
) {
    record_event(
        &state.data_dir(),
        level,
        module,
        event,
        operation_id,
        message,
        details,
        error,
        duration,
    );
}

pub fn command_start(
    state: &AppState,
    module: &str,
    event: &str,
    operation_id: &str,
    details: Value,
) {
    record_state_event(
        state,
        "info",
        module,
        event,
        Some(operation_id),
        "operation started",
        details,
        None,
        None,
    );
}

pub fn command_finish(
    state: &AppState,
    module: &str,
    event: &str,
    operation_id: &str,
    started_at: Instant,
    result: &Result<Value, String>,
    details: Value,
) {
    match result {
        Ok(value) => record_state_event(
            state,
            "info",
            module,
            event,
            Some(operation_id),
            "operation succeeded",
            merge_details(details, json!({ "result": value })),
            None,
            Some(started_at.elapsed()),
        ),
        Err(err) => record_state_event(
            state,
            "error",
            module,
            event,
            Some(operation_id),
            "operation failed",
            details,
            Some(json!({ "message": err })),
            Some(started_at.elapsed()),
        ),
    }
}

pub fn merge_details(base: Value, extra: Value) -> Value {
    let mut merged = match base {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        other => {
            let mut map = Map::new();
            map.insert("value".to_string(), other);
            map
        }
    };
    if let Value::Object(extra_map) = extra {
        for (key, value) in extra_map {
            merged.insert(key, value);
        }
    }
    Value::Object(merged)
}

fn extract_text(value: &Value, key: &str) -> Value {
    extract_string(value, key)
        .map(Value::String)
        .unwrap_or(Value::Null)
}

fn extract_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn redacts_nested_sensitive_fields() {
        let redacted = redact_value(&json!({
            "apiKey": "abc",
            "headers": {
                "Authorization": "Bearer abc",
                "x-api-key": "abc",
                "normal": "kept"
            },
            "tokenUsage": {
                "prompt": 10,
                "completion": 5,
                "total": 15
            },
            "items": [
                { "password": "pw" },
                { "path": "C:\\tmp\\file.txt" }
            ]
        }));

        assert_eq!(redacted["apiKey"], REDACTED);
        assert_eq!(redacted["headers"]["Authorization"], REDACTED);
        assert_eq!(redacted["headers"]["x-api-key"], REDACTED);
        assert_eq!(redacted["headers"]["normal"], "kept");
        assert_eq!(redacted["tokenUsage"]["prompt"], 10);
        assert_eq!(redacted["tokenUsage"]["completion"], 5);
        assert_eq!(redacted["tokenUsage"]["total"], 15);
        assert_eq!(redacted["items"][0]["password"], REDACTED);
        assert_eq!(redacted["items"][1]["path"], "C:\\tmp\\file.txt");
    }

    #[test]
    fn writes_jsonl_record_without_leaking_secret_values() {
        let dir = std::env::temp_dir().join(format!("aicleaner-diagnostics-{}", Uuid::new_v4()));
        record_event(
            &dir,
            "info",
            "test",
            "unit_test",
            Some("op_test"),
            "test message",
            json!({
                "taskId": "task_1",
                "apiKey": "secret",
                "path": "C:\\tmp\\file.txt"
            }),
            None,
            Some(Duration::from_millis(12)),
        );

        let entries = fs::read_dir(dir.join("logs"))
            .expect("logs dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("log files");
        assert_eq!(entries.len(), 1);
        let raw = fs::read_to_string(entries[0].path()).expect("read log");
        assert!(raw.contains("\"operationId\":\"op_test\""));
        assert!(raw.contains(REDACTED));
        assert!(!raw.contains("secret"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn writes_separate_files_per_task_with_sortable_names() {
        let dir = std::env::temp_dir().join(format!("aicleaner-diagnostics-{}", Uuid::new_v4()));
        let task_a = format!("org_{}", Uuid::new_v4().simple());
        let task_b = format!("org_{}", Uuid::new_v4().simple());

        record_event(
            &dir,
            "info",
            "organizer",
            "first",
            Some("op_first"),
            "first task event",
            json!({ "taskId": task_a }),
            None,
            None,
        );
        record_event(
            &dir,
            "info",
            "organizer",
            "second",
            Some("op_second"),
            "same task event",
            json!({ "taskId": task_a }),
            None,
            None,
        );
        record_event(
            &dir,
            "info",
            "organizer",
            "third",
            Some("op_third"),
            "other task event",
            json!({ "taskId": task_b }),
            None,
            None,
        );

        let mut entries = fs::read_dir(dir.join("logs"))
            .expect("logs dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("log files");
        entries.sort_by_key(|entry| entry.file_name());
        assert_eq!(entries.len(), 2);
        for entry in &entries {
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(name.starts_with("aicleaner-diagnostics-20"));
            assert!(name.ends_with(".jsonl"));
        }
        let first_raw = fs::read_to_string(entries[0].path()).expect("read first task log");
        let second_raw = fs::read_to_string(entries[1].path()).expect("read second task log");
        assert_eq!(first_raw.lines().count() + second_raw.lines().count(), 3);
        assert!(
            first_raw.lines().count() == 2 || second_raw.lines().count() == 2,
            "one task log should contain both events for the same task"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn write_failure_does_not_panic() {
        let file_path =
            std::env::temp_dir().join(format!("aicleaner-diagnostics-file-{}", Uuid::new_v4()));
        fs::write(&file_path, b"not a directory").expect("seed blocking file");

        record_event(
            &file_path,
            "info",
            "test",
            "write_failure",
            Some("op_test"),
            "test message",
            json!({ "path": "C:\\tmp\\file.txt" }),
            None,
            None,
        );

        let _ = fs::remove_file(file_path);
    }
}
