# AI Prompts Inventory / AI 提示词清单

## Overview / 概览

中文：
- 以代码为准，本文只保留当前仍在使用的提示词与摘要模板索引。
- 当前产品主链路已经收敛为 `归类 -> 顾问`，不再包含独立盘点阶段。
- 实际发送给模型的提示词语言由 `response_language` 决定。
- organizer 与 advisor 的很多 `user prompt` 都是 JSON payload，而不是长篇自然语言包装。

English:
- The code is the source of truth; this document only keeps an index of prompts and summary templates that are still in use.
- The product flow is now `organizer -> advisor`; there is no standalone scan stage anymore.
- The language sent to the model is selected by `response_language`.
- Many organizer and advisor requests send structured JSON payloads instead of long natural-language wrappers.

## Current Prompts / 当前在用提示词

### 1. Organizer Tree Clustering / Organizer 树状聚类

Code / 代码位置:
- `src-tauri/src/organizer_runtime.rs`
- `build_summary_agent_system_prompt`
- `build_organize_system_prompt`
- `summarize_directory_for_prompt`
- `summarize_batch_with_agent`
- `classify_organize_batch`

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
- 当前主流程直接把 JSON 作为 `user prompt` 发送给模型。

English:
- The organizer flow sends a JSON payload directly as the `user prompt`.

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

### 2. Directory Summary Template / 目录摘要模板

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

### 3. File Summary Fallback / 文件摘要降级模板

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

### 4. Advisor Suggestion Flow / 顾问页建议流

Code / 代码位置:
- `src-tauri/src/advisor_runtime`
- `build_preference_extraction_system_prompt`
- `build_suggestion_generation_system_prompt`
- `build_suggestion_revision_system_prompt`
- `generate_suggestions_from_context`
- `advisor_session_start`
- `advisor_message_send`

中文：
- 顾问启动时会优先复用最近一次归类结果；如果没有，就退回到目录元信息上下文。
- 顾问发给模型的 `user prompt` 主要是 JSON payload。

English:
- The advisor prefers reusing the latest organizer result and falls back to directory metadata when none exists.
- The advisor mostly sends JSON payloads as the `user prompt`.

#### Suggestion Generation System Prompt / 初始建议 system prompt

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

```json
{
  "mode": "{mode}",
  "rootPath": "{root_path}",
  "organizeSummary": "{context_summary.organizeSummary_or_null}",
  "directorySummary": "{context_summary.directorySummary}",
  "fileCandidates": [
    {
      "path": "{path}",
      "name": "{name}",
      "categoryPath": ["... optional"],
      "summary": "{summary_or_null}",
      "risk": "low|medium|high",
      "kindHint": "move|delete|review"
    }
  ],
  "existingTree": "{latest_tree_or_null}",
  "preferences": [],
  "recentConversationSummary": []
}
```

#### Preference Extraction Prompt / 偏好提取 prompt

```json
{
  "message": "{user_message}",
  "mode": "{mode}",
  "preferences": "{current_preferences}",
  "outputLanguage": "{localized_output_language_name}"
}
```

#### Suggestion Revision Prompt / 建议修订 prompt

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
- 旧的独立盘点提示词已经从主链路移除。
- 文档现在只保留当前主流程仍在使用的提示词。

English:
- The old standalone scan prompts have been removed from the main flow.
- This document only keeps prompts still used by the current flow.
