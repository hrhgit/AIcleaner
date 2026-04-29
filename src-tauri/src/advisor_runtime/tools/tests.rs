#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::AppState;
    use std::fs;
    use std::path::PathBuf;

    fn make_test_state() -> AppState {
        let root =
            std::env::temp_dir().join(format!("wipeout-advisor-tools-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create temp dir");
        AppState::bootstrap(PathBuf::from(root)).expect("bootstrap app state")
    }

    fn make_item(path: &str, kind: &str, risk: &str) -> InventoryItem {
        InventoryItem {
            path: path.to_string(),
            name: basename(path),
            size: 10,
            created_at: None,
            modified_at: None,
            kind: kind.to_string(),
            category_id: String::new(),
            parent_category_id: None,
            category_path: Vec::new(),
            representation: FileRepresentation::default(),
            risk: risk.to_string(),
        }
    }

    #[test]
    fn find_files_filters_by_name_query() {
        let inventory = vec![
            make_item(r"C:\test\shot1.png", "screenshot", "low"),
            make_item(r"C:\test\setup.exe", "installer", "medium"),
        ];
        let matches = filter_inventory_by_args(
            &inventory,
            &json!({
                "nameQuery": "shot",
                "sortBy": "size",
                "sortOrder": "desc",
            }),
        );
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, "screenshot");
    }

    #[test]
    fn fs_fallback_inventory_is_bounded_and_skips_heavy_dirs() {
        let root = std::env::temp_dir().join(format!(
            "wipeout-advisor-fs-fallback-test-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(root.join("node_modules")).expect("create skipped dir");
        fs::write(root.join("node_modules").join("ignored.js"), "ignored").expect("write ignored");
        for idx in 0..(FS_FALLBACK_MAX_FILES + 20) {
            fs::write(root.join(format!("file-{idx}.txt")), "x").expect("write file");
        }

        let rows = collect_inventory_from_fs(&root.to_string_lossy(), &HashMap::new());

        assert_eq!(rows.len(), FS_FALLBACK_MAX_FILES);
        assert!(!rows.iter().any(|item| item.path.contains("node_modules")));
    }

    #[test]
    fn preview_plan_returns_selection_and_job_shape() {
        let matches = vec![
            make_item(r"C:\test\shot1.png", "screenshot", "low"),
            make_item(r"C:\test\shot2.png", "screenshot", "high"),
        ];
        let preview = build_plan_preview_for_matches(
            r"C:\test",
            "session_1",
            "selection_1",
            "删截图",
            "recycle",
            "Recycle",
            &matches,
            "zh",
        );
        assert_eq!(
            preview["selectionId"],
            Value::String("selection_1".to_string())
        );
        assert_eq!(preview["summary"]["total"], Value::from(2));
        assert_eq!(preview["summary"]["canExecute"], Value::from(1));
    }

    #[test]
    fn reclassification_apply_and_rollback_restore_session_overrides() {
        let mut session = json!({
            "sessionMeta": {
                "inventoryOverrides": {}
            }
        });
        let items = vec![json!({ "path": r"C:\test\shot1.png" })];
        let rollback = apply_reclassification_selection(
            &mut session,
            &items,
            &["截图".to_string(), "待整理".to_string()],
        )
        .expect("apply reclassification");
        assert_eq!(
            session["sessionMeta"]["inventoryOverrides"]
                [persist::create_root_path_key(r"C:\test\shot1.png")],
            json!(["截图", "待整理"])
        );
        rollback_reclassification_rows(&mut session, &rollback).expect("rollback");
        assert_eq!(
            session["sessionMeta"]["inventoryOverrides"]
                [persist::create_root_path_key(r"C:\test\shot1.png")],
            Value::Null
        );
    }

    #[test]
    fn directory_overview_includes_root_path_and_saved_preferences() {
        let state = make_test_state();
        let service = ToolService::new(&state);
        persist::save_advisor_memory(
            &state.db_path(),
            &json!({
                "memoryId": Uuid::new_v4().to_string(),
                "sessionId": Value::Null,
                "scope": "global",
                "enabled": true,
                "text": "不要删截图",
                "createdAt": now_iso(),
                "updatedAt": now_iso(),
            }),
        )
        .expect("save memory");

        let overview = service
            .get_directory_overview(r"C:\workspace", None, None, "zh")
            .expect("build overview");
        assert_eq!(overview.context_bar["rootPath"], "C:\\workspace");
        assert_eq!(overview.context_bar["memorySummary"]["activeCount"], 1);
        assert_eq!(
            overview.context_bar["memorySummary"]["message"],
            Value::String("已加载 1 条已保存偏好。".to_string())
        );
    }

    #[test]
    fn advisor_model_view_compacts_tree_and_expands_category_args() {
        let real_category_id = category_id_from_path(&["文档".to_string()]);
        let session = json!({
            "derivedTree": {
                "nodeId": "root",
                "categoryId": "root",
                "categoryPath": [],
                "name": "root",
                "children": [{
                    "nodeId": real_category_id,
                    "categoryId": real_category_id,
                    "categoryPath": ["文档"],
                    "name": "文档",
                    "children": []
                }]
            }
        });

        let model_tree = compact_advisor_model_value(&session, &session["derivedTree"]);
        assert_eq!(model_tree["children"][0]["nodeId"], "n1");
        assert_eq!(model_tree["children"][0]["categoryId"], "n1");
        assert_ne!(model_tree["children"][0]["categoryId"], real_category_id);

        let expanded = expand_advisor_model_args(
            &session,
            &json!({
                "categoryIds": ["n1"],
                "request": {
                    "change": {
                        "sourceCategoryId": "n1",
                        "targetCategoryId": "n1"
                    }
                }
            }),
        );
        assert_eq!(expanded["categoryIds"][0], real_category_id);
        assert_eq!(
            expanded["request"]["change"]["sourceCategoryId"],
            real_category_id
        );
        assert_eq!(
            expanded["request"]["change"]["targetCategoryId"],
            real_category_id
        );
    }

    #[test]
    fn apply_reclassification_accepts_model_short_category_id_after_expansion() {
        let state = make_test_state();
        let service = ToolService::new(&state);
        let source_path = vec!["文档".to_string()];
        let real_category_id = category_id_from_path(&source_path);
        let path_key = persist::create_root_path_key(r"C:\test\report.pdf");
        let mut session = json!({
            "sessionId": "session_1",
            "rootPath": r"C:\test",
            "responseLanguage": "zh",
            "derivedTree": {
                "nodeId": "root",
                "categoryId": "root",
                "categoryPath": [],
                "name": "root",
                "children": [{
                    "nodeId": real_category_id,
                    "categoryId": real_category_id,
                    "categoryPath": source_path,
                    "name": "文档",
                    "children": []
                }]
            },
            "sessionMeta": {
                "inventoryOverrides": {}
            }
        });
        session["sessionMeta"]["inventoryOverrides"]
            .as_object_mut()
            .expect("inventory overrides object")
            .insert(path_key.clone(), json!(["文档"]));
        let backend_args = expand_advisor_model_args(
            &session,
            &json!({
                "request": {
                    "change": {
                        "type": "rename_category",
                        "sourceCategoryId": "n1",
                        "newCategoryName": "资料"
                    }
                }
            }),
        );

        service
            .apply_reclassification_request(
                &mut session,
                backend_args.get("request").unwrap(),
                false,
            )
            .expect("apply reclassification with expanded short id");

        assert_eq!(
            session["sessionMeta"]["inventoryOverrides"][path_key],
            json!(["资料"])
        );
    }

    #[test]
    fn list_preferences_returns_global_and_session_records() {
        let state = make_test_state();
        let service = ToolService::new(&state);
        let db_path = state.db_path();
        let session_id = "session_1";
        persist::save_advisor_memory(
            &db_path,
            &json!({
                "memoryId": Uuid::new_v4().to_string(),
                "sessionId": Value::Null,
                "scope": "global",
                "enabled": true,
                "text": "global rule",
                "createdAt": now_iso(),
                "updatedAt": now_iso(),
            }),
        )
        .expect("save global memory");
        persist::save_advisor_memory(
            &db_path,
            &json!({
                "memoryId": Uuid::new_v4().to_string(),
                "sessionId": session_id,
                "scope": "session",
                "enabled": true,
                "text": "session rule",
                "createdAt": now_iso(),
                "updatedAt": now_iso(),
            }),
        )
        .expect("save session memory");

        let all = service
            .list_preferences(Some(session_id))
            .expect("load all preferences");
        assert_eq!(all.len(), 2);

        let globals = service
            .list_preferences(None)
            .expect("load global preferences");
        assert_eq!(globals.len(), 1);
        assert_eq!(globals[0]["scope"], "global");
    }

    #[test]
    fn summary_row_to_tool_item_prunes_representation_by_level() {
        let row = json!({
            "path": r"C:\test\report.pdf",
            "name": "report.pdf",
            "representation": {
                "metadata": "report.pdf，document，1.0 MB",
                "short": "季度报告",
                "long": "季度报告，包含财务指标与结论。",
                "source": "model",
                "degraded": false,
                "confidence": "high",
                "keywords": ["季度", "财务"]
            }
        });

        let short_item = summary_row_to_tool_item(&row, RepresentationLevel::Short);
        assert_eq!(
            short_item
                .pointer("/representation/metadata")
                .and_then(Value::as_str),
            Some("report.pdf，document，1.0 MB")
        );
        assert_eq!(
            short_item
                .pointer("/representation/short")
                .and_then(Value::as_str),
            Some("季度报告")
        );
        assert_eq!(
            short_item.pointer("/representation/long"),
            Some(&Value::Null)
        );
    }

    #[test]
    fn retryable_statuses_match_scheduler_policy() {
        assert!(is_retryable_status(StatusCode::REQUEST_TIMEOUT));
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_retryable_status(StatusCode::GATEWAY_TIMEOUT));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
    }
}
