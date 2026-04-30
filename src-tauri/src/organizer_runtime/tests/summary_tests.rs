use super::support::*;
use super::*;

#[test]
fn normalize_summary_strategy_defaults_to_filename_only() {
    assert_eq!(
        normalize_summary_mode(None),
        SUMMARY_MODE_FILENAME_ONLY.to_string()
    );
    assert_eq!(
        normalize_summary_mode(Some("local_summary")),
        SUMMARY_MODE_LOCAL_SUMMARY.to_string()
    );
    assert_eq!(
        normalize_summary_mode(Some("agent_summary")),
        SUMMARY_MODE_AGENT_SUMMARY.to_string()
    );
    assert_eq!(
        normalize_summary_mode(Some("bad-mode")),
        SUMMARY_MODE_FILENAME_ONLY.to_string()
    );
}

#[test]
fn relative_age_uses_compact_backend_format() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-04-06T12:00:00Z")
        .expect("parse now")
        .with_timezone(&chrono::Utc);

    assert_eq!(
        compute_relative_age_at(Some("2026-04-06T11:59:45Z"), now).as_deref(),
        Some("lt1m")
    );
    assert_eq!(
        compute_relative_age_at(Some("2026-04-06T09:00:00Z"), now).as_deref(),
        Some("3h")
    );
    assert_eq!(
        compute_relative_age_at(Some("2026-03-30T12:00:00Z"), now).as_deref(),
        Some("1w")
    );
    assert_eq!(
        compute_relative_age_at(Some("2025-12-06T12:00:00Z"), now).as_deref(),
        Some("4mo")
    );
    assert_eq!(
        compute_relative_age_at(Some("2024-04-06T12:00:00Z"), now).as_deref(),
        Some("2y")
    );
    assert_eq!(compute_relative_age_at(Some("bad-date"), now), None);
    assert_eq!(compute_relative_age_at(None, now), None);
}

#[test]
fn local_summary_skips_large_plain_text_inputs() {
    let root = temp_dir("large-text-summary");
    let path = root.join("notes.txt");
    write_text_file(&path, "small content");
    let mut unit = make_test_unit(&path);
    unit.size = LOCAL_SUMMARY_MAX_PLAIN_TEXT_BYTES + 1;
    let extracted = summary::extract_plain_text_summary(&unit);
    assert!(extracted.excerpt.is_empty());
    assert!(extracted
        .warnings
        .iter()
        .any(|warning| warning.starts_with("summary_input_too_large:")));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn local_summary_falls_back_to_filename_for_unsupported_file() {
    let root = temp_dir("filename-fallback");
    let path = root.join("archive.bin");
    write_text_file(&path, "not actually parsed");
    let mut unit = make_test_unit(&path);
    unit.modality = "binary".to_string();
    let stop = AtomicBool::new(false);
    let extracted = summary::extract_unit_content_for_summary(&unit, "zh-CN", &stop);
    let summary = summary::build_local_summary(&unit, &extracted);
    assert!(extracted.excerpt.is_empty());
    assert_eq!(summary.representation.source, SUMMARY_SOURCE_FILENAME_ONLY);
    assert!(summary.representation.degraded);
    assert_eq!(
        summary.representation.metadata.as_deref(),
        Some("archive.bin")
    );
    assert!(summary.representation.short.is_none());
    assert!(summary.representation.long.is_none());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn legacy_tika_defaults_are_upgraded_to_auto_start() {
    let config = extraction_tool_config_from_settings(&json!({
        "contentExtraction": {
            "tika": {
                "enabled": false,
                "autoStart": false,
                "url": DEFAULT_TIKA_URL,
                "jarPath": ""
            }
        }
    }));
    assert!(config.tika_enabled);
    assert!(config.tika_auto_start);
    assert!(!config.tika_ready);
}

#[test]
fn summary_modes_force_enable_tika_runtime() {
    let mut config = ExtractionToolConfig {
        tika_enabled: false,
        tika_url: String::new(),
        tika_auto_start: false,
        tika_jar_path: String::new(),
        tika_ready: false,
    };

    force_enable_tika_for_summary_mode(&mut config);

    assert!(config.tika_enabled);
    assert!(config.tika_auto_start);
    assert_eq!(config.tika_url, DEFAULT_TIKA_URL.to_string());
}

#[test]
fn local_summary_does_not_parse_pdf_binary_as_plain_text() {
    let root = temp_dir("pdf-fallback");
    let path = root.join("paper.pdf");
    write_text_file(&path, "%PDF-1.7\nstream\n\x00\x01");
    let mut unit = make_test_unit(&path);
    unit.modality = "text".to_string();
    let stop = AtomicBool::new(false);

    let extracted = summary::extract_unit_content_for_summary(&unit, "zh-CN", &stop);
    assert_eq!(extracted.parser, "unavailable");
    assert!(extracted.excerpt.is_empty());
    assert!(extracted
        .warnings
        .iter()
        .any(|warning| warning == "filename_only_fallback"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn extraction_profile_uses_weighted_budget_and_hard_gates() {
    let root = temp_dir("extraction-profile");
    let txt_path = root.join("notes.txt");
    let pdf_path = root.join("large.pdf");
    let xlsx_path = root.join("sheet.xlsx");
    write_text_file(&txt_path, "plain text");
    fs::write(&pdf_path, vec![b'x'; 9 * 1024 * 1024]).expect("write large pdf");
    write_file(&xlsx_path);
    let route = text_route("http://127.0.0.1:9/v1".to_string());
    let mut routes = HashMap::new();
    routes.insert("text".to_string(), route);
    let task = make_summary_test_runtime_with_extraction_tool(
        &root,
        routes,
        ExtractionToolConfig {
            tika_enabled: true,
            tika_ready: true,
            ..ExtractionToolConfig::default()
        },
    );

    let text_profile = extraction_profile(
        SUMMARY_MODE_LOCAL_SUMMARY,
        &make_test_unit(&txt_path),
        &task,
    );
    assert_eq!(text_profile.global_cost, 1);
    assert!(!text_profile.uses_tika);

    let pdf_profile = extraction_profile(
        SUMMARY_MODE_LOCAL_SUMMARY,
        &make_test_unit(&pdf_path),
        &task,
    );
    assert_eq!(pdf_profile.global_cost, 8);
    assert!(pdf_profile.uses_tika);
    assert!(pdf_profile.uses_heavy_doc);

    let xlsx_profile = extraction_profile(
        SUMMARY_MODE_LOCAL_SUMMARY,
        &make_test_unit(&xlsx_path),
        &task,
    );
    assert_eq!(xlsx_profile.global_cost, 16);
    assert!(xlsx_profile.uses_tika);
    assert!(xlsx_profile.uses_heavy_doc);

    let filename_profile = extraction_profile(
        SUMMARY_MODE_FILENAME_ONLY,
        &make_test_unit(&xlsx_path),
        &task,
    );
    assert_eq!(filename_profile.global_cost, 1);
    assert!(!filename_profile.uses_tika);
    assert!(!filename_profile.uses_heavy_doc);

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn weighted_summary_extraction_restores_original_unit_order() {
    let root = temp_dir("weighted-summary-order");
    let first = root.join("a.txt");
    let second = root.join("b.txt");
    let third = root.join("c.txt");
    write_text_file(&first, "first");
    write_text_file(&second, "second");
    write_text_file(&third, "third");
    let route = text_route("http://127.0.0.1:9/v1".to_string());
    let mut routes = HashMap::new();
    routes.insert("text".to_string(), route);
    let task = make_summary_test_runtime(&root, routes);

    let units = vec![
        make_test_unit(&first),
        make_test_unit(&second),
        make_test_unit(&third),
    ];
    let (prepared, stats) =
        prepare_summary_units_weighted(task, SUMMARY_MODE_LOCAL_SUMMARY, &units)
            .await
            .expect("prepare weighted summaries");

    assert_eq!(prepared.len(), 3);
    assert_eq!(prepared[0].unit.name, "a.txt");
    assert_eq!(prepared[1].unit.name, "b.txt");
    assert_eq!(prepared[2].unit.name, "c.txt");
    assert_eq!(stats.prepared_units, 3);
    assert_eq!(stats.text_units, 3);
    assert_eq!(stats.tika_units, 0);
    assert_eq!(stats.total_cost, 3);

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn prepare_summary_batch_local_summary_preserves_contract() {
    let root = temp_dir("summary-prefetch-local");
    let path = root.join("notes.txt");
    write_text_file(&path, "alpha beta gamma");
    let unit = make_test_unit(&path);
    let route = text_route("http://127.0.0.1:9/v1".to_string());
    let mut routes = HashMap::new();
    routes.insert("text".to_string(), route.clone());
    let task = make_summary_test_runtime(&root, routes);

    let prepared = prepare_summary_batch(
        task,
        route.clone(),
        SUMMARY_MODE_LOCAL_SUMMARY.to_string(),
        1,
        vec![unit],
    )
    .await
    .expect("prepare local summary batch");

    assert_eq!(prepared.batch_idx, 1);
    assert_eq!(prepared.summary_usage.total, 0);
    assert_eq!(prepared.batch_rows.len(), 1);
    let row = &prepared.batch_rows[0];
    assert_eq!(row.get("itemId").and_then(Value::as_str), Some("batch2_1"));
    assert_eq!(
        row.get("summaryStrategy").and_then(Value::as_str),
        Some(SUMMARY_MODE_LOCAL_SUMMARY)
    );
    assert_eq!(
        row.pointer("/representation/source")
            .and_then(Value::as_str),
        Some(SUMMARY_SOURCE_LOCAL_SUMMARY)
    );
    assert_eq!(
        row.pointer("/localExtraction/parser")
            .and_then(Value::as_str),
        Some("plain_text")
    );
    assert_eq!(
        row.get("provider").and_then(Value::as_str),
        Some(route.endpoint.as_str())
    );
    assert_eq!(row.get("model").and_then(Value::as_str), Some("test-model"));

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn prepare_summary_batch_agent_summary_keeps_usage_with_current_batch() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let addr = listener.local_addr().expect("server addr");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        let mut buffer = [0u8; 4096];
        let _ = stream.read(&mut buffer).expect("read request");
        let body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": r#"{
                        "items": [{
                            "itemId": "batch3_1",
                            "summaryShort": "short summary",
                            "summaryLong": "long summary",
                            "keywords": ["alpha"],
                            "confidence": "high",
                            "warnings": ["source_sparse"]
                        }]
                    }"#
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 11,
                "completion_tokens": 7,
                "total_tokens": 18
            }
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    });

    let root = temp_dir("summary-prefetch-agent");
    let path = root.join("notes.txt");
    write_text_file(&path, "alpha beta gamma");
    let unit = make_test_unit(&path);
    let route = text_route(format!("http://{addr}/v1"));
    let mut routes = HashMap::new();
    routes.insert("text".to_string(), route.clone());
    let task = make_summary_test_runtime(&root, routes);

    let prepared = prepare_summary_batch(
        task,
        route,
        SUMMARY_MODE_AGENT_SUMMARY.to_string(),
        2,
        vec![unit],
    )
    .await
    .expect("prepare agent summary batch");

    server.join().expect("server joined");
    assert_eq!(prepared.batch_idx, 2);
    assert_eq!(prepared.summary_usage.prompt, 11);
    assert_eq!(prepared.summary_usage.completion, 7);
    assert_eq!(prepared.summary_usage.total, 18);
    let row = &prepared.batch_rows[0];
    assert_eq!(row.get("itemId").and_then(Value::as_str), Some("batch3_1"));
    assert_eq!(
        row.pointer("/representation/source")
            .and_then(Value::as_str),
        Some(SUMMARY_SOURCE_AGENT_SUMMARY)
    );
    assert_eq!(
        row.pointer("/representation/long").and_then(Value::as_str),
        Some("long summary")
    );
    assert_eq!(
        row.pointer("/representation/confidence")
            .and_then(Value::as_str),
        Some("high")
    );
    assert!(row
        .get("summaryWarnings")
        .and_then(Value::as_array)
        .map(|warnings| {
            warnings
                .iter()
                .any(|value| value.as_str() == Some("source_sparse"))
        })
        .unwrap_or(false));

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn agent_summary_microbatch_splits_by_item_limit_and_preserves_order() {
    let response = summary_response_for_test_ids();
    let (endpoint, server) =
        start_mock_chat_server_sequence(vec![response.clone(), response.clone(), response]);
    let root = temp_dir("summary-agent-item-limit");
    let files = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]
        .into_iter()
        .map(|name| {
            let path = root.join(name);
            write_text_file(&path, name);
            path
        })
        .collect::<Vec<_>>();
    let units = files
        .iter()
        .map(|path| make_test_unit(path))
        .collect::<Vec<_>>();
    let route = text_route(endpoint);
    let mut routes = HashMap::new();
    routes.insert("text".to_string(), route.clone());
    let task = make_summary_test_runtime(&root, routes);
    let basic_rows = build_basic_batch_rows(&units, 2);

    let (prepared, _extraction, agent_stats, usage) = prepare_agent_summary_batches_microbatched(
        task,
        route,
        SUMMARY_MODE_AGENT_SUMMARY.to_string(),
        &units,
        2,
        &basic_rows,
        SummaryAgentBatchConfig {
            max_chars: usize::MAX,
            max_items: 2,
            flush_ms: 500,
            max_in_flight: 50,
        },
    )
    .await
    .expect("prepare agent summary microbatches");

    let requests = server.join().expect("server joined");
    assert_eq!(requests.len(), 3);
    assert!(requests
        .iter()
        .all(|request| summary_request_item_count(request) <= 2));
    assert_eq!(agent_stats.agent_batch_count, 3);
    assert_eq!(usage.total, 54);
    let item_ids = prepared
        .iter()
        .flat_map(|batch| batch.batch_rows.iter())
        .filter_map(|row| row.get("itemId").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(
        item_ids,
        vec!["batch1_1", "batch1_2", "batch2_1", "batch2_2", "batch3_1"]
    );
    assert!(prepared
        .iter()
        .flat_map(|batch| batch.batch_rows.iter())
        .all(|row| row
            .pointer("/representation/source")
            .and_then(Value::as_str)
            == Some(SUMMARY_SOURCE_AGENT_SUMMARY)));

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn agent_summary_microbatch_splits_by_char_budget() {
    let response = summary_response_for_test_ids();
    let (endpoint, server) =
        start_mock_chat_server_sequence(vec![response.clone(), response.clone(), response]);
    let root = temp_dir("summary-agent-char-budget");
    let files = ["a.txt", "b.txt", "c.txt"]
        .into_iter()
        .map(|name| {
            let path = root.join(name);
            write_text_file(&path, &"x".repeat(200));
            path
        })
        .collect::<Vec<_>>();
    let units = files
        .iter()
        .map(|path| make_test_unit(path))
        .collect::<Vec<_>>();
    let route = text_route(endpoint);
    let mut routes = HashMap::new();
    routes.insert("text".to_string(), route.clone());
    let task = make_summary_test_runtime(&root, routes);
    let basic_rows = build_basic_batch_rows(&units, 3);

    let (_prepared, _extraction, agent_stats, _usage) = prepare_agent_summary_batches_microbatched(
        task,
        route,
        SUMMARY_MODE_AGENT_SUMMARY.to_string(),
        &units,
        3,
        &basic_rows,
        SummaryAgentBatchConfig {
            max_chars: 1,
            max_items: 20,
            flush_ms: 500,
            max_in_flight: 50,
        },
    )
    .await
    .expect("prepare agent summary char batches");

    let requests = server.join().expect("server joined");
    assert_eq!(requests.len(), 3);
    assert_eq!(agent_stats.agent_batch_count, 3);
    assert!(requests
        .iter()
        .all(|request| summary_request_item_count(request) == 1));

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn agent_summary_microbatch_falls_back_per_failed_batch() {
    let bad_response = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "not json"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 11,
            "completion_tokens": 7,
            "total_tokens": 18
        }
    });
    let good_response = summary_response_for_test_ids();
    let (endpoint, server) = start_mock_chat_server_sequence(vec![bad_response, good_response]);
    let root = temp_dir("summary-agent-fallback");
    let files = ["a.txt", "b.txt", "c.txt"]
        .into_iter()
        .map(|name| {
            let path = root.join(name);
            write_text_file(&path, name);
            path
        })
        .collect::<Vec<_>>();
    let units = files
        .iter()
        .map(|path| make_test_unit(path))
        .collect::<Vec<_>>();
    let route = text_route(endpoint);
    let mut routes = HashMap::new();
    routes.insert("text".to_string(), route.clone());
    let task = make_summary_test_runtime(&root, routes);
    let basic_rows = build_basic_batch_rows(&units, 3);

    let (prepared, _extraction, agent_stats, _usage) = prepare_agent_summary_batches_microbatched(
        task,
        route,
        SUMMARY_MODE_AGENT_SUMMARY.to_string(),
        &units,
        3,
        &basic_rows,
        SummaryAgentBatchConfig {
            max_chars: usize::MAX,
            max_items: 2,
            flush_ms: 500,
            max_in_flight: 1,
        },
    )
    .await
    .expect("prepare agent summary fallback batches");

    let requests = server.join().expect("server joined");
    assert_eq!(requests.len(), 2);
    assert_eq!(agent_stats.failed_batches, 1);
    let rows = prepared
        .iter()
        .flat_map(|batch| batch.batch_rows.iter())
        .collect::<Vec<_>>();
    let degraded = rows
        .iter()
        .filter(|row| {
            row.get("summaryDegraded")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    assert_eq!(degraded, 2);
    assert_eq!(
        rows.iter()
            .filter(|row| row
                .pointer("/representation/source")
                .and_then(Value::as_str)
                == Some(SUMMARY_SOURCE_AGENT_FALLBACK_LOCAL))
            .count(),
        2
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row
                .pointer("/representation/source")
                .and_then(Value::as_str)
                == Some(SUMMARY_SOURCE_AGENT_SUMMARY))
            .count(),
        1
    );

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn agent_summary_microbatch_respects_in_flight_limit() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let (endpoint, server) =
        start_delayed_mock_chat_server(4, 150, active.clone(), max_active.clone());
    let root = temp_dir("summary-agent-in-flight");
    let files = ["a.txt", "b.txt", "c.txt", "d.txt"]
        .into_iter()
        .map(|name| {
            let path = root.join(name);
            write_text_file(&path, name);
            path
        })
        .collect::<Vec<_>>();
    let units = files
        .iter()
        .map(|path| make_test_unit(path))
        .collect::<Vec<_>>();
    let route = text_route(endpoint);
    let mut routes = HashMap::new();
    routes.insert("text".to_string(), route.clone());
    let task = make_summary_test_runtime(&root, routes);
    let basic_rows = build_basic_batch_rows(&units, 4);

    let (_prepared, _extraction, agent_stats, _usage) = prepare_agent_summary_batches_microbatched(
        task,
        route,
        SUMMARY_MODE_AGENT_SUMMARY.to_string(),
        &units,
        4,
        &basic_rows,
        SummaryAgentBatchConfig {
            max_chars: usize::MAX,
            max_items: 1,
            flush_ms: 500,
            max_in_flight: 2,
        },
    )
    .await
    .expect("prepare agent summary with in-flight limit");

    let requests = server.join().expect("server joined");
    assert_eq!(requests.len(), 4);
    assert_eq!(agent_stats.agent_batch_count, 4);
    assert!(max_active.load(std::sync::atomic::Ordering::SeqCst) <= 2);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn parse_summary_agent_output_reads_items() {
    let parsed = summary::parse_summary_agent_output(
        r#"{
            "items": [
                {
                    "itemId": "batch1_1",
                    "summaryShort": "预算表，包含负责人和金额。",
                    "summaryLong": "预算表，包含项目负责人、金额等预算信息。",
                    "keywords": ["预算", "项目", "金额"],
                    "confidence": "high",
                    "warnings": ["source_sparse"]
                }
            ]
        }"#,
    )
    .expect("parse summary agent output");
    let item = parsed.get("batch1_1").expect("item exists");
    assert_eq!(item.summary_short, "预算表，包含负责人和金额。");
    assert_eq!(
        item.summary_long,
        "预算表，包含项目负责人、金额等预算信息。"
    );
    assert_eq!(item.keywords, vec!["预算", "项目", "金额"]);
    assert_eq!(item.confidence.as_deref(), Some("high"));
    assert_eq!(item.warnings, vec!["source_sparse"]);
}

#[test]
fn classification_batch_items_exclude_raw_extraction_fields() {
    let items = summary::build_classification_batch_items(&[json!({
        "itemId": "batch1_1",
        "name": "report.pdf",
        "path": "E:\\docs\\report.pdf",
        "relativePath": "docs\\report.pdf",
        "size": 1234,
        "createdAt": "2026-04-01T00:00:00Z",
        "modifiedAt": "2026-04-05T00:00:00Z",
        "itemType": "file",
        "modality": "text",
        "representation": {
            "metadata": "report.pdf，document，1.2 KB",
            "short": "季度财务报告",
            "long": "季度财务报告，包含季度财务指标与结论。",
            "source": "agent_summary",
            "degraded": false,
            "confidence": "high",
            "keywords": ["财务", "季度"]
        },
        "summaryKeywords": ["财务", "季度"],
        "summaryWarnings": ["source_sparse"],
        "localExtraction": {
            "parser": "tika",
            "excerpt": "very long raw extraction text"
        }
    })]);

    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(
        item.get("evidence").and_then(Value::as_str),
        Some("季度财务报告，包含季度财务指标与结论。")
    );
    assert_eq!(
        item.get("keywords").and_then(Value::as_array).map(Vec::len),
        Some(2),
    );
    assert!(item.get("summaryText").is_none());
    assert!(item.get("representation").is_none());
    assert!(item.get("createdAge").is_some());
    assert!(item.get("modifiedAge").is_some());
    assert!(item.get("localExtraction").is_none());
    assert!(item.get("path").is_none());
    assert!(item.get("size").is_none());
    assert!(item.get("createdAt").is_none());
    assert!(item.get("modifiedAt").is_none());
}

#[test]
fn classification_file_index_only_includes_paths_for_duplicate_names() {
    let file_index = summary::build_classification_file_index(&[
        json!({
            "itemId": "batch1_1",
            "name": "report.pdf",
            "relativePath": "finance\\report.pdf",
            "itemType": "file",
            "modality": "text",
            "representation": { "metadata": "should not be copied" }
        }),
        json!({
            "itemId": "batch1_2",
            "name": "report.pdf",
            "relativePath": "sales\\report.pdf",
            "itemType": "file",
            "modality": "text"
        }),
        json!({
            "itemId": "batch1_3",
            "name": "cover.png",
            "relativePath": "images\\cover.png",
            "itemType": "file",
            "modality": "image"
        }),
    ]);

    assert_eq!(file_index.len(), 3);
    assert_eq!(
        file_index[0]["relativePath"],
        Value::from("finance\\report.pdf")
    );
    assert_eq!(
        file_index[1]["relativePath"],
        Value::from("sales\\report.pdf")
    );
    assert!(file_index[2].get("relativePath").is_none());
    for row in file_index {
        assert!(row.get("itemType").is_none());
        assert!(row.get("modality").is_none());
        assert!(row.get("representation").is_none());
        assert!(row.get("summaryText").is_none());
    }
}
