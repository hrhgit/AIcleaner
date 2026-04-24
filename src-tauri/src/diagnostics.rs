use crate::backend::AppState;
use serde_json::{json, Map, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};
use uuid::Uuid;

const REDACTED: &str = "[REDACTED]";

pub fn new_operation_id() -> String {
    format!("op_{}", Uuid::new_v4().simple())
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
    normalized.contains("apikey")
        || normalized.contains("authorization")
        || normalized == "apikey"
        || normalized == "key"
        || normalized.contains("token")
        || normalized.contains("password")
        || normalized.contains("secret")
        || normalized.contains("credential")
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
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let logs_dir = data_dir.join("logs");
    let path = logs_dir.join(format!("aicleaner-diagnostics-{date}.jsonl"));
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
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(|text| Value::String(text.to_string()))
        .unwrap_or(Value::Null)
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
            "items": [
                { "password": "pw" },
                { "path": "C:\\tmp\\file.txt" }
            ]
        }));

        assert_eq!(redacted["apiKey"], REDACTED);
        assert_eq!(redacted["headers"]["Authorization"], REDACTED);
        assert_eq!(redacted["headers"]["x-api-key"], REDACTED);
        assert_eq!(redacted["headers"]["normal"], "kept");
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
