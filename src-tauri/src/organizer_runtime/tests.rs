#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
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

    fn assess_directory(
        path: &Path,
        stop: &AtomicBool,
        prefer_whole: bool,
    ) -> DirectoryAssessment {
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

    #[test]
    fn ensure_path_creates_nested_tree() {
        let mut tree = default_tree();
        let leaf = ensure_path(&mut tree, &["group".to_string(), "leaf".to_string()]);
        let path = category_path_for_id(&tree, &leaf).expect("path");
        assert_eq!(path, vec!["group".to_string(), "leaf".to_string()]);
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
            row.pointer("/representation/source").and_then(Value::as_str),
            Some(SUMMARY_SOURCE_LOCAL_SUMMARY)
        );
        assert_eq!(
            row.pointer("/localExtraction/parser").and_then(Value::as_str),
            Some("plain_text")
        );
        assert_eq!(row.get("provider").and_then(Value::as_str), Some(route.endpoint.as_str()));
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
            row.pointer("/representation/source").and_then(Value::as_str),
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
            item.get("summaryText").and_then(Value::as_str),
            Some("季度财务报告，包含季度财务指标与结论。")
        );
        assert_eq!(
            item.pointer("/representation/keywords")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2),
        );
        assert_eq!(
            item.pointer("/representation/source")
                .and_then(Value::as_str),
            Some("agent_summary")
        );
        assert!(item.get("createdAge").is_some());
        assert!(item.get("modifiedAge").is_some());
        assert!(item.get("localExtraction").is_none());
        assert!(item.get("path").is_none());
        assert!(item.get("size").is_none());
        assert!(item.get("createdAt").is_none());
        assert!(item.get("modifiedAt").is_none());
    }

    #[test]
    fn category_inventory_groups_history_without_summaries() {
        let mut tree = default_tree();
        let contract_node = ensure_path(
            &mut tree,
            &["文档".to_string(), "合同协议".to_string()],
        );
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
        let assessment =
            assess_directory(&root, &stop, true);
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
        let assessment =
            assess_directory(&root, &stop, true);
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
        let assessment =
            assess_directory(&root, &stop, false);
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
        let assessment =
            assess_directory(&root, &stop, true);
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
        let assessment =
            assess_directory(&shell, &stop, true);
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
        let assessment =
            assess_directory(&root, &stop, false);
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
        let assessment =
            assess_directory(&root, &stop, false);
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
        let assessment =
            assess_directory(&root, &stop, true);
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
        let assessment =
            assess_directory(&root, &stop, true);
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
        let assessment =
            assess_directory(&root, &stop, true);
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
        let assessment =
            assess_directory(&root, &stop, true);
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
        let assessment =
            assess_directory(&root, &stop, true);
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
        let assessment =
            assess_directory(&root, &stop, true);
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
