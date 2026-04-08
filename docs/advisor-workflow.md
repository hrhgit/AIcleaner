# 顾问页流程设计文档

以代码为准；本文只写目标业务流程和要做的事。

## 1. 目标

1. 扫描文件和目录，拿到基础信息。
2. 为文件和目录生成摘要。
3. 基于基础信息和摘要生成完整归类结果。
4. 基于完整归类结果生成树视图。
5. 在顾问页展示树视图。
6. AI 基于分类结果给出首轮自然语言建议。
7. 用户用自然语言表达自己的处理想法。
8. AI 在用户意图明确后通过 tool use 生成执行计划和预览。
9. 程序负责校验、执行和回滚。

## 2. 各阶段职责

### 2.1 扫描

要做的事：

1. 收集路径。
2. 收集名称。
3. 收集类型。
4. 收集大小。
5. 收集创建时间和修改时间。
6. 收集基础目录结构。

不做的事：

1. 不做风险判断。
2. 不做清理建议。
3. 不做保留建议。
4. 不做归档建议。
5. 不做 AI 分类。

### 2.2 摘要

要做的事：

1. 为文件生成摘要。
2. 为目录生成摘要。
3. 支持 `metadata_summary`、`model_summary_short`、`model_summary_normal` 三种摘要模式。
4. 默认批量摘要使用 `model_summary_short`。
5. 用户明确要求更详细时，或 AI 判断当前信息不足时，再使用 `model_summary_normal`。
6. 摘要结果写入数据库。
7. 支持系统流程调用，也支持顾问对话中按需调用。
8. 支持批量输入。
9. 支持并发请求。
10. 支持分批、重试、降并发等调度兜底。
11. 调度兜底后仍失败时返回错误结果。
12. 不自动退回到 `metadata_summary`。
13. 是否改用 `metadata_summary` 由用户决定。
14. 输出摘要置信度。
15. 输出摘要 warnings。

### 2.3 归类

要做的事：

1. 生成完整归类结果。
2. 为每个文件分配分类。
3. 保存文件基础信息、摘要、归类结果、建议方向等完整信息。
4. 基于完整归类结果生成不同用途的树视图。

### 2.4 顾问

要做的事：

1. 读取分类结果。
2. 读取偏好记忆。
3. 给出首轮自然语言建议。
4. 理解用户自由输入。
5. 判断用户意图是否明确。
6. 在意图明确后调用工具生成计划和预览。

### 2.5 程序

要做的事：

1. 做安全校验。
2. 生成执行预览。
3. 执行动作。
4. 记录执行结果。
5. 支持回滚。

## 3. 偏好

要做的事：

1. 用自然语言保存长期偏好。
2. 用自然语言保存当前会话目标。
3. 在顾问阶段把偏好作为 AI 上下文使用。
4. 保留少量结构化规则用于程序安全校验。

偏好记录结构：

```json
{
  "id": "mem_001",
  "scope": "global",
  "text": "安装包通常归档，不要直接删除。",
  "createdAt": "2026-04-09T10:30:00+08:00"
}
```

说明：

1. `scope` 只保留两种取值：
   - `global`
   - `session`
2. `global` 表示长期处理习惯。
3. `session` 表示当前会话内需要持续记住的内容。
4. 不再单独区分“本次目标”和“特殊例外”。
5. 当 `session` 和 `global` 冲突时，以 `session` 为准。

## 4. 首轮展示

要做的事：

1. 先展示归类结果树。
2. 再展示 2 到 4 条首轮自然语言建议。
3. 提供自然语言输入框让用户继续表达。

## 5. 执行计划

要做的事：

1. 不在聊天文本里输出执行 schema。
2. 在用户意图明确后通过 tool use 生成执行计划。
3. 通过 tool use 生成执行预览。
4. 由程序返回可执行数量、风险和阻止原因。

## 6. Tool Use

建议要做的工具：

### 6.1 `advisor_get_directory_overview`

作用：

1. 给模型看当前目录的概览。
2. 返回适合当前轮次的树视图。
3. 让模型先建立全局认知，再决定是否继续筛选文件。

什么时候调：

1. 顾问页初始化后。
2. AI 准备给首轮建议时。
3. AI 需要重新查看当前目录整体情况时。

输入：

1. `sessionId`
2. `viewType`
   - `summaryTree`
   - `sizeTree`
   - `timeTree`
   - `executionTree`
   - `partialTree`
3. `rootCategoryId`
4. `maxDepth`

输出：

1. `message`
2. `treeText`
3. `viewType`

### 6.2 `advisor_find_files`

作用：

1. 按文件维度从当前会话结果里取候选集。
2. 支持按类别、大小、时间、名称等条件组合筛选。
3. 支持排序。
4. 支持把“搜索单个文件”作为按文件名筛选的一种调用。

什么时候调：

1. 用户要求处理某些具体文件特征时。
2. AI 看完目录概览后要缩小范围时。
3. 生成执行计划前，需要先拿到命中文件时。
4. AI 想按文件名搜索某一个文件时。

输入：

1. `sessionId`
2. `categoryIds`
3. `nameQuery`
4. `nameExact`
5. `pathContains`
6. `extensions`
7. `minSizeBytes`
8. `maxSizeBytes`
9. `olderThanDays`
10. `newerThanDays`
11. `sortBy`
   - `name`
   - `size`
   - `modifiedAt`
12. `sortOrder`
   - `asc`
   - `desc`
13. `limit`

输出：

1. `message`
2. `total`
3. `selectionId`
4. `querySummary`
5. `sortBy`
6. `sortOrder`
7. `files`
   - `path`
   - `name`
   - `categoryId`
   - `sizeText`
   - `modifiedAgeText`
   - `summaryShort`
      - 无摘要时为 `null`

说明：

1. 每次筛选结果都要生成可复用的 `selectionId`。
2. 后续计划、预览和执行都基于 `selectionId`，不直接基于类别执行。
3. `querySummary` 用于帮助模型识别这次筛选到底是哪一批结果。

### 6.3 `advisor_summarize_files`

作用：

1. 为一批文件补摘要或刷新摘要。
2. 既可被系统流程调用，也可被模型在对话中调用。
3. 生成后的摘要要写入数据库。
4. 在当前模式无法完成时返回错误结果。

什么时候调：

1. 归类前需要为一批文件补摘要时。
2. 用户要求“详细看看这些文件”时。
3. AI 判断当前信息不够，无法继续判断时。
4. 某一批文件当前没有摘要，但后续决策需要摘要时。

输入：

1. `sessionId`
2. `paths`
3. `categoryIds`
4. `mode`
   - `metadata_summary`
   - `model_summary_short`
   - `model_summary_normal`
5. `missingOnly`
6. `batchSize`
7. `maxConcurrency`

输出：

1. `status`
   - `ok`
   - `error`
2. `message`
3. `mode`
4. `total`
5. `completed`
6. `failed`
7. `items`
   - `path`
   - `name`
   - `summaryShort`
   - `summaryNormal`
   - `warning`
8. `errors`
   - `path`
   - `reason`

说明：

1. 这个工具支持批量输入。
2. 这个工具支持并发请求。
3. 这个工具的内部实现要支持分批、重试、降并发。
4. 如果因为批量限制、限流、超时或网络不稳定而最终无法完成，就返回 `status=error`。
5. 不自动退回到 `metadata_summary`。

### 6.4 `advisor_read_only_file_summaries`

作用：

1. 只读取已有摘要。
2. 不触发生成。
3. 不写入数据库。

什么时候调：

1. 用户明确说“只看已有摘要”时。
2. 用户明确说“不要重新生成摘要”时。
3. 后台只读展示已有摘要时。

输入：

1. `sessionId`
2. `paths`
3. `categoryIds`
4. `detailLevel`
   - `short`
   - `normal`
5. `limit`

输出：

1. `message`
2. `total`
3. `items`
   - `path`
   - `name`
   - `summaryShort`
   - `summaryNormal`

### 6.5 `advisor_capture_preference`

作用：

1. 保存自然语言偏好。
2. 保存当前会话目标。
3. 让 AI 记住用户表达，而不是只依赖上下文窗口。

什么时候调：

1. 用户明确表达长期习惯时。
2. 用户说明这次任务目标时。
3. AI 判断这句话值得记忆时。

输入：

1. `sessionId`
2. `scope`
   - `session`
   - `global`
3. `text`
4. `sourceMessage`

输出：

1. `preferenceId`
2. `scope`
3. `text`
4. `createdAt`

### 6.6 `advisor_list_preferences`

作用：

1. 读取当前会话偏好和全局偏好。
2. 让 AI 在生成建议或计划前先对齐用户习惯。
3. 让 AI 判断新表达是否与已有偏好冲突。

什么时候调：

1. 对话开始时。
2. 用户提到“按之前习惯来”时。
3. AI 需要确认当前偏好背景时。

输入：

1. `sessionId`

输出：

1. `sessionPreferences`
2. `globalPreferences`

### 6.7 `advisor_preview_plan`

作用：

1. 接收模型直接输出的固定 `plan JSON`。
2. 把执行计划映射到文件级动作。
3. 给出可执行数量、阻止原因和风险信息。
4. 作为用户确认前的最后检查层。

模型输出的 `plan JSON` 结构：

```json
{
  "intentSummary": "先处理截图，文档别动",
  "targets": [
    {
      "selectionId": "selection_001",
      "action": "move"
    }
  ]
}
```

字段说明：

1. `intentSummary`
   - 记录这次计划的整体意图
   - 也承载补充说明
2. `targets`
   - 记录这次计划实际要操作的筛选结果
3. `selectionId`
   - 对应一次已存在的筛选结果
4. `action`
   - `archive`
   - `move`
   - `keep`
   - `review`
   - `delete`

什么时候调：

1. AI 判断用户意图已经足够明确时。
2. AI 已经输出固定格式的 `plan JSON` 时。
3. 当前计划已经对应到已有 `selectionId` 时。
4. 任何执行前都必须先调用 preview。
5. AI 准备请求用户确认前。

输入：

1. `sessionId`
2. `plan`
   - `intentSummary`
   - `targets`
     - `selectionId`
     - `action`

输出：

1. `previewId`
2. `summary`
   - `total`
   - `canExecute`
   - `blocked`
3. `entries`
   - `sourcePath`
   - `targetPath`
   - `action`
   - `risk`
   - `canExecute`
   - `warnings`

错误处理：

1. 如果 `plan` 中没有 `selectionId`，返回错误。
2. 如果 `selectionId` 无效、过期或不存在，返回错误。
3. 错误信息要明确提示模型先调用 `advisor_find_files` 生成筛选结果。
4. preview 阶段只做程序化一致性校验，避免把旧的筛选结果误用于当前意图。

### 6.8 `advisor_execute_plan`

作用：

1. 执行已经通过预览的计划。
2. 记录执行结果。
3. 返回执行摘要和逐项结果。

什么时候调：

1. 用户明确确认执行时。
2. 已有 `previewId` 且预览有效时。

约束：

1. 不允许跳过 preview 直接执行。
2. 没有有效 `previewId` 时必须先生成 preview。

输入：

1. `sessionId`
2. `previewId`

输出：

1. `jobId`
2. `summary`
   - `total`
   - `moved`
   - `archived`
   - `recycled`
   - `failed`
3. `entries`

### 6.9 `advisor_rollback_plan`

作用：

1. 回滚上一次可回滚的执行计划。
2. 返回回滚结果摘要。
3. 让顾问页保留“先执行，再撤回”的能力。

什么时候调：

1. 用户要求撤回最近一次操作时。
2. 已有 `jobId` 且该任务存在可回滚动作时。

输入：

1. `sessionId`
2. `jobId`

输出：

1. `rollbackId`
2. `summary`
   - `rolledBack`
   - `notRollbackable`
   - `failed`
3. `entries`

## 7. 页面结构

顾问页要做的区域：

1. 归类结果树区域。
2. 首轮建议区域。
3. 对话输入区域。
4. 执行预览区域。
5. 执行与回滚区域。

## 8. 上下文组装规范

上下文组装不是模型工具，而是后台自动完成的工作。

每轮顾问调用前，后台要自动组装以下几部分：

1. `system`
2. `context payload`
3. 当前轮用户消息
4. 最近必要的工具结果

### 8.1 `system`

`system` 只放稳定规则，不放当前目录事实，不放大段业务数据。

建议格式：

```text
你是文件整理顾问。

你的任务：
1. 基于当前上下文理解目录结构、用户偏好和本轮用户意图。
2. 首轮回复使用简短自然语言建议。
3. 当用户意图足够明确时，通过 tool use 生成计划和预览。

行为规则：
1. 不重新生成扫描结果。
2. 不重新生成归类结果。
3. 不在聊天文本中输出执行 schema。
4. 读取信息优先使用已有上下文；信息不足时再调用工具。
5. 当 session 记忆和 global 记忆冲突时，以 session 为准。
6. 无法确认时先澄清，不直接假设高风险动作。
7. 回复简短，优先给结论和下一步。
```

`system` 中要放的内容：

1. 顾问角色定义。
2. 首轮回复风格。
3. tool use 使用原则。
4. 记忆优先级规则。
5. 高风险场景下的保守原则。

`system` 中不要放的内容：

1. 当前目录路径。
2. 当前目录树结果。
3. 文件列表。
4. 用户偏好原文列表。
5. 最近一次预览明细。
6. 最近一次执行明细。

### 8.2 `context payload`

`context payload` 放当前轮真正需要的事实数据，推荐用 JSON。

建议格式：

```json
{
  "session": {
    "sessionId": "session_001",
    "mode": "balanced",
    "workflowStage": "understand",
    "rollbackAvailable": false
  },
  "directory": {
    "rootPath": "E:\\Download"
  },
  "memory": {
    "session": [
      "这次先处理截图，不动文档。"
    ],
    "global": [
      "安装包通常归档，不要直接删除。"
    ]
  },
  "overview": {
    "viewType": "summaryTree",
    "treeText": "下载目录\\n- 安装包: 42 项, 18.6 GB\\n- 截图: 310 项, 2.1 GB\\n- 文档: 67 项, 1.4 GB"
  },
  "recentState": {
    "activeSelectionCard": null,
    "activePreviewCard": null,
    "latestExecutionCard": null
  }
}
```

卡片格式：

```json
{
  "activeSelectionCard": {
    "selectionId": "selection_001",
    "querySummary": "截图类，90 天前，按大小降序",
    "total": 24
  },
  "activePreviewCard": {
    "previewId": "preview_001",
    "planId": "plan_001",
    "intentSummary": "先处理截图，文档不动",
    "topActions": [
      "截图 -> move",
      "安装包 -> review"
    ],
    "summary": {
      "total": 42,
      "canExecute": 30,
      "blocked": 12
    },
    "topBlockedReasons": [
      "项目目录保护",
      "文档被排除"
    ]
  },
  "latestExecutionCard": {
    "jobId": "job_001",
    "intentSummary": "归档安装包并整理截图",
    "summary": {
      "total": 28,
      "moved": 20,
      "archived": 8,
      "failed": 2
    }
  }
}
```

`context payload` 中要放的内容：

1. 当前会话标识。
2. 当前模式。
3. 当前主流程阶段。
4. 当前是否可回滚。
5. 当前根目录。
6. `session` 记忆文本列表。
7. `global` 记忆文本列表。
8. 当前默认树视图文本。
9. 当前活跃的筛选卡片。
10. 当前活跃的预览卡片。
11. 当前会话内最近一次执行卡片。

`context payload` 中不要默认放的内容：

1. 完整归类主结果全文。
2. 全量文件列表。
3. 全部树视图。
4. 预览逐项明细。
5. 执行逐项明细。

### 8.3 当前轮用户消息

当前轮用户输入不要并进 `context payload`，而是单独作为普通用户消息传入。

建议格式：

```text
先处理截图，文档别动，安装包我想再看看。
```

这一层只放：

1. 用户本轮原始表达。

这一层不要放：

1. 系统规则。
2. 上下文 JSON。
3. 工具结果拼接文本。

### 8.4 工具结果

工具结果不预先全部注入，只在需要时按轮次补充。

建议格式：

```text
[advisor_find_files]
找到 12 个文件，已按大小从大到小排序。
1. Screenshot-001.png | 截图 | 24 MB | 3 天前
2. Screenshot-002.png | 截图 | 18 MB | 7 天前
```

工具结果要放的内容：

1. 工具名。
2. 本次查询条件的摘要。
3. 命中数量。
4. 结果列表或结果摘要。

工具结果不要放的内容：

1. 无关工具的历史结果。
2. 重复的大段树文本。
3. 完整归类主结果全文。

### 8.5 组装规则

1. `system` 只放稳定规则。
2. 目录事实、记忆和最近状态放进 `context payload`。
3. 当前轮用户原话单独传，不并进 JSON。
4. 只有在信息不足时才补工具结果。
5. 默认只放一个树视图。
6. 默认只放摘要级结果，不放全量明细。
7. 进入执行前，优先补充预览相关结果。
8. 预览卡片和执行卡片可以出现在多轮对话中，但不能只依赖聊天历史。
9. 当前活跃的预览卡片和当前会话内最近一次执行卡片仍要显式放进 `context payload`。

### 8.6 阶段化 Tool Policy

阶段状态：

1. `workflowStage`
   - `understand`
   - `preview_ready`
   - `execute_ready`
2. `rollbackAvailable`
   - `true`
   - `false`

规则：

1. `advisor_find_files` 是进入主流程的起点。
2. 只有拿到有效 `selectionId` 后，才允许 `advisor_preview_plan`。
3. 只有拿到有效 `previewId` 后，才允许 `advisor_execute_plan`。
4. `advisor_execute_plan` 成功后：
   - `workflowStage` 回到 `understand`
   - `activeSelectionCard` 失效
   - `activePreviewCard` 失效
   - `latestExecutionCard` 更新
   - `rollbackAvailable=true`
5. `advisor_rollback_plan` 不属于主流程阶段切换条件。
6. `advisor_rollback_plan` 只取决于 `rollbackAvailable` 是否为 `true`。

工具暴露规则：

1. 严格受阶段限制的工具只有三个：
   - `advisor_find_files`
   - `advisor_preview_plan`
   - `advisor_execute_plan`
2. 其它工具默认可用：
   - `advisor_get_directory_overview`
   - `advisor_summarize_files`
   - `advisor_read_only_file_summaries`
   - `advisor_capture_preference`
   - `advisor_list_preferences`
   - `advisor_rollback_plan`
3. `advisor_rollback_plan` 默认可见，但只有 `rollbackAvailable=true` 时才允许真正调用成功。

优先级：

1. `understand`
   - 优先：`advisor_get_directory_overview`、`advisor_find_files`
   - 其次：`advisor_summarize_files`、`advisor_list_preferences`
2. `preview_ready`
   - 优先：`advisor_preview_plan`
   - 其次：`advisor_find_files`、`advisor_summarize_files`
3. `execute_ready`
   - 优先：`advisor_execute_plan`
   - 其次：`advisor_preview_plan`
4. 任意阶段
   - 若 `rollbackAvailable=true`，则 `advisor_rollback_plan` 可作为侧边能力保留

## 9. 归类主结果

归类阶段要保存完整结果数据，不只保存一棵展示树。

完整结果至少包含：

1. 文件基础信息
   - `path`
   - `name`
   - `type`
   - `sizeBytes`
   - `sizeText`
   - `createdAt`
   - `modifiedAt`
2. 文件摘要信息
   - `summary`
   - `summaryConfidence`
   - `summaryWarnings`
3. 文件归类信息
   - `categoryId`
   - `categoryPath`
   - `leafNodeId`
4. 分类节点信息
   - `categoryId`
   - `categoryName`
   - `parentCategoryId`
5. 分类统计信息
   - `itemCount`
   - `totalSizeBytes`
   - `totalSizeText`
6. 决策辅助信息
   - `risk`
   - `suggestedAction`

## 10. 树视图

树视图不是唯一主数据，而是完整归类结果的派生视图。

要支持的视图类型：

1. `summaryTree`
2. `sizeTree`
3. `timeTree`
4. `executionTree`
5. `partialTree`

## 11. 目标流程

1. 扫描基础信息。
2. 生成摘要。
3. 生成完整归类结果。
4. 基于完整归类结果生成树视图。
5. 展示分类树。
6. AI 给出首轮建议。
7. 用户自由输入想法。
8. AI 理解意图。
9. AI 调用 tool use 生成计划。
10. 程序返回预览。
11. 用户确认执行。
12. 程序执行和回滚。

## 12. 分类树结构

分类树是完整归类结果的一种视图。

每个节点都要包含：

1. `categoryId`
2. `categoryName`
3. `parentCategoryId`
4. `itemCount`
5. `totalSizeText`
6. `timeSummary`
7. `risk`
8. `suggestedAction`
9. `children`

节点说明：

1. `categoryId`
   - 节点唯一标识
2. `categoryName`
   - 节点名称
3. `parentCategoryId`
   - 父节点 ID
   - 根节点可为空
4. `itemCount`
   - 当前节点下的文件数量
5. `totalSizeText`
   - 当前节点下的总大小，人类可读文本，供 AI 理解和 UI 展示
6. `timeSummary`
   - 当前节点下的时间信息文本
   - 例如：`最近修改 3 天前`
7. `risk`
   - 当前节点的整体风险级别
8. `suggestedAction`
   - 当前节点的默认建议方向
   - 可取值：`archive` `move` `keep` `review` `delete`
9. `children`
   - 子节点数组

建议的节点结构：

```json
{
  "categoryId": "images",
  "categoryName": "图片",
  "parentCategoryId": null,
  "itemCount": 320,
  "totalSizeText": "2.1 GB",
  "timeSummary": "最近修改 2 天前",
  "risk": "medium",
  "suggestedAction": "move",
  "children": [
    {
      "categoryId": "images-screenshots",
      "categoryName": "截图",
      "parentCategoryId": "images",
      "itemCount": 280,
      "totalSizeText": "1.7 GB",
      "timeSummary": "最近修改 1 天前",
      "risk": "medium",
      "suggestedAction": "move",
      "children": []
    }
  ]
}
```

## 13. 数值字段表示

完整归类结果保留机器值。

树视图只返回给 AI 和 UI 更容易理解的文本值。

大小字段要求：

1. 保留 `sizeBytes`
2. 保留 `sizeText`
3. 保留 `totalSizeBytes`
4. 保留 `totalSizeText`

树视图中的大小字段：

1. 返回 `sizeText`
2. 返回 `totalSizeText`
3. 不返回 `sizeBytes`
4. 不返回 `totalSizeBytes`

时间字段要求：

1. 保留 `createdAt`
2. 保留 `modifiedAt`

树视图中的时间字段：

1. 返回 `timeSummary`
2. 不返回 `createdAt`
3. 不返回 `modifiedAt`

## 14. 后续继续讨论

1. 首轮建议的长度和口吻。
2. 完整归类结果还需要哪些字段。
3. 顾问级 tool 的最终接口。
4. 偏好记忆的数据模型。
5. 用户意图何时算明确。
