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

说明：

1. 不再要求用户在会话开始前先选择“清理优先”或“整理优先”。
2. 这类处理倾向由用户在对话中直接表达，或以后作为可选快捷入口存在。

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

1. 用自然语言保存长期处理偏好。
2. 用自然语言保存长期归类偏好。
3. 用自然语言保存当前会话目标。
4. 在顾问阶段把偏好作为 AI 上下文使用。
5. 保留少量结构化规则用于程序安全校验。

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
6. 偏好既可以描述处理方式，也可以描述归类方式。
7. 当用户对建议、执行方式或归类结果提出反对、修正或补充时，默认优先记为偏好候选。

归类偏好示例：

1. “安装包和压缩包不要放在同一类。”
2. “截图和聊天图片分开。”
3. “财务表格优先归到文档资料下。”
4. “项目源码压缩包单独成类。”

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

### 6.1 `get_directory_overview`

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

### 6.2 `find_files`

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

### 6.3 `summarize_files`

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

### 6.4 `read_only_file_summaries`

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

### 6.5 `capture_preference`

作用：

1. 保存自然语言偏好。
2. 保存当前会话目标。
3. 让 AI 记住用户表达，而不是只依赖上下文窗口。

什么时候调：

1. 用户明确表达长期习惯时。
2. 用户说明这次任务目标时。
3. 用户对建议、执行方式或归类结果提出反对、修正或补充时。
4. AI 判断这句话值得记忆时。

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

### 6.6 `list_preferences`

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

### 6.7 `preview_plan`

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
3. 错误信息要明确提示模型先调用 `find_files` 生成筛选结果。
4. preview 阶段只做程序化一致性校验，避免把旧的筛选结果误用于当前意图。

### 6.8 `execute_plan`

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

### 6.9 `rollback_plan`

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

### 6.10 `apply_reclassification`

作用：

1. 接收模型直接输出的固定 `reclassificationRequest JSON`。
2. 支持局部修订，不要求整棵树重跑。
3. 直接应用归类修订。
4. 让后续顾问和筛选基于新的分类继续工作。

什么时候调：

1. 用户明确表示当前分类不满意时。
2. 用户已经说明希望怎么改分类时。
3. AI 已经把用户意图整理成固定格式的归类修订请求时。

输入：

1. `sessionId`
2. `request`
   - `intentSummary`
   - `change`
     - `type`
     - `selectionId`
     - `sourceCategoryId`
     - `targetCategoryId`
     - `newCategoryName`
3. `applyPreferenceCapture`

输出：

1. `reclassificationJobId`
2. `message`
3. `changeSummary`
4. `updatedTreeText`
5. `change`
   - `type`
   - `sourceCategoryId`
   - `targetCategoryId`
   - `newCategoryName`
   - `selectionId`
   - `fileCount`
6. `invalidated`
   - `selection`
   - `preview`

说明：

1. `change` 是统一入口，内部通过 `change.type` 分派到不同修改模板。
2. 这个工具优先做局部归类修订，不默认全量重跑。
3. 模型不直接提交整棵新树，而是提交结构化归类修订请求。
4. 这个工具不负责再做一次自然语言理解，不要求在 tool 内再调用模型。
5. 这个工具直接应用归类 patch，不要求先 preview。
6. 应用成功后当前活跃的 `selection` 和 `preview` 都要失效。

支持的 `change.type`：

1. `rename_category`
   - 必填：`sourceCategoryId`、`newCategoryName`
2. `move_selection_to_category`
   - 必填：`selectionId`、`targetCategoryId`
3. `split_selection_to_new_category`
   - 必填：`selectionId`、`sourceCategoryId`、`newCategoryName`
4. `merge_category_into_category`
   - 必填：`sourceCategoryId`、`targetCategoryId`
5. `delete_empty_category`
   - 必填：`sourceCategoryId`

模型输出的 `reclassificationRequest JSON` 结构：

```json
{
  "intentSummary": "把安装包和压缩包分开，当前这一批压缩文件不要放在安装包里。",
  "change": {
    "type": "split_selection_to_new_category",
    "selectionId": "selection_003",
    "sourceCategoryId": "installers",
    "newCategoryName": "压缩包"
  }
}
```

典型工具输入示例：

```json
{
  "sessionId": "session_001",
  "request": {
    "intentSummary": "把安装包和压缩包分开，当前这一批压缩文件不要放在安装包里。",
    "change": {
      "type": "split_selection_to_new_category",
      "selectionId": "selection_003",
      "sourceCategoryId": "installers",
      "newCategoryName": "压缩包"
    }
  },
  "applyPreferenceCapture": true
}
```

典型输出示例：

```json
{
  "reclassificationJobId": "reclass_job_001",
  "message": "已应用归类修订。",
  "changeSummary": "已把 12 个压缩文件从“安装包”中拆出，形成新分类“压缩包”。",
  "updatedTreeText": "安装包 (42 -> 30)\\n新增: 压缩包 (12)",
  "change": {
    "type": "split_selection_to_new_category",
    "sourceCategoryId": "installers",
    "targetCategoryId": "archives",
    "newCategoryName": "压缩包",
    "selectionId": "selection_003",
    "fileCount": 12
  },
  "invalidated": ["selection", "preview"]
}
```

### 6.11 `rollback_reclassification`

作用：

1. 回滚最近一次可回滚的归类修订。
2. 恢复归类主结果和树视图。
3. 让用户可以在直接应用后撤回归类修改。

什么时候调：

1. 用户要求撤回最近一次归类修改时。
2. 已有有效 `reclassificationJobId` 且该修改可回滚时。

输入：

1. `sessionId`
2. `reclassificationJobId`

输出：

1. `message`
2. `updatedTreeText`
3. `rolledBack`
4. `invalidated`
   - `selection`
   - `preview`

说明：

1. 归类修订回滚不影响执行类回滚。
2. 回滚成功后当前活跃的 `selection` 和 `preview` 都要失效。

## 7. Tool 错误返回

错误返回原则：

1. 给模型的错误返回优先使用一句话文本。
2. 这句话同时包含错误信息和下一步建议。
3. 不默认要求向模型返回 JSON 错误对象。
4. 如果内部需要错误码，可在程序内部保留，不必直接暴露给模型。

建议格式：

```text
当前筛选结果已失效，请先重新调用 find_files 生成新的 selection，再继续生成预览。
```

示例：

1. `preview_plan`
   - `当前计划缺少 selectionId，请先调用 find_files 生成筛选结果，再继续生成预览。`
2. `execute_plan`
   - `当前预览不存在或已过期，请先重新生成 preview，再执行。`
3. `apply_reclassification`
   - `当前归类修改缺少必填字段，请补齐 change.type 对应参数后重试。`
4. `rollback_plan`
   - `当前执行记录不可回滚，请不要继续尝试回滚这个任务。`
5. `rollback_reclassification`
   - `当前归类修改记录不存在或不可回滚，请先确认最近一次归类修改是否成功。`

## 8. 页面结构

顾问页采用单列对话式 AI 工作台布局。

普通建议保持自然语言回复，只有结构化结果才卡片化。

页面从上到下分为：

1. 顶部会话初始化区 / 上下文条。
2. 主消息流。
3. 结果型卡片插入位。
4. 底部固定输入区。

### 8.1 页面主骨架

顾问页不再以左右双栏作为核心结构，而是以单列消息流为主。

页面骨架规则：

1. 主视线永远落在消息流。
2. 树结果、分类修改、偏好、预览、执行结果都挂在消息流中。
3. 不再把建议区、预览区、执行区做成并列后台面板。
4. 输入区固定在页面底部，消息流独立滚动。
5. 页面以桌面端优先，但窄屏下仍保持单列结构。

### 8.2 顶部会话初始化区与上下文条

首次进入顾问页时，页面顶部展示会话初始化区。

初始化区要包含：

1. 当前工作目录。
2. 顾问模式。
3. 最近可复用的盘点来源。
4. 启动会话入口。

初始化区行为：

1. 用户首次进入时默认展开。
2. 用户启动会话后，初始化区自动折叠为上下文条。
3. 折叠后的上下文条持续显示：
   - 当前目录
   - 当前模式
   - 当前盘点来源
   - 展开入口
4. 用户可以再次展开上下文条，重新选择目录、切换模式或重建会话。

这样做的目的：

1. 会话开始前保证用户知道 AI 基于什么上下文工作。
2. 会话开始后减少顶部占高，把注意力还给消息流。

### 8.3 主消息流

消息流是顾问页的主舞台。

消息流按时间顺序展示每一轮内容：

1. 用户消息。
2. AI 自然语言回复。
3. 该轮附着的结构化结果卡片。

消息流规则：

1. 普通 AI 建议只用自然语言表达，不单独生成建议卡。
2. 一轮对话可以只包含文本，也可以包含文本加结果卡。
3. 结果卡是某一轮消息的附着物，不是独立常驻区域。
4. 新内容插入后，页面自动定位到最新一轮。
5. 会话开始后，首批消息应优先建立目录认知，而不是直接进入执行。

### 8.4 结果型卡片

顾问页只保留以下 5 类结果型卡片，卡片样式统一。

#### 8.4.1 树结果卡

作用：

1. 展示当前分类状态。
2. 可以是全量树，也可以是局部树。

规则：

1. 只读。
2. 不承担计划确认或执行操作。
3. 用于帮助用户和模型理解当前目录结构。
4. 全量树用于建立整体认知，局部树用于追问某一路径或某一类时补充展示。

#### 8.4.2 分类修改结果卡

作用：

1. 说明这一轮对话让哪些分类结果发生了变化。

规则：

1. 只读。
2. 要说明从什么分类改成什么分类。
3. 要说明影响了哪些对象。
4. 要说明这次修改是否会影响后续建议或计划。

#### 8.4.3 偏好提炼卡

作用：

1. 展示 AI 从用户话里提炼出的明确偏好。

规则：

1. 不做常驻区域。
2. 只在本轮确实提炼出偏好时出现。
3. 带操作按钮：
   - `应用 / 保存`
   - `撤销`
4. 用户处理完后，页面继续回到普通消息流。

#### 8.4.4 计划预览卡

作用：

1. 展示已经收敛成确定计划的结构化预览结果。

规则：

1. 只在用户意图足够明确且系统已生成有效预览时出现。
2. 必须明确这是“预览”，不是已执行结果。
3. 带操作按钮：
   - `执行`
4. 计划预览卡是从理解阶段进入落地阶段的关键分界点。

#### 8.4.5 执行结果卡

作用：

1. 展示执行后的程序结果。
2. 也可承载最近一次回滚结果摘要。

规则：

1. 带操作按钮：
   - `撤销`
2. 只有 `rollbackAvailable=true` 时，撤销入口才允许可用。
3. 执行结果卡用于形成“预览 -> 执行 -> 回滚”的闭环反馈。

#### 8.4.6 卡片总规则

1. 普通 AI 建议不做结果卡，只保留自然语言回复。
2. 只有程序状态已经形成结构化结果时，才使用卡片。
3. 所有卡片使用统一骨架，不为不同卡片设计完全不同的视觉语言。

### 8.5 底部固定输入区

页面底部固定一个持续可用的对话输入区。

输入区要包含：

1. 多行输入框。
2. 发送按钮。
3. 当前会话状态提示。
4. 必要时的轻量输入引导。

输入区规则：

1. 始终可见，不随消息流滚走。
2. 支持连续多轮输入。
3. 发送后自动滚动到最新一轮内容。
4. 输入区是整个顾问页最稳定的交互锚点。

### 8.6 页面状态

顾问页需要在页面结构中显式定义状态，而不是临时拼 UI。

#### 8.6.1 空状态

1. 未开始会话时，展示初始化引导。
2. 无树结果时，提示需要先初始化或先让 AI 理解目录。
3. 无计划时，不显示计划预览卡。
4. 无执行记录时，不显示执行结果卡。

#### 8.6.2 加载状态

1. AI 回复中要有统一的处理中表现。
2. 生成预览中要有统一的处理中表现。
3. 执行中要有统一的处理中表现。
4. 回滚中要有统一的处理中表现。

#### 8.6.3 定位与滚动

1. 页面滚动主体是消息流。
2. 顶部上下文条固定在页面上部。
3. 底部输入区固定在页面下部。
4. 新卡片插入后自动定位到最新一轮。

### 8.7 统一交互与样式规则

所有结果卡采用统一样式骨架。

统一结构：

1. 卡片头部。
   - 类型标签
   - 标题
   - 时间或状态信息
2. 卡片正文。
   - 承载该类结果的核心内容
3. 卡片底部操作区。
   - 只在可操作卡片中出现

统一规则：

1. 只读卡没有底部操作区。
2. 可操作卡的按钮位置保持一致。
3. `树结果卡` 与 `分类修改结果卡` 为只读卡。
4. `偏好提炼卡`、`计划预览卡`、`执行结果卡` 为可操作卡。
5. 卡片之间通过内容和状态区分，不通过完全不同的样式语言区分。

## 9. 上下文组装规范

上下文组装不是模型工具，而是后台自动完成的工作。

每轮顾问调用前，后台要自动组装以下几部分：

1. `system`
2. `context payload`
3. 当前轮用户消息
4. 最近必要的工具结果

### 9.1 `system`

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

### 9.2 `context payload`

`context payload` 放当前轮真正需要的事实数据，推荐用 JSON。

建议格式：

```json
{
  "session": {
    "sessionId": "session_001",
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
2. 当前主流程阶段。
3. 当前是否可回滚。
4. 当前根目录。
5. `session` 记忆文本列表。
6. `global` 记忆文本列表。
7. 当前默认树视图文本。
8. 当前活跃的筛选卡片。
9. 当前活跃的预览卡片。
10. 当前会话内最近一次执行卡片。

`context payload` 中不要默认放的内容：

1. 完整归类主结果全文。
2. 全量文件列表。
3. 全部树视图。
4. 预览逐项明细。
5. 执行逐项明细。

### 9.3 当前轮用户消息

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

### 9.4 工具结果

工具结果不预先全部注入，只在需要时按轮次补充。

建议格式：

```text
[find_files]
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

### 9.5 组装规则

1. `system` 只放稳定规则。
2. 目录事实、记忆和最近状态放进 `context payload`。
3. 当前轮用户原话单独传，不并进 JSON。
4. 只有在信息不足时才补工具结果。
5. 默认只放一个树视图。
6. 默认只放摘要级结果，不放全量明细。
7. 进入执行前，优先补充预览相关结果。
8. 预览卡片和执行卡片可以出现在多轮对话中，但不能只依赖聊天历史。
9. 当前活跃的预览卡片和当前会话内最近一次执行卡片仍要显式放进 `context payload`。

### 9.6 阶段化 Tool Policy

阶段状态：

1. `workflowStage`
   - `understand`
   - `preview_ready`
   - `execute_ready`
2. `rollbackAvailable`
   - `true`
   - `false`

规则：

1. `find_files` 是进入主流程的起点。
2. 只有拿到有效 `selectionId` 后，才允许 `preview_plan`。
3. 只有拿到有效 `previewId` 后，才允许 `execute_plan`。
4. `execute_plan` 成功后：
   - `workflowStage` 回到 `understand`
   - `activeSelectionCard` 失效
   - `activePreviewCard` 失效
   - `latestExecutionCard` 更新
   - `rollbackAvailable=true`
5. `apply_reclassification` 成功后：
   - `workflowStage` 回到 `understand`
   - `activeSelectionCard` 失效
   - `activePreviewCard` 失效
   - 后续筛选和预览必须基于新的分类结果重新生成
6. `rollback_plan` 不属于主流程阶段切换条件。
7. `rollback_plan` 只取决于 `rollbackAvailable` 是否为 `true`。
8. `rollback_reclassification` 不属于主流程阶段切换条件。
9. `rollback_reclassification` 默认可用，但只有存在可回滚的归类修改记录时才允许真正调用成功。

工具暴露规则：

1. 严格受阶段限制的工具只有三个：
   - `find_files`
   - `preview_plan`
   - `execute_plan`
2. 其它工具默认可用：
   - `get_directory_overview`
   - `summarize_files`
   - `read_only_file_summaries`
   - `capture_preference`
   - `list_preferences`
   - `apply_reclassification`
   - `rollback_reclassification`
   - `rollback_plan`
3. `rollback_plan` 默认可见，但只有 `rollbackAvailable=true` 时才允许真正调用成功。

优先级：

1. `understand`
   - 优先：`get_directory_overview`、`find_files`
   - 其次：`summarize_files`、`list_preferences`
2. `preview_ready`
   - 优先：`preview_plan`
   - 其次：`find_files`、`summarize_files`
3. `execute_ready`
   - 优先：`execute_plan`
   - 其次：`preview_plan`
4. 任意阶段
   - 若 `rollbackAvailable=true`，则 `rollback_plan` 可作为侧边能力保留

## 10. 归类主结果

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

## 11. 树视图

树视图不是唯一主数据，而是完整归类结果的派生视图。

要支持的视图类型：

1. `summaryTree`
2. `sizeTree`
3. `timeTree`
4. `executionTree`
5. `partialTree`

## 12. 目标流程

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

## 13. 分类树结构

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

## 14. 数值字段表示

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

## 15. 后续继续讨论

1. 首轮建议的长度和口吻。
2. 完整归类结果还需要哪些字段。
3. 顾问级 tool 的最终接口。
4. 偏好记忆的数据模型。
5. 用户意图何时算明确。
