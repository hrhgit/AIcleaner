# AI Prompts Inventory / AI 提示词清单

## Overview / 概览

中文：
- 以代码为准，本文仅作为当前 prompt 与摘要模板的索引说明。
- 这个文档整理了当前项目里仍在使用的 AI 提示词与摘要模板。
- 扫描和 organizer 都已经支持中英双模板。
- organizer 默认每批 20 个条目。
- 实际发送给 AI 的提示词语言由 `response_language` 决定：
  - 中文回复语言：发送中文提示词
  - 其他回复语言：发送英文提示词
- organizer 里不仅 `system prompt` 会切语言，摘要标签和原目录结构上下文也会一起切语言。

English:
- The code is the source of truth; this document is only an index of the current prompts and summary templates.
- This document lists the AI prompts and summary templates that are still in active use.
- Both scan and organizer now support bilingual prompt templates.
- Organizer now uses a default batch size of 20 items.
- The language sent to the model is selected by `response_language`:
  - Chinese response language: send Chinese prompts
  - Other response languages: send English prompts
- In organizer, not only the `system prompt` but also summary labels and reference-structure context switch language together.

## Current Prompts / 当前在用提示词

### 1. Scan Safety Review / 扫描安全判断

Code / 代码位置:
- `src-tauri/src/scan_runtime.rs`
- `build_scan_system_prompt`: 构造扫描阶段的 `system prompt`
- `scan_prompt_classification_tokens`: 提供目录/文件分类 token 与中英模板
- `analyze_scan_node`: 根据节点类型拼接目录或文件的 `user prompt`
- `chat_completion`: 实际把 `system` / `user` message 发给模型

#### Directory System Prompt / 目录 system prompt

中文模板：

```text
你是一个磁盘清理安全分析助手。
只能返回 JSON。
输出结构：{"classification":"全部删除|全部保留|展开分析","reason":"...","risk":"low|medium|high"}
分类含义：全部删除 = 整个项目都可以删除，全部保留 = 整个项目都应该保留，展开分析 = 需要继续深入分析或人工复核。
保持保守判断；如果不确定，优先使用展开分析，不要使用全部删除。
`reason` 字段只能使用{response_language}。
```

English template:

```text
You are a disk cleanup safety assistant.
Return JSON only.
Final schema: {"classification":"delete_all|keep_all|expand_analysis","reason":"...","risk":"low|medium|high"}
Classification meanings: delete_all = delete the whole item, keep_all = keep the whole item, expand_analysis = inspect deeper or require manual review.
Be conservative. If unsure, prefer expand_analysis over delete_all.
The "reason" field must be written in {response_language} only.
```

#### Directory User Prompt / 目录 user prompt

中文模板：

```text
类型：目录
路径：{path}
名称：{name}
大小：{formatted_size}
目录画像：
{portrait_summary}
直接子目录：
{child_summary}
只能选择一个 classification：全部删除、全部保留、展开分析。
只有当整个目录都可以安全删除时，才能使用全部删除。
当整个目录都应保留时，使用全部保留。
只要任一子项需要继续深入判断，或者你无法确定，就使用展开分析。
如果不确定，优先使用展开分析，不要使用全部删除。
```

English template:

```text
Type: directory
Path: {path}
Name: {name}
Size: {formatted_size}
Directory portrait:
{portrait_summary}
Direct child directories:
{child_summary}
Choose one classification only: delete_all, keep_all, or expand_analysis.
Use delete_all only when the whole directory can be deleted safely.
Use keep_all when the whole directory should be kept.
Use expand_analysis when any child needs deeper inspection or when you are uncertain.
If unsure, prefer expand_analysis over delete_all.
```

#### File System Prompt / 文件 system prompt

中文模板：

```text
你是一个磁盘清理安全分析助手。
只能返回 JSON。
输出结构：{"classification":"全部删除|全部保留|展开分析","reason":"...","risk":"low|medium|high"}
分类含义：全部删除 = 整个项目都可以删除，全部保留 = 整个项目都应该保留，展开分析 = 需要继续深入分析或人工复核。
保持保守判断；如果不确定，优先使用展开分析，不要使用全部删除。
`reason` 字段只能使用{response_language}。
```

English template:

```text
You are a disk cleanup safety assistant.
Return JSON only.
Final schema: {"classification":"delete_all|keep_all|expand_analysis","reason":"...","risk":"low|medium|high"}
Classification meanings: delete_all = delete the whole item, keep_all = keep the whole item, expand_analysis = inspect deeper or require manual review.
Be conservative. If unsure, prefer expand_analysis over delete_all.
The "reason" field must be written in {response_language} only.
```

#### File User Prompt / 文件 user prompt

中文模板：

```text
类型：文件
路径：{path}
名称：{name}
大小：{formatted_size}
只能选择一个 classification：全部删除、全部保留、展开分析。
只有当文件可以安全删除时，才能使用全部删除。
当文件应保留时，使用全部保留。
如果无法确定，就使用展开分析。
如果不确定，优先使用展开分析，不要使用全部删除。
```

English template:

```text
Type: file
Path: {path}
Name: {name}
Size: {formatted_size}
Choose one classification only: delete_all, keep_all, or expand_analysis.
Use delete_all only when the file can be deleted safely.
Use keep_all when the file should be kept.
Use expand_analysis when you are uncertain.
If unsure, prefer expand_analysis over delete_all.
```

### 2. Organizer Tree Clustering / Organizer 树状聚类

Code / 代码位置:
- `src-tauri/src/organizer_runtime.rs`
- `build_summary_agent_system_prompt`: 文件摘要阶段的 `system prompt`
- `build_organize_system_prompt`: 树状聚类阶段的 `system prompt`
- `summarize_directory_for_prompt`: 目录摘要本地模板
- `summarize_batch_with_agent`: 批量摘要时实际发送 `system prompt` 与 JSON `user prompt`
- `classify_organize_batch`: 聚类时实际发送 `system prompt` 与 JSON `user prompt`

#### Clustering System Prompt / 聚类 system prompt

中文模板：

```text
你需要把一批文件摘要聚成一个分层分类树。只能返回 JSON，输出结构为 {"tree":{...},"assignments":[{"itemId":"...","leafNodeId":"... optional","categoryPath":["..."],"reason":"..."}]}。现有节点已经有稳定的 nodeId；当你复用、重命名或移动已有节点时，必须保留原 nodeId。分类名称请使用{response_language}，并保持简短。
```

English template:

```text
You cluster file summaries into a hierarchical category tree. Return JSON only with schema {"tree":{...},"assignments":[{"itemId":"...","leafNodeId":"... optional","categoryPath":["..."],"reason":"..."}]}. Existing nodes already have stable nodeId values; keep nodeId when you reuse, rename, or move existing nodes. Use {response_language} names and keep labels short.
```

#### Clustering User Payload / 聚类 user payload

中文：
- 当前主流程直接把 JSON 作为 `user prompt` 发送给模型，不再额外包装成旧的平面分类自然语言 prompt。

English:
- The current organizer flow sends a JSON payload directly as the `user prompt`, instead of wrapping it in the old flat-classification prompt.

```json
{
  "maxClusterDepth": "{max_cluster_depth_or_null}",
  "existingTree": "{current_tree_json}",
  "items": [
    {
      "itemId": "batch{n}_{m}",
      "name": "{name}",
      "path": "{path}",
      "relativePath": "{relative_path}",
      "size": "{size}",
      "createdAt": "{created_at_or_null}",
      "modifiedAt": "{modified_at_or_null}",
      "itemType": "{file_or_directory}",
      "modality": "{text|image|video|audio|directory}",
      "summary": "{summary_text}",
      "summaryDegraded": "{bool}",
      "summaryWarnings": ["..."],
      "provider": "{endpoint}",
      "model": "{model}"
    }
  ],
  "useWebSearch": "{bool}",
  "referenceStructure": "{optional_directory_structure_summary}"
}
```

### 3. Directory Summary Template / 目录摘要模板

中文：
- 目录摘要不会单独调用模型。
- 它会先被本地生成，再写入 `items[].summary`，和其他条目一起进入树状聚类。

English:
- The directory summary is not a standalone model prompt.
- It is generated locally first, then inserted into `items[].summary` before tree clustering.

Code / 代码位置:
- `src-tauri/src/organizer_runtime.rs`
- `summarize_directory_for_prompt`

中文模板：

```text
相对路径={relative_path}
总大小={size}
创建时间={created_at}
修改时间={modified_at}
文件数={file_count}
目录数={dir_count}
标记文件={marker_files}
应用特征={app_signals}
顶层条目={top_level_entries}
主要扩展名={dominant_extensions}
```

English template:

```text
relativePath={relative_path}
totalSize={size}
createdAt={created_at}
modifiedAt={modified_at}
totalFiles={file_count}
totalDirectories={dir_count}
markerFiles={marker_files}
appSignals={app_signals}
topLevelEntries={top_level_entries}
dominantExtensions={dominant_extensions}
```

### 4. File Summary Fallback / 文件摘要降级模板

中文：
- 当文本正文不可读，或文件只能走 metadata 摘要时，会使用这些模板。

English:
- These templates are used when text content cannot be read or when the item can only be summarized from metadata.

Code / 代码位置:
- `src-tauri/src/organizer_runtime.rs`
- `extract_plain_text_summary`
- `build_empty_extraction`
- `build_local_summary`

#### Text File Fallback / 文本文件降级摘要

中文模板：

```text
名称={name}
相对路径={relative_path}
大小={size}
创建时间={created_at}
修改时间={modified_at}
```

English template:

```text
name={name}
relativePath={relative_path}
size={size}
createdAt={created_at}
modifiedAt={modified_at}
```

#### Metadata-only Summary / 仅 metadata 摘要

中文模板：

```text
名称={name}
相对路径={relative_path}
模态={modality}
大小={size}
创建时间={created_at}
修改时间={modified_at}
```

English template:

```text
name={name}
relativePath={relative_path}
modality={modality}
size={size}
createdAt={created_at}
modifiedAt={modified_at}
```

### 5. Advisor Suggestion Flow / 顾问页建议流

中文：
- 顾问页当前使用的 prompt 不在旧文档版本里，实际以 `src-tauri/src/advisor_runtime.rs` 为准。
- 顾问会话启动时，如果本地已有扫描/整理结果，会优先复用本地建议；只有本地建议不足时，才会调用 AI 生成建议。
- 顾问页发给 AI 的 `user prompt` 主要是 JSON payload，不是固定自然语言长提示词。

English:
- The advisor prompts were missing from older versions of this document; the source of truth is `src-tauri/src/advisor_runtime.rs`.
- On session start, the advisor prefers existing local scan/organize suggestions and only calls the model when those are insufficient.
- The advisor mostly sends a JSON payload as the `user prompt` rather than a long natural-language wrapper.

Code / 代码位置:
- `src-tauri/src/advisor_runtime.rs`
- `build_preference_extraction_system_prompt`: 从用户消息提取偏好的 `system prompt`
- `build_suggestion_generation_system_prompt`: 生成建议列表的 `system prompt`
- `build_suggestion_revision_system_prompt`: 根据后续反馈修订建议的 `system prompt`
- `generate_suggestions_from_context`: 组装顾问初始化 `user prompt` JSON
- `advisor_session_start`: 启动顾问会话，并在本地建议不足时触发建议生成
- `advisor_message_send`: 发送顾问页消息，触发偏好提取与建议修订
- `chat_completion`: 实际把 `system` / `user` message 发给模型

#### Suggestion Generation System Prompt / 初始建议 system prompt

中文说明：
- 当前代码中该 prompt 直接写成英文字符串，再通过 `response_language` 约束输出字段语言。

English template:

```text
You are an AI file cleanup and organization advisor.
Return JSON only.
Schema: {"suggestions":[{"suggestionId":"...","kind":"move|archive|delete|keep|review","path":"...","targetPath":"... optional","title":"...","summary":"...","risk":"low|medium|high","confidence":"high|medium|low","why":["..."],"triggeredPreferences":["..."],"requiresConfirmation":true,"executable":true}],"reply":"..."}
Follow the current advisor mode exactly: {mode}.
Prefer using structured local context first.
Never recommend direct deletion for uncertain items.
Use kind=review for ambiguous or risky items.
Use kind=keep when the item is likely important or protected by user preference.
Use kind=delete only for low-risk items.
Use targetPath only for move or archive suggestions.
Keep suggestions practical and non-overlapping.
The reply, title, summary, why, and triggeredPreferences fields must be written in {output_language} only.
```

#### Suggestion Generation User Payload / 初始建议 user payload

中文：
- 顾问初始化建议时，直接把下面结构的 JSON 序列化后作为 `user prompt` 发送。

English:
- During initial advisor suggestion generation, the app serializes the following JSON structure and sends it as the `user prompt`.

```json
{
  "mode": "{mode}",
  "rootPath": "{root_path}",
  "scanSummary": "{context_summary.scanSummary}",
  "fileCandidates": [
    {
      "path": "{path}",
      "name": "{name}",
      "categoryPath": ["... optional"],
      "summary": "{summary_or_null}",
      "reason": "{scan_reason_optional}",
      "risk": "low|medium|high",
      "kindHint": "move|delete"
    }
  ],
  "existingTree": "{latest_tree_or_null}",
  "preferences": [],
  "recentConversationSummary": []
}
```

#### Preference Extraction Prompt / 偏好提取 prompt

Code / 代码位置:
- `src-tauri/src/advisor_runtime.rs`
- `build_preference_extraction_system_prompt`
- `advisor_message_send` 中的 `preference_prompt`

中文：
- 当用户在顾问页输入一句话后，系统会先发送一轮偏好提取请求。

English user payload:

```json
{
  "message": "{user_message}",
  "mode": "{mode}",
  "preferences": "{current_preferences}",
  "outputLanguage": "{localized_output_language_name}"
}
```

#### Suggestion Revision Prompt / 建议修订 prompt

Code / 代码位置:
- `src-tauri/src/advisor_runtime.rs`
- `build_suggestion_revision_system_prompt`
- `advisor_message_send` 中的 revision payload

中文：
- 如果当前会话里已经有建议，顾问页不会重新整包生成，而是发送“修订建议”请求。

English user payload:

```json
{
  "message": "{user_message}",
  "suggestions": "{current_suggestions}",
  "preferences": "{current_preferences}",
  "mode": "{mode}"
}
```

## Notes / 备注

中文：
- 旧的 organizer 平面分类提示词已经从代码中删除。
- 文档现在只保留当前主流程仍在使用的提示词。
- 如果后面还要继续调 prompt，建议把模板再收拢成单独的 Rust 常量文件。

English:
- The old flat organizer prompts have been removed from the codebase.
- This document now only keeps prompts that are still used by the current flow.
- If you plan to iterate on prompts frequently, it would be better to centralize them into a dedicated Rust prompt constants file.
