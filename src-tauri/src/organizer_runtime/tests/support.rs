use super::*;

pub(super) fn temp_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("wipeout-organizer-{name}-{}", Uuid::new_v4()))
}

pub(super) fn write_file(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, b"test").expect("write file");
}

pub(super) fn write_text_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, content.as_bytes()).expect("write text file");
}

pub(super) fn assess_directory(
    path: &Path,
    stop: &AtomicBool,
    prefer_whole: bool,
) -> DirectoryAssessment {
    let excluded = normalize_excluded(None);
    let mut report = CollectionReport::default();
    evaluate_directory_assessment(path, &excluded, stop, prefer_whole, &mut report)
        .expect("assessment exists")
}

pub(super) fn make_test_unit(path: &Path) -> OrganizeUnit {
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

pub(super) fn make_summary_test_runtime(
    root: &Path,
    routes: HashMap<String, RouteConfig>,
) -> Arc<OrganizeTaskRuntime> {
    make_summary_test_runtime_with_extraction_tool(root, routes, ExtractionToolConfig::default())
}

pub(super) fn make_summary_test_runtime_with_extraction_tool(
    root: &Path,
    routes: HashMap<String, RouteConfig>,
    extraction_tool: ExtractionToolConfig,
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
            token_usage_by_stage: Value::Null,
            timing_ms: Value::Null,
            duration_ms: None,
            request_count: None,
            error_count: None,
            results: Vec::new(),
            preview: Vec::new(),
            created_at: "2026-04-29T00:00:00Z".to_string(),
            completed_at: None,
            job_id: None,
        }),
        routes,
        search_api_key: String::new(),
        response_language: "zh-CN".to_string(),
        extraction_tool,
        diagnostics: OrganizerDiagnostics {
            data_dir: root.to_path_buf(),
            operation_id: "summary_test_operation".to_string(),
            task_id: "summary_test_task".to_string(),
        },
        job: Mutex::new(None),
    })
}

pub(super) fn text_route(endpoint: String) -> RouteConfig {
    RouteConfig {
        endpoint,
        api_key: "test-key".to_string(),
        model: "test-model".to_string(),
    }
}

pub(super) fn read_mock_http_request(stream: &mut TcpStream) -> String {
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
        let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
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

pub(super) fn start_mock_chat_server(response_body: Value) -> (String, JoinHandle<String>) {
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

pub(super) fn start_mock_chat_server_sequence(
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

pub(super) fn summary_response_for_test_ids() -> Value {
    let mut items = Vec::new();
    for batch_idx in 1..=8 {
        for item_idx in 1..=20 {
            let item_id = format!("batch{batch_idx}_{item_idx}");
            items.push(json!({
                "itemId": item_id,
                "summaryShort": format!("short {batch_idx}-{item_idx}"),
                "summaryLong": format!("long {batch_idx}-{item_idx}"),
                "keywords": ["alpha"],
                "confidence": "high",
                "warnings": []
            }));
        }
    }
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": json!({ "items": items }).to_string()
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 11,
            "completion_tokens": 7,
            "total_tokens": 18
        }
    })
}

pub(super) fn start_delayed_mock_chat_server(
    request_count: usize,
    delay_ms: u64,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind delayed mock chat server");
    let addr = listener
        .local_addr()
        .expect("delayed mock chat server addr");
    let handle = thread::spawn(move || {
        let mut handlers = Vec::new();
        for _ in 0..request_count {
            let (mut stream, _) = listener.accept().expect("accept request");
            let active = active.clone();
            let max_active = max_active.clone();
            handlers.push(thread::spawn(move || {
            let request = read_mock_http_request(&mut stream);
            let current = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            loop {
                let observed = max_active.load(std::sync::atomic::Ordering::SeqCst);
                if current <= observed {
                    break;
                }
                if max_active
                    .compare_exchange(
                        observed,
                        current,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(delay_ms));
            let body = summary_response_for_test_ids().to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            request
        }));
        }
        handlers
            .into_iter()
            .map(|handle| handle.join().expect("join delayed handler"))
            .collect::<Vec<_>>()
    });
    (format!("http://{addr}/v1"), handle)
}

pub(super) fn request_json_body(request: &str) -> Value {
    let (_, body) = request.split_once("\r\n\r\n").expect("request body");
    serde_json::from_str(body).expect("parse request json")
}

pub(super) fn summary_request_item_count(request: &str) -> usize {
    let body = request_json_body(request);
    let content = body
        .pointer("/messages/1/content")
        .and_then(Value::as_str)
        .expect("summary request content");
    let payload: Value = serde_json::from_str(content).expect("summary payload json");
    payload
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default()
}

pub(super) fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} must be set"))
}
