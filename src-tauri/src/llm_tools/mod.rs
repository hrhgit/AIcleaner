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
use std::sync::atomic::{AtomicUsize, Ordering};

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
    SubmitInitialTree,
    SubmitClassificationBatch,
    ReviseTreeDraft,
    ReviewOrganizeDraft,
    SubmitReconciledTree,
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
    pub organizer_search_counter: Option<&'a AtomicUsize>,
    pub organizer_search_gate: Option<&'a tokio::sync::Semaphore>,
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
        self.register(SubmitInitialTreeTool);
        self.register(SubmitClassificationBatchTool);
        self.register(ReviseTreeDraftTool);
        self.register(ReviewOrganizeDraftTool);
        self.register(SubmitReconciledTreeTool);
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
        parameters: Some(json!({
            "type": "object",
            "description": "此工具不需要参数，调用时传空对象。",
            "properties": {},
            "additionalProperties": false
        })),
    }
}

fn category_tree_schema() -> Value {
    json!({
        "type": "object",
        "description": "最终分类树根节点。复用、重命名或移动已有节点时必须保留原 nodeId；新增节点应使用稳定的新 nodeId。",
        "properties": {
            "nodeId": {
                "type": "string",
                "description": "分类节点的稳定 ID。根节点通常为 root；已有节点必须保留原值。"
            },
            "name": {
                "type": "string",
                "description": "分类节点展示名称。根节点可以为空字符串。"
            },
            "children": {
                "type": "array",
                "description": "子分类节点列表；叶子节点使用空数组。",
                "maxItems": 80,
                "items": { "$ref": "#/properties/tree/$defs/categoryNode" }
            }
        },
        "required": ["nodeId", "name", "children"],
        "additionalProperties": false,
        "$defs": {
            "categoryNode": {
                "type": "object",
                "description": "分类树中的一个节点。",
                "properties": {
                    "nodeId": {
                        "type": "string",
                        "description": "分类节点的稳定 ID。已有节点必须保留原值。"
                    },
                    "name": {
                        "type": "string",
                        "description": "分类节点展示名称，应简短、实用。"
                    },
                    "children": {
                        "type": "array",
                        "description": "子分类节点列表；叶子节点使用空数组。",
                        "maxItems": 80,
                        "items": { "$ref": "#/properties/tree/$defs/categoryNode" }
                    }
                },
                "required": ["nodeId", "name", "children"],
                "additionalProperties": false
            }
        }
    })
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
            "当本地文件证据不足且确实需要外部背景时联网搜索。返回 formattedContext，后续判断仍应以本地证据为主。",
            json!({
                "type": "object",
                "description": "联网搜索请求。每次只提交一个简短查询。",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "简短搜索查询，用来补足本地证据缺失的外部上下文；不要放入文件内容或长段文本。",
                        "minLength": 1,
                        "maxLength": 160
                    },
                    "reason": {
                        "type": "string",
                        "description": "说明为什么本地证据不足、需要这次搜索；用于诊断和后续判断。",
                        "maxLength": 240
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        )
    }

    fn available(&self, ctx: &ToolContext<'_>) -> bool {
        match ctx.workflow {
            ToolWorkflow::Advisor => ctx.web_search_allowed && advisor_session_enabled(ctx),
            ToolWorkflow::Organizer => {
                ctx.stage == "classification_batch"
                    && ctx.web_search_allowed
                    && ctx.search_remaining > 0
            }
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

        let _organizer_search_permit = if ctx.workflow == ToolWorkflow::Organizer {
            let Some(gate) = ctx.organizer_search_gate else {
                return Ok(ToolResult::blocked(
                    "organizer web_search is missing task-level search concurrency gate.",
                ));
            };
            Some(gate.acquire().await.map_err(|e| e.to_string())?)
        } else {
            None
        };

        if ctx.workflow == ToolWorkflow::Organizer {
            let Some(counter) = ctx.organizer_search_counter else {
                return Ok(ToolResult::blocked(
                    "organizer web_search is missing task-level search budget counter.",
                ));
            };
            loop {
                let current = counter.load(Ordering::Relaxed);
                if current >= crate::organizer_runtime::ORGANIZER_WEB_SEARCH_BUDGET {
                    return Ok(ToolResult::blocked(local_text(
                        ctx.response_language,
                        "联网搜索额度已用完，请基于已有证据提交最终结果。",
                        "Web search budget is exhausted. Submit the final result from existing evidence.",
                    )));
                }
                if counter
                    .compare_exchange(
                        current,
                        current.saturating_add(1),
                        Ordering::SeqCst,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    break;
                }
            }
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

fn category_path_schema(description: &str) -> Value {
    json!({
        "type": "array",
        "description": description,
        "maxItems": 8,
        "items": { "type": "string", "description": "分类路径中的一个节点名称。" }
    })
}

fn assignment_schema() -> Value {
    json!({
        "type": "object",
        "description": "把当前 item 分配到初始树中的已有叶子节点。",
        "properties": {
            "reason": { "type": "string", "description": "分类证据和不确定性说明。", "maxLength": 300 },
            "itemId": { "type": "string", "description": "当前 batch 输入中的 itemId，必须原样填写。" },
            "leafNodeId": { "type": "string", "description": "目标已有叶子节点的 nodeId。" },
            "categoryPath": category_path_schema("目标分类路径，用于校验和兜底。"),
            "confidence": { "type": "string", "description": "分类置信度。", "enum": ["high", "medium", "low"] },
            "needsReview": { "type": "boolean", "description": "该分配是否需要 reconciliation 阶段局部审查。" }
        },
        "required": ["itemId", "leafNodeId", "reason"],
        "additionalProperties": false
    })
}

fn deferred_assignment_schema() -> Value {
    json!({
        "type": "object",
        "description": "需要等待树结构 proposal 处理后才能稳定落位的 item。",
        "properties": {
            "reason": { "type": "string", "description": "为什么不能直接放入已有节点。", "maxLength": 300 },
            "itemId": { "type": "string", "description": "当前 batch 输入中的 itemId，必须原样填写。" },
            "proposalId": { "type": "string", "description": "该 item 依赖的 treeProposal proposalId。" },
            "suggestedPath": category_path_schema("建议落位路径。"),
            "confidence": { "type": "string", "description": "建议置信度。", "enum": ["high", "medium", "low"] }
        },
        "required": ["itemId", "proposalId", "reason"],
        "additionalProperties": false
    })
}

fn tree_proposal_schema() -> Value {
    json!({
        "type": "object",
        "description": "并行分类阶段提出的树结构修改建议；不会直接修改全局树。",
        "properties": {
            "proposalId": { "type": "string", "description": "当前 batch 内稳定 proposal ID。" },
            "operation": { "type": "string", "description": "建议操作。", "enum": ["add_node", "merge_nodes", "split_node", "rename_node"] },
            "targetNodeId": { "type": "string", "description": "被修改或作为父节点的现有 nodeId，可为空。" },
            "sourceNodeIds": { "type": "array", "description": "merge/split 涉及的现有节点。", "maxItems": 20, "items": { "type": "string" } },
            "suggestedName": { "type": "string", "description": "建议节点名称。" },
            "suggestedPath": category_path_schema("建议的新路径。"),
            "evidenceItemIds": { "type": "array", "description": "支持该建议的当前 itemId。", "maxItems": 200, "items": { "type": "string" } },
            "reason": { "type": "string", "description": "简短说明建议原因。", "maxLength": 500 }
        },
        "required": ["proposalId", "operation", "reason"],
        "additionalProperties": false
    })
}

fn string_array_schema(description: &str) -> Value {
    json!({
        "type": "array",
        "description": description,
        "maxItems": 500,
        "items": { "type": "string" }
    })
}

fn ensure_object_field(value: &Value, field: &str, tool: &str) -> Result<Value, String> {
    let field_value = value
        .get(field)
        .cloned()
        .ok_or_else(|| format!("{tool} is missing {field}"))?;
    if !field_value.is_object() {
        return Err(format!("{tool} {field} must be an object"));
    }
    Ok(field_value)
}

fn ensure_array_field(value: &Value, field: &str, tool: &str) -> Result<Value, String> {
    let field_value = value
        .get(field)
        .cloned()
        .ok_or_else(|| format!("{tool} is missing {field}"))?;
    if !field_value.is_array() {
        return Err(format!("{tool} {field} must be an array"));
    }
    Ok(field_value)
}

fn parse_initial_tree_arguments(value: &Value) -> Result<Value, String> {
    Ok(json!({
        "tree": ensure_object_field(value, "tree", "submit_initial_tree")?,
        "notes": value.get("notes").and_then(Value::as_str).unwrap_or("").to_string(),
    }))
}

fn parse_classification_batch_arguments(value: &Value) -> Result<Value, String> {
    Ok(json!({
        "baseTreeVersion": value.get("baseTreeVersion").and_then(Value::as_u64).unwrap_or(0),
        "assignments": ensure_array_field(value, "assignments", "submit_classification_batch")?,
        "deferredAssignments": value.get("deferredAssignments").cloned().filter(Value::is_array).unwrap_or_else(|| json!([])),
        "treeProposals": value.get("treeProposals").cloned().filter(Value::is_array).unwrap_or_else(|| json!([])),
    }))
}

fn parse_revise_tree_draft_arguments(value: &Value) -> Result<Value, String> {
    Ok(json!({
        "draftTree": ensure_object_field(value, "draftTree", "revise_tree_draft")?,
        "proposalMappings": value.get("proposalMappings").cloned().filter(Value::is_array).unwrap_or_else(|| json!([])),
        "rejectedProposalIds": value.get("rejectedProposalIds").cloned().filter(Value::is_array).unwrap_or_else(|| json!([])),
        "notes": value.get("notes").and_then(Value::as_str).unwrap_or("").to_string(),
    }))
}

fn parse_review_organize_draft_arguments(value: &Value) -> Result<Value, String> {
    Ok(json!({
        "issues": ensure_array_field(value, "issues", "review_organize_draft")?,
        "recommendedOperations": value.get("recommendedOperations").cloned().filter(Value::is_array).unwrap_or_else(|| json!([])),
        "needsRevision": value.get("needsRevision").and_then(Value::as_bool).unwrap_or(false),
        "notes": value.get("notes").and_then(Value::as_str).unwrap_or("").to_string(),
    }))
}

fn parse_reconciled_tree_arguments(value: &Value) -> Result<Value, String> {
    Ok(json!({
        "finalTree": ensure_object_field(value, "finalTree", "submit_reconciled_tree")?,
        "proposalMappings": ensure_array_field(value, "proposalMappings", "submit_reconciled_tree")?,
        "finalAssignments": ensure_array_field(value, "finalAssignments", "submit_reconciled_tree")?,
        "unresolvedItemIds": value.get("unresolvedItemIds").cloned().filter(Value::is_array).unwrap_or_else(|| json!([])),
    }))
}

macro_rules! organizer_submit_tool {
    ($tool:ident, $id:ident, $name:literal, $description:literal, $schema:expr, $stage:expr, $parser:ident) => {
        struct $tool;

        #[async_trait]
        impl LlmTool for $tool {
            fn id(&self) -> ToolId {
                ToolId::$id
            }

            fn spec(&self) -> ToolSpec {
                tool_spec(self.id(), $name, $description, $schema)
            }

            fn available(&self, ctx: &ToolContext<'_>) -> bool {
                ctx.workflow == ToolWorkflow::Organizer && ctx.stage == $stage
            }

            async fn execute(
                &self,
                ctx: &mut ToolExecutionContext<'_>,
                args: &Value,
            ) -> Result<ToolResult, String> {
                if ctx.workflow != ToolWorkflow::Organizer || ctx.stage != $stage {
                    return Ok(ToolResult::blocked(format!(
                        "{} is only available during organizer stage {}.",
                        $name, $stage
                    )));
                }
                Ok(ToolResult::result($parser(args)?))
            }
        }
    };
}

organizer_submit_tool!(
    SubmitInitialTreeTool,
    SubmitInitialTree,
    "submit_initial_tree",
    "提交 organizer 初始分类树。此阶段只生成树结构，不分配文件，不联网搜索。",
    json!({
        "type": "object",
        "description": "初始树提交结果。",
        "properties": {
            "tree": category_tree_schema(),
            "notes": { "type": "string", "description": "可选，说明主要分类依据。", "maxLength": 800 }
        },
        "required": ["tree"],
        "additionalProperties": false
    }),
    "initial_tree",
    parse_initial_tree_arguments
);

organizer_submit_tool!(
    SubmitClassificationBatchTool,
    SubmitClassificationBatch,
    "submit_classification_batch",
    "提交当前并行分类 batch 的一次性结果。只能引用已有节点或提出 treeProposals，不能返回完整 tree。",
    json!({
        "type": "object",
        "description": "并行分类 batch 输出。",
        "properties": {
            "baseTreeVersion": { "type": "integer", "description": "当前 batch 使用的 baseTreeVersion。", "minimum": 0 },
            "assignments": { "type": "array", "description": "可直接落入已有叶子节点的分配。", "maxItems": 200, "items": assignment_schema() },
            "deferredAssignments": { "type": "array", "description": "依赖 tree proposal 的待定分配。", "maxItems": 200, "items": deferred_assignment_schema() },
            "treeProposals": { "type": "array", "description": "树结构修改建议。", "maxItems": 100, "items": tree_proposal_schema() }
        },
        "required": ["baseTreeVersion", "assignments"],
        "additionalProperties": false
    }),
    "classification_batch",
    parse_classification_batch_arguments
);

organizer_submit_tool!(
    ReviseTreeDraftTool,
    ReviseTreeDraft,
    "revise_tree_draft",
    "提交 reconciliation 阶段的一版 draft tree 和 proposal 映射。程序会立即校验并返回下一步。",
    json!({
        "type": "object",
        "description": "树结构草稿修订结果。",
        "properties": {
            "draftTree": category_tree_schema(),
            "proposalMappings": { "type": "array", "description": "proposalId 到最终 leafNodeId/path 的映射。", "maxItems": 500, "items": { "type": "object", "description": "单个 proposal 映射。", "properties": { "proposalId": { "type": "string", "description": "被处理的 proposalId。" }, "leafNodeId": { "type": "string", "description": "映射后的 leafNodeId。" }, "categoryPath": category_path_schema("映射后的分类路径。") }, "required": ["proposalId"], "additionalProperties": false } },
            "rejectedProposalIds": string_array_schema("明确拒绝的 proposalId。"),
            "notes": { "type": "string", "description": "本轮调整说明。", "maxLength": 1000 }
        },
        "required": ["draftTree"],
        "additionalProperties": false
    }),
    "reconcile_tree",
    parse_revise_tree_draft_arguments
);

organizer_submit_tool!(
    ReviewOrganizeDraftTool,
    ReviewOrganizeDraft,
    "review_organize_draft",
    "提交局部审查结论。只审 runtime 给出的局部范围，不直接修改树。",
    json!({
        "type": "object",
        "description": "局部审查结果。",
        "properties": {
            "issues": { "type": "array", "description": "审查发现的问题。", "maxItems": 100, "items": { "type": "object", "description": "单个审查问题。", "properties": { "type": { "type": "string", "description": "问题类型。" }, "nodeIds": string_array_schema("相关 nodeId。"), "itemIds": string_array_schema("相关 itemId。"), "severity": { "type": "string", "description": "问题严重程度。", "enum": ["low", "medium", "high"] }, "reason": { "type": "string", "description": "问题原因。", "maxLength": 500 } }, "required": ["type", "reason"], "additionalProperties": false } },
            "recommendedOperations": { "type": "array", "description": "建议后续 revise_tree_draft 采用的操作。", "maxItems": 100, "items": tree_proposal_schema() },
            "needsRevision": { "type": "boolean", "description": "是否需要回到 revise_tree_draft。" },
            "notes": { "type": "string", "description": "审查说明。", "maxLength": 1000 }
        },
        "required": ["issues", "needsRevision"],
        "additionalProperties": false
    }),
    "review_tree",
    parse_review_organize_draft_arguments
);

organizer_submit_tool!(
    SubmitReconciledTreeTool,
    SubmitReconciledTree,
    "submit_reconciled_tree",
    "提交通过审查后的最终分类树和最终文件分配。只有 runtime 校验通过后才会生成 preview/apply。",
    json!({
        "type": "object",
        "description": "最终 reconciliation 结果。",
        "properties": {
            "finalTree": category_tree_schema(),
            "proposalMappings": { "type": "array", "description": "所有 proposal 的处理结果。", "maxItems": 500, "items": { "type": "object", "description": "单个 proposal 的最终处理结果。", "properties": { "proposalId": { "type": "string", "description": "被处理的 proposalId。" }, "status": { "type": "string", "description": "proposal 的处理状态。", "enum": ["accepted", "merged", "rejected"] }, "leafNodeId": { "type": "string", "description": "接受或合并后的 leafNodeId。" }, "categoryPath": category_path_schema("最终分类路径。") }, "required": ["proposalId", "status"], "additionalProperties": false } },
            "finalAssignments": { "type": "array", "description": "所有有效文件的最终分配。", "maxItems": 2000, "items": assignment_schema() },
            "unresolvedItemIds": string_array_schema("仍无法稳定分类的 itemId。")
        },
        "required": ["finalTree", "proposalMappings", "finalAssignments"],
        "additionalProperties": false
    }),
    "submit_reconciled_tree",
    parse_reconciled_tree_arguments
);

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
            "查看当前分类树或目录概览。用于理解现状，不会生成摘要、筛选文件或修改分类。",
            json!({
                "type": "object",
                "description": "目录概览读取参数。所有参数可选。",
                "properties": {
                    "viewType": {
                        "type": "string",
                        "description": "要读取的视图类型；默认由工具选择最适合当前上下文的概览。",
                        "enum": ["summaryTree", "sizeTree", "timeTree", "executionTree", "partialTree"]
                    },
                    "rootCategoryId": {
                        "type": "string",
                        "description": "只查看某个分类节点下的子树；为空时查看根树。"
                    },
                    "maxDepth": {
                        "type": "integer",
                        "description": "返回树的最大深度；用于控制上下文大小。",
                        "minimum": 1,
                        "maximum": 8
                    }
                },
                "additionalProperties": false
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
            "按类别、名称、路径、扩展名、大小和时间筛选文件候选集，并返回 selectionId。后续 preview_plan 应使用这个 selectionId。",
            json!({
                "type": "object",
                "description": "文件筛选条件。通常先用较宽条件找到候选集，再用 selectionId 生成预览。",
                "properties": {
                    "categoryIds": {
                        "type": "array",
                        "description": "限定在这些分类节点内查找；不确定分类时不要填写。",
                        "maxItems": 20,
                        "items": {
                            "type": "string",
                            "description": "分类节点 nodeId。"
                        }
                    },
                    "nameQuery": {
                        "type": "string",
                        "description": "文件名模糊关键词，适合搜索一类文件。",
                        "maxLength": 120
                    },
                    "nameExact": {
                        "type": "string",
                        "description": "精确文件名；只有用户明确给出完整名称时使用。",
                        "maxLength": 260
                    },
                    "pathContains": {
                        "type": "string",
                        "description": "路径中应包含的片段；用于限定目录或来源。",
                        "maxLength": 260
                    },
                    "extensions": {
                        "type": "array",
                        "description": "文件扩展名过滤，例如 pdf、zip、exe；不要包含点号。",
                        "maxItems": 20,
                        "items": {
                            "type": "string",
                            "description": "不带点号的文件扩展名。"
                        }
                    },
                    "minSizeBytes": {
                        "type": "integer",
                        "description": "最小文件大小，单位字节。",
                        "minimum": 0
                    },
                    "maxSizeBytes": {
                        "type": "integer",
                        "description": "最大文件大小，单位字节。",
                        "minimum": 0
                    },
                    "olderThanDays": {
                        "type": "integer",
                        "description": "只返回修改时间早于多少天前的文件。",
                        "minimum": 0,
                        "maximum": 36500
                    },
                    "newerThanDays": {
                        "type": "integer",
                        "description": "只返回修改时间晚于多少天内的文件。",
                        "minimum": 0,
                        "maximum": 36500
                    },
                    "sortBy": {
                        "type": "string",
                        "description": "候选文件排序字段。",
                        "enum": ["name", "size", "modifiedAt"]
                    },
                    "sortOrder": {
                        "type": "string",
                        "description": "排序方向。",
                        "enum": ["asc", "desc"]
                    },
                    "limit": {
                        "type": "integer",
                        "description": "最多返回候选数量；保持较小以便后续预览。",
                        "minimum": 1,
                        "maximum": 500
                    }
                },
                "additionalProperties": false
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
            "为指定文件或分类补摘要或刷新摘要，并写入顾问摘要库。用于证据不足时补充本地摘要，不会直接移动文件。",
            json!({
                "type": "object",
                "description": "摘要生成请求。优先使用 paths 精确指定，或用 categoryIds 指定分类范围。",
                "properties": {
                    "paths": {
                        "type": "array",
                        "description": "需要摘要的相对路径或已知路径；只放当前任务相关文件。",
                        "maxItems": 100,
                        "items": {
                            "type": "string",
                            "description": "文件路径。"
                        }
                    },
                    "categoryIds": {
                        "type": "array",
                        "description": "需要补摘要的分类节点 ID；范围大时配合 missingOnly 使用。",
                        "maxItems": 20,
                        "items": {
                            "type": "string",
                            "description": "分类节点 nodeId。"
                        }
                    },
                    "representationLevel": {
                        "type": "string",
                        "description": "摘要粒度：metadata 只用元数据，short 生成短摘要，long 生成更详细摘要。",
                        "enum": ["metadata", "short", "long"]
                    },
                    "missingOnly": {
                        "type": "boolean",
                        "description": "为 true 时只补缺失摘要，避免重复刷新已有摘要。"
                    },
                    "batchSize": {
                        "type": "integer",
                        "description": "每批处理数量。",
                        "minimum": 1,
                        "maximum": 50
                    },
                    "maxConcurrency": {
                        "type": "integer",
                        "description": "并发处理数量；保持较小以避免资源占用过高。",
                        "minimum": 1,
                        "maximum": 8
                    }
                },
                "additionalProperties": false
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
            "只读取已有文件摘要，不触发生成或刷新。用于低成本查看证据。",
            json!({
                "type": "object",
                "description": "已有摘要读取请求。不会修改摘要库。",
                "properties": {
                    "paths": {
                        "type": "array",
                        "description": "要读取摘要的具体路径。",
                        "maxItems": 100,
                        "items": {
                            "type": "string",
                            "description": "文件路径。"
                        }
                    },
                    "categoryIds": {
                        "type": "array",
                        "description": "要读取摘要的分类节点 ID。",
                        "maxItems": 20,
                        "items": {
                            "type": "string",
                            "description": "分类节点 nodeId。"
                        }
                    },
                    "representationLevel": {
                        "type": "string",
                        "description": "期望读取的摘要粒度。",
                        "enum": ["metadata", "short", "long"]
                    },
                    "limit": {
                        "type": "integer",
                        "description": "最多返回摘要数量。",
                        "minimum": 1,
                        "maximum": 300
                    }
                },
                "additionalProperties": false
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
            "从用户消息中提炼可复用偏好并暂存，后续由卡片动作确认保存。只在用户明确表达偏好或规则时使用。",
            json!({
                "type": "object",
                "description": "偏好草稿。工具只创建待确认卡片，不直接静默写入长期偏好。",
                "properties": {
                    "scope": {
                        "type": "string",
                        "description": "偏好建议保存范围：session 只影响当前会话，global 可复用于后续会话。",
                        "enum": ["session", "global"]
                    },
                    "text": {
                        "type": "string",
                        "description": "提炼后的偏好正文，应简短、可执行，不要包含敏感信息。",
                        "minLength": 1,
                        "maxLength": 500
                    },
                    "sourceMessage": {
                        "type": "string",
                        "description": "触发偏好提炼的用户原始表述或摘要，便于用户确认。",
                        "minLength": 1,
                        "maxLength": 1000
                    }
                },
                "required": ["text", "sourceMessage"],
                "additionalProperties": false
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
        no_arg_tool_spec(
            self.id(),
            "list_preferences",
            "读取当前会话和全局偏好。用于回答偏好相关问题或在计划前确认约束。",
        )
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
            "根据 plan JSON 和 selectionId 生成文件级预览。返回 previewId，后续 execute_plan 必须使用该 previewId。",
            json!({
                "type": "object",
                "description": "执行前预览请求。必须先有 find_files 返回的 selectionId。",
                "properties": {
                    "plan": {
                        "type": "object",
                        "description": "待预览的计划。预览只计算影响范围，不直接执行文件操作。",
                        "properties": {
                            "intentSummary": {
                                "type": "string",
                                "description": "用户意图的简短摘要，用于预览卡说明。",
                                "maxLength": 300
                            },
                            "targets": {
                                "type": "array",
                                "description": "要对一个或多个 selectionId 执行的动作。",
                                "minItems": 1,
                                "maxItems": 20,
                                "items": {
                                    "type": "object",
                                    "description": "单个 selectionId 的预览动作。",
                                    "properties": {
                                        "selectionId": {
                                            "type": "string",
                                            "description": "find_files 返回的候选集 ID。"
                                        },
                                        "action": {
                                            "type": "string",
                                            "description": "要预览的处理动作；delete 属高风险动作，只在用户明确要求时使用。",
                                            "enum": ["archive", "move", "keep", "review", "delete"]
                                        }
                                    },
                                    "required": ["selectionId", "action"],
                                    "additionalProperties": false
                                }
                            }
                        },
                        "required": ["targets"],
                        "additionalProperties": false
                    }
                },
                "required": ["plan"],
                "additionalProperties": false
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
            "执行已经通过预览的计划。必须使用 preview_plan 返回的 previewId；执行后会返回结果卡，并可能提供 rollback_plan 可用状态。",
            json!({
                "type": "object",
                "description": "计划执行请求。只执行已经生成预览且仍有效的 previewId。",
                "properties": {
                    "previewId": {
                        "type": "string",
                        "description": "preview_plan 返回的预览 ID。"
                    },
                    "intentSummary": {
                        "type": "string",
                        "description": "执行意图摘要，用于结果卡展示。",
                        "maxLength": 300
                    }
                },
                "required": ["previewId"],
                "additionalProperties": false
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
            "回滚最近一次可回滚的执行计划。只在 execute_plan 返回可回滚状态或用户明确要求撤销时使用。",
            json!({
                "type": "object",
                "description": "计划回滚请求。",
                "properties": {
                    "jobId": {
                        "type": "string",
                        "description": "要回滚的执行任务 ID，通常来自 execute_plan 返回结果。"
                    }
                },
                "required": ["jobId"],
                "additionalProperties": false
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
            "按结构化 reclassificationRequest 应用局部分类修正。用于重命名、移动、拆分、合并或删除空分类，执行后会更新当前分类树。",
            json!({
                "type": "object",
                "description": "局部分类修正请求。只修改分类结构或选中项归类，不执行文件移动。",
                "properties": {
                    "request": {
                        "type": "object",
                        "description": "重分类请求主体。",
                        "properties": {
                            "intentSummary": {
                                "type": "string",
                                "description": "用户希望调整分类的简短意图摘要。",
                                "maxLength": 300
                            },
                            "change": {
                                "type": "object",
                                "description": "具体分类修改动作及其必要目标。",
                                "properties": {
                                    "type": {
                                        "type": "string",
                                        "description": "分类修改类型。根据用户意图选择一个最小动作。",
                                        "enum": ["rename_category", "move_selection_to_category", "split_selection_to_new_category", "merge_category_into_category", "delete_empty_category"]
                                    },
                                    "selectionId": {
                                        "type": "string",
                                        "description": "需要移动或拆分的候选集 ID，通常来自 find_files。"
                                    },
                                    "sourceCategoryId": {
                                        "type": "string",
                                        "description": "源分类节点 ID，用于重命名、合并或删除空分类。"
                                    },
                                    "targetCategoryId": {
                                        "type": "string",
                                        "description": "目标分类节点 ID，用于移动或合并。"
                                    },
                                    "newCategoryName": {
                                        "type": "string",
                                        "description": "新分类名称或重命名后的名称，应简短实用。",
                                        "maxLength": 120
                                    }
                                },
                                "required": ["type"],
                                "additionalProperties": false
                            }
                        },
                        "required": ["change"],
                        "additionalProperties": false
                    },
                    "applyPreferenceCapture": {
                        "type": "boolean",
                        "description": "是否同时应用刚捕获的偏好；只有用户明确确认相关偏好时才设为 true。"
                    }
                },
                "required": ["request"],
                "additionalProperties": false
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
            "回滚最近一次可回滚的分类修正。执行后会恢复分类树并返回更新后的树结果卡。",
            json!({
                "type": "object",
                "description": "分类修正回滚请求。",
                "properties": {
                    "reclassificationJobId": {
                        "type": "string",
                        "description": "要回滚的分类修正任务 ID，通常来自 apply_reclassification 返回结果。"
                    }
                },
                "required": ["reclassificationJobId"],
                "additionalProperties": false
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
        assert!(!tools.contains(&"submit_classification_batch"));

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
            stage: "classification_batch",
            session: None,
            bootstrap_turn: false,
            response_language: "zh",
            web_search_allowed: true,
            search_remaining: 1,
        };
        let tools = registry.available_names(&ctx);
        assert!(tools.contains(&"web_search"));
        assert!(tools.contains(&"submit_classification_batch"));
        assert!(!tools.contains(&"execute_plan"));

        let exhausted_ctx = ToolContext {
            web_search_allowed: false,
            search_remaining: 0,
            ..ctx
        };
        let exhausted_tools = registry.available_names(&exhausted_ctx);
        assert!(!exhausted_tools.contains(&"web_search"));
        assert!(exhausted_tools.contains(&"submit_classification_batch"));
    }

    #[test]
    fn tool_definitions_include_descriptions_and_closed_object_schemas() {
        let registry = ToolRegistry::new();
        for tool in registry.tools.iter() {
            let spec = tool.spec();
            assert!(
                !spec.description.trim().is_empty(),
                "{} is missing tool description",
                spec.name
            );
            let definition = spec.definition();
            let function = definition
                .get("function")
                .expect("function wrapper is present");
            let description = function
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                !description.trim().is_empty(),
                "{} definition is missing description",
                spec.name
            );
            let parameters = function
                .get("parameters")
                .unwrap_or_else(|| panic!("{} is missing parameters schema", spec.name));
            assert_object_schemas_are_closed(parameters, spec.name);
            assert_property_schemas_are_described(parameters, spec.name);
        }
    }

    #[test]
    fn list_preferences_has_explicit_empty_parameters_schema() {
        let registry = ToolRegistry::new();
        let ctx = ToolContext {
            workflow: ToolWorkflow::Advisor,
            stage: WORKFLOW_UNDERSTAND,
            session: None,
            bootstrap_turn: false,
            response_language: "zh",
            web_search_allowed: false,
            search_remaining: 0,
        };
        let definition = registry
            .definitions(&ctx)
            .into_iter()
            .find(|definition| {
                definition.pointer("/function/name").and_then(Value::as_str)
                    == Some("list_preferences")
            })
            .expect("list_preferences is available");
        let parameters = definition
            .pointer("/function/parameters")
            .expect("list_preferences parameters");
        assert_eq!(
            parameters.get("type").and_then(Value::as_str),
            Some("object")
        );
        assert_eq!(
            parameters
                .get("additionalProperties")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert!(parameters
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| properties.is_empty()));
    }

    #[test]
    fn new_organizer_submit_tools_validate_shape() {
        let parsed = parse_initial_tree_arguments(&json!({
            "tree": {}
        }))
        .expect("valid initial tree");
        assert!(parsed["tree"].is_object());
        let parsed = parse_classification_batch_arguments(&json!({
            "baseTreeVersion": 1,
            "assignments": []
        }))
        .expect("valid classification batch");
        assert_eq!(parsed["baseTreeVersion"], Value::from(1));
        assert!(parse_reconciled_tree_arguments(&json!({
            "finalTree": [],
            "proposalMappings": [],
            "finalAssignments": []
        }))
        .is_err());
    }

    fn assert_object_schemas_are_closed(schema: &Value, path: &str) {
        if schema.get("type").and_then(Value::as_str) == Some("object") {
            assert_eq!(
                schema.get("additionalProperties").and_then(Value::as_bool),
                Some(false),
                "{} object schema must set additionalProperties=false",
                path
            );
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (name, value) in properties {
                assert_object_schemas_are_closed(value, &format!("{path}.properties.{name}"));
            }
        }
        if let Some(items) = schema.get("items") {
            assert_object_schemas_are_closed(items, &format!("{path}.items"));
        }
        if let Some(defs) = schema.get("$defs").and_then(Value::as_object) {
            for (name, value) in defs {
                assert_object_schemas_are_closed(value, &format!("{path}.$defs.{name}"));
            }
        }
    }

    fn assert_property_schemas_are_described(schema: &Value, path: &str) {
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (name, value) in properties {
                let description = value
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert!(
                    !description.trim().is_empty(),
                    "{}.properties.{} is missing description",
                    path,
                    name
                );
                assert_property_schemas_are_described(value, &format!("{path}.properties.{name}"));
            }
        }
        if let Some(items) = schema.get("items") {
            assert_property_schemas_are_described(items, &format!("{path}.items"));
        }
        if let Some(defs) = schema.get("$defs").and_then(Value::as_object) {
            for (name, value) in defs {
                assert_property_schemas_are_described(value, &format!("{path}.$defs.{name}"));
            }
        }
    }
}
