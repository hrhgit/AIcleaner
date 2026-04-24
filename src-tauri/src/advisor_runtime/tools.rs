use super::llm::AdvisorLlm;
use super::types::{
    array_of_strings, basename, local_text, normalize_message, normalize_path_key, now_iso,
    set_inventory_override, ContextAssets, DirectoryOverview, InventoryItem,
};
use crate::backend::AppState;
use crate::file_representation::{FileRepresentation, RepresentationLevel};
use crate::llm_protocol::{
    apply_auth_headers, build_completion_payload, build_messages_url, detect_api_format,
    parse_completion_response, DEFAULT_MAX_TOKENS,
};
use crate::persist;
use crate::system_ops;
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use uuid::Uuid;

const FS_FALLBACK_MAX_FILES: usize = 500;
const FS_FALLBACK_MAX_DIRS: usize = 1500;
const FS_FALLBACK_MAX_DEPTH: usize = 5;
const FS_FALLBACK_SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "dist",
    "build",
    "out",
    "target",
    "Windows",
    "Program Files",
    "Program Files (x86)",
];

#[derive(Default)]
struct TreeDraftNode {
    name: String,
    item_count: u64,
    children: BTreeMap<String, TreeDraftNode>,
}

#[derive(Clone, Debug, Default)]
struct SummarySchedulerStats {
    initial_concurrency: usize,
    final_concurrency: usize,
    retry_rounds: usize,
    degraded_concurrency: bool,
}

#[derive(Clone, Debug)]
struct SummaryErrorRow {
    path: String,
    reason: String,
    retryable: bool,
}

#[derive(Clone, Debug)]
struct SummaryFailedItem {
    item: InventoryItem,
    error: SummaryErrorRow,
}

pub(super) struct ToolService<'a> {
    state: &'a AppState,
}

impl<'a> ToolService<'a> {
    pub(super) fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    pub(super) fn get_directory_overview(
        &self,
        root_path: &str,
        organize_task_id: Option<&str>,
        session: Option<&Value>,
        response_language: &str,
    ) -> Result<DirectoryOverview, String> {
        let assets = self.load_assets(root_path, organize_task_id)?;
        let inventory = self.build_inventory(root_path, &assets, session);
        let preferences = self.list_preferences(
            session
                .and_then(|value| value.get("sessionId"))
                .and_then(Value::as_str),
        )?;
        let derived_tree =
            build_derived_tree(root_path, &inventory).or_else(|| assets.latest_tree.clone());
        let context_bar = build_context_bar(
            root_path,
            session,
            &assets,
            &inventory,
            &preferences,
            response_language,
            assets.organize_snapshot.is_some() || assets.latest_tree.is_some(),
        );
        Ok(DirectoryOverview {
            assets,
            inventory,
            derived_tree,
            context_bar,
        })
    }

    pub(super) fn get_directory_overview_tool(
        &self,
        session: &Value,
        view_type: Option<&str>,
        _root_category_id: Option<&str>,
        _max_depth: Option<u64>,
    ) -> Result<Value, String> {
        let overview = self.get_directory_overview(
            session
                .get("rootPath")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            session_organize_task_id(session),
            Some(session),
            session
                .get("responseLanguage")
                .and_then(Value::as_str)
                .unwrap_or("zh"),
        )?;
        let tree = overview.derived_tree.unwrap_or(Value::Null);
        let view_type = normalize_view_type(view_type.unwrap_or("summaryTree"));
        Ok(json!({
            "message": local_text(
                session.get("responseLanguage").and_then(Value::as_str).unwrap_or("zh"),
                "已返回当前目录概览。",
                "Returned the current directory overview."
            ),
            "viewType": view_type,
            "treeText": build_tree_text(&tree),
            "tree": tree
        }))
    }

    pub(super) fn find_files_by_args(
        &self,
        session: &Value,
        args: &Value,
    ) -> Result<Value, String> {
        let overview = self.get_directory_overview(
            session
                .get("rootPath")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            session_organize_task_id(session),
            Some(session),
            session
                .get("responseLanguage")
                .and_then(Value::as_str)
                .unwrap_or("zh"),
        )?;
        let matches = filter_inventory_by_args(&overview.inventory, args);
        let query_summary = summarize_find_query(args);
        let selection = create_selection(
            &self.state.db_path(),
            session
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            &query_summary,
            &matches,
        )?;
        Ok(json!({
            "message": local_text(
                session.get("responseLanguage").and_then(Value::as_str).unwrap_or("zh"),
                &format!("已找到 {} 个候选文件。", matches.len()),
                &format!("Found {} candidate files.", matches.len())
            ),
            "total": selection.get("total").cloned().unwrap_or(Value::from(0)),
            "selectionId": selection.get("selectionId").cloned().unwrap_or(Value::Null),
            "querySummary": selection.get("querySummary").cloned().unwrap_or(Value::String(query_summary)),
            "sortBy": args.get("sortBy").cloned().unwrap_or(Value::String("size".to_string())),
            "sortOrder": args.get("sortOrder").cloned().unwrap_or(Value::String("desc".to_string())),
            "files": selection.get("items").cloned().unwrap_or_else(|| json!([])),
        }))
    }

    pub(super) fn capture_preference(
        &self,
        session: &Value,
        scope: &str,
        text: &str,
        source_message: &str,
    ) -> Result<Value, String> {
        let lang = session
            .get("responseLanguage")
            .and_then(Value::as_str)
            .unwrap_or("zh");
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(json!({
            "preferenceId": Uuid::new_v4().to_string(),
            "scope": if scope.eq_ignore_ascii_case("global") { "global" } else { "session" },
            "text": text.trim(),
            "sourceMessage": source_message.trim(),
            "summary": local_text(
                lang,
                &format!("偏好提炼：{}", text.trim()),
                &format!("Preference extracted: {}", text.trim()),
            ),
            "kind": infer_preference_kind(text),
            "suggestedScope": if scope.eq_ignore_ascii_case("global") { "global" } else { "session" },
            "createdAt": now_iso(),
            "sessionId": session_id,
        }))
    }

    pub(super) fn list_preferences(&self, session_id: Option<&str>) -> Result<Vec<Value>, String> {
        persist::load_advisor_memories(&self.state.db_path(), session_id)
    }

    pub(super) fn list_preferences_tool(&self, session_id: Option<&str>) -> Result<Value, String> {
        let rows = self.list_preferences(session_id)?;
        let mut session_preferences = Vec::new();
        let mut global_preferences = Vec::new();
        for row in rows {
            if row.get("scope").and_then(Value::as_str) == Some("global") {
                global_preferences.push(row);
            } else {
                session_preferences.push(row);
            }
        }
        Ok(json!({
            "sessionPreferences": session_preferences,
            "globalPreferences": global_preferences,
        }))
    }

    pub(super) async fn summarize_files_tool(
        &self,
        session: &Value,
        args: &Value,
    ) -> Result<Value, String> {
        self.summarize_files_tool_impl(session, args).await
    }

    pub(super) fn read_only_file_summaries_tool(
        &self,
        session: &Value,
        args: &Value,
    ) -> Result<Value, String> {
        self.read_only_file_summaries_tool_impl(session, args)
    }

    pub(super) fn preview_plan_from_value(
        &self,
        session: &Value,
        plan: &Value,
    ) -> Result<Value, String> {
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let lang = session
            .get("responseLanguage")
            .and_then(Value::as_str)
            .unwrap_or("zh");
        let targets = plan
            .get("targets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if targets.is_empty() {
            return Err(
                "当前计划缺少 selectionId，请先调用 find_files 生成筛选结果，再继续生成预览。"
                    .to_string(),
            );
        }

        let mut all_entries = Vec::new();
        let mut preview_selection_id = None::<String>;
        for target in targets {
            let selection_id = target
                .get("selectionId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    "当前计划缺少 selectionId，请先调用 find_files 生成筛选结果，再继续生成预览。"
                        .to_string()
                })?;
            let action = target
                .get("action")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("review");
            let selection = persist::load_advisor_selection(&self.state.db_path(), selection_id)?
                .ok_or_else(|| {
                "当前筛选结果已失效，请先重新调用 find_files 生成新的 selection，再继续生成预览。"
                    .to_string()
            })?;
            ensure_session_owned(&selection, session_id, "selection")?;
            let items = selection
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let inventory = items
                .iter()
                .map(value_to_inventory_item)
                .collect::<Vec<_>>();
            let label = action_label(action, lang);
            let preview = build_plan_preview_for_matches(
                session
                    .get("rootPath")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                session_id,
                selection_id,
                plan.get("intentSummary")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                normalize_plan_action(action),
                &label,
                &inventory,
                lang,
            );
            if preview_selection_id.is_none() {
                preview_selection_id = Some(selection_id.to_string());
            }
            all_entries.extend(
                preview
                    .get("entries")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
        }

        let can_execute = all_entries
            .iter()
            .filter(|row| {
                row.get("canExecute")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .count();
        let preview_id = Uuid::new_v4().to_string();
        let preview = json!({
            "previewId": preview_id,
            "planId": preview_id,
            "intentSummary": plan.get("intentSummary").cloned().unwrap_or(Value::String(String::new())),
            "selectionId": preview_selection_id.clone(),
            "summary": {
                "total": all_entries.len(),
                "canExecute": can_execute,
                "blocked": all_entries.len().saturating_sub(can_execute),
            },
            "entries": all_entries,
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
        });
        let job = json!({
            "jobId": preview_id,
            "sessionId": session_id,
            "selectionId": preview_selection_id,
            "status": "preview_ready",
            "preview": preview,
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
        });
        persist::save_advisor_plan_job(&self.state.db_path(), &job)?;
        Ok(job)
    }

    pub(super) fn execute_plan(
        &self,
        session_id: &str,
        job_id: &str,
    ) -> Result<(Value, Value, Value), String> {
        let mut job = persist::load_advisor_plan_job(&self.state.db_path(), job_id)?
            .ok_or_else(|| "plan job not found".to_string())?;
        ensure_session_owned(&job, session_id, "plan job")?;
        let (result, rollback) = execute_plan_job(&job)?;
        if let Some(obj) = job.as_object_mut() {
            obj.insert("status".to_string(), Value::String("executed".to_string()));
            obj.insert("result".to_string(), result.clone());
            obj.insert("rollback".to_string(), rollback.clone());
            obj.insert("updatedAt".to_string(), Value::String(now_iso()));
        }
        persist::save_advisor_plan_job(&self.state.db_path(), &job)?;
        Ok((job, result, rollback))
    }

    pub(super) fn execute_plan_by_preview_id(
        &self,
        session_id: &str,
        preview_id: &str,
    ) -> Result<(Value, Value, Value), String> {
        self.execute_plan(session_id, preview_id)
            .map_err(|_| "当前预览不存在或已过期，请先重新生成 preview，再执行。".to_string())
    }

    pub(super) fn rollback_plan(
        &self,
        session_id: &str,
        job_id: &str,
    ) -> Result<(Value, Value), String> {
        let mut job = persist::load_advisor_plan_job(&self.state.db_path(), job_id)?
            .ok_or_else(|| "plan job not found".to_string())?;
        ensure_session_owned(&job, session_id, "plan job")?;
        let rollback_result = rollback_plan_job(&job)?;
        if let Some(obj) = job.as_object_mut() {
            obj.insert(
                "status".to_string(),
                Value::String("rolled_back".to_string()),
            );
            obj.insert("rollbackResult".to_string(), rollback_result.clone());
            obj.insert("updatedAt".to_string(), Value::String(now_iso()));
        }
        persist::save_advisor_plan_job(&self.state.db_path(), &job)?;
        Ok((job, rollback_result))
    }

    pub(super) fn apply_reclassification(
        &self,
        session: &mut Value,
        selection_id: &str,
        category_path: &[String],
    ) -> Result<Value, String> {
        let selection = persist::load_advisor_selection(&self.state.db_path(), selection_id)?
            .ok_or_else(|| "selection not found".to_string())?;
        ensure_session_owned(
            &selection,
            session
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "selection",
        )?;
        let items = selection
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let rollback_rows = apply_reclassification_selection(session, &items, category_path)?;
        let overview = self.get_directory_overview(
            session
                .get("rootPath")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            session_organize_task_id(session),
            Some(session),
            session
                .get("responseLanguage")
                .and_then(Value::as_str)
                .unwrap_or("zh"),
        )?;
        Ok(json!({
            "jobId": Uuid::new_v4().to_string(),
            "sessionId": session.get("sessionId").cloned().unwrap_or(Value::Null),
            "status": "applied",
            "request": {
                "selectionId": selection_id,
                "categoryPath": category_path,
            },
            "result": {
                "summary": local_text(
                    session.get("responseLanguage").and_then(Value::as_str).unwrap_or("zh"),
                    "分类修正已写入当前会话树。旧选择和旧预览已失效。",
                    "The reclassification has been applied to the session tree. Previous selections and previews are now invalid.",
                ),
                "tree": overview.derived_tree,
            },
            "rollback": {
                "entries": rollback_rows,
            },
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
        }))
    }

    pub(super) fn apply_reclassification_request(
        &self,
        session: &mut Value,
        request: &Value,
        apply_preference_capture: bool,
    ) -> Result<Value, String> {
        self.apply_reclassification_request_impl(session, request, apply_preference_capture)
    }

    pub(super) fn rollback_reclassification(
        &self,
        session: &mut Value,
        job_id: &str,
    ) -> Result<(Value, Value, Option<Value>), String> {
        let mut job = persist::load_advisor_reclass_job(&self.state.db_path(), job_id)?
            .ok_or_else(|| "reclass job not found".to_string())?;
        ensure_session_owned(
            &job,
            session
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "reclassification job",
        )?;
        let rows = job
            .pointer("/rollback/entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        rollback_reclassification_rows(session, &rows)?;
        let overview = self.get_directory_overview(
            session
                .get("rootPath")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            session_organize_task_id(session),
            Some(session),
            session
                .get("responseLanguage")
                .and_then(Value::as_str)
                .unwrap_or("zh"),
        )?;
        if let Some(obj) = job.as_object_mut() {
            obj.insert(
                "status".to_string(),
                Value::String("rolled_back".to_string()),
            );
            obj.insert(
                "rollbackResult".to_string(),
                json!({
                    "tree": overview.derived_tree,
                }),
            );
            obj.insert("updatedAt".to_string(), Value::String(now_iso()));
        }
        persist::save_advisor_reclass_job(&self.state.db_path(), &job)?;
        Ok((
            job,
            json!({
                "message": local_text(
                    session.get("responseLanguage").and_then(Value::as_str).unwrap_or("zh"),
                    "已回滚最近一次分类修正。",
                    "Rolled back the latest reclassification."
                ),
                "updatedTreeText": build_tree_text(&overview.derived_tree.clone().unwrap_or(Value::Null)),
                "rolledBack": true,
                "invalidated": ["selection", "preview"],
            }),
            overview.derived_tree,
        ))
    }

    async fn summarize_files_tool_impl(
        &self,
        session: &Value,
        args: &Value,
    ) -> Result<Value, String> {
        let lang = session
            .get("responseLanguage")
            .and_then(Value::as_str)
            .unwrap_or("zh");
        let root_path = session
            .get("rootPath")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let overview = self.get_directory_overview(
            root_path,
            session_organize_task_id(session),
            Some(session),
            lang,
        )?;
        let level =
            RepresentationLevel::parse(args.get("representationLevel").and_then(Value::as_str));
        let selected = filter_inventory_by_args(&overview.inventory, args);
        let total = selected.len();
        let missing_only = args
            .get("missingOnly")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if level == RepresentationLevel::Metadata {
            let mut items = Vec::new();
            for item in selected {
                if missing_only {
                    if let Some(row) =
                        load_summary_row(&self.state.db_path(), root_path, &item.path, level, true)?
                    {
                        items.push(summary_row_to_tool_item(
                            &row,
                            RepresentationLevel::Metadata,
                        ));
                        continue;
                    }
                }
                let row = self.persist_summary(root_path, &item, level, None)?;
                items.push(summary_row_to_tool_item(&row, level));
            }
            return Ok(json!({
                "status": "ok",
                "message": local_text(
                    lang,
                    &format!("已完成 {} 条文件摘要。", items.len()),
                    &format!("Completed {} file summaries.", items.len())
                ),
                "representationLevel": level.as_str(),
                "total": total,
                "completed": items.len(),
                "failed": 0,
                "items": items,
                "errors": [],
                "scheduler": {
                    "initialConcurrency": 1,
                    "finalConcurrency": 1,
                    "retryRounds": 0,
                    "degradedConcurrency": false
                },
            }));
        }

        let mut cached_rows = Vec::new();
        let mut generation_items = Vec::new();
        for item in selected {
            if missing_only {
                if let Some(row) =
                    load_summary_row(&self.state.db_path(), root_path, &item.path, level, true)?
                {
                    cached_rows.push(row);
                    continue;
                }
            }
            generation_items.push(item);
        }

        let batch_size = args.get("batchSize").and_then(Value::as_u64).unwrap_or(8) as usize;
        let max_concurrency = args
            .get("maxConcurrency")
            .and_then(Value::as_u64)
            .unwrap_or(2) as usize;
        let route = AdvisorLlm::new(self.state).resolve_route(None, None)?;
        let mut rows = cached_rows;
        let (generated_rows, errors, scheduler) = run_summary_scheduler(
            &route,
            root_path,
            &generation_items,
            level,
            lang,
            batch_size.max(1),
            max_concurrency.max(1),
        )
        .await?;
        rows.extend(generated_rows);
        for row in &rows {
            persist::save_advisor_file_summary(&self.state.db_path(), row)?;
        }
        let items = rows
            .iter()
            .map(|row| summary_row_to_tool_item(row, level))
            .collect::<Vec<_>>();
        Ok(json!({
            "status": if errors.is_empty() { "ok" } else { "error" },
            "message": if errors.is_empty() {
                Value::String(local_text(
                    lang,
                    &format!("已完成 {} 条文件摘要。", items.len()),
                    &format!("Completed {} file summaries.", items.len())
                ).to_string())
            } else {
                Value::String(local_text(
                    lang,
                    "部分摘要生成失败，请检查 errors 并重试。",
                    "Some summaries failed. Check errors and retry."
                ).to_string())
            },
            "representationLevel": level.as_str(),
            "total": total,
            "completed": items.len(),
            "failed": errors.len(),
            "items": items,
            "errors": errors.into_iter().map(|error| json!({
                "path": error.path,
                "reason": error.reason,
            })).collect::<Vec<_>>(),
            "scheduler": {
                "initialConcurrency": scheduler.initial_concurrency,
                "finalConcurrency": scheduler.final_concurrency,
                "retryRounds": scheduler.retry_rounds,
                "degradedConcurrency": scheduler.degraded_concurrency,
            },
        }))
    }

    fn read_only_file_summaries_tool_impl(
        &self,
        session: &Value,
        args: &Value,
    ) -> Result<Value, String> {
        let overview = self.get_directory_overview(
            session
                .get("rootPath")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            session_organize_task_id(session),
            Some(session),
            session
                .get("responseLanguage")
                .and_then(Value::as_str)
                .unwrap_or("zh"),
        )?;
        let level =
            RepresentationLevel::parse(args.get("representationLevel").and_then(Value::as_str));
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(24) as usize;
        let root_path = session
            .get("rootPath")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let items = filter_inventory_by_args(&overview.inventory, args)
            .into_iter()
            .take(limit)
            .filter_map(|item| {
                read_existing_summary_item(&self.state.db_path(), root_path, &item, level)
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({
            "message": local_text(
                session.get("responseLanguage").and_then(Value::as_str).unwrap_or("zh"),
                &format!("已读取 {} 条已有摘要。", items.len()),
                &format!("Read {} existing summaries.", items.len())
            ),
            "total": items.len(),
            "items": items,
        }))
    }

    fn persist_summary(
        &self,
        root_path: &str,
        item: &InventoryItem,
        level: RepresentationLevel,
        representation: Option<FileRepresentation>,
    ) -> Result<Value, String> {
        let representation = representation
            .unwrap_or_else(|| metadata_representation(item))
            .prune_to_level(level);
        let row = json!({
            "rootPathKey": persist::create_root_path_key(root_path),
            "pathKey": persist::create_root_path_key(&item.path),
            "path": item.path,
            "name": item.name,
            "representation": representation.to_value(),
            "summaryShort": representation.short.clone(),
            "summaryNormal": representation.long.clone(),
            "source": representation.source,
            "representationLevel": level.as_str(),
            "updatedAt": now_iso(),
        });
        persist::save_advisor_file_summary(&self.state.db_path(), &row)?;
        Ok(row)
    }

    fn apply_reclassification_request_impl(
        &self,
        session: &mut Value,
        request: &Value,
        apply_preference_capture: bool,
    ) -> Result<Value, String> {
        let change = request.get("change").cloned().unwrap_or_else(|| json!({}));
        let change_type = change
            .get("type")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
            })?;
        let rollback_entries = match change_type {
            "move_selection_to_category" => {
                let selection_id = required_change_str(&change, "selectionId")?;
                let target_category_id = required_change_str(&change, "targetCategoryId")?;
                let selection = persist::load_advisor_selection(&self.state.db_path(), selection_id)?
                    .ok_or_else(|| "当前筛选结果已失效，请先重新调用 find_files 生成新的 selection，再继续分类修正。".to_string())?;
                let items = selection
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let category_path = category_path_from_category_id(session, target_category_id)?
                    .ok_or_else(|| {
                        "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
                    })?;
                apply_reclassification_selection(session, &items, &category_path)?
            }
            "split_selection_to_new_category" => {
                let selection_id = required_change_str(&change, "selectionId")?;
                let source_category_id = required_change_str(&change, "sourceCategoryId")?;
                let new_category_name = required_change_str(&change, "newCategoryName")?;
                let selection = persist::load_advisor_selection(&self.state.db_path(), selection_id)?
                    .ok_or_else(|| "当前筛选结果已失效，请先重新调用 find_files 生成新的 selection，再继续分类修正。".to_string())?;
                let items = selection
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut category_path =
                    category_path_from_category_id(session, source_category_id)?
                        .unwrap_or_default();
                category_path.push(new_category_name.to_string());
                apply_reclassification_selection(session, &items, &category_path)?
            }
            "rename_category" => rename_category(session, &change)?,
            "merge_category_into_category" => merge_category_into_category(session, &change)?,
            "delete_empty_category" => delete_empty_category(session, &change)?,
            _ => {
                return Err(
                    "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string(),
                )
            }
        };
        if let Some(obj) = session.as_object_mut() {
            obj.insert("activeSelectionId".to_string(), Value::Null);
            obj.insert("activePreviewId".to_string(), Value::Null);
        }
        if apply_preference_capture {
            persist::save_advisor_memory(
                &self.state.db_path(),
                &json!({
                    "memoryId": Uuid::new_v4().to_string(),
                    "sessionId": session.get("sessionId").cloned().unwrap_or(Value::Null),
                    "scope": "session",
                    "enabled": true,
                    "text": request.get("intentSummary").and_then(Value::as_str).unwrap_or_default(),
                    "createdAt": now_iso(),
                    "updatedAt": now_iso(),
                }),
            )?;
        }
        let overview = self.get_directory_overview(
            session
                .get("rootPath")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            session_organize_task_id(session),
            Some(session),
            session
                .get("responseLanguage")
                .and_then(Value::as_str)
                .unwrap_or("zh"),
        )?;
        Ok(json!({
            "jobId": Uuid::new_v4().to_string(),
            "sessionId": session.get("sessionId").cloned().unwrap_or(Value::Null),
            "status": "applied",
            "request": request,
            "result": {
                "message": local_text(
                    session.get("responseLanguage").and_then(Value::as_str).unwrap_or("zh"),
                    "已应用归类修订。",
                    "Reclassification applied."
                ),
                "changeSummary": build_reclassification_change_summary(&change, &rollback_entries),
                "updatedTreeText": build_tree_text(&overview.derived_tree.clone().unwrap_or(Value::Null)),
                "tree": overview.derived_tree,
                "invalidated": ["selection", "preview"],
            },
            "rollback": {
                "entries": rollback_entries,
            },
            "createdAt": now_iso(),
            "updatedAt": now_iso(),
        }))
    }

    fn load_assets(
        &self,
        root_path: &str,
        organize_task_id: Option<&str>,
    ) -> Result<ContextAssets, String> {
        let db_path = self.state.db_path();
        let resolved_organize_task_id = if let Some(task_id) = organize_task_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(task_id.to_string())
        } else {
            persist::find_latest_organize_task_id_for_root(&db_path, root_path)?
        };
        let organize_snapshot = resolved_organize_task_id
            .as_deref()
            .map(|task_id| persist::load_organize_snapshot(&db_path, task_id))
            .transpose()?
            .flatten();
        let latest_tree = persist::load_latest_organize_tree(&db_path, root_path)?
            .map(|(tree, _)| tree)
            .or_else(|| organize_snapshot.as_ref().map(|row| row.tree.clone()));

        Ok(ContextAssets {
            organize_task_id: resolved_organize_task_id,
            organize_snapshot,
            latest_tree,
        })
    }

    fn build_inventory(
        &self,
        root_path: &str,
        assets: &ContextAssets,
        session: Option<&Value>,
    ) -> Vec<InventoryItem> {
        let overrides = session
            .map(super::types::inventory_overrides)
            .unwrap_or_default();
        let mut map = HashMap::<String, InventoryItem>::new();

        if let Some(snapshot) = &assets.organize_snapshot {
            for row in &snapshot.results {
                let path = row
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if path.is_empty() {
                    continue;
                }
                let key = normalize_path_key(&path);
                let representation = representation_from_organize_row(row);
                let summary = representation.best_text();
                let mut category_path = overrides
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| array_of_strings(row.get("categoryPath")));
                if category_path.is_empty() {
                    category_path.push("其他待定".to_string());
                }
                let category_id = category_id_from_path(&category_path);
                let parent_category_id = parent_category_id_from_path(&category_path);
                let (created_at, modified_at) =
                    infer_inventory_timestamps(&path, row.get("createdAt"), row.get("modifiedAt"));
                map.insert(
                    key,
                    InventoryItem {
                        name: row
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| basename(&path)),
                        path: path.clone(),
                        size: row.get("size").and_then(Value::as_u64).unwrap_or(0),
                        created_at,
                        modified_at,
                        kind: infer_kind(&path, &summary, ""),
                        category_id,
                        parent_category_id,
                        category_path,
                        representation,
                        risk: "medium".to_string(),
                    },
                );
            }
        }

        if map.is_empty() {
            for item in collect_inventory_from_fs(root_path, &overrides) {
                map.insert(normalize_path_key(&item.path), item);
            }
        }

        let mut rows = map.into_values().collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            right
                .size
                .cmp(&left.size)
                .then_with(|| left.path.cmp(&right.path))
        });
        rows
    }
}

pub(super) fn build_context_bar(
    root_path: &str,
    session: Option<&Value>,
    assets: &ContextAssets,
    inventory: &[InventoryItem],
    preferences: &[Value],
    lang: &str,
    tree_available: bool,
) -> Value {
    let memory_summary_message = if preferences.is_empty() {
        local_text(
            lang,
            "当前没有已保存偏好。",
            "No saved preferences are active.",
        )
        .to_string()
    } else {
        local_text(
            lang,
            &format!("已加载 {} 条已保存偏好。", preferences.len()),
            &format!("{} saved preferences are active.", preferences.len()),
        )
        .to_string()
    };
    json!({
        "collapsed": session
            .and_then(|value| value.pointer("/contextBar/collapsed"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        "rootPath": session
            .and_then(|value| value.get("rootPath"))
            .and_then(Value::as_str)
            .unwrap_or(root_path),
        "mode": {
            "id": "single_agent",
            "label": local_text(lang, "顾问模式：单智能体", "Advisor Mode: Single Agent")
        },
        "organizeTaskId": assets.organize_task_id,
        "organizeSummary": {
            "totalFiles": assets.organize_snapshot.as_ref().map(|row| row.total_files).unwrap_or(0),
            "processedFiles": assets.organize_snapshot.as_ref().map(|row| row.processed_files).unwrap_or(0),
        },
        "memorySummary": {
            "activeCount": preferences.len(),
            "message": memory_summary_message,
        },
        "directorySummary": {
            "itemCount": inventory.len(),
            "treeAvailable": tree_available,
            "message": if tree_available {
                local_text(lang, "已载入最近一次归类结果，可直接对话收敛方案。", "Latest organize result loaded and ready for guided cleanup.")
            } else {
                local_text(lang, "当前没有可复用的归类结果，顾问会先基于目录元信息工作。", "No reusable organize result is available yet, so advisor starts from directory metadata.")
            }
        }
    })
}

pub(super) fn build_tree_card(
    session_id: &str,
    turn_id: &str,
    tree: &Value,
    inventory: &[InventoryItem],
    lang: &str,
) -> Value {
    json!({
        "cardId": Uuid::new_v4().to_string(),
        "sessionId": session_id,
        "turnId": turn_id,
        "cardType": super::types::CARD_TREE,
        "status": "ready",
        "title": local_text(lang, "当前分类树", "Current Tree"),
        "body": {
            "tree": tree,
            "stats": tree_stats(inventory),
        },
        "actions": [],
        "createdAt": now_iso(),
        "updatedAt": now_iso(),
    })
}

fn tree_insert(node: &mut TreeDraftNode, path: &[String]) {
    node.item_count += 1;
    if let Some(head) = path.first() {
        let child = node
            .children
            .entry(head.clone())
            .or_insert_with(|| TreeDraftNode {
                name: head.clone(),
                ..Default::default()
            });
        tree_insert(child, &path[1..]);
    }
}

fn tree_to_value(prefix: &str, node: &TreeDraftNode) -> Value {
    let mut children = node
        .children
        .values()
        .map(|child| tree_to_value(&format!("{prefix}/{}", child.name), child))
        .collect::<Vec<_>>();
    children.sort_by(|left, right| {
        right["itemCount"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&left["itemCount"].as_u64().unwrap_or(0))
            .then_with(|| {
                left["name"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["name"].as_str().unwrap_or_default())
            })
    });
    json!({
        "nodeId": persist::create_node_id(&format!("advisor-tree:{prefix}")),
        "name": node.name,
        "itemCount": node.item_count,
        "children": children,
    })
}

fn build_derived_tree(root_path: &str, inventory: &[InventoryItem]) -> Option<Value> {
    if inventory.is_empty() {
        return None;
    }
    let mut root = TreeDraftNode {
        name: basename(root_path),
        ..Default::default()
    };
    for item in inventory {
        let category_path = if item.category_path.is_empty() {
            vec!["其他待定".to_string()]
        } else {
            item.category_path.clone()
        };
        tree_insert(&mut root, &category_path);
    }
    Some(tree_to_value(root_path, &root))
}

fn tree_stats(inventory: &[InventoryItem]) -> Value {
    let mut kinds = HashMap::<String, u64>::new();
    for item in inventory {
        *kinds.entry(item.kind.clone()).or_default() += 1;
    }
    json!({
        "itemCount": inventory.len(),
        "kinds": kinds,
    })
}

fn infer_kind(path: &str, summary: &str, fallback: &str) -> String {
    let lower = format!("{} {}", normalize_message(path), normalize_message(summary));
    if lower.contains("screenshot")
        || lower.contains("截图")
        || lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
    {
        "screenshot".to_string()
    } else if lower.ends_with(".exe")
        || lower.ends_with(".msi")
        || lower.contains("installer")
        || lower.contains("安装")
    {
        "installer".to_string()
    } else if lower.ends_with(".zip")
        || lower.ends_with(".rar")
        || lower.ends_with(".7z")
        || lower.contains("压缩")
    {
        "archive".to_string()
    } else if lower.ends_with(".log") || lower.contains("日志") {
        "log".to_string()
    } else if lower.contains("\\temp\\")
        || lower.contains("\\tmp\\")
        || lower.contains("缓存")
        || lower.contains("cache")
        || lower.contains("临时")
    {
        "temp".to_string()
    } else if lower.ends_with(".mp4")
        || lower.ends_with(".mov")
        || lower.ends_with(".mkv")
        || lower.contains("视频")
    {
        "video".to_string()
    } else if lower.ends_with(".mp3")
        || lower.ends_with(".wav")
        || lower.ends_with(".flac")
        || lower.contains("音频")
    {
        "audio".to_string()
    } else if lower.ends_with(".pdf")
        || lower.ends_with(".doc")
        || lower.ends_with(".docx")
        || lower.ends_with(".xls")
        || lower.ends_with(".xlsx")
        || lower.contains("文档")
    {
        "document".to_string()
    } else if !fallback.trim().is_empty() {
        fallback.to_string()
    } else {
        "file".to_string()
    }
}

fn session_organize_task_id(session: &Value) -> Option<&str> {
    session
        .pointer("/sessionMeta/organizeTaskId")
        .and_then(Value::as_str)
}

fn create_selection(
    db_path: &Path,
    session_id: &str,
    query_summary: &str,
    items: &[InventoryItem],
) -> Result<Value, String> {
    let now = now_iso();
    let selection = json!({
        "selectionId": Uuid::new_v4().to_string(),
        "sessionId": session_id,
        "querySummary": query_summary,
        "total": items.len(),
        "items": items.iter().map(|item| json!({
            "path": item.path,
            "name": item.name,
            "size": item.size,
            "sizeText": format_size_text(item.size),
            "createdAt": item.created_at,
            "modifiedAt": item.modified_at,
            "modifiedAgeText": modified_age_text(item),
            "kind": item.kind,
            "categoryId": item.category_id,
            "parentCategoryId": item.parent_category_id,
            "categoryPath": item.category_path,
            "representation": item.representation.to_value(),
            "risk": item.risk,
        })).collect::<Vec<_>>(),
        "createdAt": now,
        "updatedAt": now,
    });
    persist::save_advisor_selection(db_path, &selection)?;
    Ok(selection)
}

fn archive_target_path(root_path: &str, item: &InventoryItem) -> String {
    PathBuf::from(root_path)
        .join("_advisor_archive")
        .join(item.name.clone())
        .to_string_lossy()
        .to_string()
}

fn move_target_path(root_path: &str, item: &InventoryItem) -> String {
    let mut base = PathBuf::from(root_path).join("_advisor_sorted");
    for part in &item.category_path {
        base.push(part);
    }
    base.push(item.name.clone());
    base.to_string_lossy().to_string()
}

pub(super) fn build_plan_preview_for_matches(
    root_path: &str,
    session_id: &str,
    selection_id: &str,
    query_summary: &str,
    action: &str,
    label: &str,
    matches: &[InventoryItem],
    lang: &str,
) -> Value {
    let entries = matches
        .iter()
        .map(|item| {
            let warnings = if action == "recycle" && item.risk == "high" {
                vec![local_text(
                    lang,
                    "高风险项默认不直接回收",
                    "High-risk items are blocked from direct recycle by default",
                )
                .to_string()]
            } else {
                Vec::new()
            };
            let can_execute = warnings.is_empty() && !matches!(action, "keep" | "review");
            json!({
                "name": item.name,
                "sourcePath": item.path,
                "targetPath": match action {
                    "archive" => Value::String(archive_target_path(root_path, item)),
                    "move" => Value::String(move_target_path(root_path, item)),
                    _ => Value::Null,
                },
                "action": action,
                "kind": item.kind,
                "risk": item.risk,
                "canExecute": can_execute,
                "warnings": warnings,
            })
        })
        .collect::<Vec<_>>();
    let can_execute = entries
        .iter()
        .filter(|row| {
            row.get("canExecute")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    json!({
        "jobId": Uuid::new_v4().to_string(),
        "sessionId": session_id,
        "selectionId": selection_id,
        "querySummary": query_summary,
        "action": action,
        "label": label,
        "summary": {
            "total": entries.len(),
            "canExecute": can_execute,
            "blocked": entries.len().saturating_sub(can_execute),
        },
        "entries": entries,
        "createdAt": now_iso(),
        "updatedAt": now_iso(),
    })
}

fn execute_plan_job(job: &Value) -> Result<(Value, Value), String> {
    let preview = job.get("preview").cloned().unwrap_or_else(|| json!({}));
    let entries = preview
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut rollback_entries = Vec::new();
    let mut result_entries = Vec::new();

    for entry in entries {
        let source_path = entry
            .get("sourcePath")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let can_execute = entry
            .get("canExecute")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !can_execute {
            result_entries.push(json!({
                "sourcePath": source_path,
                "status": "blocked",
                "error": "blocked_in_preview",
            }));
            continue;
        }

        let action = entry
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let outcome = if action == "recycle" {
            system_ops::move_to_recycle_bin(Path::new(source_path)).map(|_| {
                json!({
                    "sourcePath": source_path,
                    "status": "recycled",
                    "targetPath": Value::Null,
                    "action": action,
                })
            })
        } else {
            let target_path = entry
                .get("targetPath")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if source_path.is_empty() || target_path.is_empty() {
                Err("missing_path".to_string())
            } else if Path::new(target_path).exists() {
                Err("target_conflict".to_string())
            } else {
                if let Some(parent) = Path::new(target_path).parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::rename(source_path, target_path).map_err(|e| e.to_string())?;
                rollback_entries.push(json!({
                    "fromPath": source_path,
                    "toPath": target_path,
                }));
                Ok(json!({
                    "sourcePath": source_path,
                    "targetPath": target_path,
                    "status": "moved",
                    "action": action,
                }))
            }
        };

        match outcome {
            Ok(value) => result_entries.push(value),
            Err(error) => result_entries.push(json!({
                "sourcePath": source_path,
                "status": "failed",
                "error": error,
            })),
        }
    }

    let moved_count = result_entries
        .iter()
        .filter(|row| row.get("status").and_then(Value::as_str) == Some("moved"))
        .count();
    Ok((
        json!({
            "entries": result_entries,
            "summary": {
                "total": result_entries.len(),
                "moved": moved_count,
                "archived": result_entries.iter().filter(|row| row.get("action").and_then(Value::as_str) == Some("archive") && row.get("status").and_then(Value::as_str) == Some("moved")).count(),
                "recycled": result_entries.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("recycled")).count(),
                "failed": result_entries.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("failed")).count(),
                "blocked": result_entries.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("blocked")).count(),
            }
        }),
        json!({
            "entries": rollback_entries,
            "available": !rollback_entries.is_empty(),
        }),
    ))
}

fn rollback_plan_job(job: &Value) -> Result<Value, String> {
    let entries = job
        .pointer("/rollback/entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut results = Vec::new();
    for entry in entries {
        let from_path = entry
            .get("fromPath")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let to_path = entry
            .get("toPath")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let result = if from_path.is_empty() || to_path.is_empty() {
            Err("missing_path".to_string())
        } else if !Path::new(to_path).exists() {
            Err("target_not_found".to_string())
        } else if Path::new(from_path).exists() {
            Err("source_already_exists".to_string())
        } else {
            if let Some(parent) = Path::new(from_path).parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::rename(to_path, from_path).map_err(|e| e.to_string())
        };
        match result {
            Ok(()) => results.push(json!({
                "fromPath": from_path,
                "toPath": to_path,
                "status": "rolled_back",
            })),
            Err(error) => results.push(json!({
                "fromPath": from_path,
                "toPath": to_path,
                "status": "failed",
                "error": error,
            })),
        }
    }
    Ok(json!({
        "rollbackId": Uuid::new_v4().to_string(),
        "entries": results,
        "summary": {
            "rolledBack": results.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("rolled_back")).count(),
            "notRollbackable": 0,
            "failed": results.iter().filter(|row| row.get("status").and_then(Value::as_str) == Some("failed")).count(),
        }
    }))
}

pub(super) fn apply_reclassification_selection(
    session: &mut Value,
    items: &[Value],
    category_path: &[String],
) -> Result<Vec<Value>, String> {
    let mut rollback_rows = Vec::new();
    for row in items {
        let Some(path) = row.get("path").and_then(Value::as_str) else {
            continue;
        };
        let key = normalize_path_key(path);
        let previous = set_inventory_override(session, &key, category_path)?;
        rollback_rows.push(json!({
            "path": key,
            "previousCategoryPath": previous,
        }));
    }
    Ok(rollback_rows)
}

pub(super) fn rollback_reclassification_rows(
    session: &mut Value,
    rows: &[Value],
) -> Result<(), String> {
    for row in rows {
        let Some(path) = row.get("path").and_then(Value::as_str) else {
            continue;
        };
        let previous = array_of_strings(row.get("previousCategoryPath"));
        if previous.is_empty() {
            super::types::remove_inventory_override(session, path)?;
        } else {
            set_inventory_override(session, path, &previous)?;
        }
    }
    Ok(())
}

fn required_change_str<'a>(change: &'a Value, key: &str) -> Result<&'a str, String> {
    change
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string())
}

fn category_id_from_path(category_path: &[String]) -> String {
    persist::create_node_id(&format!("advisor-category:{}", category_path.join("/")))
}

fn parent_category_id_from_path(category_path: &[String]) -> Option<String> {
    if category_path.len() <= 1 {
        None
    } else {
        Some(category_id_from_path(
            &category_path[..category_path.len() - 1],
        ))
    }
}

fn category_path_from_category_id(
    session: &Value,
    category_id: &str,
) -> Result<Option<Vec<String>>, String> {
    let tree = session.get("derivedTree").cloned().unwrap_or(Value::Null);
    Ok(find_category_path_in_tree(&tree, category_id))
}

fn find_category_path_in_tree(tree: &Value, category_id: &str) -> Option<Vec<String>> {
    let obj = tree.as_object()?;
    if obj.get("categoryId").and_then(Value::as_str) == Some(category_id) {
        return Some(array_of_strings(obj.get("categoryPath")));
    }
    obj.get("children")
        .and_then(Value::as_array)
        .and_then(|children| {
            children
                .iter()
                .find_map(|child| find_category_path_in_tree(child, category_id))
        })
}

fn infer_inventory_timestamps(
    path: &str,
    created_at: Option<&Value>,
    modified_at: Option<&Value>,
) -> (Option<String>, Option<String>) {
    let created = created_at.and_then(Value::as_str).map(str::to_string);
    let modified = modified_at.and_then(Value::as_str).map(str::to_string);
    if created.is_some() || modified.is_some() {
        return (created, modified);
    }
    let meta = fs::metadata(path).ok();
    let created = meta
        .as_ref()
        .and_then(|value| value.created().ok())
        .map(system_time_to_iso);
    let modified = meta
        .as_ref()
        .and_then(|value| value.modified().ok())
        .map(system_time_to_iso);
    (created, modified)
}

fn system_time_to_iso(value: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(value)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn metadata_summary_short(item: &InventoryItem) -> String {
    format!(
        "{}，{}，{}",
        item.name,
        item.kind,
        format_size_text(item.size)
    )
}

fn metadata_representation(item: &InventoryItem) -> FileRepresentation {
    FileRepresentation {
        metadata: Some(metadata_summary_short(item)),
        short: None,
        long: None,
        source: "metadata".to_string(),
        degraded: false,
        confidence: None,
        keywords: Vec::new(),
    }
}

fn representation_from_organize_row(row: &Value) -> FileRepresentation {
    if let Some(value) = row.get("representation") {
        let parsed = FileRepresentation::from_value(value);
        if parsed.has_level(RepresentationLevel::Metadata)
            || parsed.has_level(RepresentationLevel::Short)
            || parsed.has_level(RepresentationLevel::Long)
        {
            return parsed;
        }
    }
    let summary = row
        .get("summary")
        .or_else(|| row.get("reason"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    FileRepresentation {
        metadata: summary.clone(),
        short: summary.clone(),
        long: summary,
        source: row
            .get("summarySource")
            .and_then(Value::as_str)
            .unwrap_or("organize")
            .to_string(),
        degraded: row
            .get("summaryDegraded")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        confidence: row
            .get("summaryConfidence")
            .and_then(Value::as_str)
            .map(str::to_string),
        keywords: row
            .get("summaryKeywords")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect(),
    }
}

fn collect_inventory_from_fs(
    root_path: &str,
    overrides: &HashMap<String, Vec<String>>,
) -> Vec<InventoryItem> {
    if root_path.trim().is_empty() {
        return Vec::new();
    }

    let mut output = Vec::new();
    let mut visited_dirs = 0usize;
    let mut stack = vec![(PathBuf::from(root_path), 0usize)];
    while let Some((current, depth)) = stack.pop() {
        visited_dirs += 1;
        if visited_dirs > FS_FALLBACK_MAX_DIRS {
            break;
        }
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                if depth < FS_FALLBACK_MAX_DEPTH && !should_skip_fallback_dir(&path) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            if output.len() >= FS_FALLBACK_MAX_FILES {
                return output;
            }
            let path_string = path.to_string_lossy().to_string();
            let key = normalize_path_key(&path_string);
            let mut category_path = overrides.get(&key).cloned().unwrap_or_default();
            if category_path.is_empty() {
                category_path.push("其他待定".to_string());
            }
            let category_id = category_id_from_path(&category_path);
            let parent_category_id = parent_category_id_from_path(&category_path);
            let mut item = InventoryItem {
                path: path_string.clone(),
                name: basename(&path_string),
                size: metadata.len(),
                created_at: metadata.created().ok().map(system_time_to_iso),
                modified_at: metadata.modified().ok().map(system_time_to_iso),
                kind: infer_kind(&path_string, "", ""),
                category_id,
                parent_category_id,
                category_path,
                representation: FileRepresentation::default(),
                risk: "unknown".to_string(),
            };
            item.representation = metadata_representation(&item);
            output.push(item);
        }
    }
    output
}

fn should_skip_fallback_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| {
            FS_FALLBACK_SKIP_DIRS
                .iter()
                .any(|blocked| name.eq_ignore_ascii_case(blocked))
        })
}

fn modified_age_text(item: &InventoryItem) -> String {
    let Some(modified_at) = item.modified_at.as_deref() else {
        return String::new();
    };
    let Ok(modified) = chrono::DateTime::parse_from_rfc3339(modified_at) else {
        return String::new();
    };
    let age = chrono::Utc::now().signed_duration_since(modified.with_timezone(&chrono::Utc));
    if age.num_days() > 0 {
        format!("modified {} days ago", age.num_days())
    } else if age.num_hours() > 0 {
        format!("modified {} hours ago", age.num_hours())
    } else if age.num_minutes() > 0 {
        format!("modified {} minutes ago", age.num_minutes())
    } else {
        "modified just now".to_string()
    }
}

fn load_summary_row(
    db_path: &Path,
    root_path: &str,
    path: &str,
    level: RepresentationLevel,
    missing_only: bool,
) -> Result<Option<Value>, String> {
    let row = persist::load_advisor_file_summary(
        db_path,
        &persist::create_root_path_key(root_path),
        &persist::create_root_path_key(path),
    )?;
    if !missing_only {
        return Ok(row);
    }
    Ok(row.filter(|value| {
        FileRepresentation::from_value(value.get("representation").unwrap_or(&Value::Null))
            .has_level(level)
    }))
}

fn read_existing_summary_item(
    db_path: &Path,
    root_path: &str,
    item: &InventoryItem,
    level: RepresentationLevel,
) -> Result<Option<Value>, String> {
    if let Some(row) = persist::load_advisor_file_summary(
        db_path,
        &persist::create_root_path_key(root_path),
        &persist::create_root_path_key(&item.path),
    )? {
        let representation =
            FileRepresentation::from_value(row.get("representation").unwrap_or(&Value::Null));
        if representation.has_level(level) {
            return Ok(Some(summary_row_to_read_item(&row, level)));
        }
    }
    if item.representation.has_level(level) {
        return Ok(Some(json!({
            "path": item.path,
            "name": item.name,
            "representation": item.representation.prune_to_level(level).to_value(),
            "warning": Value::Null,
        })));
    }
    Ok(None)
}

fn summary_row_to_tool_item(row: &Value, level: RepresentationLevel) -> Value {
    json!({
        "path": row.get("path").cloned().unwrap_or(Value::Null),
        "name": row.get("name").cloned().unwrap_or(Value::Null),
        "representation": FileRepresentation::from_value(
            row.get("representation").unwrap_or(&Value::Null),
        ).prune_to_level(level).to_value(),
        "warning": Value::Null,
    })
}

fn summary_row_to_read_item(row: &Value, level: RepresentationLevel) -> Value {
    json!({
        "path": row.get("path").cloned().unwrap_or(Value::Null),
        "name": row.get("name").cloned().unwrap_or(Value::Null),
        "representation": FileRepresentation::from_value(
            row.get("representation").unwrap_or(&Value::Null),
        ).prune_to_level(level).to_value(),
    })
}

fn rename_category(session: &mut Value, change: &Value) -> Result<Vec<Value>, String> {
    let source_category_id = required_change_str(change, "sourceCategoryId")?;
    let new_category_name = required_change_str(change, "newCategoryName")?;
    let source_path =
        category_path_from_category_id(session, source_category_id)?.ok_or_else(|| {
            "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
        })?;
    let mut target_path = source_path.clone();
    if let Some(last) = target_path.last_mut() {
        *last = new_category_name.to_string();
    }
    reclass_by_category_path(session, &source_path, &target_path)
}

fn merge_category_into_category(session: &mut Value, change: &Value) -> Result<Vec<Value>, String> {
    let source_category_id = required_change_str(change, "sourceCategoryId")?;
    let target_category_id = required_change_str(change, "targetCategoryId")?;
    let source_path =
        category_path_from_category_id(session, source_category_id)?.ok_or_else(|| {
            "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
        })?;
    let target_path =
        category_path_from_category_id(session, target_category_id)?.ok_or_else(|| {
            "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
        })?;
    reclass_by_category_path(session, &source_path, &target_path)
}

fn delete_empty_category(session: &mut Value, change: &Value) -> Result<Vec<Value>, String> {
    let source_category_id = required_change_str(change, "sourceCategoryId")?;
    let source_path =
        category_path_from_category_id(session, source_category_id)?.ok_or_else(|| {
            "当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。".to_string()
        })?;
    let overrides = super::types::inventory_overrides(session);
    if overrides.values().any(|path| path == &source_path) {
        return Err("当前分类下仍有文件，不能删除非空分类。".to_string());
    }
    Ok(Vec::new())
}

fn reclass_by_category_path(
    session: &mut Value,
    source_path: &[String],
    target_path: &[String],
) -> Result<Vec<Value>, String> {
    let overrides = super::types::inventory_overrides(session);
    let matched = overrides
        .iter()
        .filter(|(_, path)| path.as_slice() == source_path)
        .map(|(path, _)| json!({ "path": path }))
        .collect::<Vec<_>>();
    apply_reclassification_selection(session, &matched, target_path)
}

fn build_reclassification_change_summary(change: &Value, rollback_entries: &[Value]) -> String {
    let change_type = change
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let file_count = rollback_entries.len();
    format!("{change_type}: {file_count} items updated")
}

async fn summarize_batch_with_model(
    client: &reqwest::Client,
    route: &super::llm::AdvisorModelRoute,
    root_path: &str,
    batch: &[InventoryItem],
    level: RepresentationLevel,
    lang: &str,
) -> (Vec<Value>, Vec<SummaryFailedItem>) {
    let mut output = Vec::new();
    let mut errors = Vec::new();
    for item in batch {
        match summarize_item_with_model(client, route, root_path, item, level, lang).await {
            Ok(row) => output.push(row),
            Err(error) => errors.push(SummaryFailedItem {
                item: item.clone(),
                error,
            }),
        }
    }
    (output, errors)
}

async fn summarize_item_with_model(
    client: &reqwest::Client,
    route: &super::llm::AdvisorModelRoute,
    root_path: &str,
    item: &InventoryItem,
    level: RepresentationLevel,
    lang: &str,
) -> Result<Value, SummaryErrorRow> {
    let prompt = build_summary_prompt(item, level, lang);
    let api_format = detect_api_format(&route.endpoint);
    let request = build_completion_payload(
        api_format,
        &route.model,
        &[
            json!({ "role": "system", "content": summary_system_prompt(lang, level) }),
            json!({ "role": "user", "content": prompt }),
        ],
        None,
        0.0,
        DEFAULT_MAX_TOKENS,
    )
    .map_err(|reason| SummaryErrorRow {
        path: item.path.clone(),
        reason,
        retryable: false,
    })?;

    let mut last_error = None;
    for _attempt in 0..2 {
        let req = client
            .post(build_messages_url(&route.endpoint, api_format))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&request);
        let req = apply_auth_headers(req, api_format, &route.api_key);
        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                let body = match resp.text().await {
                    Ok(body) => body,
                    Err(err) => {
                        last_error = Some(SummaryErrorRow {
                            path: item.path.clone(),
                            reason: format!("summary_body_read_failed:{err}"),
                            retryable: false,
                        });
                        continue;
                    }
                };
                if !status.is_success() {
                    last_error = Some(SummaryErrorRow {
                        path: item.path.clone(),
                        reason: format_http_summary_error(status, &body),
                        retryable: is_retryable_status(status),
                    });
                    continue;
                }
                let assistant_text = parse_completion_response(api_format, status, &body)
                    .map(|parsed| parsed.assistant_text)
                    .unwrap_or_else(|_| body.clone());
                let Some((short, long)) = parse_summary_response(&assistant_text) else {
                    last_error = Some(SummaryErrorRow {
                        path: item.path.clone(),
                        reason: "summary_response_parse_failed".to_string(),
                        retryable: false,
                    });
                    continue;
                };
                let metadata = item
                    .representation
                    .metadata
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .or_else(|| Some(metadata_summary_short(item)));
                let representation = FileRepresentation {
                    metadata,
                    short: Some(short),
                    long: Some(long),
                    source: "model".to_string(),
                    degraded: false,
                    confidence: item.representation.confidence.clone(),
                    keywords: item.representation.keywords.clone(),
                };
                return Ok(build_summary_row(root_path, item, level, representation));
            }
            Err(err) => {
                last_error = Some(SummaryErrorRow {
                    path: item.path.clone(),
                    reason: format_transport_summary_error(&err),
                    retryable: is_retryable_transport_error(&err),
                });
            }
        }
    }
    Err(last_error.unwrap_or_else(|| SummaryErrorRow {
        path: item.path.clone(),
        reason: "summary_generation_failed".to_string(),
        retryable: false,
    }))
}

fn build_summary_row(
    root_path: &str,
    item: &InventoryItem,
    level: RepresentationLevel,
    representation: FileRepresentation,
) -> Value {
    let representation = representation.prune_to_level(level);
    json!({
        "rootPathKey": persist::create_root_path_key(root_path),
        "pathKey": persist::create_root_path_key(&item.path),
        "path": item.path,
        "name": item.name,
        "representation": representation.to_value(),
        "summaryShort": representation.short.clone(),
        "summaryNormal": representation.long.clone(),
        "source": representation.source,
        "representationLevel": level.as_str(),
        "updatedAt": now_iso(),
    })
}

fn is_retryable_transport_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect()
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn format_transport_summary_error(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        format!("summary_request_timeout:{err}")
    } else if err.is_connect() {
        format!("summary_request_connect_failed:{err}")
    } else {
        format!("summary_request_failed:{err}")
    }
}

fn format_http_summary_error(status: StatusCode, body: &str) -> String {
    let snippet = body.trim().chars().take(240).collect::<String>();
    if snippet.is_empty() {
        format!("summary_http_{}", status.as_u16())
    } else {
        format!("summary_http_{}:{}", status.as_u16(), snippet)
    }
}

async fn run_summary_round(
    route: &super::llm::AdvisorModelRoute,
    root_path: &str,
    items: &[InventoryItem],
    level: RepresentationLevel,
    lang: &str,
    batch_size: usize,
    max_concurrency: usize,
) -> Result<(Vec<Value>, Vec<SummaryFailedItem>), String> {
    if items.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())?;
    let semaphore = Arc::new(Semaphore::new(max_concurrency.max(1)));
    let mut handles = Vec::new();
    for chunk in items.chunks(batch_size.max(1)) {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| e.to_string())?;
        let client = client.clone();
        let route = route.clone();
        let root_path = root_path.to_string();
        let lang = lang.to_string();
        let batch = chunk.to_vec();
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            summarize_batch_with_model(&client, &route, &root_path, &batch, level, &lang).await
        }));
    }
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for handle in handles {
        let (mut batch_rows, mut batch_failures) = handle
            .await
            .map_err(|e| format!("summary_batch_join_failed:{e}"))?;
        rows.append(&mut batch_rows);
        failures.append(&mut batch_failures);
    }
    Ok((rows, failures))
}

async fn run_summary_scheduler(
    route: &super::llm::AdvisorModelRoute,
    root_path: &str,
    items: &[InventoryItem],
    level: RepresentationLevel,
    lang: &str,
    batch_size: usize,
    max_concurrency: usize,
) -> Result<(Vec<Value>, Vec<SummaryErrorRow>, SummarySchedulerStats), String> {
    let initial_concurrency = max_concurrency.max(1);
    if items.is_empty() {
        return Ok((
            Vec::new(),
            Vec::new(),
            SummarySchedulerStats {
                initial_concurrency,
                final_concurrency: initial_concurrency,
                retry_rounds: 0,
                degraded_concurrency: false,
            },
        ));
    }

    let mut completed_rows = Vec::new();
    let mut final_errors = Vec::new();
    let mut pending = items.to_vec();
    let mut current_concurrency = initial_concurrency;
    let mut retry_rounds = 0usize;
    let mut degraded_concurrency = false;
    let backoffs_ms = [500u64, 1000u64, 2000u64];

    loop {
        let (mut rows, failures) = run_summary_round(
            route,
            root_path,
            &pending,
            level,
            lang,
            batch_size,
            current_concurrency,
        )
        .await?;
        completed_rows.append(&mut rows);

        let mut retryable_items = Vec::new();
        for failure in failures {
            if failure.error.retryable {
                retryable_items.push(failure);
            } else {
                final_errors.push(failure.error);
            }
        }

        if retryable_items.is_empty() {
            break;
        }
        if current_concurrency == 1 {
            final_errors.extend(retryable_items.into_iter().map(|failure| failure.error));
            break;
        }

        retry_rounds += 1;
        let next_concurrency = (current_concurrency / 2).max(1);
        if next_concurrency < current_concurrency {
            degraded_concurrency = true;
        }
        let delay_ms = backoffs_ms
            .get(retry_rounds.saturating_sub(1))
            .copied()
            .unwrap_or(2000);
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        current_concurrency = next_concurrency;
        pending = retryable_items
            .into_iter()
            .map(|failure| failure.item)
            .collect();
    }

    Ok((
        completed_rows,
        final_errors,
        SummarySchedulerStats {
            initial_concurrency,
            final_concurrency: current_concurrency,
            retry_rounds,
            degraded_concurrency,
        },
    ))
}

fn summary_system_prompt(lang: &str, level: RepresentationLevel) -> String {
    let target = if level == RepresentationLevel::Short {
        "Return one short summary sentence and one longer summary sentence."
    } else {
        "Return one concise short summary sentence and one richer long summary sentence."
    };
    if lang.eq_ignore_ascii_case("en") {
        format!("{target} Return JSON only: {{\"summaryShort\":\"...\",\"summaryLong\":\"...\"}}")
    } else {
        format!("{target} 只返回 JSON：{{\"summaryShort\":\"...\",\"summaryLong\":\"...\"}}")
    }
}

fn build_summary_prompt(item: &InventoryItem, _level: RepresentationLevel, _lang: &str) -> String {
    format!(
        "path: {}\nname: {}\ncategory: {}\nsize: {}\nexisting metadata: {}\nexisting short summary: {}\nexisting long summary: {}",
        item.path,
        item.name,
        item.category_path.join(" / "),
        format_size_text(item.size),
        item.representation.metadata.clone().unwrap_or_default(),
        item.representation.short.clone().unwrap_or_default(),
        item.representation.long.clone().unwrap_or_default()
    )
}

fn parse_summary_response(body: &str) -> Option<(String, String)> {
    let parsed = serde_json::from_str::<Value>(body).ok().or_else(|| {
        let start = body.find('{')?;
        let end = body.rfind('}')?;
        serde_json::from_str::<Value>(&body[start..=end]).ok()
    })?;
    let content = parsed
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or(body);
    let payload = serde_json::from_str::<Value>(content).ok().or_else(|| {
        let start = content.find('{')?;
        let end = content.rfind('}')?;
        serde_json::from_str::<Value>(&content[start..=end]).ok()
    })?;
    Some((
        payload.get("summaryShort")?.as_str()?.trim().to_string(),
        payload.get("summaryLong")?.as_str()?.trim().to_string(),
    ))
}

fn ensure_session_owned(record: &Value, session_id: &str, label: &str) -> Result<(), String> {
    let owner = record
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if owner == session_id {
        Ok(())
    } else {
        Err(format!("{label} does not belong to the active session"))
    }
}

fn normalize_view_type(value: &str) -> &'static str {
    match value.trim() {
        "sizeTree" => "sizeTree",
        "timeTree" => "timeTree",
        "executionTree" => "executionTree",
        "partialTree" => "partialTree",
        _ => "summaryTree",
    }
}

fn build_tree_text(tree: &Value) -> String {
    let Some(obj) = tree.as_object() else {
        return String::new();
    };
    let mut lines = Vec::new();
    append_tree_text_lines(obj, 0, &mut lines);
    lines.join("\n")
}

fn append_tree_text_lines(
    node: &serde_json::Map<String, Value>,
    depth: usize,
    lines: &mut Vec<String>,
) {
    let indent = "  ".repeat(depth);
    let name = node.get("name").and_then(Value::as_str).unwrap_or("-");
    let count = node.get("itemCount").and_then(Value::as_u64).unwrap_or(0);
    lines.push(format!("{indent}{name}: {count}"));
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children.iter().take(12) {
            if let Some(child_obj) = child.as_object() {
                append_tree_text_lines(child_obj, depth + 1, lines);
            }
        }
    }
}

fn infer_preference_kind(text: &str) -> String {
    let lower = normalize_message(text);
    if ["归档", "archive"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        "archive_preferred".to_string()
    } else if ["不要删", "别删", "保留", "keep", "don't delete"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        "keep".to_string()
    } else if ["总是", "始终", "always"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        "long_term_preference".to_string()
    } else {
        "general_preference".to_string()
    }
}

fn summarize_find_query(args: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(category_ids) = args.get("categoryIds").and_then(Value::as_array) {
        let labels = category_ids
            .iter()
            .filter_map(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .collect::<Vec<_>>();
        if !labels.is_empty() {
            parts.push(labels.join(", "));
        }
    }
    for key in ["nameQuery", "nameExact", "pathContains"] {
        if let Some(value) = args
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            parts.push(value.to_string());
        }
    }
    if parts.is_empty() {
        "当前筛选".to_string()
    } else {
        parts.join(" | ")
    }
}

fn filter_inventory_by_args(inventory: &[InventoryItem], args: &Value) -> Vec<InventoryItem> {
    let category_ids = args
        .get("categoryIds")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(normalize_message)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let name_query = args
        .get("nameQuery")
        .and_then(Value::as_str)
        .map(normalize_message)
        .unwrap_or_default();
    let name_exact = args
        .get("nameExact")
        .and_then(Value::as_str)
        .map(normalize_message)
        .unwrap_or_default();
    let path_contains = args
        .get("pathContains")
        .and_then(Value::as_str)
        .map(normalize_message)
        .unwrap_or_default();
    let extensions = args
        .get("extensions")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let paths = args
        .get("paths")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(normalize_path_key)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let min_size = args
        .get("minSizeBytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let max_size = args.get("maxSizeBytes").and_then(Value::as_u64);
    let older_than_days = args.get("olderThanDays").and_then(Value::as_i64);
    let newer_than_days = args.get("newerThanDays").and_then(Value::as_i64);
    let sort_by = args.get("sortBy").and_then(Value::as_str).unwrap_or("size");
    let sort_order = args
        .get("sortOrder")
        .and_then(Value::as_str)
        .unwrap_or("desc");
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(24) as usize;

    let mut rows = inventory
        .iter()
        .filter(|item| {
            if !category_ids.is_empty()
                && !category_ids.iter().any(|needle| {
                    item.category_id == *needle
                        || item
                            .category_path
                            .iter()
                            .map(|value| normalize_message(value))
                            .any(|part| part == *needle)
                })
            {
                return false;
            }
            if !name_query.is_empty()
                && !normalize_message(&item.name).contains(&name_query)
                && !normalize_message(&item.path).contains(&name_query)
            {
                return false;
            }
            if !name_exact.is_empty() && normalize_message(&item.name) != name_exact {
                return false;
            }
            if !path_contains.is_empty() && !normalize_message(&item.path).contains(&path_contains)
            {
                return false;
            }
            if !paths.is_empty()
                && !paths
                    .iter()
                    .any(|path| path == &normalize_path_key(&item.path))
            {
                return false;
            }
            if !extensions.is_empty() {
                let ext = Path::new(&item.path)
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(|value| format!(".{}", value.to_ascii_lowercase()))
                    .unwrap_or_default();
                if !extensions.iter().any(|value| value == &ext) {
                    return false;
                }
            }
            if item.size < min_size {
                return false;
            }
            if max_size.is_some_and(|value| item.size > value) {
                return false;
            }
            if let Some(days) = older_than_days {
                let Some(modified_at) = item.modified_at.as_deref() else {
                    return false;
                };
                let Ok(modified) = chrono::DateTime::parse_from_rfc3339(modified_at) else {
                    return false;
                };
                if chrono::Utc::now()
                    .signed_duration_since(modified.with_timezone(&chrono::Utc))
                    .num_days()
                    < days
                {
                    return false;
                }
            }
            if let Some(days) = newer_than_days {
                let Some(modified_at) = item.modified_at.as_deref() else {
                    return false;
                };
                let Ok(modified) = chrono::DateTime::parse_from_rfc3339(modified_at) else {
                    return false;
                };
                if chrono::Utc::now()
                    .signed_duration_since(modified.with_timezone(&chrono::Utc))
                    .num_days()
                    > days
                {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect::<Vec<_>>();

    rows.sort_by(|left, right| {
        let ordering = match sort_by {
            "name" => left.name.cmp(&right.name),
            "modifiedAt" => left
                .modified_at
                .cmp(&right.modified_at)
                .then_with(|| left.path.cmp(&right.path)),
            _ => left
                .size
                .cmp(&right.size)
                .then_with(|| left.path.cmp(&right.path)),
        };
        if sort_order.eq_ignore_ascii_case("asc") {
            ordering
        } else {
            ordering.reverse()
        }
    });
    rows.truncate(limit);
    rows
}

fn format_size_text(size: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let size_f = size as f64;
    if size_f >= GB {
        format!("{:.1} GB", size_f / GB)
    } else if size_f >= MB {
        format!("{:.1} MB", size_f / MB)
    } else if size_f >= KB {
        format!("{:.1} KB", size_f / KB)
    } else {
        format!("{size} B")
    }
}

fn value_to_inventory_item(value: &Value) -> InventoryItem {
    InventoryItem {
        path: value
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        size: value.get("size").and_then(Value::as_u64).unwrap_or(0),
        created_at: value
            .get("createdAt")
            .and_then(Value::as_str)
            .map(str::to_string),
        modified_at: value
            .get("modifiedAt")
            .and_then(Value::as_str)
            .map(str::to_string),
        kind: value
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("review")
            .to_string(),
        category_id: value
            .get("categoryId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        parent_category_id: value
            .get("parentCategoryId")
            .and_then(Value::as_str)
            .map(str::to_string),
        category_path: array_of_strings(value.get("categoryPath")),
        representation: FileRepresentation::from_value(
            value.get("representation").unwrap_or(&Value::Null),
        ),
        risk: value
            .get("risk")
            .and_then(Value::as_str)
            .unwrap_or("medium")
            .to_string(),
    }
}

fn normalize_plan_action(action: &str) -> &'static str {
    match action.trim() {
        "delete" => "recycle",
        "archive" => "archive",
        "move" => "move",
        "keep" => "keep",
        "review" => "review",
        _ => "review",
    }
}

fn action_label(action: &str, lang: &str) -> String {
    match action.trim() {
        "delete" => local_text(lang, "回收", "Recycle").to_string(),
        "archive" => local_text(lang, "归档", "Archive").to_string(),
        "move" => local_text(lang, "移动", "Move").to_string(),
        "keep" => local_text(lang, "保留", "Keep").to_string(),
        _ => local_text(lang, "复核", "Review").to_string(),
    }
}

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
