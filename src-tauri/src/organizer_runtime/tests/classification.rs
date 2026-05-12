use super::support::*;
use super::*;
use std::collections::HashMap;

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

    let input = pending_reconcile_input(1, 7, &parsed, Some("")).expect("pending reconcile input");

    assert_eq!(input["batchIndex"], json!(1));
    assert_eq!(input["baseTreeVersion"], json!(7));
    assert!(input.get("output").is_none());
    assert!(input.get("assignments").is_none());
    assert_eq!(input["treeProposals"][0]["proposalId"], json!("proposal_1"));
    assert_eq!(
        input["treeProposals"][0]["targetNodeId"],
        json!("documents")
    );
    assert!(input["treeProposals"][0].get("reason").is_none());
    assert_eq!(input["deferredAssignments"][0]["itemId"], json!("batch1_2"));
    assert!(input["deferredAssignments"][0].get("reason").is_none());
    assert!(input["deferredAssignments"][0]
        .get("categoryPath")
        .is_none());
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
                            "name": "submit_tree_shape",
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
    assert!(output
        .parsed
        .as_ref()
        .and_then(|value| value.pointer("/finalTree/nodeId"))
        .and_then(Value::as_str)
        .is_some());

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
    assert!(!third_messages[1]
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains("classificationResults"));
}

#[test]
fn oversized_subtree_candidates_choose_deepest_only() {
    let mut tree = default_tree();
    let reports_leaf = ensure_path(
        &mut tree,
        &[
            "Documents".to_string(),
            "Reports".to_string(),
            "Invoices".to_string(),
        ],
    );
    let archive_leaf = ensure_path(
        &mut tree,
        &[
            "Documents".to_string(),
            "Reports".to_string(),
            "Archive".to_string(),
        ],
    );
    let notes_leaf = ensure_path(&mut tree, &["Documents".to_string(), "Notes".to_string()]);

    let mut assignments: HashMap<String, (String, Vec<String>, String)> = HashMap::new();
    for idx in 0..21 {
        assignments.insert(
            format!("invoice_{idx}"),
            (
                reports_leaf.clone(),
                vec![
                    "Documents".to_string(),
                    "Reports".to_string(),
                    "Invoices".to_string(),
                ],
                "invoice".to_string(),
            ),
        );
    }
    assignments.insert(
        "archive_1".to_string(),
        (
            archive_leaf,
            vec![
                "Documents".to_string(),
                "Reports".to_string(),
                "Archive".to_string(),
            ],
            "archive".to_string(),
        ),
    );
    assignments.insert(
        "note_1".to_string(),
        (
            notes_leaf,
            vec!["Documents".to_string(), "Notes".to_string()],
            "note".to_string(),
        ),
    );

    let candidates = select_oversized_subtree_candidates(&tree, &assignments, 20);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].category_path, vec!["Documents", "Reports", "Invoices"]);
    assert_eq!(candidates[0].item_count, 21);
}

#[test]
fn oversized_subtree_candidates_are_fixed_before_refine() {
    let mut tree = default_tree();
    let reports_leaf = ensure_path(&mut tree, &["Documents".to_string(), "Reports".to_string()]);
    let mut assignments: HashMap<String, (String, Vec<String>, String)> = HashMap::new();
    for idx in 0..21 {
        assignments.insert(
            format!("report_{idx}"),
            (
                reports_leaf.clone(),
                vec!["Documents".to_string(), "Reports".to_string()],
                "report".to_string(),
            ),
        );
    }

    let initial_candidates = select_oversized_subtree_candidates(&tree, &assignments, 20);
    assert_eq!(initial_candidates.len(), 1);

    let split_leaf = ensure_path(
        &mut tree,
        &[
            "Documents".to_string(),
            "Reports".to_string(),
            "2026".to_string(),
        ],
    );
    for idx in 0..10 {
        assignments.insert(
            format!("report_{idx}"),
            (
                split_leaf.clone(),
                vec![
                    "Documents".to_string(),
                    "Reports".to_string(),
                    "2026".to_string(),
                ],
                "split".to_string(),
            ),
        );
    }

    assert_eq!(initial_candidates.len(), 1);
    assert_eq!(initial_candidates[0].category_path, vec!["Documents", "Reports"]);
    let rescanned = select_oversized_subtree_candidates(&tree, &assignments, 20);
    assert!(rescanned.is_empty());
}

#[test]
fn local_refine_rejects_assignment_outside_subtree() {
    let mut tree = default_tree();
    let reports_leaf = ensure_path(
        &mut tree,
        &["Documents".to_string(), "Reports".to_string(), "Invoices".to_string()],
    );
    let notes_leaf = ensure_path(&mut tree, &["Documents".to_string(), "Notes".to_string()]);
    let candidate = OversizedSubtreeCandidate {
        node_id: category_path_for_id(&tree, &reports_leaf)
            .and_then(|_| Some(reports_leaf.clone()))
            .unwrap(),
        category_path: vec![
            "Documents".to_string(),
            "Reports".to_string(),
            "Invoices".to_string(),
        ],
        item_count: 1,
    };
    let mut assignments = HashMap::new();
    assignments.insert(
        "item_1".to_string(),
        (
            reports_leaf.clone(),
            candidate.category_path.clone(),
            "invoice".to_string(),
        ),
    );

    let err = apply_local_refine_assignments(
        &candidate,
        &mut tree,
        &mut assignments,
        &json!({
            "assignments": [{
                "itemId": "item_1",
                "leafNodeId": notes_leaf,
                "reason": "wrong scope"
            }]
        }),
    )
    .expect_err("should reject out-of-scope leaf");
    assert!(err.contains("outside subtree"));
}

#[tokio::test]
async fn local_refine_payload_and_writeback_stay_inside_subtree() {
    let mut tree = default_tree();
    let invoices_leaf = ensure_path(
        &mut tree,
        &["Documents".to_string(), "Reports".to_string(), "Invoices".to_string()],
    );
    let receipts_leaf = ensure_path(
        &mut tree,
        &["Documents".to_string(), "Reports".to_string(), "Receipts".to_string()],
    );
    let notes_leaf = ensure_path(&mut tree, &["Documents".to_string(), "Notes".to_string()]);

    let candidate = OversizedSubtreeCandidate {
        node_id: ensure_path(&mut tree, &["Documents".to_string(), "Reports".to_string()]),
        category_path: vec!["Documents".to_string(), "Reports".to_string()],
        item_count: 2,
    };

    let mut assignments = HashMap::new();
    assignments.insert(
        "item_1".to_string(),
        (
            invoices_leaf.clone(),
            vec![
                "Documents".to_string(),
                "Reports".to_string(),
                "Invoices".to_string(),
            ],
            "invoice".to_string(),
        ),
    );
    assignments.insert(
        "item_2".to_string(),
        (
            receipts_leaf.clone(),
            vec![
                "Documents".to_string(),
                "Reports".to_string(),
                "Receipts".to_string(),
            ],
            "receipt".to_string(),
        ),
    );
    assignments.insert(
        "item_3".to_string(),
        (
            notes_leaf.clone(),
            vec!["Documents".to_string(), "Notes".to_string()],
            "note".to_string(),
        ),
    );

    let rows_by_id = HashMap::from([
        (
            "item_1".to_string(),
            (
                0usize,
                json!({
                    "itemId": "item_1",
                    "name": "invoice-jan.pdf",
                    "relativePath": "Reports\\invoice-jan.pdf",
                    "itemType": "file",
                    "modality": "text",
                    "representation": {
                        "metadata": "invoice-jan.pdf",
                        "short": "January invoice",
                        "long": "January invoice for vendor billing.",
                        "source": "local_summary",
                        "degraded": false,
                        "keywords": ["invoice", "billing"]
                    }
                }),
            ),
        ),
        (
            "item_2".to_string(),
            (
                0usize,
                json!({
                    "itemId": "item_2",
                    "name": "receipt-feb.pdf",
                    "relativePath": "Reports\\receipt-feb.pdf",
                    "itemType": "file",
                    "modality": "text",
                    "representation": {
                        "metadata": "receipt-feb.pdf",
                        "short": "February receipt",
                        "long": "February receipt for expense reimbursement.",
                        "source": "local_summary",
                        "degraded": false,
                        "keywords": ["receipt", "expense"]
                    }
                }),
            ),
        ),
        (
            "item_3".to_string(),
            (
                0usize,
                json!({
                    "itemId": "item_3",
                    "name": "notes.txt",
                    "relativePath": "Notes\\notes.txt",
                    "itemType": "file",
                    "modality": "text",
                    "representation": {
                        "metadata": "notes.txt",
                        "short": "Notes",
                        "long": "Random notes.",
                        "source": "local_summary",
                        "degraded": false,
                        "keywords": ["notes"]
                    }
                }),
            ),
        ),
    ]);

    let submitted = json!({
        "assignments": [{
            "itemId": "item_1",
            "categoryPath": ["Invoices"],
            "reason": "invoice stays"
        }, {
            "itemId": "item_2",
            "categoryPath": ["Expense Receipts"],
            "reason": "receipt split"
        }]
    });
    let response_body = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_submit_local_refine",
                    "type": "function",
                    "function": {
                        "name": "submit_local_reclassification",
                        "arguments": submitted.to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 15,
            "completion_tokens": 8,
            "total_tokens": 23
        }
    });
    let (endpoint, server) = start_mock_chat_server(response_body);

    let subtree = build_local_refine_subtree_value(&tree, &candidate.node_id);
    let payload_items = build_local_refine_items(&candidate, &tree, &assignments, &rows_by_id);
    let output = summary::refine_local_subtree_once(
        &text_route(endpoint),
        "en-US",
        &AtomicBool::new(false),
        &subtree,
        &payload_items,
        None,
    )
    .await
    .expect("local refine output");
    assert!(output.error.is_none());

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
            .expect("user payload"),
    )
    .expect("parse local refine payload");
    let payload_text = messages[1]
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(payload_text.contains("subtree_only"));
    assert!(!payload_text.contains("Notes"));
    assert_eq!(
        user_payload["items"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        user_payload["subtree"]["name"].as_str(),
        Some("Reports")
    );

    let parsed = output.parsed.expect("parsed local refine output");
    apply_local_refine_assignments(&candidate, &mut tree, &mut assignments, &parsed)
        .expect("apply local refine");

    let item_1 = assignments.get("item_1").expect("item_1 assignment");
    assert_eq!(item_1.0, invoices_leaf);
    assert_eq!(
        item_1.1,
        vec![
            "Documents".to_string(),
            "Reports".to_string(),
            "Invoices".to_string(),
        ]
    );

    let item_2 = assignments.get("item_2").expect("item_2 assignment");
    assert_eq!(
        item_2.1,
        vec![
            "Documents".to_string(),
            "Reports".to_string(),
            "Expense Receipts".to_string(),
        ]
    );
    assert!(category_path_for_id(&tree, &item_2.0).is_some());

    let item_3 = assignments.get("item_3").expect("item_3 assignment");
    assert_eq!(item_3.0, notes_leaf);
    assert_eq!(item_3.1, vec!["Documents".to_string(), "Notes".to_string()]);
}
