use super::support::*;
use super::*;

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
    let concurrency = env::var("WIPEOUT_CLASSIFICATION_SMOKE_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(CLASSIFICATION_BATCH_CONCURRENCY);

    assert!(root.is_dir(), "smoke root must be a directory: {:?}", root);
    assert!(
        matches!(
            summary_strategy.as_str(),
            SUMMARY_MODE_FILENAME_ONLY | SUMMARY_MODE_LOCAL_SUMMARY
        ),
        "real-model smoke supports filename_only or local_summary"
    );

    let smoke_started_at = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let collect_started_at = Instant::now();
    let collection = collect_units(&root, true, &normalize_excluded(None), stop.as_ref());
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
    let mut total_usage = TokenUsage::default();
    let mut total_rows = 0usize;
    let mut total_assigned = 0usize;
    let mut last_parsed = Value::Null;
    let mut prepared_batches = Vec::new();

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
        prepared_batches.push((chunk_idx, prepared, batch_summary_elapsed));
    }

    let classify_wall_started_at = Instant::now();
    let classify_semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let shared_search_calls = Arc::new(AtomicUsize::new(0));
    let shared_search_gate = Arc::new(tokio::sync::Semaphore::new(ORGANIZER_SEARCH_CONCURRENCY));
    let mut handles = Vec::new();

    for (chunk_idx, prepared, batch_summary_elapsed) in prepared_batches {
        let permit = classify_semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("classification concurrency semaphore");
        let route = route.clone();
        let stop = stop.clone();
        let shared_search_calls = shared_search_calls.clone();
        let shared_search_gate = shared_search_gate.clone();
        handles.push(tauri::async_runtime::spawn(async move {
            let _permit = permit;
            let batch_rows = prepared.batch_rows;
            let item_count = batch_rows.len();
            let tree = deterministic_initial_tree(&batch_rows);
            let classify_started_at = Instant::now();
            let output = summary::classify_organize_batch(
                &route,
                "zh-CN",
                stop.as_ref(),
                &tree,
                1,
                &batch_rows,
                &[],
                None,
                false,
                "",
                shared_search_calls,
                shared_search_gate,
                None,
                "real_folder_classification_smoke",
            )
            .await
            .expect("run real folder classification smoke");
            let batch_classify_elapsed = classify_started_at.elapsed();
            (
                chunk_idx,
                item_count,
                batch_summary_elapsed,
                batch_classify_elapsed,
                output,
            )
        }));
    }

    let mut classify_elapsed_sum = Duration::default();
    for handle in handles {
        let (chunk_idx, item_count, batch_summary_elapsed, batch_classify_elapsed, output) =
            handle.await.expect("join real folder classification smoke");
        classify_elapsed_sum += batch_classify_elapsed;

        println!(
            "batch={} items={} timing=summary:{}ms,classify_model:{}ms usage=prompt:{},completion:{},total:{}",
            chunk_idx + 1,
            item_count,
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
            assigned >= item_count,
            "real model did not assign all smoke items in batch {}: direct={}, deferred={}, items={}",
            chunk_idx + 1,
            direct,
            deferred,
            item_count
        );

        total_rows += item_count;
        total_assigned += assigned;
        total_usage.prompt += output.usage.prompt;
        total_usage.completion += output.usage.completion;
        total_usage.total += output.usage.total;
        last_parsed = parsed;
    }
    let classify_wall_elapsed = classify_wall_started_at.elapsed();

    let total_elapsed = smoke_started_at.elapsed();

    println!("root={}", root.display());
    println!("items={}", total_rows);
    println!("collected_items={}", collected_count);
    println!("chunk_size={}", chunk_size);
    println!("concurrency={}", concurrency);
    println!("chunks={}", units.chunks(chunk_size).len());
    println!(
        "timing=collect:{}ms,summary:{}ms,classify_model_sum:{}ms,classify_wall:{}ms,total:{}ms",
        collect_elapsed.as_millis(),
        summary_elapsed.as_millis(),
        classify_elapsed_sum.as_millis(),
        classify_wall_elapsed.as_millis(),
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

fn env_or_default(name: &str, fallback: &str) -> String {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn parse_capacity_batch_sizes() -> Vec<usize> {
    if let Ok(raw) = env::var("WIPEOUT_CAPACITY_SWEEP_BATCH_SIZES") {
        let values = raw
            .split([',', ';', ' '])
            .filter_map(|value| value.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
            .collect::<Vec<_>>();
        if !values.is_empty() {
            return values;
        }
    }
    (20..=110).step_by(10).collect()
}

fn parse_capacity_concurrency_values() -> Vec<usize> {
    if let Ok(raw) = env::var("WIPEOUT_CONCURRENCY_SWEEP_VALUES") {
        let values = raw
            .split([',', ';', ' '])
            .filter_map(|value| value.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
            .collect::<Vec<_>>();
        if !values.is_empty() {
            return values;
        }
    }
    vec![4, 8, 12, 16, 24]
}

fn collect_assignment_item_ids(parsed: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    for key in ["assignments", "deferredAssignments"] {
        for assignment in parsed
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if let Some(item_id) = assignment.get("itemId").and_then(Value::as_str) {
                ids.push(item_id.to_string());
            }
        }
    }
    ids
}

fn percentile_u128(values: &[u128], ratio: f64) -> u128 {
    if values.is_empty() {
        return 0;
    }
    let idx = (((values.len() as f64) * ratio).ceil() as usize)
        .saturating_sub(1)
        .min(values.len() - 1);
    values[idx]
}

fn resolve_capacity_api_key(endpoint: &str) -> String {
    if let Ok(value) = env::var("WIPEOUT_CAPACITY_SWEEP_API_KEY")
        .or_else(|_| env::var("WIPEOUT_CLASSIFICATION_SMOKE_API_KEY"))
    {
        let trimmed = value.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }

    let data_dir = PathBuf::from(env_or_default(
        "WIPEOUT_CAPACITY_SWEEP_DATA_DIR",
        "E:\\Cache\\AIcleaner",
    ));
    let state = crate::backend::AppState::bootstrap(data_dir)
        .expect("bootstrap app state for stored provider credential");
    crate::backend::resolve_provider_api_key(&state, endpoint)
        .expect("resolve provider API key from env or stored app credentials")
}

#[tokio::test]
#[ignore = "sends real model requests; set WIPEOUT_CAPACITY_SWEEP_ROOT or use E:\\Download"]
async fn real_folder_single_batch_capacity_sweep_with_real_model() {
    let root = PathBuf::from(env_or_default(
        "WIPEOUT_CAPACITY_SWEEP_ROOT",
        &env_or_default("WIPEOUT_CLASSIFICATION_SMOKE_ROOT", "E:\\Download"),
    ));
    let endpoint = env_or_default(
        "WIPEOUT_CAPACITY_SWEEP_ENDPOINT",
        &env_or_default(
            "WIPEOUT_CLASSIFICATION_SMOKE_ENDPOINT",
            "https://api.deepseek.com",
        ),
    );
    let model = env_or_default(
        "WIPEOUT_CAPACITY_SWEEP_MODEL",
        &env_or_default("WIPEOUT_CLASSIFICATION_SMOKE_MODEL", "deepseek-v4-flash"),
    );
    let api_key = resolve_capacity_api_key(&endpoint);
    let summary_strategy = env_or_default(
        "WIPEOUT_CAPACITY_SWEEP_SUMMARY_STRATEGY",
        SUMMARY_MODE_FILENAME_ONLY,
    );
    let batch_sizes = parse_capacity_batch_sizes();
    let request_concurrency = env::var("WIPEOUT_CAPACITY_SWEEP_REQUEST_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(batch_sizes.len())
        .min(batch_sizes.len())
        .max(1);
    let repeats = env::var("WIPEOUT_CAPACITY_SWEEP_REPEATS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1);

    assert!(
        root.is_dir(),
        "capacity sweep root must be a directory: {:?}",
        root
    );
    assert!(
        matches!(
            summary_strategy.as_str(),
            SUMMARY_MODE_FILENAME_ONLY | SUMMARY_MODE_LOCAL_SUMMARY
        ),
        "capacity sweep supports filename_only or local_summary"
    );

    let stop = Arc::new(AtomicBool::new(false));
    let collection_started_at = Instant::now();
    let collection = collect_units(&root, true, &normalize_excluded(None), stop.as_ref());
    let collection_elapsed = collection_started_at.elapsed();
    let units = collection.units;
    let max_batch_size = batch_sizes.iter().copied().max().unwrap_or(0);
    assert!(
        units.len() >= max_batch_size,
        "capacity sweep needs at least {} units but found {} in {:?}",
        max_batch_size,
        units.len(),
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
    let semaphore = Arc::new(tokio::sync::Semaphore::new(request_concurrency));
    let shared_search_calls = Arc::new(AtomicUsize::new(0));
    let shared_search_gate = Arc::new(tokio::sync::Semaphore::new(ORGANIZER_SEARCH_CONCURRENCY));
    let sweep_started_at = Instant::now();
    let mut handles = Vec::new();

    println!(
        "capacity_sweep_config root={} collected_units={} collection_ms={} batch_sizes={:?} request_concurrency={} repeats={} summary_strategy={}",
        root.display(),
        units.len(),
        collection_elapsed.as_millis(),
        batch_sizes,
        request_concurrency,
        repeats,
        summary_strategy
    );
    println!(
        "capacity_result,repeat,batch_size,ok,duration_ms,summary_ms,items,assigned,unique_assigned,missing,duplicates,unknown,prompt_tokens,completion_tokens,total_tokens,raw_chars,error"
    );

    for repeat_idx in 0..repeats {
        for batch_size in &batch_sizes {
            let batch_size = *batch_size;
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("capacity sweep request semaphore");
            let route = route.clone();
            let task = task.clone();
            let stop = stop.clone();
            let summary_strategy = summary_strategy.clone();
            let batch_units = units.iter().take(batch_size).cloned().collect::<Vec<_>>();
            let shared_search_calls = shared_search_calls.clone();
            let shared_search_gate = shared_search_gate.clone();

            handles.push(tauri::async_runtime::spawn(async move {
                let _permit = permit;
                let summary_started_at = Instant::now();
                let prepared = prepare_summary_batch(
                    task,
                    route.clone(),
                    summary_strategy,
                    batch_size,
                    batch_units,
                )
                .await;
                let summary_elapsed = summary_started_at.elapsed();
                let prepared = match prepared {
                    Ok(value) => value,
                    Err(err) => {
                        return (
                            repeat_idx + 1,
                            batch_size,
                            false,
                            0u128,
                            summary_elapsed.as_millis(),
                            0usize,
                            0usize,
                            0usize,
                            0usize,
                            0usize,
                            0usize,
                            TokenUsage::default(),
                            0usize,
                            err,
                        );
                    }
                };
                let item_count = prepared.batch_rows.len();
                let tree = deterministic_initial_tree(&prepared.batch_rows);
                let classify_started_at = Instant::now();
                let output = summary::classify_organize_batch(
                    &route,
                    "zh-CN",
                    stop.as_ref(),
                    &tree,
                    1,
                    &prepared.batch_rows,
                    &[],
                    None,
                    false,
                    "",
                    shared_search_calls,
                    shared_search_gate,
                    None,
                    &format!(
                        "capacity_sweep_repeat_{}_batch_size_{batch_size}",
                        repeat_idx + 1
                    ),
                )
                .await;
                let classify_elapsed = classify_started_at.elapsed();

                match output {
                    Ok(output) => {
                        let expected_ids = prepared
                            .batch_rows
                            .iter()
                            .filter_map(|row| row.get("itemId").and_then(Value::as_str))
                            .map(str::to_string)
                            .collect::<std::collections::HashSet<_>>();
                        let mut assigned = 0usize;
                        let mut unique_assigned = 0usize;
                        let mut duplicates = 0usize;
                        let mut unknown = 0usize;
                        let mut error = output.error.clone().unwrap_or_default();
                        if let Some(parsed) = output.parsed.as_ref() {
                            let assignment_ids = collect_assignment_item_ids(parsed);
                            assigned = assignment_ids.len();
                            let mut seen = std::collections::HashSet::new();
                            for item_id in assignment_ids {
                                if !expected_ids.contains(&item_id) {
                                    unknown += 1;
                                } else if !seen.insert(item_id) {
                                    duplicates += 1;
                                }
                            }
                            unique_assigned = seen.len();
                            if parsed.get("baseTreeVersion").and_then(Value::as_u64) != Some(1) {
                                error = "base_tree_version_mismatch".to_string();
                            }
                        } else if error.is_empty() {
                            error = "missing_parsed_tool_result".to_string();
                        }
                        let missing = item_count.saturating_sub(unique_assigned);
                        let ok =
                            error.is_empty() && missing == 0 && duplicates == 0 && unknown == 0;
                        (
                            repeat_idx + 1,
                            batch_size,
                            ok,
                            classify_elapsed.as_millis(),
                            summary_elapsed.as_millis(),
                            item_count,
                            assigned,
                            unique_assigned,
                            missing,
                            duplicates,
                            unknown,
                            output.usage,
                            output.raw_output.chars().count(),
                            error,
                        )
                    }
                    Err(err) => (
                        repeat_idx + 1,
                        batch_size,
                        false,
                        classify_elapsed.as_millis(),
                        summary_elapsed.as_millis(),
                        item_count,
                        0,
                        0,
                        item_count,
                        0,
                        0,
                        TokenUsage::default(),
                        0,
                        err,
                    ),
                }
            }));
        }
    }

    let mut rows = Vec::new();
    for handle in handles {
        rows.push(handle.await.expect("join capacity sweep request"));
    }
    rows.sort_by_key(|row| (row.1, row.0));
    let mut grouped: std::collections::BTreeMap<
        usize,
        (usize, usize, Vec<u128>, usize, usize, usize, usize),
    > = std::collections::BTreeMap::new();
    for (
        repeat,
        batch_size,
        ok,
        duration_ms,
        summary_ms,
        items,
        assigned,
        unique_assigned,
        missing,
        duplicates,
        unknown,
        usage,
        raw_chars,
        error,
    ) in rows
    {
        let entry = grouped
            .entry(batch_size)
            .or_insert((0, 0, Vec::new(), 0, 0, 0, 0));
        entry.0 += 1;
        if ok {
            entry.1 += 1;
        }
        entry.2.push(duration_ms);
        entry.3 += missing;
        entry.4 += duplicates;
        entry.5 += unknown;
        if !error.is_empty() {
            entry.6 += 1;
        }
        let sanitized_error = error.replace(['\r', '\n', ','], " ");
        println!(
            "capacity_result,{repeat},{batch_size},{ok},{duration_ms},{summary_ms},{items},{assigned},{unique_assigned},{missing},{duplicates},{unknown},{},{},{},{raw_chars},{}",
            usage.prompt, usage.completion, usage.total, sanitized_error
        );
    }
    println!(
        "capacity_summary,batch_size,success,total,success_rate,p50_ms,p95_ms,max_ms,missing,duplicates,unknown,error_count"
    );
    for (batch_size, (total, success, mut durations, missing, duplicates, unknown, errors)) in
        grouped
    {
        durations.sort_unstable();
        let p50 = percentile_u128(&durations, 0.50);
        let p95 = percentile_u128(&durations, 0.95);
        let max = durations.last().copied().unwrap_or(0);
        let success_rate = if total == 0 {
            0.0
        } else {
            success as f64 / total as f64
        };
        println!(
            "capacity_summary,{batch_size},{success},{total},{success_rate:.3},{p50},{p95},{max},{missing},{duplicates},{unknown},{errors}"
        );
    }
    println!(
        "capacity_sweep_total_ms={}",
        sweep_started_at.elapsed().as_millis()
    );
}

#[tokio::test]
#[ignore = "sends real model requests; set WIPEOUT_CONCURRENCY_SWEEP_ROOT or use E:\\Download"]
async fn real_folder_small_batch_concurrency_sweep_with_real_model() {
    let root = PathBuf::from(env_or_default(
        "WIPEOUT_CONCURRENCY_SWEEP_ROOT",
        &env_or_default("WIPEOUT_CAPACITY_SWEEP_ROOT", "E:\\Download"),
    ));
    let endpoint = env_or_default(
        "WIPEOUT_CONCURRENCY_SWEEP_ENDPOINT",
        &env_or_default(
            "WIPEOUT_CAPACITY_SWEEP_ENDPOINT",
            "https://api.deepseek.com",
        ),
    );
    let model = env_or_default(
        "WIPEOUT_CONCURRENCY_SWEEP_MODEL",
        &env_or_default("WIPEOUT_CAPACITY_SWEEP_MODEL", "deepseek-v4-flash"),
    );
    let api_key = env::var("WIPEOUT_CONCURRENCY_SWEEP_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| resolve_capacity_api_key(&endpoint));
    let batch_size = env::var("WIPEOUT_CONCURRENCY_SWEEP_BATCH_SIZE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(10);
    let max_items = env::var("WIPEOUT_CONCURRENCY_SWEEP_MAX_ITEMS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(240);
    let summary_strategy = env_or_default(
        "WIPEOUT_CONCURRENCY_SWEEP_SUMMARY_STRATEGY",
        SUMMARY_MODE_FILENAME_ONLY,
    );
    let concurrency_values = parse_capacity_concurrency_values();

    assert!(
        matches!(
            summary_strategy.as_str(),
            SUMMARY_MODE_FILENAME_ONLY | SUMMARY_MODE_LOCAL_SUMMARY
        ),
        "concurrency sweep supports filename_only or local_summary"
    );
    assert!(
        root.is_dir(),
        "concurrency sweep root must be a directory: {:?}",
        root
    );

    let stop = Arc::new(AtomicBool::new(false));
    let collection_started_at = Instant::now();
    let collection = collect_units(&root, true, &normalize_excluded(None), stop.as_ref());
    let collection_elapsed = collection_started_at.elapsed();
    let units = collection
        .units
        .into_iter()
        .take(max_items)
        .collect::<Vec<_>>();
    assert!(
        units.len() >= batch_size,
        "concurrency sweep needs at least {} units but found {} in {:?}",
        batch_size,
        units.len(),
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
    let preparation_started_at = Instant::now();
    let mut prepared_batches = Vec::new();
    for (batch_idx, chunk) in units.chunks(batch_size).enumerate() {
        let prepared = prepare_summary_batch(
            task.clone(),
            route.clone(),
            summary_strategy.clone(),
            batch_idx,
            chunk.to_vec(),
        )
        .await
        .expect("prepare concurrency sweep batch");
        prepared_batches.push(prepared.batch_rows);
    }
    let preparation_elapsed = preparation_started_at.elapsed();
    assert!(
        prepared_batches.len() >= concurrency_values.iter().copied().max().unwrap_or(1),
        "not enough batches ({}) to exercise max concurrency {:?}; increase max_items or reduce batch_size",
        prepared_batches.len(),
        concurrency_values
    );

    println!(
        "concurrency_sweep_config root={} collected_units={} used_units={} batch_size={} batches={} collection_ms={} preparation_ms={} concurrency_values={:?} summary_strategy={}",
        root.display(),
        units.len(),
        units.len(),
        batch_size,
        prepared_batches.len(),
        collection_elapsed.as_millis(),
        preparation_elapsed.as_millis(),
        concurrency_values,
        summary_strategy
    );
    println!(
        "concurrency_result,concurrency,ok,wall_ms,model_sum_ms,p50_ms,p95_ms,max_ms,batches,failed_batches,items,assigned,unique_assigned,missing,duplicates,unknown,prompt_tokens,completion_tokens,total_tokens,errors"
    );

    for concurrency in concurrency_values {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
        let shared_search_calls = Arc::new(AtomicUsize::new(0));
        let shared_search_gate =
            Arc::new(tokio::sync::Semaphore::new(ORGANIZER_SEARCH_CONCURRENCY));
        let sweep_started_at = Instant::now();
        let mut handles = Vec::new();

        for (batch_idx, batch_rows) in prepared_batches.iter().cloned().enumerate() {
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("concurrency sweep semaphore");
            let route = route.clone();
            let stop = stop.clone();
            let shared_search_calls = shared_search_calls.clone();
            let shared_search_gate = shared_search_gate.clone();
            handles.push(tauri::async_runtime::spawn(async move {
                let _permit = permit;
                let item_count = batch_rows.len();
                let tree = deterministic_initial_tree(&batch_rows);
                let started_at = Instant::now();
                let output = summary::classify_organize_batch(
                    &route,
                    "zh-CN",
                    stop.as_ref(),
                    &tree,
                    1,
                    &batch_rows,
                    &[],
                    None,
                    false,
                    "",
                    shared_search_calls,
                    shared_search_gate,
                    None,
                    &format!("concurrency_sweep_{}_batch_{}", concurrency, batch_idx + 1),
                )
                .await;
                let elapsed_ms = started_at.elapsed().as_millis();
                (batch_idx, item_count, batch_rows, elapsed_ms, output)
            }));
        }

        let mut durations = Vec::new();
        let mut model_sum_ms = 0u128;
        let mut failed_batches = 0usize;
        let mut total_items = 0usize;
        let mut assigned_total = 0usize;
        let mut unique_total = 0usize;
        let mut missing_total = 0usize;
        let mut duplicate_total = 0usize;
        let mut unknown_total = 0usize;
        let mut usage_total = TokenUsage::default();
        let mut errors = Vec::new();

        for handle in handles {
            let (batch_idx, item_count, batch_rows, elapsed_ms, output) =
                handle.await.expect("join concurrency sweep request");
            durations.push(elapsed_ms);
            model_sum_ms += elapsed_ms;
            total_items += item_count;

            match output {
                Ok(output) => {
                    add_token_usage(&mut usage_total, &output.usage);
                    let expected_ids = batch_rows
                        .iter()
                        .filter_map(|row| row.get("itemId").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect::<std::collections::HashSet<_>>();
                    let mut assigned = 0usize;
                    let mut unique_assigned = 0usize;
                    let mut duplicates = 0usize;
                    let mut unknown = 0usize;
                    let mut error = output.error.clone().unwrap_or_default();
                    if let Some(parsed) = output.parsed.as_ref() {
                        let assignment_ids = collect_assignment_item_ids(parsed);
                        assigned = assignment_ids.len();
                        let mut seen = std::collections::HashSet::new();
                        for item_id in assignment_ids {
                            if !expected_ids.contains(&item_id) {
                                unknown += 1;
                            } else if !seen.insert(item_id) {
                                duplicates += 1;
                            }
                        }
                        unique_assigned = seen.len();
                        if parsed.get("baseTreeVersion").and_then(Value::as_u64) != Some(1) {
                            error = "base_tree_version_mismatch".to_string();
                        }
                    } else if error.is_empty() {
                        error = "missing_parsed_tool_result".to_string();
                    }
                    let missing = item_count.saturating_sub(unique_assigned);
                    assigned_total += assigned;
                    unique_total += unique_assigned;
                    missing_total += missing;
                    duplicate_total += duplicates;
                    unknown_total += unknown;
                    if !error.is_empty() || missing > 0 || duplicates > 0 || unknown > 0 {
                        failed_batches += 1;
                        let error_label = if error.is_empty() {
                            "invalid_assignment_shape".to_string()
                        } else {
                            error
                        };
                        errors.push(format!(
                            "batch{}:{} missing={} duplicate={} unknown={}",
                            batch_idx + 1,
                            error_label,
                            missing,
                            duplicates,
                            unknown
                        ));
                    }
                }
                Err(err) => {
                    failed_batches += 1;
                    missing_total += item_count;
                    errors.push(format!("batch{}:{err}", batch_idx + 1));
                }
            }
        }

        durations.sort_unstable();
        let percentile = |values: &[u128], ratio: f64| -> u128 {
            if values.is_empty() {
                return 0;
            }
            let idx = (((values.len() as f64) * ratio).ceil() as usize)
                .saturating_sub(1)
                .min(values.len() - 1);
            values[idx]
        };
        let p50_ms = percentile(&durations, 0.50);
        let p95_ms = percentile(&durations, 0.95);
        let max_ms = durations.last().copied().unwrap_or(0);
        let wall_ms = sweep_started_at.elapsed().as_millis();
        let ok = failed_batches == 0;
        let error_summary = errors.join(" | ").replace(['\r', '\n', ','], " ");

        println!(
            "concurrency_result,{concurrency},{ok},{wall_ms},{model_sum_ms},{p50_ms},{p95_ms},{max_ms},{},{failed_batches},{total_items},{assigned_total},{unique_total},{missing_total},{duplicate_total},{unknown_total},{},{},{},{}",
            prepared_batches.len(),
            usage_total.prompt,
            usage_total.completion,
            usage_total.total,
            error_summary
        );
    }
}
