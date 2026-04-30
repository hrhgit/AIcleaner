use super::*;

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
fn organize_file_done_event_payload_always_carries_task_id() {
    let payload = build_organize_file_done_payload(
        "org_expected",
        &json!({
            "taskId": "org_stale",
            "index": 1,
            "name": "done.txt",
            "path": r"C:\root\done.txt",
            "categoryPath": ["Docs"]
        }),
    );

    assert_eq!(
        payload.get("taskId").and_then(Value::as_str),
        Some("org_expected")
    );
    assert_eq!(
        payload.get("name").and_then(Value::as_str),
        Some("done.txt")
    );
}

#[test]
fn organize_summary_ready_event_payload_carries_task_id() {
    let payload = summary::build_organize_summary_ready_payload(
        "org_summary",
        2,
        &json!({
            "name": "summary.txt",
            "path": r"C:\root\summary.txt",
            "summaryStrategy": "local_summary"
        }),
    );

    assert_eq!(
        payload.get("taskId").and_then(Value::as_str),
        Some("org_summary")
    );
    assert_eq!(payload.get("batchIndex").and_then(Value::as_u64), Some(2));
    assert_eq!(
        payload.get("summaryStrategy").and_then(Value::as_str),
        Some("local_summary")
    );
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
        token_usage_by_stage: Value::Null,
        timing_ms: Value::Null,
        duration_ms: None,
        request_count: None,
        error_count: None,
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
