#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::env;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;
    use uuid::Uuid;

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wipeout-organizer-{name}-{}", Uuid::new_v4()))
    }

    fn write_file(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, b"test").expect("write file");
    }

    fn write_text_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, content.as_bytes()).expect("write text file");
    }

    fn assess_directory(path: &Path, stop: &AtomicBool, prefer_whole: bool) -> DirectoryAssessment {
        let excluded = normalize_excluded(None);
        let mut report = CollectionReport::default();
        evaluate_directory_assessment(path, &excluded, stop, prefer_whole, &mut report)
            .expect("assessment exists")
    }

    fn make_test_unit(path: &Path) -> OrganizeUnit {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string();
        OrganizeUnit {
            name,
            path: path.to_string_lossy().to_string(),
            relative_path: path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_string(),
            size: fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
            created_at: None,
            modified_at: None,
            item_type: "file".to_string(),
            modality: "text".to_string(),
            directory_assessment: None,
        }
    }

    fn make_summary_test_runtime(
        root: &Path,
        routes: HashMap<String, RouteConfig>,
    ) -> Arc<OrganizeTaskRuntime> {
        Arc::new(OrganizeTaskRuntime {
            stop: AtomicBool::new(false),
            snapshot: Mutex::new(OrganizeSnapshot {
                id: "summary_test_task".to_string(),
                status: "idle".to_string(),
                error: None,
                root_path: root.to_string_lossy().to_string(),
                recursive: true,
                excluded_patterns: Vec::new(),
                batch_size: 2,
                summary_strategy: SUMMARY_MODE_LOCAL_SUMMARY.to_string(),
                use_web_search: false,
                web_search_enabled: false,
                selected_model: "test-model".to_string(),
                selected_models: json!({}),
                selected_providers: json!({}),
                supports_multimodal: false,
                tree: json!({}),
                tree_version: 0,
                initial_tree: Value::Null,
                base_tree_version: 0,
                batch_outputs: Vec::new(),
                tree_proposals: Vec::new(),
                draft_tree: Value::Null,
                proposal_mappings: Vec::new(),
                review_issues: Vec::new(),
                final_tree: Value::Null,
                final_assignments: Vec::new(),
                classification_errors: Vec::new(),
                processed_files: 0,
                total_files: 0,
                processed_batches: 0,
                total_batches: 0,
                progress: organize_progress(
                    "idle",
                    "Idle",
                    Some("Waiting to start organize task.".to_string()),
                    None,
                    None,
                    None,
                    true,
                ),
                token_usage: TokenUsage::default(),
                results: Vec::new(),
                preview: Vec::new(),
                created_at: "2026-04-29T00:00:00Z".to_string(),
                completed_at: None,
                job_id: None,
            }),
            routes,
            search_api_key: String::new(),
            response_language: "zh-CN".to_string(),
            extraction_tool: ExtractionToolConfig::default(),
            diagnostics: OrganizerDiagnostics {
                data_dir: root.to_path_buf(),
                operation_id: "summary_test_operation".to_string(),
                task_id: "summary_test_task".to_string(),
            },
            job: Mutex::new(None),
        })
    }

    fn text_route(endpoint: String) -> RouteConfig {
        RouteConfig {
            endpoint,
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
        }
    }

    fn read_mock_http_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = stream.read(&mut chunk).expect("read request");
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if buffer.len() >= header_end + 4 + content_length {
                break;
            }
        }
        String::from_utf8_lossy(&buffer).into_owned()
    }

    fn start_mock_chat_server(response_body: Value) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock chat server");
        let addr = listener.local_addr().expect("mock chat server addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let request = read_mock_http_request(&mut stream);
            let body = response_body.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            request
        });
        (format!("http://{addr}/v1"), handle)
    }

    fn start_mock_chat_server_sequence(
        response_bodies: Vec<Value>,
    ) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock chat server");
        let addr = listener.local_addr().expect("mock chat server addr");
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for response_body in response_bodies {
                let (mut stream, _) = listener.accept().expect("accept request");
                let request = read_mock_http_request(&mut stream);
                let body = response_body.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
                requests.push(request);
            }
            requests
        });
        (format!("http://{addr}/v1"), handle)
    }

    fn request_json_body(request: &str) -> Value {
        let (_, body) = request.split_once("\r\n\r\n").expect("request body");
        serde_json::from_str(body).expect("parse request json")
    }

    fn required_env(name: &str) -> String {
        env::var(name).unwrap_or_else(|_| panic!("{name} must be set"))
    }

    #[test]
    fn ensure_path_creates_nested_tree() {
        let mut tree = default_tree();
        let leaf = ensure_path(&mut tree, &["group".to_string(), "leaf".to_string()]);
        let path = category_path_for_id(&tree, &leaf).expect("path");
        assert_eq!(path, vec!["group".to_string(), "leaf".to_string()]);
    }

    #[test]
    fn organize_progress_contract_carries_stage_and_batch_counts() {
        let mut progress = organize_progress(
            "summary",
            "Preparing summaries",
            Some("Prepared summary batch 2 of 5.".to_string()),
            Some(2),
            Some(5),
            Some("batches"),
            false,
        );
        assert_eq!(progress.stage, "summary");
        assert_eq!(progress.current, Some(2));
        assert_eq!(progress.total, Some(5));
        assert_eq!(progress.unit.as_deref(), Some("batches"));
        assert!(!progress.indeterminate);

        progress = organize_progress(
            "error",
            "Error",
            Some("classification response missing assignments for 1 item(s)".to_string()),
            None,
            None,
            None,
            true,
        );
        assert_eq!(progress.stage, "error");
        assert!(progress
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("classification response missing assignments"));
        assert!(progress.indeterminate);
    }

    #[test]
    fn build_preview_uses_nested_category_path() {
        let preview = planner::build_preview(
            r"C:\root",
            &[json!({
                "name": "foo.txt",
                "path": r"C:\root\foo.txt",
                "itemType": "file",
                "leafNodeId": "leaf",
                "categoryPath": ["group", "leaf"]
            })],
        );
        assert_eq!(
            preview[0].get("targetPath").and_then(Value::as_str),
            Some(r"C:\root\group\leaf\foo.txt")
        );
    }

    #[test]
    fn build_preview_skips_classification_error_rows() {
        let preview = planner::build_preview(
            r"C:\root",
            &[
                json!({
                    "name": "bad.txt",
                    "path": r"C:\root\bad.txt",
                    "itemType": "file",
                    "reason": RESULT_REASON_CLASSIFICATION_ERROR,
                    "category": CATEGORY_CLASSIFICATION_ERROR
                }),
                json!({
                    "name": "good.txt",
                    "path": r"C:\root\good.txt",
                    "itemType": "file",
                    "leafNodeId": "leaf",
                    "categoryPath": ["group", "leaf"]
                }),
            ],
        );
        assert_eq!(preview.len(), 1);
        assert_eq!(
            preview[0].get("sourcePath").and_then(Value::as_str),
            Some(r"C:\root\good.txt")
        );
    }

    #[test]
    fn build_apply_plan_skips_classification_error_rows_even_if_preview_is_stale() {
        let snapshot = OrganizeSnapshot {
            id: "task_1".to_string(),
            status: "completed".to_string(),
            error: None,
            root_path: r"C:\root".to_string(),
            recursive: true,
            excluded_patterns: Vec::new(),
            batch_size: 20,
            summary_strategy: SUMMARY_MODE_FILENAME_ONLY.to_string(),
            use_web_search: false,
            web_search_enabled: false,
            selected_model: "deepseek-chat".to_string(),
            selected_models: json!({}),
            selected_providers: json!({}),
            supports_multimodal: false,
            tree: json!({}),
            tree_version: 0,
            initial_tree: Value::Null,
            base_tree_version: 0,
            batch_outputs: Vec::new(),
            tree_proposals: Vec::new(),
            draft_tree: Value::Null,
            proposal_mappings: Vec::new(),
            review_issues: Vec::new(),
            final_tree: Value::Null,
            final_assignments: Vec::new(),
            classification_errors: Vec::new(),
            processed_files: 1,
            total_files: 1,
            processed_batches: 1,
            total_batches: 1,
            progress: organize_progress(
                "completed",
                "Completed",
                Some("Organize results are ready.".to_string()),
                Some(1),
                Some(1),
                Some("batches"),
                false,
            ),
            token_usage: TokenUsage::default(),
            results: vec![json!({
                "name": "bad.txt",
                "path": r"C:\root\bad.txt",
                "itemType": "file",
                "reason": RESULT_REASON_CLASSIFICATION_ERROR,
                "category": CATEGORY_CLASSIFICATION_ERROR
            })],
            preview: vec![json!({
                "sourcePath": r"C:\root\bad.txt",
                "category": CATEGORY_OTHER_PENDING,
                "categoryPath": ["鍏朵粬寰呭畾"],
                "leafNodeId": "leaf",
                "targetPath": r"C:\root\鍏朵粬寰呭畾\bad.txt",
                "itemType": "file"
            })],
            created_at: "2026-03-28T00:00:00Z".to_string(),
            completed_at: None,
            job_id: None,
        };
        let plan = planner::build_apply_plan(&snapshot);
        assert!(plan.is_empty());
    }

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
            item.get("keywords")
                .and_then(Value::as_array)
                .map(Vec::len),
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
        assert_eq!(file_index[0]["relativePath"], Value::from("finance\\report.pdf"));
        assert_eq!(file_index[1]["relativePath"], Value::from("sales\\report.pdf"));
        assert!(file_index[2].get("relativePath").is_none());
        for row in file_index {
            assert!(row.get("itemType").is_none());
            assert!(row.get("modality").is_none());
            assert!(row.get("representation").is_none());
            assert!(row.get("summaryText").is_none());
        }
    }

    #[tokio::test]
    async fn classification_batch_submits_assignments_and_preserves_reduced_payload() {
        let mut tree = default_tree();
        let report_leaf = ensure_path(&mut tree, &["Documents".to_string(), "Reports".to_string()]);
        let batch_rows = vec![
            json!({
                "itemId": "batch1_1",
                "name": "quarterly-report.pdf",
                "path": "E:\\raw\\quarterly-report.pdf",
                "relativePath": "raw\\quarterly-report.pdf",
                "size": 4096,
                "createdAt": "2026-04-01T00:00:00Z",
                "modifiedAt": "2026-04-05T00:00:00Z",
                "itemType": "file",
                "modality": "text",
                "representation": {
                    "metadata": "quarterly-report.pdf",
                    "short": "Quarterly finance report",
                    "long": "Quarterly finance report with budget and revenue notes.",
                    "source": "agent_summary",
                    "degraded": false,
                    "confidence": "high",
                    "keywords": ["finance", "quarterly"]
                },
                "summaryWarnings": ["source_sparse"],
                "localExtraction": {
                    "parser": "tika",
                    "excerpt": "raw extracted body should never reach classification"
                }
            }),
            json!({
                "itemId": "batch1_2",
                "name": "meeting-audio.wav",
                "path": "E:\\raw\\meeting-audio.wav",
                "relativePath": "raw\\meeting-audio.wav",
                "size": 2048,
                "createdAt": "2026-04-02T00:00:00Z",
                "modifiedAt": "2026-04-06T00:00:00Z",
                "itemType": "file",
                "modality": "audio",
                "representation": {
                    "metadata": "meeting-audio.wav",
                    "short": "Meeting audio recording",
                    "long": "Meeting audio recording from the product review.",
                    "source": "local_summary",
                    "degraded": false,
                    "confidence": "medium",
                    "keywords": ["meeting", "audio"]
                },
                "summaryWarnings": [],
                "localExtraction": {
                    "parser": "audio_probe",
                    "excerpt": "raw audio metadata should never reach classification"
                }
            }),
        ];
        let category_inventory = vec![json!({
            "nodeId": report_leaf.clone(),
            "path": ["Documents", "Reports"],
            "count": 1,
            "files": ["old-report.pdf"],
            "truncated": false
        })];
        let submitted = json!({
            "baseTreeVersion": 7,
            "assignments": [{
                "itemId": "batch1_1",
                "leafNodeId": "n2",
                "reason": "financial report"
            }],
            "treeProposals": [{
                "proposalId": "p_audio",
                "suggestedPath": ["Media", "Audio"]
            }],
            "deferredAssignments": [{
                "itemId": "batch1_2",
                "proposalId": "p_audio",
                "reason": "audio recording"
            }]
        });
        let response_body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_submit",
                        "type": "function",
                        "function": {
                            "name": "submit_classification_batch",
                            "arguments": submitted.to_string()
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 31,
                "completion_tokens": 17,
                "total_tokens": 48
            }
        });
        let (endpoint, server) = start_mock_chat_server(response_body);
        let output = summary::classify_organize_batch(
            &text_route(endpoint),
            "en-US",
            &AtomicBool::new(false),
            &tree,
            7,
            &batch_rows,
            &category_inventory,
            None,
            false,
            "",
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            Arc::new(tokio::sync::Semaphore::new(1)),
            None,
            "classification_batch_test",
        )
        .await
        .expect("classify batch");

        assert!(output.error.is_none());
        assert_eq!(output.usage.total, 48);
        assert!(output.raw_output.contains("submit_classification_batch"));
        let parsed = output.parsed.expect("parsed classification output");
        assert_eq!(
            parsed.get("baseTreeVersion").and_then(Value::as_u64),
            Some(7)
        );
        assert_eq!(
            parsed
                .get("assignments")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            parsed
                .get("treeProposals")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            parsed
                .get("deferredAssignments")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );

        let request = server.join().expect("mock server joined");
        let request_body = request_json_body(&request);
        let messages = request_body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");
        let user_payload: Value = serde_json::from_str(
            messages[1]
                .get("content")
                .and_then(Value::as_str)
                .expect("user payload content"),
        )
        .expect("parse classification payload");
        assert_eq!(user_payload["baseTreeVersion"], Value::from(7));
        assert_eq!(
            user_payload
                .pointer("/categoryInventory/0/nodeId")
                .and_then(Value::as_str),
            Some("n2")
        );
        assert_ne!(
            user_payload
                .pointer("/categoryInventory/0/nodeId")
                .and_then(Value::as_str),
            Some(report_leaf.as_str())
        );

        let items = user_payload
            .get("items")
            .and_then(Value::as_array)
            .expect("payload items");
        assert_eq!(items.len(), 2);
        for row in items {
            assert!(row.get("path").is_none(), "items leaked path");
            assert!(row.get("size").is_none(), "items leaked size");
            assert!(
                row.get("localExtraction").is_none(),
                "items leaked localExtraction"
            );
            assert!(row.get("createdAt").is_none(), "items leaked createdAt");
            assert!(row.get("modifiedAt").is_none(), "items leaked modifiedAt");
            assert!(row.get("summaryText").is_none());
            assert!(row.get("representation").is_none());
            assert!(row.get("evidence").is_some());
            assert!(row.get("relativePath").is_some());
        }
        assert_eq!(
            items[0].get("evidence").and_then(Value::as_str),
            Some("Quarterly finance report with budget and revenue notes.")
        );
        assert_eq!(
            items[0]
                .get("keywords")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );

        let file_index = user_payload
            .get("fileIndex")
            .and_then(Value::as_array)
            .expect("payload file index");
        assert_eq!(file_index.len(), 2);
        for row in file_index {
            assert!(row.get("itemId").is_some());
            assert!(row.get("name").is_some());
            assert!(row.get("relativePath").is_none());
            assert!(row.get("path").is_none());
            assert!(row.get("size").is_none());
            assert!(row.get("itemType").is_none());
            assert!(row.get("modality").is_none());
            assert!(row.get("summaryText").is_none());
            assert!(row.get("representation").is_none());
            assert!(row.get("evidence").is_none());
        }
    }

    #[tokio::test]
    async fn classification_batch_without_required_tool_returns_error() {
        let mut tree = default_tree();
        ensure_path(&mut tree, &["Documents".to_string()]);
        let batch_rows = vec![json!({
            "itemId": "batch1_1",
            "name": "loose-note.txt",
            "relativePath": "loose-note.txt",
            "itemType": "file",
            "modality": "text",
            "representation": {
                "metadata": "loose-note.txt",
                "short": "Loose note",
                "long": "Loose note with incomplete context.",
                "source": "local_summary",
                "degraded": false,
                "keywords": []
            },
            "summaryWarnings": []
        })];
        let response_body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "I can classify this as a document, but I will not call the tool."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 11,
                "completion_tokens": 5,
                "total_tokens": 16
            }
        });
        let (endpoint, server) = start_mock_chat_server(response_body);
        let output = summary::classify_organize_batch(
            &text_route(endpoint),
            "en-US",
            &AtomicBool::new(false),
            &tree,
            3,
            &batch_rows,
            &[],
            None,
            false,
            "",
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            Arc::new(tokio::sync::Semaphore::new(1)),
            None,
            "classification_batch_test",
        )
        .await
        .expect("classify batch");

        server.join().expect("mock server joined");
        assert!(output.parsed.is_none());
        assert!(output
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("did not call a required organizer tool"));
        assert!(output
            .raw_output
            .contains("I can classify this as a document"));
        assert!(!output.raw_output.contains(CATEGORY_OTHER_PENDING));
    }

    #[test]
    fn pending_reconcile_input_omits_confirmed_assignments() {
        let parsed = json!({
            "baseTreeVersion": 7,
            "assignments": [{
                "itemId": "batch1_1",
                "leafNodeId": "documents",
                "categoryPath": ["Documents"],
                "reason": "already classified"
            }],
            "treeProposals": [],
            "deferredAssignments": []
        });

        assert!(pending_reconcile_input(1, 7, &parsed, None).is_none());
    }

    #[test]
    fn pending_reconcile_input_keeps_only_pending_fields() {
        let parsed = json!({
            "baseTreeVersion": 7,
            "assignments": [{
                "itemId": "batch1_1",
                "leafNodeId": "documents",
                "categoryPath": ["Documents"],
                "reason": "already classified"
            }],
            "treeProposals": [{
                "proposalId": "proposal_1",
                "operation": "add_node",
                "targetNodeId": "documents",
                "suggestedPath": ["Documents", "Receipts"]
            }],
            "deferredAssignments": [{
                "itemId": "batch1_2",
                "proposalId": "proposal_1",
                "suggestedPath": ["Documents", "Receipts"],
                "categoryPath": ["Should", "Drop"],
                "reason": "needs proposed category"
            }]
        });

        let input = pending_reconcile_input(1, 7, &parsed, Some(""))
            .expect("pending reconcile input");

        assert_eq!(input["batchIndex"], json!(1));
        assert_eq!(input["baseTreeVersion"], json!(7));
        assert!(input.get("output").is_none());
        assert!(input.get("assignments").is_none());
        assert_eq!(
            input["treeProposals"][0]["proposalId"],
            json!("proposal_1")
        );
        assert_eq!(input["treeProposals"][0]["targetNodeId"], json!("documents"));
        assert!(input["treeProposals"][0].get("reason").is_none());
        assert_eq!(
            input["deferredAssignments"][0]["itemId"],
            json!("batch1_2")
        );
        assert!(input["deferredAssignments"][0].get("reason").is_none());
        assert!(input["deferredAssignments"][0].get("categoryPath").is_none());
    }

    #[tokio::test]
    async fn reconcile_receives_only_tree_and_classification_results() {
        let mut tree = default_tree();
        let report_leaf = ensure_path(&mut tree, &["Documents".to_string(), "Reports".to_string()]);
        let initial_tree = category_tree_to_value(&tree);
        let compact_tree = json!({
            "nodeId": "root",
            "name": "",
            "children": [{
                "nodeId": "n1",
                "name": "Documents",
                "children": [{
                    "nodeId": "n2",
                    "name": "Reports",
                    "children": []
                }]
            }]
        });
        let classification_results = vec![json!({
            "batchIndex": 1,
            "baseTreeVersion": 4,
            "treeProposals": [{
                "proposalId": "proposal_1",
                "operation": "add_node",
                "targetNodeId": report_leaf,
                "reason": "should be stripped",
                "suggestedPath": ["Documents", "Reports", "Invoices"]
            }],
            "deferredAssignments": [{
                "itemId": "batch1_1",
                "proposalId": "proposal_1",
                "reason": "should be stripped",
                "suggestedPath": ["Documents", "Reports", "Invoices"]
            }],
            "error": ""
        })];
        let revise_args = json!({
            "draftTree": compact_tree,
            "proposalMappings": [{
                "proposalId": "proposal_1",
                "leafNodeId": "n2"
            }],
            "rejectedProposalIds": []
        });
        let review_args = json!({
            "issues": [],
            "recommendedOperations": [],
            "needsRevision": false
        });
        let submit_args = json!({
            "finalTree": compact_tree,
            "proposalMappings": [{
                "proposalId": "proposal_1",
                "status": "merged",
                "leafNodeId": "n2"
            }],
            "finalAssignments": [{
                "itemId": "batch1_1",
                "leafNodeId": "n2"
            }]
        });
        let responses = vec![
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_revise",
                            "type": "function",
                            "function": {
                                "name": "revise_tree_draft",
                                "arguments": revise_args.to_string()
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 }
            }),
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_review",
                            "type": "function",
                            "function": {
                                "name": "review_organize_draft",
                                "arguments": review_args.to_string()
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10 }
            }),
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_submit",
                            "type": "function",
                            "function": {
                                "name": "submit_reconciled_tree",
                                "arguments": submit_args.to_string()
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 9, "completion_tokens": 3, "total_tokens": 12 }
            }),
        ];
        let (endpoint, server) = start_mock_chat_server_sequence(responses);

        let output = summary::reconcile_organize_batches(
            &text_route(endpoint),
            "en-US",
            &AtomicBool::new(false),
            &initial_tree,
            &classification_results,
            None,
        )
        .await
        .expect("reconcile output");
        assert!(output.error.is_none());
        assert_eq!(
            output
                .parsed
                .as_ref()
                .and_then(|value| value.pointer("/finalAssignments/0/leafNodeId"))
                .and_then(Value::as_str),
            Some(report_leaf.as_str())
        );

        let requests = server.join().expect("mock server joined");
        let first_request = request_json_body(&requests[0]);
        let messages = first_request
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");
        let user_payload: Value = serde_json::from_str(
            messages[1]
                .get("content")
                .and_then(Value::as_str)
                .expect("user payload content"),
        )
        .expect("parse reconcile payload");

        assert!(user_payload.get("initialTree").is_some());
        assert!(user_payload.get("classificationResults").is_some());
        assert_eq!(
            user_payload.pointer("/initialTree/children/0/children/0/nodeId"),
            Some(&json!("n2"))
        );
        assert_eq!(
            user_payload.pointer("/classificationResults/0/treeProposals/0/targetNodeId"),
            Some(&json!("n2"))
        );
        assert!(user_payload.get("fileIndex").is_none());
        assert!(user_payload.get("batchOutputs").is_none());
        assert!(!messages[1]
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains(&report_leaf));
        assert!(!messages[1]
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("modelRawOutput"));
        assert!(!messages[1]
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("should be stripped"));

        let second_request = request_json_body(&requests[1]);
        let second_messages = second_request
            .get("messages")
            .and_then(Value::as_array)
            .expect("second messages");
        assert_eq!(second_messages.len(), 2);
        assert!(!second_messages[1]
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("classificationResults"));

        let third_request = request_json_body(&requests[2]);
        let third_messages = third_request
            .get("messages")
            .and_then(Value::as_array)
            .expect("third messages");
        assert_eq!(third_messages.len(), 2);
        assert!(!third_messages[1]
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("classificationResults"));
    }

    #[tokio::test]
    #[ignore = "requires WIPEOUT_CLASSIFICATION_SMOKE_ROOT, ENDPOINT, API_KEY, and MODEL"]
    async fn real_folder_classification_smoke_with_real_model() {
        let root = PathBuf::from(required_env("WIPEOUT_CLASSIFICATION_SMOKE_ROOT"));
        let endpoint = required_env("WIPEOUT_CLASSIFICATION_SMOKE_ENDPOINT");
        let api_key = required_env("WIPEOUT_CLASSIFICATION_SMOKE_API_KEY");
        let model = required_env("WIPEOUT_CLASSIFICATION_SMOKE_MODEL");
        let summary_strategy = env::var("WIPEOUT_CLASSIFICATION_SMOKE_SUMMARY_STRATEGY")
            .unwrap_or_else(|_| SUMMARY_MODE_FILENAME_ONLY.to_string());
        let max_items = env::var("WIPEOUT_CLASSIFICATION_SMOKE_MAX_ITEMS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(8);
        let chunk_size = env::var("WIPEOUT_CLASSIFICATION_SMOKE_CHUNK_SIZE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(max_items)
            .min(max_items)
            .max(1);

        assert!(root.is_dir(), "smoke root must be a directory: {:?}", root);
        assert!(
            matches!(
                summary_strategy.as_str(),
                SUMMARY_MODE_FILENAME_ONLY | SUMMARY_MODE_LOCAL_SUMMARY
            ),
            "real-model smoke supports filename_only or local_summary"
        );

        let smoke_started_at = Instant::now();
        let stop = AtomicBool::new(false);
        let collect_started_at = Instant::now();
        let collection = collect_units(&root, true, &normalize_excluded(None), &stop);
        let collect_elapsed = collect_started_at.elapsed();
        let collected_units = collection.units;
        let collected_count = collected_units.len();
        let units = collected_units
            .into_iter()
            .take(max_items)
            .collect::<Vec<_>>();
        assert!(
            !units.is_empty(),
            "real folder smoke found no classifiable units in {:?}",
            root
        );

        let route = RouteConfig {
            endpoint,
            api_key,
            model,
        };
        let mut routes = HashMap::new();
        routes.insert("text".to_string(), route.clone());
        let task = make_summary_test_runtime(&root, routes);
        let mut summary_elapsed = Duration::default();
        let mut classify_elapsed = Duration::default();
        let mut total_usage = TokenUsage::default();
        let mut total_rows = 0usize;
        let mut total_assigned = 0usize;
        let mut last_parsed = Value::Null;

        for (chunk_idx, chunk) in units.chunks(chunk_size).enumerate() {
            let summary_started_at = Instant::now();
            let prepared = prepare_summary_batch(
                task.clone(),
                route.clone(),
                summary_strategy.clone(),
                chunk_idx,
                chunk.to_vec(),
            )
            .await
            .expect("prepare real folder smoke summary batch");
            let batch_summary_elapsed = summary_started_at.elapsed();
            summary_elapsed += batch_summary_elapsed;

            let tree = deterministic_initial_tree(&prepared.batch_rows);
            let classify_started_at = Instant::now();
            let output = summary::classify_organize_batch(
                &route,
                "zh-CN",
                &stop,
                &tree,
                1,
                &prepared.batch_rows,
                &[],
                None,
                false,
                "",
                Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                Arc::new(tokio::sync::Semaphore::new(1)),
                None,
                "real_folder_classification_smoke",
            )
            .await
            .expect("run real folder classification smoke");
            let batch_classify_elapsed = classify_started_at.elapsed();
            classify_elapsed += batch_classify_elapsed;

            println!(
                "batch={} items={} timing=summary:{}ms,classify_model:{}ms usage=prompt:{},completion:{},total:{}",
                chunk_idx + 1,
                prepared.batch_rows.len(),
                batch_summary_elapsed.as_millis(),
                batch_classify_elapsed.as_millis(),
                output.usage.prompt,
                output.usage.completion,
                output.usage.total
            );

            assert!(
                output.error.is_none(),
                "real model classification failed in batch {}: {:?}\n{}",
                chunk_idx + 1,
                output.error,
                output.raw_output
            );
            let parsed = output.parsed.expect("real model submitted classification");
            assert_eq!(
                parsed.get("baseTreeVersion").and_then(Value::as_u64),
                Some(1)
            );
            let direct = parsed
                .get("assignments")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            let deferred = parsed
                .get("deferredAssignments")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            let assigned = direct + deferred;
            assert!(
                assigned >= prepared.batch_rows.len(),
                "real model did not assign all smoke items in batch {}: direct={}, deferred={}, items={}",
                chunk_idx + 1,
                direct,
                deferred,
                prepared.batch_rows.len()
            );

            total_rows += prepared.batch_rows.len();
            total_assigned += assigned;
            total_usage.prompt += output.usage.prompt;
            total_usage.completion += output.usage.completion;
            total_usage.total += output.usage.total;
            last_parsed = parsed;
        }

        let total_elapsed = smoke_started_at.elapsed();

        println!("root={}", root.display());
        println!("items={}", total_rows);
        println!("collected_items={}", collected_count);
        println!("chunk_size={}", chunk_size);
        println!("chunks={}", units.chunks(chunk_size).len());
        println!(
            "timing=collect:{}ms,summary:{}ms,classify_model:{}ms,total:{}ms",
            collect_elapsed.as_millis(),
            summary_elapsed.as_millis(),
            classify_elapsed.as_millis(),
            total_elapsed.as_millis()
        );
        println!(
            "usage=prompt:{},completion:{},total:{}",
            total_usage.prompt, total_usage.completion, total_usage.total
        );
        println!("assigned={}", total_assigned);
        if total_rows <= 16 {
            println!("parsed={}", last_parsed);
        }
    }

    #[test]
    fn category_inventory_groups_history_without_summaries() {
        let mut tree = default_tree();
        let contract_node = ensure_path(&mut tree, &["文档".to_string(), "合同协议".to_string()]);
        let media_node = ensure_path(&mut tree, &["媒体".to_string(), "图片".to_string()]);

        let inventory = summary::build_category_inventory(
            &tree,
            &[
                json!({
                    "leafNodeId": contract_node.clone(),
                    "categoryPath": ["文档", "合同协议"],
                    "name": "租赁合同.pdf",
                    "relativePath": "contracts\\租赁合同.pdf",
                    "summaryText": "should not be copied",
                    "representation": { "long": "should not be copied" }
                }),
                json!({
                    "leafNodeId": contract_node.clone(),
                    "categoryPath": ["文档", "合同协议"],
                    "name": "服务协议.docx",
                    "relativePath": "contracts\\服务协议.docx"
                }),
                json!({
                    "leafNodeId": contract_node.clone(),
                    "categoryPath": ["文档", "合同协议"],
                    "name": "采购协议.pdf",
                    "relativePath": "2024\\采购协议.pdf"
                }),
                json!({
                    "leafNodeId": contract_node.clone(),
                    "categoryPath": ["文档", "合同协议"],
                    "name": "补充协议.pdf",
                    "relativePath": "2024\\补充协议.pdf"
                }),
                json!({
                    "leafNodeId": media_node.clone(),
                    "categoryPath": ["媒体", "图片"],
                    "name": "cover.png",
                    "relativePath": "images\\cover.png"
                }),
                json!({
                    "leafNodeId": "",
                    "category": CATEGORY_CLASSIFICATION_ERROR,
                    "reason": RESULT_REASON_CLASSIFICATION_ERROR,
                    "name": "bad.txt"
                }),
            ],
            3,
        );

        assert_eq!(inventory.len(), 2);
        let contract_entry = inventory
            .iter()
            .find(|entry| entry.get("nodeId").and_then(Value::as_str) == Some(&contract_node))
            .expect("contract inventory exists");
        assert_eq!(contract_entry.get("count").and_then(Value::as_u64), Some(4));
        assert_eq!(
            contract_entry
                .get("files")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(3)
        );
        assert_eq!(
            contract_entry.get("truncated").and_then(Value::as_bool),
            Some(true)
        );
        assert!(contract_entry.get("summaryText").is_none());
        assert!(contract_entry.get("representation").is_none());

        let media_entry = inventory
            .iter()
            .find(|entry| entry.get("nodeId").and_then(Value::as_str) == Some(&media_node))
            .expect("media inventory exists");
        assert_eq!(media_entry.get("count").and_then(Value::as_u64), Some(1));
        assert_eq!(
            media_entry.get("truncated").and_then(Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn collection_root_detects_download_like_root() {
        let root = temp_dir("download-root").join("Download");
        fs::create_dir_all(root.join("Buzz-1.4.2-Windows-X64")).expect("create buzz dir");
        fs::create_dir_all(root.join("QuickRestart")).expect("create quick dir");
        fs::create_dir_all(root.join("Fonts")).expect("create fonts dir");
        fs::create_dir_all(root.join("Docs")).expect("create docs dir");
        write_file(&root.join("setup.exe"));
        write_file(&root.join("paper.pdf"));
        write_file(&root.join("archive.zip"));
        write_file(&root.join("image.png"));

        let stop = AtomicBool::new(false);
        let mut report = CollectionReport::default();
        assert!(is_collection_root(
            &root,
            &normalize_excluded(None),
            &stop,
            &mut report
        ));

        let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
    }

    #[test]
    fn evaluates_whole_for_app_bundle_directory() {
        let root = temp_dir("app-bundle");
        fs::create_dir_all(&root).expect("create root");
        write_file(&root.join("Buzz-1.4.2-windows.exe"));
        write_file(&root.join("Buzz-1.4.2-windows-1.bin"));
        write_file(&root.join("Buzz-1.4.2-windows-2.bin"));

        let stop = AtomicBool::new(false);
        let assessment = assess_directory(&root, &stop, true);
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_whole_for_plugin_bundle_directory() {
        let root = temp_dir("plugin-bundle");
        fs::create_dir_all(&root).expect("create root");
        write_file(&root.join("QuickRestart.dll"));
        write_file(&root.join("QuickRestart.json"));
        write_file(&root.join("QuickRestart.pck"));

        let stop = AtomicBool::new(false);
        let assessment = assess_directory(&root, &stop, true);
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_whole_for_font_pack_directory() {
        let root = temp_dir("font-pack");
        fs::create_dir_all(&root).expect("create root");
        write_file(&root.join("generica.otf"));
        write_file(&root.join("generica bold.otf"));

        let stop = AtomicBool::new(false);
        let assessment = assess_directory(&root, &stop, false);
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_whole_for_document_bundle_directory() {
        let root = temp_dir("doc-bundle");
        fs::create_dir_all(root.join("我的女友是冒险游戏（待续）")).expect("create child dir");
        for idx in 0..8 {
            write_file(&root.join(format!("chapter-{idx}.txt")));
        }
        write_file(&root.join("collection.zip"));
        write_file(&root.join("extras.zip"));

        let stop = AtomicBool::new(false);
        let assessment = assess_directory(&root, &stop, true);
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_wrapper_passthrough_for_single_child_shell() {
        let root = temp_dir("wrapper");
        let shell = root.join("DwnlData");
        let target = shell.join("32858");
        fs::create_dir_all(&target).expect("create target");
        write_file(&target.join("app.exe"));
        write_file(&target.join("payload.bin"));

        let stop = AtomicBool::new(false);
        let assessment = assess_directory(&shell, &stop, true);
        assert_eq!(
            assessment.result_kind,
            DirectoryResultKind::WholeWrapperPassthrough
        );
        assert_eq!(
            assessment.wrapper_target_path.as_deref(),
            Some(target.to_string_lossy().as_ref())
        );

        let units = collect_units(&root, true, &normalize_excluded(None), &stop).units;
        assert!(units
            .iter()
            .any(|unit| unit.item_type == "directory"
                && unit.relative_path.ends_with("DwnlData\\32858")));
        assert!(!units
            .iter()
            .any(|unit| unit.item_type == "directory" && unit.relative_path == "DwnlData"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_mixed_split_for_mixed_directory() {
        let root = temp_dir("mixed");
        fs::create_dir_all(root.join("photos")).expect("create photos dir");
        fs::create_dir_all(root.join("docs")).expect("create docs dir");
        fs::create_dir_all(root.join("tools")).expect("create tools dir");
        write_file(&root.join("setup.exe"));
        write_file(&root.join("paper.pdf"));
        write_file(&root.join("cover.png"));
        write_file(&root.join("song.mp3"));
        write_file(&root.join("font.ttf"));
        write_file(&root.join("notes.txt"));

        let stop = AtomicBool::new(false);
        let assessment = assess_directory(&root, &stop, false);
        assert_eq!(assessment.result_kind, DirectoryResultKind::MixedSplit);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_staging_junk_for_runtime_cache_shell() {
        let root = temp_dir("junk");
        fs::create_dir_all(root.join("logs")).expect("create logs dir");
        fs::create_dir_all(root.join("cache")).expect("create cache dir");
        write_file(&root.join("telemetry_cache.json"));
        write_file(&root.join("update_cache.json"));
        write_file(&root.join("session.dat"));

        let stop = AtomicBool::new(false);
        let assessment = assess_directory(&root, &stop, false);
        assert_eq!(assessment.result_kind, DirectoryResultKind::StagingJunk);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_whole_for_complex_windows_app_directory() {
        let root = temp_dir("windows-app");
        fs::create_dir_all(root.join("Config")).expect("create config dir");
        fs::create_dir_all(root.join("Images")).expect("create images dir");
        write_file(&root.join("App.exe"));
        for idx in 0..10 {
            write_file(&root.join(format!("runtime-{idx}.dll")));
        }
        for idx in 0..6 {
            write_file(&root.join(format!("asset-{idx}.png")));
        }
        for idx in 0..6 {
            write_file(&root.join(format!("strings-{idx}.res")));
        }
        write_file(&root.join("App.exe.config"));
        write_file(&root.join("readme.txt"));

        let stop = AtomicBool::new(false);
        let assessment = assess_directory(&root, &stop, true);
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
        assert_eq!(assessment.integrity_kind, "app_bundle");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evaluates_whole_for_multi_variant_package_with_docs() {
        let root = temp_dir("dism-bundle").join("Dism++10.1.1002.1B");
        fs::create_dir_all(root.join("Config")).expect("create config dir");
        write_file(&root.join("Dism++ARM64.exe"));
        write_file(&root.join("Dism++x64.exe"));
        write_file(&root.join("Dism++x86.exe"));
        write_file(&root.join("ReadMe for NCleaner.txt"));
        write_file(&root.join("ReadMe for Dism++x86.txt"));
        write_file(&root.join("Dism++x86 usage notes.txt"));

        let stop = AtomicBool::new(false);
        let assessment = assess_directory(&root, &stop, true);
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
        assert_eq!(assessment.integrity_kind, "app_bundle");

        let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
    }

    #[test]
    fn evaluates_whole_for_cursor_theme_pack_directory() {
        let root = temp_dir("cursor-pack").join("Nagasaki Soyo-theme-pack");
        fs::create_dir_all(root.join("optional-replacements")).expect("create alt dir");
        for name in [
            "Alternate.ani",
            "Busy.ani",
            "Diagonal Resize 1.ani",
            "Diagonal Resize 2.ani",
            "Help Select.ani",
            "Horizontal Resize.ani",
            "Link.ani",
            "Location Select.ani",
            "Move.ani",
            "Normal Select.ani",
            "Person Select.ani",
            "Precision Select.ani",
            "Text Select.ani",
            "Unavailable.ani",
            "Vertical Resize.ani",
            "Work.ani",
            "Arrow.cur",
            "Hand.cur",
        ] {
            write_file(&root.join(name));
        }
        write_file(&root.join("cursor-preview.jpg"));
        write_file(&root.join("license.txt"));
        write_file(&root.join("install.inf"));

        let stop = AtomicBool::new(false);
        let assessment = assess_directory(&root, &stop, true);
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
        assert_eq!(assessment.integrity_kind, "theme_pack");

        let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
    }

    #[test]
    fn evaluates_whole_for_document_collection_with_weak_name_families() {
        let root = temp_dir("doc-collection").join("MC-article-collection-1");
        fs::create_dir_all(root.join("我的女友是冒险游戏（待续）")).expect("create child dir");
        for name in [
            "article-01-prologue.txt",
            "article-02-background.txt",
            "article-03-character-notes.txt",
            "article-04-worldbuilding.txt",
            "article-05-chapter-outline.txt",
            "article-06-dialogue-draft.txt",
            "article-07-side-story.txt",
            "article-08-ending-notes.txt",
            "article-09-reading-guide.txt",
            "article-10-author-commentary.txt",
            "article-11-extra-scenes.txt",
            "article-12-appendix.txt",
        ] {
            write_file(&root.join(name));
        }
        for name in [
            "MC-article-collection-1.zip",
            "article-drafts-backup.zip",
            "reading-materials.zip",
            "game-notes-archive.zip",
            "extras.7z",
        ] {
            write_file(&root.join(name));
        }

        let stop = AtomicBool::new(false);
        let assessment = assess_directory(&root, &stop, true);
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
        assert_eq!(assessment.integrity_kind, "doc_bundle");

        let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
    }

    #[test]
    fn evaluates_whole_for_single_installer_with_readme() {
        let root = temp_dir("installer-docs").join("IDM-main");
        fs::create_dir_all(&root).expect("create root");
        write_file(&root.join("IDM_v6.41.2_Setup_by-System3206.exe"));
        write_file(&root.join("README.md"));

        let stop = AtomicBool::new(false);
        let assessment = assess_directory(&root, &stop, true);
        assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
        assert_eq!(assessment.integrity_kind, "app_bundle");

        let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
    }

    #[test]
    fn dll_only_directory_does_not_become_whole() {
        let root = temp_dir("dll-only");
        fs::create_dir_all(&root).expect("create root");
        write_file(&root.join("a.dll"));
        write_file(&root.join("b.dll"));
        write_file(&root.join("c.dll"));

        let stop = AtomicBool::new(false);
        let assessment = assess_directory(&root, &stop, true);
        assert_ne!(assessment.result_kind, DirectoryResultKind::Whole);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_chat_completion_http_body_extracts_content_and_usage() {
        let raw_body = r#"{
          "choices": [
            {
              "message": {
                "content": "{\"tree\":{\"name\":\"\",\"nodeId\":\"root\",\"children\":[]},\"assignments\":[]}"
              }
            }
          ],
          "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 34,
            "total_tokens": 46
          }
        }"#;
        let parsed = summary::parse_chat_completion_http_body(
            "https://api.openai.com/v1",
            StatusCode::OK,
            raw_body,
        )
        .expect("parse success");
        assert!(parsed.content.contains("\"assignments\":[]"));
        assert_eq!(parsed.usage.prompt, 12);
        assert_eq!(parsed.usage.completion, 34);
        assert_eq!(parsed.usage.total, 46);
    }

    #[test]
    fn parse_chat_completion_http_body_keeps_raw_body_on_decode_error() {
        let raw_body = "<html>upstream gateway error</html>";
        let err = summary::parse_chat_completion_http_body(
            "https://api.openai.com/v1",
            StatusCode::OK,
            raw_body,
        )
        .expect_err("decode error");
        assert!(err.message.contains("error decoding response body"));
        assert!(err.message.contains("upstream gateway error"));
        assert_eq!(err.raw_body, raw_body);
    }

    #[test]
    fn parse_chat_completion_http_body_accepts_tool_calls_without_text() {
        let raw_body = r#"{
          "choices": [
            {
              "message": {
                "content": null,
                "tool_calls": [
                  {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                      "name": "submit_classification_batch",
                      "arguments": "{\"baseTreeVersion\":1,\"assignments\":[]}"
                    }
                  }
                ]
              }
            }
          ],
          "usage": {
            "prompt_tokens": 3,
            "completion_tokens": 5,
            "total_tokens": 8
          }
        }"#;
        let parsed = summary::parse_chat_completion_http_body(
            "https://api.openai.com/v1",
            StatusCode::OK,
            raw_body,
        )
        .expect("parse success");
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "submit_classification_batch");
        assert_eq!(parsed.usage.total, 8);
    }
}
