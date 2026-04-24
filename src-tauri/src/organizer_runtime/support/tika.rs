fn extraction_tool_config_from_settings(settings: &Value) -> ExtractionToolConfig {
    let tika = settings
        .get("contentExtraction")
        .and_then(|value| value.get("tika"));
    let configured_tika_enabled = tika
        .and_then(|value| value.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let configured_tika_auto_start = tika
        .and_then(|value| value.get("autoStart"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tika_jar_path = tika
        .and_then(|value| value.get("jarPath"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let tika_url = tika
        .and_then(|value| value.get("url"))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_TIKA_URL)
        .trim()
        .trim_end_matches('/')
        .to_string();
    let legacy_default_config = !configured_tika_enabled
        && !configured_tika_auto_start
        && tika_url == DEFAULT_TIKA_URL
        && tika_jar_path.is_empty();
    ExtractionToolConfig {
        tika_enabled: (configured_tika_enabled
            || configured_tika_auto_start
            || legacy_default_config)
            && !tika_url.is_empty(),
        tika_url,
        tika_auto_start: configured_tika_auto_start || legacy_default_config,
        tika_jar_path,
        tika_ready: false,
    }
}

fn force_enable_tika_for_summary_mode(config: &mut ExtractionToolConfig) {
    if config.tika_url.trim().is_empty() {
        config.tika_url = DEFAULT_TIKA_URL.to_string();
    }
    config.tika_enabled = true;
    config.tika_auto_start = true;
}

async fn is_tika_server_available(url: &str) -> bool {
    let normalized = url.trim().trim_end_matches('/');
    if normalized.is_empty() {
        return false;
    }
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    match client.get(format!("{normalized}/version")).send().await {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

fn looks_like_tika_server_jar(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|value| {
            let lower = value.to_ascii_lowercase();
            lower.starts_with("tika-server-standard-") && lower.ends_with(".jar")
        })
        .unwrap_or(false)
}

fn find_tika_server_jar_in_dir(dir: &Path) -> Option<PathBuf> {
    let mut candidates = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && looks_like_tika_server_jar(path))
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    candidates.into_iter().next()
}

fn resolve_tika_server_jar(state: &AppState, configured_path: &str) -> Option<PathBuf> {
    let configured = configured_path.trim();
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Some(path);
        }
    }

    if let Ok(value) = std::env::var("TIKA_SERVER_JAR") {
        let path = PathBuf::from(value.trim());
        if path.is_file() {
            return Some(path);
        }
    }

    let mut roots = Vec::<PathBuf>::new();
    if let Ok(dir) = std::env::current_dir() {
        roots.push(dir.clone());
        roots.push(dir.join("bin"));
        roots.push(dir.join("tools"));
        roots.push(dir.join("resources"));
    }
    let data_dir = state.data_dir();
    roots.push(data_dir.clone());
    roots.push(data_dir.join("bin"));
    roots.push(data_dir.join("tools"));
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            roots.push(exe_dir.to_path_buf());
            roots.push(exe_dir.join("bin"));
            roots.push(exe_dir.join("resources"));
            if let Some(parent) = exe_dir.parent() {
                roots.push(parent.to_path_buf());
                roots.push(parent.join("bin"));
                roots.push(parent.join("resources"));
            }
        }
    }

    let mut seen = HashSet::<PathBuf>::new();
    for root in roots {
        if !seen.insert(root.clone()) {
            continue;
        }
        if let Some(found) = find_tika_server_jar_in_dir(&root) {
            return Some(found);
        }
    }
    None
}

fn parse_tika_binding(url: &str) -> Option<(String, u16)> {
    let parsed = Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_string();
    let port = parsed.port_or_known_default()?;
    Some((host, port))
}

fn managed_tika_process_alive(process: &mut crate::backend::ManagedTikaProcess) -> bool {
    match process.child.try_wait() {
        Ok(None) => true,
        Ok(Some(_)) | Err(_) => false,
    }
}

async fn ensure_tika_server_running(state: &AppState, extraction_tool: &mut ExtractionToolConfig) {
    extraction_tool.tika_ready = false;
    if !extraction_tool.tika_enabled {
        return;
    }
    if is_tika_server_available(&extraction_tool.tika_url).await {
        extraction_tool.tika_ready = true;
        return;
    }
    if !extraction_tool.tika_auto_start {
        return;
    }
    if extraction_tool.tika_jar_path.trim().is_empty() {
        let Some(path) = resolve_tika_server_jar(state, &extraction_tool.tika_jar_path) else {
            return;
        };
        extraction_tool.tika_jar_path = path.to_string_lossy().to_string();
    }
    let waiting_for_existing_process = {
        let mut guard = state.tika_process.lock();
        if let Some(process) = guard.as_mut() {
            if process.url == extraction_tool.tika_url && managed_tika_process_alive(process) {
                true
            } else {
                *guard = None;
                false
            }
        } else {
            false
        }
    };
    if waiting_for_existing_process {
        for _ in 0..25 {
            if is_tika_server_available(&extraction_tool.tika_url).await {
                extraction_tool.tika_ready = true;
                return;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        return;
    }

    let mut command = Command::new("java");
    command.arg("-jar").arg(&extraction_tool.tika_jar_path);
    if let Some((host, port)) = parse_tika_binding(&extraction_tool.tika_url) {
        command.arg("--host").arg(host);
        command.arg("--port").arg(port.to_string());
    }
    let Ok(child) = command.stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return;
    };
    {
        let mut guard = state.tika_process.lock();
        *guard = Some(crate::backend::ManagedTikaProcess {
            url: extraction_tool.tika_url.clone(),
            child,
        });
    }
    for _ in 0..30 {
        if is_tika_server_available(&extraction_tool.tika_url).await {
            extraction_tool.tika_ready = true;
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

