use crate::advisor_runtime::tools::ToolService;
use crate::advisor_runtime::types::{
    local_text, CARD_EXECUTION, CARD_PLAN_PREVIEW, CARD_PREFERENCE, CARD_RECLASS, CARD_TREE,
    WORKFLOW_EXECUTE_READY, WORKFLOW_PREVIEW_READY, WORKFLOW_UNDERSTAND,
};
use crate::backend::{resolve_search_api_key, AppState};
use crate::llm_protocol::ParsedToolCall;
use crate::organizer_runtime::OrganizerDiagnostics;
use crate::persist;
use crate::web_search::{format_web_search_context, parse_web_search_request, tavily_search};
use async_trait::async_trait;
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ToolId {
    WebSearch,
    GetDirectoryOverview,
    FindFiles,
    SummarizeFiles,
    ReadOnlyFileSummaries,
    CapturePreference,
    ListPreferences,
    PreviewPlan,
    ExecutePlan,
    RollbackPlan,
    ApplyReclassification,
    RollbackReclassification,
    SubmitOrganizeResult,
}

#[derive(Clone, Debug)]
pub(crate) struct ToolSpec {
    pub id: ToolId,
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Option<Value>,
}

impl ToolSpec {
    pub(crate) fn definition(&self) -> Value {
        let mut function = json!({
            "name": self.name,
            "description": self.description,
        });
        if let Some(parameters) = &self.parameters {
            function["parameters"] = parameters.clone();
        }
        json!({
            "type": "function",
            "function": function,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolWorkflow {
    Advisor,
    Organizer,
}

pub(crate) struct ToolContext<'a> {
    pub workflow: ToolWorkflow,
    pub stage: &'a str,
    pub session: Option<&'a Value>,
    pub bootstrap_turn: bool,
    pub response_language: &'a str,
    pub web_search_allowed: bool,
    pub search_remaining: usize,
}

pub(crate) struct ToolExecutionContext<'a> {
    pub workflow: ToolWorkflow,
    pub stage: &'a str,
    pub session: Option<&'a mut Value>,
    pub bootstrap_turn: bool,
    pub response_language: &'a str,
    pub web_search_allowed: bool,
    pub search_remaining: usize,
    pub state: Option<&'a AppState>,
    pub search_api_key: Option<&'a str>,
    pub diagnostics: Option<&'a OrganizerDiagnostics>,
}

#[derive(Clone, Debug)]
pub(crate) struct ToolResult {
    pub result: Value,
    pub card: Option<Value>,
    pub cards: Vec<Value>,
    pub diagnostics: Option<Value>,
}

impl ToolResult {
    pub(crate) fn result(result: Value) -> Self {
        Self {
            result,
            card: None,
            cards: Vec::new(),
            diagnostics: None,
        }
    }

    pub(crate) fn with_card(mut self, card: Value) -> Self {
        self.card = Some(card);
        self
    }

    pub(crate) fn with_cards(mut self, cards: Vec<Value>) -> Self {
        self.cards = cards;
        self
    }

    pub(crate) fn with_diagnostics(mut self, diagnostics: Value) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }

    pub(crate) fn blocked(message: impl Into<String>) -> Self {
        Self::result(json!({
            "ok": false,
            "blocked": true,
            "message": message.into(),
        }))
    }

    pub(crate) fn envelope(&self) -> Value {
        let mut out = json!({ "result": self.result });
        if let Some(card) = &self.card {
            out["card"] = card.clone();
        }
        if !self.cards.is_empty() {
            out["cards"] = Value::Array(self.cards.clone());
        }
        if let Some(diagnostics) = &self.diagnostics {
            out["diagnostics"] = diagnostics.clone();
        }
        out
    }
}

#[async_trait]
pub(crate) trait LlmTool: Send + Sync {
    fn id(&self) -> ToolId;
    fn spec(&self) -> ToolSpec;
    fn available(&self, ctx: &ToolContext<'_>) -> bool;
    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolResult, String>;
}

pub(crate) struct ToolRegistry {
    tools: Vec<Box<dyn LlmTool>>,
}

impl ToolRegistry {
    pub(crate) fn new() -> Self {
        let mut registry = Self { tools: Vec::new() };
        registry.register_all_tools();
        registry
    }

    fn register_all_tools(&mut self) {
        self.register(WebSearchTool);
        self.register(GetDirectoryOverviewTool);
        self.register(FindFilesTool);
        self.register(SummarizeFilesTool);
        self.register(ReadOnlyFileSummariesTool);
        self.register(CapturePreferenceTool);
        self.register(ListPreferencesTool);
        self.register(PreviewPlanTool);
        self.register(ExecutePlanTool);
        self.register(RollbackPlanTool);
        self.register(ApplyReclassificationTool);
        self.register(RollbackReclassificationTool);
        self.register(SubmitOrganizeResultTool);
    }

    fn register<T>(&mut self, tool: T)
    where
        T: LlmTool + 'static,
    {
        self.tools.push(Box::new(tool));
    }

    pub(crate) fn definitions(&self, ctx: &ToolContext<'_>) -> Vec<Value> {
        let _ = ctx.response_language;
        self.tools
            .iter()
            .filter(|tool| tool.available(ctx))
            .map(|tool| tool.spec().definition())
            .collect()
    }

    #[allow(dead_code)]
    pub(crate) fn available_names(&self, ctx: &ToolContext<'_>) -> Vec<&'static str> {
        self.tools
            .iter()
            .filter(|tool| tool.available(ctx))
            .map(|tool| tool.spec().name)
            .collect()
    }

    pub(crate) fn id_for_name(&self, name: &str) -> Option<ToolId> {
        self.tools
            .iter()
            .find(|tool| tool.spec().name == name)
            .map(|tool| tool.spec().id)
    }

    pub(crate) async fn dispatch(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        call: &ParsedToolCall,
    ) -> Result<ToolResult, String> {
        let available_ctx = ToolContext {
            workflow: ctx.workflow,
            stage: ctx.stage,
            session: ctx.session.as_deref(),
            bootstrap_turn: ctx.bootstrap_turn,
            response_language: ctx.response_language,
            web_search_allowed: ctx.web_search_allowed,
            search_remaining: ctx.search_remaining,
        };
        let Some(tool) = self
            .tools
            .iter()
            .find(|tool| tool.spec().name == call.name && tool.available(&available_ctx))
        else {
            return Err(format!(
                "unsupported {} tool: {}",
                workflow_name(ctx.workflow),
                call.name
            ));
        };
        tool.execute(ctx, &call.arguments).await
    }
}

fn workflow_name(workflow: ToolWorkflow) -> &'static str {
    match workflow {
        ToolWorkflow::Advisor => "advisor",
        ToolWorkflow::Organizer => "organizer",
    }
}

#[allow(dead_code)]
fn advisor_session_enabled(ctx: &ToolContext<'_>) -> bool {
    ctx.session
        .and_then(|session| session.get("webSearchEnabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[allow(dead_code)]
fn advisor_non_bootstrap(ctx: &ToolContext<'_>) -> bool {
    ctx.workflow == ToolWorkflow::Advisor && !ctx.bootstrap_turn
}

#[allow(dead_code)]
fn advisor_any_turn(ctx: &ToolContext<'_>) -> bool {
    ctx.workflow == ToolWorkflow::Advisor
}

#[allow(dead_code)]
fn advisor_stage_is(ctx: &ToolContext<'_>, stages: &[&str]) -> bool {
    advisor_non_bootstrap(ctx) && stages.contains(&ctx.stage)
}

fn tool_spec(
    id: ToolId,
    name: &'static str,
    description: &'static str,
    parameters: Value,
) -> ToolSpec {
    ToolSpec {
        id,
        name,
        description,
        parameters: Some(parameters),
    }
}

fn no_arg_tool_spec(id: ToolId, name: &'static str, description: &'static str) -> ToolSpec {
    ToolSpec {
        id,
        name,
        description,
        parameters: None,
    }
}

struct WebSearchTool;

#[async_trait]
impl LlmTool for WebSearchTool {
    fn id(&self) -> ToolId {
        ToolId::WebSearch
    }

    fn spec(&self) -> ToolSpec {
        tool_spec(
            self.id(),
            "web_search",
            "Search the web for external context that is missing from local evidence.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "reason": { "type": "string" }
                },
                "required": ["query"]
            }),
        )
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        match ctx.workflow {
            ToolWorkflow::Advisor => ctx.web_search_allowed && advisor_session_enabled(ctx),
            ToolWorkflow::Organizer => ctx.web_search_allowed && ctx.search_remaining > 0,
        }
    }

    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolResult, String> {
        if !ctx.web_search_allowed {
            return Ok(ToolResult::blocked(local_text(
                ctx.response_language,
                "当前轮次不能联网搜索，请基于已有证据继续。",
                "Web search is unavailable for the current step. Continue from existing evidence.",
            )));
        }
        if ctx.workflow == ToolWorkflow::Organizer && ctx.search_remaining == 0 {
            return Ok(ToolResult::blocked(local_text(
                ctx.response_language,
                "联网搜索额度已用完，请基于已有证据提交最终结果。",
                "Web search budget is exhausted. Submit the final result from existing evidence.",
            )));
        }
        let request_payload = json!({
            "action": "web_search",
            "query": args.get("query").cloned().unwrap_or(Value::Null),
            "reason": args.get("reason").cloned().unwrap_or(Value::Null),
        });
        let Some(request) = parse_web_search_request(&request_payload) else {
            return match ctx.workflow {
                ToolWorkflow::Advisor => Err(local_text(
                    ctx.response_language,
                    "web_search 需要非空 query。",
                    "web_search requires a non-empty query.",
                )
                .to_string()),
                ToolWorkflow::Organizer => Ok(ToolResult::result(json!({
                    "ok": false,
                    "error": "web_search requires a non-empty query",
                }))
                .with_diagnostics(json!({ "searchConsumed": true }))),
            };
        };

        let key = match ctx.workflow {
            ToolWorkflow::Advisor => {
                let state = ctx
                    .state
                    .ok_or_else(|| "advisor web_search is missing AppState".to_string())?;
                match resolve_search_api_key(state) {
                    Ok(key) if !key.trim().is_empty() => key,
                    Ok(_) | Err(_) => {
                        return Ok(ToolResult::blocked(local_text(
                            ctx.response_language,
                            "当前没有可用的联网搜索密钥，请基于已有本地证据继续。",
                            "No web search API key is available. Continue from local evidence.",
                        )))
                    }
                }
            }
            ToolWorkflow::Organizer => ctx.search_api_key.unwrap_or_default().to_string(),
        };
        if key.trim().is_empty() {
            return Ok(ToolResult::blocked(local_text(
                ctx.response_language,
                "当前没有可用的联网搜索密钥，请基于已有证据继续。",
                "No web search API key is available. Continue from existing evidence.",
            )));
        }

        match tavily_search(&key, &request).await {
            Ok(trace) => {
                if ctx.workflow == ToolWorkflow::Organizer {
                    if let Some(diagnostics) = ctx.diagnostics {
                        diagnostics.web_search_succeeded(ctx.stage, &trace);
                    }
                }
                let result = match ctx.workflow {
                    ToolWorkflow::Advisor => json!({
                        "query": trace.query,
                        "reason": trace.reason,
                        "answer": trace.answer,
                        "results": trace.results,
                        "formattedContext": format_web_search_context(&trace, ctx.response_language),
                    }),
                    ToolWorkflow::Organizer => json!({
                        "ok": true,
                        "query": trace.query,
                        "reason": trace.reason,
                        "answer": trace.answer,
                        "results": trace.results,
                        "formattedContext": format_web_search_context(&trace, ctx.response_language),
                    }),
                };
                let out = ToolResult::result(result);
                if ctx.workflow == ToolWorkflow::Organizer {
                    Ok(out.with_diagnostics(json!({ "searchConsumed": true })))
                } else {
                    Ok(out)
                }
            }
            Err(err) => match ctx.workflow {
                ToolWorkflow::Advisor => Err(err),
                ToolWorkflow::Organizer => {
                    if let Some(diagnostics) = ctx.diagnostics {
                        diagnostics.web_search_failed(ctx.stage, args, &err);
                    }
                    Ok(ToolResult::result(json!({
                        "ok": false,
                        "error": err,
                    }))
                    .with_diagnostics(json!({ "searchConsumed": true })))
                }
            },
        }
    }
}

struct SubmitOrganizeResultTool;

#[async_trait]
impl LlmTool for SubmitOrganizeResultTool {
    fn id(&self) -> ToolId {
        ToolId::SubmitOrganizeResult
    }

    fn spec(&self) -> ToolSpec {
        tool_spec(
            self.id(),
            "submit_organize_result",
            "Submit the final category tree and assignments for this batch.",
            json!({
                "type": "object",
                "properties": {
                    "tree": { "type": "object" },
                    "assignments": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "reason": { "type": "string" },
                                "itemId": { "type": "string" },
                                "leafNodeId": { "type": "string" },
                                "categoryPath": {
                                    "type": "array",
                                    "items": { "type": "string" }
                                }
                            },
                            "required": ["itemId"]
                        }
                    }
                },
                "required": ["tree", "assignments"]
            }),
        )
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        ctx.workflow == ToolWorkflow::Organizer
    }

    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolResult, String> {
        if ctx.workflow != ToolWorkflow::Organizer {
            return Ok(ToolResult::blocked(
                "submit_organize_result 只能在 organizer workflow 中使用。",
            ));
        }
        Ok(ToolResult::result(parse_submit_organize_result_arguments(
            args,
        )?))
    }
}

fn parse_submit_organize_result_arguments(value: &Value) -> Result<Value, String> {
    let tree = value
        .get("tree")
        .cloned()
        .ok_or_else(|| "submit_organize_result is missing tree".to_string())?;
    if !tree.is_object() {
        return Err("submit_organize_result tree must be an object".to_string());
    }
    let assignments = value
        .get("assignments")
        .cloned()
        .ok_or_else(|| "submit_organize_result is missing assignments".to_string())?;
    if !assignments.is_array() {
        return Err("submit_organize_result assignments must be an array".to_string());
    }
    Ok(json!({
        "tree": tree,
        "assignments": assignments,
    }))
}

struct GetDirectoryOverviewTool;

#[async_trait]
impl LlmTool for GetDirectoryOverviewTool {
    fn id(&self) -> ToolId {
        ToolId::GetDirectoryOverview
    }

    fn spec(&self) -> ToolSpec {
        tool_spec(
            self.id(),
            "get_directory_overview",
            "查看当前目录树视图和整体概览。",
            json!({
                "type": "object",
                "properties": {
                    "viewType": { "type": "string", "enum": ["summaryTree", "sizeTree", "timeTree", "executionTree", "partialTree"] },
                    "rootCategoryId": { "type": "string" },
                    "maxDepth": { "type": "integer" }
                }
            }),
        )
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        advisor_any_turn(ctx)
    }

    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolResult, String> {
        let state = ctx
            .state
            .ok_or_else(|| "advisor tool is missing AppState".to_string())?;
        let session = ctx
            .session
            .as_deref_mut()
            .ok_or_else(|| "advisor tool is missing session".to_string())?;
        let service = ToolService::new(state);
        let result = service.get_directory_overview_tool(
            session,
            args.get("viewType").and_then(Value::as_str),
            args.get("rootCategoryId").and_then(Value::as_str),
            args.get("maxDepth").and_then(Value::as_u64),
        )?;
        let card = json!({
            "cardType": CARD_TREE,
            "status": "ready",
            "title": local_text(ctx.response_language, "当前分类树", "Current Tree"),
            "body": {
                "tree": result.get("tree").cloned().unwrap_or(Value::Null),
                "stats": {
                    "itemCount": session.pointer("/contextBar/directorySummary/itemCount").cloned().unwrap_or(Value::from(0))
                }
            },
            "actions": []
        });
        Ok(ToolResult::result(result).with_card(card))
    }
}

struct FindFilesTool;

#[async_trait]
impl LlmTool for FindFilesTool {
    fn id(&self) -> ToolId {
        ToolId::FindFiles
    }

    fn spec(&self) -> ToolSpec {
        tool_spec(
            self.id(),
            "find_files",
            "按类别、名称、路径、大小和时间筛选文件候选集，并返回 selectionId。",
            json!({
                "type": "object",
                "properties": {
                    "categoryIds": { "type": "array", "items": { "type": "string" } },
                    "nameQuery": { "type": "string" },
                    "nameExact": { "type": "string" },
                    "pathContains": { "type": "string" },
                    "extensions": { "type": "array", "items": { "type": "string" } },
                    "minSizeBytes": { "type": "integer" },
                    "maxSizeBytes": { "type": "integer" },
                    "olderThanDays": { "type": "integer" },
                    "newerThanDays": { "type": "integer" },
                    "sortBy": { "type": "string", "enum": ["name", "size", "modifiedAt"] },
                    "sortOrder": { "type": "string", "enum": ["asc", "desc"] },
                    "limit": { "type": "integer" }
                }
            }),
        )
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        advisor_stage_is(ctx, &[WORKFLOW_UNDERSTAND, WORKFLOW_PREVIEW_READY])
    }

    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolResult, String> {
        if ctx.stage != WORKFLOW_UNDERSTAND && ctx.stage != WORKFLOW_PREVIEW_READY {
            return Ok(ToolResult::blocked(
                "当前阶段不适合重新筛选，请先处理已有预览或回到理解阶段。",
            ));
        }
        let state = ctx
            .state
            .ok_or_else(|| "advisor tool is missing AppState".to_string())?;
        let session = ctx
            .session
            .as_deref_mut()
            .ok_or_else(|| "advisor tool is missing session".to_string())?;
        let service = ToolService::new(state);
        let result = service.find_files_by_args(session, args)?;
        if let Some(obj) = session.as_object_mut() {
            obj.insert(
                "activeSelectionId".to_string(),
                result.get("selectionId").cloned().unwrap_or(Value::Null),
            );
            obj.insert("activePreviewId".to_string(), Value::Null);
            obj.insert(
                "workflowStage".to_string(),
                Value::String(WORKFLOW_PREVIEW_READY.to_string()),
            );
        }
        Ok(ToolResult::result(result))
    }
}

struct SummarizeFilesTool;

#[async_trait]
impl LlmTool for SummarizeFilesTool {
    fn id(&self) -> ToolId {
        ToolId::SummarizeFiles
    }

    fn spec(&self) -> ToolSpec {
        tool_spec(
            self.id(),
            "summarize_files",
            "为一批文件补摘要或刷新摘要，并写入顾问摘要库。",
            json!({
                "type": "object",
                "properties": {
                    "paths": { "type": "array", "items": { "type": "string" } },
                    "categoryIds": { "type": "array", "items": { "type": "string" } },
                    "representationLevel": { "type": "string", "enum": ["metadata", "short", "long"] },
                    "missingOnly": { "type": "boolean" },
                    "batchSize": { "type": "integer" },
                    "maxConcurrency": { "type": "integer" }
                }
            }),
        )
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        advisor_any_turn(ctx)
    }

    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolResult, String> {
        let state = ctx
            .state
            .ok_or_else(|| "advisor tool is missing AppState".to_string())?;
        let session = ctx
            .session
            .as_deref_mut()
            .ok_or_else(|| "advisor tool is missing session".to_string())?;
        let service = ToolService::new(state);
        Ok(ToolResult::result(
            service.summarize_files_tool(session, args).await?,
        ))
    }
}

struct ReadOnlyFileSummariesTool;

#[async_trait]
impl LlmTool for ReadOnlyFileSummariesTool {
    fn id(&self) -> ToolId {
        ToolId::ReadOnlyFileSummaries
    }

    fn spec(&self) -> ToolSpec {
        tool_spec(
            self.id(),
            "read_only_file_summaries",
            "只读取已有摘要，不触发生成。",
            json!({
                "type": "object",
                "properties": {
                    "paths": { "type": "array", "items": { "type": "string" } },
                    "categoryIds": { "type": "array", "items": { "type": "string" } },
                    "representationLevel": { "type": "string", "enum": ["metadata", "short", "long"] },
                    "limit": { "type": "integer" }
                }
            }),
        )
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        advisor_any_turn(ctx)
    }

    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolResult, String> {
        let state = ctx
            .state
            .ok_or_else(|| "advisor tool is missing AppState".to_string())?;
        let session = ctx
            .session
            .as_deref_mut()
            .ok_or_else(|| "advisor tool is missing session".to_string())?;
        let service = ToolService::new(state);
        Ok(ToolResult::result(
            service.read_only_file_summaries_tool(session, args)?,
        ))
    }
}

struct CapturePreferenceTool;

#[async_trait]
impl LlmTool for CapturePreferenceTool {
    fn id(&self) -> ToolId {
        ToolId::CapturePreference
    }

    fn spec(&self) -> ToolSpec {
        tool_spec(
            self.id(),
            "capture_preference",
            "提炼并暂存用户偏好，后续由卡片动作确认保存。",
            json!({
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "enum": ["session", "global"] },
                    "text": { "type": "string" },
                    "sourceMessage": { "type": "string" }
                },
                "required": ["text", "sourceMessage"]
            }),
        )
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        advisor_non_bootstrap(ctx)
    }

    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolResult, String> {
        let state = ctx
            .state
            .ok_or_else(|| "advisor tool is missing AppState".to_string())?;
        let session = ctx
            .session
            .as_deref_mut()
            .ok_or_else(|| "advisor tool is missing session".to_string())?;
        let service = ToolService::new(state);
        let result = service.capture_preference(
            session,
            args.get("scope")
                .and_then(Value::as_str)
                .unwrap_or("session"),
            args.get("text").and_then(Value::as_str).unwrap_or_default(),
            args.get("sourceMessage")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )?;
        let card = json!({
            "cardType": CARD_PREFERENCE,
            "status": "pending",
            "title": local_text(ctx.response_language, "偏好提炼", "Preference Draft"),
            "body": {
                "preferenceId": result.get("preferenceId").cloned().unwrap_or(Value::Null),
                "kind": result.get("kind").cloned().unwrap_or(Value::Null),
                "suggestedScope": result.get("suggestedScope").cloned().unwrap_or(Value::String("session".to_string())),
                "summary": result.get("summary").cloned().unwrap_or(Value::String(String::new())),
                "message": result.get("sourceMessage").cloned().unwrap_or(Value::String(String::new())),
            },
            "actions": [
                { "action": "apply_preference", "label": local_text(ctx.response_language, "应用 / 保存", "Apply / Save"), "variant": "primary" },
                { "action": "dismiss_preference", "label": local_text(ctx.response_language, "撤销", "Dismiss"), "variant": "secondary" }
            ]
        });
        Ok(ToolResult::result(result).with_card(card))
    }
}

struct ListPreferencesTool;

#[async_trait]
impl LlmTool for ListPreferencesTool {
    fn id(&self) -> ToolId {
        ToolId::ListPreferences
    }

    fn spec(&self) -> ToolSpec {
        no_arg_tool_spec(self.id(), "list_preferences", "读取当前会话和全局偏好。")
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        advisor_any_turn(ctx)
    }

    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        _args: &Value,
    ) -> Result<ToolResult, String> {
        let state = ctx
            .state
            .ok_or_else(|| "advisor tool is missing AppState".to_string())?;
        let session_id = ctx
            .session
            .as_deref()
            .and_then(|session| session.get("sessionId"))
            .and_then(Value::as_str);
        let service = ToolService::new(state);
        Ok(ToolResult::result(
            service.list_preferences_tool(session_id)?,
        ))
    }
}

struct PreviewPlanTool;

#[async_trait]
impl LlmTool for PreviewPlanTool {
    fn id(&self) -> ToolId {
        ToolId::PreviewPlan
    }

    fn spec(&self) -> ToolSpec {
        tool_spec(
            self.id(),
            "preview_plan",
            "根据 plan JSON 和 selectionId 生成文件级预览。",
            json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "object",
                        "properties": {
                            "intentSummary": { "type": "string" },
                            "targets": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "selectionId": { "type": "string" },
                                        "action": { "type": "string", "enum": ["archive", "move", "keep", "review", "delete"] }
                                    },
                                    "required": ["selectionId", "action"]
                                }
                            }
                        },
                        "required": ["targets"]
                    }
                },
                "required": ["plan"]
            }),
        )
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        advisor_stage_is(ctx, &[WORKFLOW_PREVIEW_READY, WORKFLOW_EXECUTE_READY])
    }

    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolResult, String> {
        if ctx.stage != WORKFLOW_PREVIEW_READY && ctx.stage != WORKFLOW_EXECUTE_READY {
            return Ok(ToolResult::blocked(
                "当前阶段缺少有效筛选结果，请先调用 find_files。",
            ));
        }
        let state = ctx
            .state
            .ok_or_else(|| "advisor tool is missing AppState".to_string())?;
        let session = ctx
            .session
            .as_deref_mut()
            .ok_or_else(|| "advisor tool is missing session".to_string())?;
        let service = ToolService::new(state);
        let job = service.preview_plan_from_value(session, args.get("plan").unwrap_or(args))?;
        let preview = job.get("preview").cloned().unwrap_or_else(|| json!({}));
        if let Some(obj) = session.as_object_mut() {
            obj.insert(
                "workflowStage".to_string(),
                Value::String(WORKFLOW_EXECUTE_READY.to_string()),
            );
            obj.insert(
                "activeSelectionId".to_string(),
                job.get("selectionId").cloned().unwrap_or(Value::Null),
            );
            obj.insert(
                "activePreviewId".to_string(),
                Value::String(
                    preview
                        .get("previewId")
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| {
                            job.get("jobId").and_then(Value::as_str).unwrap_or_default()
                        })
                        .to_string(),
                ),
            );
        }
        let card = json!({
            "cardType": CARD_PLAN_PREVIEW,
            "status": "ready",
            "title": local_text(ctx.response_language, "计划预览", "Plan Preview"),
            "body": {
                "previewId": preview.get("previewId").cloned().unwrap_or(job.get("jobId").cloned().unwrap_or(Value::Null)),
                "jobId": job.get("jobId").cloned().unwrap_or(Value::Null),
                "selectionId": job.get("selectionId").cloned().unwrap_or(Value::Null),
                "intentSummary": preview.get("intentSummary").cloned().unwrap_or(Value::String(String::new())),
                "summary": preview.get("summary").cloned().unwrap_or_else(|| json!({})),
                "entries": preview.get("entries").cloned().unwrap_or_else(|| json!([])),
            },
            "actions": if preview.pointer("/summary/canExecute").and_then(Value::as_u64).unwrap_or(0) > 0 {
                vec![json!({ "action": "execute_plan", "label": local_text(ctx.response_language, "执行", "Execute"), "variant": "primary" })]
            } else {
                Vec::<Value>::new()
            }
        });
        Ok(ToolResult::result(preview).with_card(card))
    }
}

struct ExecutePlanTool;

#[async_trait]
impl LlmTool for ExecutePlanTool {
    fn id(&self) -> ToolId {
        ToolId::ExecutePlan
    }

    fn spec(&self) -> ToolSpec {
        tool_spec(
            self.id(),
            "execute_plan",
            "执行已经通过预览的计划。",
            json!({
                "type": "object",
                "properties": {
                    "previewId": { "type": "string" },
                    "intentSummary": { "type": "string" }
                },
                "required": ["previewId"]
            }),
        )
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        advisor_stage_is(ctx, &[WORKFLOW_EXECUTE_READY])
    }

    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolResult, String> {
        if ctx.stage != WORKFLOW_EXECUTE_READY {
            return Ok(ToolResult::blocked(
                "当前还没有可执行预览，请先生成并确认计划预览。",
            ));
        }
        let preview_id = args
            .get("previewId")
            .and_then(Value::as_str)
            .ok_or_else(|| "当前预览不存在或已过期，请先重新生成 preview，再执行。".to_string())?;
        let state = ctx
            .state
            .ok_or_else(|| "advisor tool is missing AppState".to_string())?;
        let session = ctx
            .session
            .as_deref_mut()
            .ok_or_else(|| "advisor tool is missing session".to_string())?;
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let service = ToolService::new(state);
        let (_job, result, rollback) =
            service.execute_plan_by_preview_id(session_id, preview_id)?;
        if let Some(obj) = session.as_object_mut() {
            obj.insert(
                "workflowStage".to_string(),
                Value::String(WORKFLOW_UNDERSTAND.to_string()),
            );
            obj.insert("activeSelectionId".to_string(), Value::Null);
            obj.insert("activePreviewId".to_string(), Value::Null);
            obj.insert(
                "rollbackAvailable".to_string(),
                Value::Bool(
                    rollback
                        .get("available")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                ),
            );
        }
        let result_payload = json!({
            "jobId": preview_id,
            "result": result,
            "rollbackAvailable": rollback.get("available").cloned().unwrap_or(Value::Bool(false)),
        });
        let card = json!({
            "cardType": CARD_EXECUTION,
            "status": "done",
            "title": local_text(ctx.response_language, "执行结果", "Execution Result"),
            "body": {
                "jobId": preview_id,
                "intentSummary": args.get("intentSummary").cloned().unwrap_or(Value::String(String::new())),
                "result": result_payload.get("result").cloned().unwrap_or(Value::Null),
                "rollbackAvailable": rollback.get("available").cloned().unwrap_or(Value::Bool(false)),
            },
            "actions": if rollback.get("available").and_then(Value::as_bool).unwrap_or(false) {
                vec![json!({ "action": "rollback_plan", "label": local_text(ctx.response_language, "撤销", "Rollback"), "variant": "secondary" })]
            } else {
                Vec::<Value>::new()
            }
        });
        Ok(ToolResult::result(result_payload).with_card(card))
    }
}

struct RollbackPlanTool;

#[async_trait]
impl LlmTool for RollbackPlanTool {
    fn id(&self) -> ToolId {
        ToolId::RollbackPlan
    }

    fn spec(&self) -> ToolSpec {
        tool_spec(
            self.id(),
            "rollback_plan",
            "回滚最近一次可回滚的执行计划。",
            json!({
                "type": "object",
                "properties": {
                    "jobId": { "type": "string" }
                },
                "required": ["jobId"]
            }),
        )
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        advisor_non_bootstrap(ctx)
    }

    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolResult, String> {
        let job_id = args
            .get("jobId")
            .and_then(Value::as_str)
            .ok_or_else(|| "当前执行记录不可回滚，请不要继续尝试回滚这个任务。".to_string())?;
        let state = ctx
            .state
            .ok_or_else(|| "advisor tool is missing AppState".to_string())?;
        let session = ctx
            .session
            .as_deref_mut()
            .ok_or_else(|| "advisor tool is missing session".to_string())?;
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let service = ToolService::new(state);
        let (_job, result) = service.rollback_plan(session_id, job_id)?;
        if let Some(obj) = session.as_object_mut() {
            obj.insert("rollbackAvailable".to_string(), Value::Bool(false));
        }
        let result_payload = json!({
            "jobId": job_id,
            "result": result,
            "rollbackAvailable": false,
        });
        let card = json!({
            "cardType": CARD_EXECUTION,
            "status": "rolled_back",
            "title": local_text(ctx.response_language, "回滚结果", "Rollback Result"),
            "body": {
                "jobId": job_id,
                "result": result_payload.get("result").cloned().unwrap_or(Value::Null),
                "rollbackAvailable": false,
            },
            "actions": []
        });
        Ok(ToolResult::result(result_payload).with_card(card))
    }
}

struct ApplyReclassificationTool;

#[async_trait]
impl LlmTool for ApplyReclassificationTool {
    fn id(&self) -> ToolId {
        ToolId::ApplyReclassification
    }

    fn spec(&self) -> ToolSpec {
        tool_spec(
            self.id(),
            "apply_reclassification",
            "按结构化 reclassificationRequest 应用局部分类修正。",
            json!({
                "type": "object",
                "properties": {
                    "request": {
                        "type": "object",
                        "properties": {
                            "intentSummary": { "type": "string" },
                            "change": {
                                "type": "object",
                                "properties": {
                                    "type": { "type": "string", "enum": ["rename_category", "move_selection_to_category", "split_selection_to_new_category", "merge_category_into_category", "delete_empty_category"] },
                                    "selectionId": { "type": "string" },
                                    "sourceCategoryId": { "type": "string" },
                                    "targetCategoryId": { "type": "string" },
                                    "newCategoryName": { "type": "string" }
                                },
                                "required": ["type"]
                            }
                        },
                        "required": ["change"]
                    },
                    "applyPreferenceCapture": { "type": "boolean" }
                },
                "required": ["request"]
            }),
        )
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        advisor_non_bootstrap(ctx)
    }

    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolResult, String> {
        let state = ctx
            .state
            .ok_or_else(|| "advisor tool is missing AppState".to_string())?;
        let session = ctx
            .session
            .as_deref_mut()
            .ok_or_else(|| "advisor tool is missing session".to_string())?;
        let service = ToolService::new(state);
        let job = service.apply_reclassification_request(
            session,
            args.get("request").unwrap_or(args),
            args.get("applyPreferenceCapture")
                .or_else(|| {
                    args.get("request")
                        .and_then(|request| request.get("applyPreferenceCapture"))
                })
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )?;
        persist::save_advisor_reclass_job(&state.db_path(), &job)?;
        let overview =
            service.get_directory_overview_tool(session, Some("summaryTree"), None, None)?;
        if let Some(obj) = session.as_object_mut() {
            obj.insert(
                "workflowStage".to_string(),
                Value::String(WORKFLOW_UNDERSTAND.to_string()),
            );
            obj.insert("activeSelectionId".to_string(), Value::Null);
            obj.insert("activePreviewId".to_string(), Value::Null);
            obj.insert("rollbackAvailable".to_string(), Value::Bool(false));
            obj.insert(
                "derivedTree".to_string(),
                overview.get("tree").cloned().unwrap_or(Value::Null),
            );
        }
        let cards = vec![
            json!({
                "cardType": CARD_RECLASS,
                "status": "done",
                "title": local_text(ctx.response_language, "分类修改结果", "Reclassification Result"),
                "body": {
                    "jobId": job.get("jobId").cloned().unwrap_or(Value::Null),
                    "reclassificationJobId": job.get("jobId").cloned().unwrap_or(Value::Null),
                    "summary": job.pointer("/result/message").cloned().unwrap_or(Value::String(String::new())),
                    "message": job.pointer("/result/message").cloned().unwrap_or(Value::String(String::new())),
                    "changeSummary": job.pointer("/result/changeSummary").cloned().unwrap_or(Value::String(String::new())),
                    "updatedTreeText": job.pointer("/result/updatedTreeText").cloned().unwrap_or(Value::String(String::new())),
                    "invalidated": job.pointer("/result/invalidated").cloned().unwrap_or_else(|| json!([])),
                },
                "actions": []
            }),
            json!({
                "cardType": CARD_TREE,
                "status": "ready",
                "title": local_text(ctx.response_language, "当前分类树", "Current Tree"),
                "body": {
                    "tree": overview.get("tree").cloned().unwrap_or(Value::Null)
                },
                "actions": []
            }),
        ];
        Ok(
            ToolResult::result(job.pointer("/result").cloned().unwrap_or(Value::Null))
                .with_cards(cards),
        )
    }
}

struct RollbackReclassificationTool;

#[async_trait]
impl LlmTool for RollbackReclassificationTool {
    fn id(&self) -> ToolId {
        ToolId::RollbackReclassification
    }

    fn spec(&self) -> ToolSpec {
        tool_spec(
            self.id(),
            "rollback_reclassification",
            "回滚最近一次可回滚的分类修正。",
            json!({
                "type": "object",
                "properties": {
                    "reclassificationJobId": { "type": "string" }
                },
                "required": ["reclassificationJobId"]
            }),
        )
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        advisor_non_bootstrap(ctx)
    }

    async fn execute(
        &self,
        ctx: &mut ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolResult, String> {
        let job_id = args
            .get("reclassificationJobId")
            .or_else(|| args.get("jobId"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                "当前归类修改记录不存在或不可回滚，请先确认最近一次归类修改是否成功。".to_string()
            })?;
        let state = ctx
            .state
            .ok_or_else(|| "advisor tool is missing AppState".to_string())?;
        let session = ctx
            .session
            .as_deref_mut()
            .ok_or_else(|| "advisor tool is missing session".to_string())?;
        let service = ToolService::new(state);
        let (_job, result, tree) = service.rollback_reclassification(session, job_id)?;
        let overview =
            service.get_directory_overview_tool(session, Some("summaryTree"), None, None)?;
        if let Some(obj) = session.as_object_mut() {
            obj.insert("activeSelectionId".to_string(), Value::Null);
            obj.insert("activePreviewId".to_string(), Value::Null);
            obj.insert("derivedTree".to_string(), tree.unwrap_or(Value::Null));
        }
        let cards = vec![
            json!({
                "cardType": CARD_RECLASS,
                "status": "rolled_back",
                "title": local_text(ctx.response_language, "分类回滚结果", "Reclassification Rollback"),
                "body": {
                    "jobId": job_id,
                    "message": result.get("message").cloned().unwrap_or(Value::String(String::new())),
                    "updatedTreeText": result.get("updatedTreeText").cloned().unwrap_or(Value::String(String::new())),
                    "rolledBack": result.get("rolledBack").cloned().unwrap_or(Value::Bool(true)),
                    "invalidated": result.get("invalidated").cloned().unwrap_or_else(|| json!([])),
                },
                "actions": []
            }),
            json!({
                "cardType": CARD_TREE,
                "status": "ready",
                "title": local_text(ctx.response_language, "当前分类树", "Current Tree"),
                "body": {
                    "tree": overview.get("tree").cloned().unwrap_or(Value::Null)
                },
                "actions": []
            }),
        ];
        Ok(ToolResult::result(result).with_cards(cards))
    }
}

pub(crate) fn serialize_tool_result_content(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn registry_tool_names_are_unique() {
        let registry = ToolRegistry::new();
        let mut names = HashSet::new();
        for tool in registry.tools.iter() {
            let spec = tool.spec();
            assert!(
                names.insert(spec.name),
                "duplicate tool name: {}",
                spec.name
            );
        }
    }

    #[test]
    fn advisor_tool_policy_respects_stage_and_search_state() {
        let registry = ToolRegistry::new();
        let session = json!({ "webSearchEnabled": true });
        let ctx = ToolContext {
            workflow: ToolWorkflow::Advisor,
            stage: WORKFLOW_UNDERSTAND,
            session: Some(&session),
            bootstrap_turn: false,
            response_language: "zh",
            web_search_allowed: true,
            search_remaining: 0,
        };
        let tools = registry.available_names(&ctx);
        assert!(tools.contains(&"get_directory_overview"));
        assert!(tools.contains(&"web_search"));
        assert!(tools.contains(&"find_files"));
        assert!(!tools.contains(&"execute_plan"));
        assert!(!tools.contains(&"submit_organize_result"));

        let execute_ctx = ToolContext {
            stage: WORKFLOW_EXECUTE_READY,
            ..ctx
        };
        let execute_tools = registry.available_names(&execute_ctx);
        assert!(execute_tools.contains(&"execute_plan"));
    }

    #[test]
    fn organizer_tool_policy_respects_search_budget() {
        let registry = ToolRegistry::new();
        let ctx = ToolContext {
            workflow: ToolWorkflow::Organizer,
            stage: "classification_batch_1",
            session: None,
            bootstrap_turn: false,
            response_language: "zh",
            web_search_allowed: true,
            search_remaining: 1,
        };
        let tools = registry.available_names(&ctx);
        assert!(tools.contains(&"web_search"));
        assert!(tools.contains(&"submit_organize_result"));
        assert!(!tools.contains(&"execute_plan"));

        let exhausted_ctx = ToolContext {
            web_search_allowed: false,
            search_remaining: 0,
            ..ctx
        };
        let exhausted_tools = registry.available_names(&exhausted_ctx);
        assert!(!exhausted_tools.contains(&"web_search"));
        assert!(exhausted_tools.contains(&"submit_organize_result"));
    }

    #[test]
    fn submit_organize_result_validates_shape() {
        let parsed = parse_submit_organize_result_arguments(&json!({
            "tree": {},
            "assignments": []
        }))
        .expect("valid submit");
        assert!(parsed["tree"].is_object());
        assert!(parse_submit_organize_result_arguments(&json!({
            "tree": [],
            "assignments": []
        }))
        .is_err());
    }
}
