# 扫描清理本地规则说明

本文档记录当前代码内置的扫描/清理本地规则，以及默认展开规则。

主要实现位置：

- `src-tauri/src/scan_runtime.rs`
- `src-tauri/src/backend.rs`

## 当前分类命名

扫描分类统一使用以下 3 个值：

- `delete_all`：整个项目/目录/文件都可以删除
- `keep_all`：整个项目/目录/文件都应保留
- `expand_analysis`：当前节点不直接下结论，需要继续展开分析或人工复核

兼容说明：

- 旧值 `safe_to_delete` 会在读取时归一化为 `delete_all`
- 旧值 `keep` 会在读取时归一化为 `keep_all`
- 旧值 `suspicious` 会在读取时归一化为 `expand_analysis`

## 规则来源层级

当前扫描分类结果可能来自以下几类来源：

- `local_rule`：代码内置的本地静态规则
- `persistent_rule`：用户持久化保存的精确规则
- `baseline_reuse`：复用历史扫描基线结果
- `ai`：模型分析结果

补充说明：

- 内置 `local_rule` 不应该长期写入设置文件作为持久规则。
- 读取持久化规则时，会清理 `source == "local_rule"` 的旧记录，避免本地规则被陈旧设置污染。

## 当前内置 `local_rule` 规则

### 直接判定为 `keep_all`

- Windows 系统/程序根目录
  - 路径：
    - `C:\Windows`
    - `C:\Program Files`
    - `C:\Program Files (x86)`
    - `C:\ProgramData`
  - 分类：`keep_all`
  - 风险：`high`

- Git 元数据目录
  - 路径模式：最后一个路径段为 `.git` 的目录
  - 分类：`keep_all`
  - 风险：`high`

- 用户内容根目录
  - 路径模式：`X:\Users\<用户名>\...` 下的用户库根目录
  - 包含：
    - `Desktop`
    - `Documents`
    - `Pictures`
    - `Videos`
    - `Music`
  - 分类：`keep_all`
  - 风险：`high`

### 直接判定为 `delete_all` 的目录

- Windows 临时目录
  - 路径：
    - `C:\Windows\Temp`
    - `%LocalAppData%\Temp` 下的任意路径
  - 分类：`delete_all`
  - 风险：`low`

- npm 缓存
  - 路径模式：包含 `%LocalAppData%\npm-cache`
  - 分类：`delete_all`
  - 风险：`low`

- pip 缓存
  - 路径模式：包含 `%LocalAppData%\pip\Cache`
  - 分类：`delete_all`
  - 风险：`low`

- 前端/构建缓存目录
  - 路径模式：
    - `...\.next\cache`
    - `...\.nuxt`
    - `...\node_modules\.cache`
  - 分类：`delete_all`
  - 风险：`low`

- 已知浏览器缓存路径
  - 说明：这是代码内硬编码白名单，不表示 Windows 对这些路径提供了稳定契约
  - 路径模式：
    - `%LocalAppData%\Google\Chrome\User Data\*\Cache`
    - `%LocalAppData%\Google\Chrome\User Data\*\Code Cache`
    - `%LocalAppData%\Microsoft\Edge\User Data\*\Cache`
    - `%LocalAppData%\Microsoft\Edge\User Data\*\Code Cache`
  - 分类：`delete_all`
  - 风险：`low`

- 基于目录画像的 temp/cache 补充命中
  - 额外条件：目录画像里带有 `cache` 或 `temp` 标签
  - 但仍然只限制在这些已知安全路径族：
    - `%LocalAppData%\Temp`
    - `%LocalAppData%\npm-cache`
    - `%LocalAppData%\pip\Cache`
  - 分类：`delete_all`
  - 风险：`low`

### 直接判定为 `delete_all` 的文件

- 临时文件或未完成下载文件
  - 扩展名：
    - `.tmp`
    - `.temp`
    - `.crdownload`
    - `.part`
  - 分类：`delete_all`
  - 风险：`low`

### 默认判定为 `expand_analysis` 并展开直接子文件夹

这类规则的目标不是“直接深入整棵树”，而是把当前目录的直接子节点加入分析队列。

- `%LocalAppData%`
- `%LocalAppData%\Packages`
- `%LocalAppData%\Microsoft`
- `%LocalAppData%\Google`
- `%LocalAppData%\Adobe`
- `%LocalAppData%\JetBrains`
- `%LocalAppData%\NVIDIA`
- `%LocalAppData%\CrashDumps`

行为：

- 当前目录本身分类为 `expand_analysis`
- `should_expand = true`
- 运行时会把当前目录的直接子节点加入分析队列
- 后续是否继续向下，由子节点自身的分类结果决定

设计目的：

- 这些目录本身常常是应用容器，不适合直接删除
- 真正可删内容往往在下一层子目录
- 比“整棵树强制深挖”更稳，噪音更低

## 内置扫描裁剪规则

这些规则用于控制扫描遍历范围，不等同于“可删除”判定。

它们只在扫描目标路径严格等于 `C:\` 时生效。

### 直接跳过整棵子树

- `C:\System Volume Information`
- `C:\Recovery`
- `C:\$Recycle.Bin`
- `C:\Documents and Settings`
- `C:\Config.Msi`
- `C:\PerfLogs`

### 限制子树最大相对深度

- `C:\Windows`：最大相对深度 `2`
- `C:\Program Files`：最大相对深度 `2`
- `C:\Program Files (x86)`：最大相对深度 `2`
- `C:\ProgramData`：最大相对深度 `3`

### 其他遍历保护

- 跳过 reparse point

## 当前明确不纳入本地静态规则的范围

以下内容当前不会被 `local_rule` 自动命中：

- 仅仅名字叫 `Cache` 或 `Temp`，但不属于已知安全路径族的目录
- `Downloads` 目录本身
- 通用日志目录
- 通用归档目录
- Firefox 缓存路径
- Brave 缓存路径
- Opera 缓存路径
- 未被硬编码白名单覆盖的其他 Chromium 衍生浏览器缓存路径

## 持久化规则命名

持久化规则字段也统一使用新命名：

- `keepAllExact`
- `deleteAllExact`

兼容说明：

- 读取时仍兼容旧字段：
  - `keepExact`
  - `safeDeleteExact`
- 保存时只会写入新字段名

## 代码定位

- 本地分类规则：`src-tauri/src/scan_runtime.rs` 中的 `maybe_local_rule_decision()`
- 扫描裁剪规则：`src-tauri/src/scan_runtime.rs` 中的 `build_scan_prune_rules()`
- 分类归一化：`src-tauri/src/scan_runtime.rs` 中的 `normalize_scan_ai_classification()`
- 持久化规则清理：`src-tauri/src/backend.rs` 中的 `read_scan_persistent_rules_with_cleanup()`
