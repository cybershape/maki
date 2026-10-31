# Upstream Tracking & Synchronization Record

本文档用于记录当前仓库（`origin/main`）与上游仓库（`upstream/main`）的分支基准点、本地分支特有修改摘要，以及后续同步上游变更的指引。

---

## 1. 上游仓库与基准分支点 (Merge Base / Sync Point)

- **上游仓库地址**: `https://github.com/tontinton/maki.git`
- **上游跟踪分支**: `upstream/main`
- **当前本地分支**: `main`（跟踪 `origin/main`: `git@github.com:cybershape/maki.git`）
- **最新同步的上游 Commit**:
  - **Commit SHA**: `7ae4c5d10eb353ae41f67e5b7a5767fa7800fc51`
  - **Commit 日期**: `Fri Oct 30 20:35:58 2026 +0200`
  - **Commit 作者**: `Tony Solomonik <tony.solomonik@gmail.com>`
  - **Commit 信息**: `fix(background): make moved tasks read clearly in the chat and \`/tasks\``
- **初始分叉点 (Initial Merge Base)**:
  - **Commit SHA**: `70348ec0a50b84544003cb119550778652e0d7d8`
  - **Commit 日期**: `Sun Oct 25 11:34:26 2026 +0100`
  - **Commit 信息**: `chore(deps): move monty to crates.io 1.x`

> 本次已成功合并 `upstream/main`（至 commit `7ae4c5d1`）。

---

## 2. 本地分支在分支点之上的提交历史

从分叉点 `70348ec0a50b84544003cb119550778652e0d7d8` 开始，当前分支所包含的提交如下（按时间顺序由旧到新）：

| Commit SHA | 提交摘要 | 说明 |
|---|---|---|
| `d144281a` | `feat(acp): use on-disk model cache and wait for discovery on first run` | ACP 协议使用磁盘模型缓存，首启等待模型发现 |
| `676f0f5e` | `fix(code_execution): alias list tool to list_dir to avoid Python name collision` | 内置工具 `list` 别名为 `list_dir`，避免与 Python 内置关键字冲突 |
| `af42e76b` | `feat(providers): retain only OpenAI Responses API and remove all built-in/third-party providers` | **核心定制**: 仅保留 OpenAI Responses API，移除所有其它内置/第三方 provider 模块与插件 |
| `0960239a` | `Merge branch 'feat/acp-wait-for-models-discovery' into main` | 合并 ACP 模型发现等待特性分支 |
| `d6e7f018` | `Merge branch 'fix/code-execution-builtin-alias' into main` | 合并代码执行内置别名修复分支 |
| `aafeca09` | `refactor(tools): rename built-in tool list to list_dir` | 工具重构：将内置工具 `list` 正式重命名为 `list_dir` |
| `afe978f9` | `feat(providers): support thinking level and full model discovery fields for responses` | **核心定制**: 为 responses provider 完善 OpenAI 标准 thinking level (reasoning effort) 及 `/models` 发现接口字段完整解析 |
| `7da1e933` | `feat(acp): ensure discovered reasoning models send reasoning effort` | ACP 协议确保发现的推理模型携带 reasoning effort 参数 |
| `5297e50f` | `test(lua): match reserved slug error message for responses provider` | 修复并匹配 responses provider 保留 slug 报错测试 |
| `06b8b8ee` | `feat(acp): support thought_level session config option for reasoning effort` | ACP 协议支持通过 `thought_level` 配置项切换 reasoning effort |
| `6e81fa86` | `ci: run build and release only on tags for windows, linux, mac-arm64` | CI 仅在 tag 触发构建发布，仅包含 windows, linux, mac-arm64 |
| `89d6d5c0` | `feat(ui): support model name in picker and status bar via prefer_model_name` | UI 支持配置 `prefer_model_name`，在模型选择器和状态栏展示友好名称 |
| `cc5c037d` | `feat(ui): display context window usage as progress bar in status bar` | 状态栏上下文窗口使用率以字符进度条形式可视化展示 |
| `9fbeeaff` | `fix(code_execution): clarify description to discourage proxying single tool calls` | 明确 code_execution 提示词，避免代理单次工具调用 |
| `5e429368` | `feat(ui): add show_cost configuration to hide cost and pricing in UI` | 新增 `show_cost` 配置项，允许隐藏 UI 中的全部开销与定价 |
| `c206adff` | `Merge branch 'upstream/main' into main` | 合并上游提交（至 `08793b9f`），包含官方 OTel SDK 导出、输入多行/语法高亮重构、代码沙箱文件系统访问等 |
| `341da1ef` | `docs: update upstream tracking record to 08793b9f` | 更新上游跟踪记录文档至 `08793b9f` |
| `546567d2` | `feat(ui): add compact_tools mode for grouped tool and thinking view` | UI 支持紧凑工具与思考视图分组模式 |
| `ce9f9fc7` | `fix(ui): align compact tool group header, status text and thinking animation with pi-my-compact-ui` | 对齐紧凑工具组头部、状态文字及思考动画样式 |
| `e50c81e1` | `fix(ui): add waiting states for compact tool groups and support late tool names` | 紧凑工具组等待状态完善及 late tool name 支持 |
| `0b3b7569` | `fix(ui): remove redundant space after in-progress spinner in compact tool rows` | 移除紧凑工具行中运行 spinner 后的多余空格 |
| `5effe64b` | `fix(ui): separate compact tool groups by text output and discard empty groups` | 紧凑工具组按文本输出切分并丢弃空工具组 |
| `42732f86` | `feat(config): control agent compatibility sources with per-agent and all switches` | 支持配置 agent 兼容源控制（per-agent 及 all 开关） |
| `58af112a` | `fix(ui): reflow compact tool and thinking groups on resize` | 终端大小调整时重新排版紧凑工具和思考组 |
| `7896ade1` | `fix(ui): wrap chat text at display column boundaries` | 聊天文本严格按显示列边界折行 |
| `d32d13cb` | `feat(ui): add max_width config and cli option to center UI` | 新增 `max_width` 配置项与 CLI 选项以居中显示 UI |
| `2897baf9` | `feat(ui): refine compact tool group styles and waiting state titles` | 精炼紧凑工具组样式与等待状态标题 |
| `44cc5ef8` | `feat(acp): return model display names in config options` | ACP 配置项中返回模型友好展示名称 |
| `45ed00a4` | `fix(acp): include enum values and map header to title for question tool` | ACP 协议修复问题工具枚举值与标题映射 |
| `4df8ab3d` | `feat(acp): emit usage_update on session/new and session/load` | ACP 会话新建与加载时触发 `usage_update` |
| `cf98b38d` | `feat(acp): emit usage_update on model switch, model refresh, and compaction` | ACP 模型切换、刷新和压缩时触发 `usage_update` |
| `1a6f0151` | `fix(ui): re-open plan form after refinement and queue implement while streaming` | 修复计划表单优化后重开及流式状态排队执行 |
| `26d5a75b` | `fix(acp): cache model metadata and push updates on display name resolution` | ACP 缓存模型元数据并在解析展示名时推送更新 |
| `401b5406` | `Merge branch 'upstream/main' into main` | 合并上游提交（至 `ec1c1a62`），包含 model tier override 优先规则、Release fat LTO 构建优化、Websearch Kagi 搜索结果编号列表优化 |
| `d6094fa8` | `docs: update upstream tracking record to ec1c1a62` | 更新上游跟踪记录文档至 `ec1c1a62` |
| `2dfe0846` | `feat(ui): show batch child tools and durations in compact groups` | 紧凑工具组展示 batch 子工具及耗时 |
| `15b7359e` | `feat(ui): unify compact group busy animations as pulsing dots` | 紧凑工具组 busy 状态动画统一为脉冲点动效 |
| `5bcd27ee` | `feat(ui): show per-turn stats line when a turn completes (#1)` | 用户 turn 完成后显示单 turn 耗时及 token 统计行 |
| `87dd0f13` | `docs: add personal fork notice and upstream diff summary to README (#2)` | README 增加个人 fork 说明与上游差异摘要 |
| `3e3cc101` | `fix(ui): preserve compact tool groups, headers, and durations on session restore` | 会话恢复时保留紧凑工具组、标题与耗时 |
| `4e5a3f45` | `fix(ui): show turn stats once when the user turn finishes (#3)` | 修复每 turn 统计仅在用户 turn 结束时展示一次 |
| `4265d676` | `fix(ui): fix compilation errors and dead code in chat turn stats` | 修复 turn 统计中的编译错误和死代码 |
| `0227856d` | `feat(markdown): render LaTeX math expressions using txm` | 使用 txm 库在 markdown 中渲染 LaTeX 数学公式 |
| `f70d08c6` | `refactor(code_execution): drop legacy list tool compatibility` | 代码执行沙箱中移除旧版本 `list` 工具兼容垫片 |
| `2bfb89fc` | `feat(ui): add left padding to chat messages panel` | 聊天消息面板增加左侧内边距 padding |
| `b7d57572` | `feat(ui): capitalize tool group titles and show thinking duration` | 工具组标题首字母大写并展示思考耗时 |
| `c72e8fa1` | `Merge branch 'upstream/main' into main` | 合并上游提交（至 `1a8b897b`），包含 serde-saphyr 替代弃用的 serde_yaml、agent-client-protocol-schema 升级至 1.13.0、`sess:send` 运行中子 agent 消息推送、日志记录 project 标识等 |
| `6069e830` | `docs: update upstream tracking record to 1a8b897b` | 更新上游跟踪记录文档至 `1a8b897b` |
| `bcaa1d21` | `feat(storage): record timestamps in session log records` | 会话日志消息持久化记录 unix 时间戳 |
| `9104593f` | `fix(ui): remove leading padding in compact tool groups` | 移除紧凑工具组内部多余的起始左侧缩进 |
| `137a821e` | `feat(ui): include waiting time in compact thinking and tool call durations` | 紧凑工具组将等待模型响应的时间计入思考和首个工具调用的耗时 |
| `da932b30` | `feat(ui): summarize tool calls and thoughts in tool group header` | 紧凑工具组头部摘要展示工具调用和思考概要 |
| `24382572` | `fix(ui): use "thinking" instead of "thought" in group summary` | 紧凑工具组概要统一使用 "thinking" 术语 |
| `25eb4d73` | `feat(ui): support true queue execution, steer shortcut, and recall editing` | UI 支持队列化执行、Alt+S 即时干预快捷键、Recall 历史输入重入编辑 |
| `4d8dcb46` | `fix(ui): use theme colors for context window progress bar in status bar` | 状态栏上下文进度条应用主题配色 |
| `7771692d` | `fix(ui): dim context percentage and window size in status bar` | 状态栏上下文占比百分比与窗口大小显示样式调暗 |
| `19e40f75` | `feat(ui): support OSC 7501 Program Status Protocol` | UI 支持 OSC 7501 程序状态通知协议（Working, Idle, Done, Error） |
| `7f136ab7` | `feat(ui): display user messages with chevron and background, remove maki prefix` | UI 用户消息采用背景色块与 Chevron 装饰展示，移除 `maki> ` 命令行前缀 |
| `2c3a9d79` | `feat: show a model-written reason on every tool call` | 工具调用支持并展示模型填写的 `toolcall_reason` 原因说明 |
| `7b40aa5e` | `feat: store how long each thinking block took` | 会话与数据模型中记录每个思考块的耗时 |
| `e6679ed4` | `feat: encourage models to fill toolcall_reason` | 系统提示词引导模型填写 `toolcall_reason` |
| `29a7e495` | `fix: tell an injected toolcall_reason by its description` | 区分注入的 `toolcall_reason` 属性定义 |
| `7b20b515` | `build: leave LTO off unless a release asks for it` | 默认构建关闭 LTO 以提高编译速度，仅在 release-lto 配置下开启 |
| `f26eff25` | `feat: adjust prompt tool usage when batch, code_execution, or task are disabled` | 当 batch、code_execution 或 task 禁用时动态调整系统提示词 |
| `91af6fff` | `feat(ui): refine compact tool summaries with toolcall_reason` | 紧凑工具行摘要优先渲染 `toolcall_reason` |
| `df344898` | `feat: tell models not to mention file names in toolcall_reason` | 提示词明确要求模型在 `toolcall_reason` 中不要冗余提及文件名 |
| `cc535772` | `docs: update fork changes in README and add maintenance rule to AGENTS` | README 更新 fork 差异总结并向 AGENTS.md 增加 fork 维护准则 |
| `5b5dae02` | `feat(mcp): replace tool_search with code_execution external libraries` | MCP 工具发现与执行改为通过 Python `code_execution` 外部库模式，替代 tool_search |
| `4a93166c` | `feat(config): enable library_help in DEFAULT_BUILTINS` | 将 `library_help` 工具加入默认内置工具清单 |
| `265a90e5` | `feat(ui): format batch child tool summaries with toolcall_reason in compact mode` | 紧凑模式下格式化 batch 子工具摘要，展示 `toolcall_reason` |
| `cf8e3c13` | `feat(ui): enable inline image rendering for Rex terminal` | UI 支持 Rex 终端行内图片渲染 |
| `686e9a76` | `fix(mcp): await mcp ready before building frame tools and compaction` | 构建 frame 工具与上下文压缩前先等待 MCP 初始化就绪 |
| `c2e04192` | `feat(ui): support ratio and negative offset in max_width` | `max_width` 支持比例与负向偏移量计算表达式 |
| `da15dc8d` | `fix(mcp): normalize hyphenated identifiers for python sandbox and library_help` | MCP 工具和参数中的连字符统一转换为下划线以兼容 Python 语法标识符 |
| `fb49721e` | `fix(ui): apply max_width once when laying out the frame` | UI 布局时仅在顶层单次应用 `max_width` |
| `67e6fec3` | `feat(ui): sum compact thinking durations in turn completion stats line` | 每 turn 统计行汇总紧凑思考耗时 |
| `51d9955f` | `docs: describe MCP-through-code_execution and old-session incompatibility` | 文档描述通过代码执行使用 MCP 工具的机制及旧 session 兼容性说明 |
| `0e1f3227` | `fix(session): keep message timestamps across a headless resume` | 修复 headless 恢复会话时保留消息时间戳 |
| `ce0c8984` | `Merge branch 'upstream/main' into main` | 合并上游提交（至 `7ae4c5d1`），包含 slow tool call 后台任务管理（Ctrl+B）、`/tasks` 嵌套展示与交互优化、bash 输出行数/体积限制及全量日志落盘、Lua 插件任务隔离调度等 |

---

## 3. 本地核心定制与修改范围

后续跟进上游（`upstream/main`）时，重点关注以下冲突和功能适配：

### 3.1. Provider 体系收敛至 OpenAI Responses API
- **变动文件**: `maki-providers/`、`plugins/`、`site/docs/content/providers/`
- **主要逻辑**:
  - 移除了 Anthropic、Google、Copilot、Z.AI、xAI 等所有特定厂商的客户端实现和插件，统一收敛至 `ResponsesProvider`。
  - 在 `maki-providers/src/providers/responses.rs` 中实现了针对 OpenAI 系列模型（如 GPT-5.5、GPT-5.6、GPT-6、Codex 等）的 reasoning effort 方言映射规则（`dialect::GPT_5_6`, `dialect::GPT_6`, `dialect::CODEX` 等）。
  - 支持动态 `/models` 发现响应中的全量字段解析（包括单 token 与每百万 token 费率归一化转换、模态视觉支持、reasoning 布尔与 effort 级别、service_tiers 加速等）。

### 3.2. 内置工具名称重构
- **变动文件**: `maki-agent/src/tools/`、`plugins/`
- **主要逻辑**:
  - 内置目录列表工具从 `list` 重命名为 `list_dir`，防止在沙箱 Python 环境中遮蔽标准库命名。

### 3.3. ACP 协议缓存与发现
- **变动文件**: `maki-acp/`
- **主要逻辑**:
  - 增加了模型发现的本地持久化缓存并在冷启动时提供同步等待机制。
  - 支持 `thought_level` 会话配置选项以映射各推理级别的 reasoning effort。

### 3.4. UI 定制与显示增强
- **变动文件**: `maki-ui/`、`maki-config/`
- **主要逻辑**:
  - `show_cost`: 支持在配置文件中设置 `show_cost = false`，从而在状态栏、Usage 统计弹窗和模型选择器中彻底隐藏价格和计费信息。
  - `prefer_model_name`: 支持在状态栏和模型选择器中优先显示模型的自然名称（如 "GPT-6.1 Sol"）而非原始 slug。
  - 上下文使用率进度条：状态栏支持以进度条（如 `[████░░░░░░] 40.0%/1.0M`）直观呈现当前会话的 context window 消耗。
  - 长通知自适应让路：状态栏 flash 通知较长时，优先保证通知文本完整展示，自动收缩或隐藏常驻的 path 与 model 显示。

### 3.5. CI 与发布流程精简
- **变动文件**: `.github/workflows/`
- **主要逻辑**:
  - 构建发布精简至 `windows`、`linux` 和 `mac-arm64`，移除了 NetBSD 等构建目标；仅在 git tag 时触发 release。

### 3.6. 解释器与沙箱文件系统访问适配
- **变动文件**: `plugins/code_execution/`、`maki-lua/`
- **主要逻辑**:
  - 沙箱文件系统操作全面适配内置工具重命名为 `list_dir` 的逻辑。

---

## 4. 跟踪与同步 Upstream 的操作指南

当需要拉取并合并上游 `upstream/main` 的最新提交时，推荐执行以下流程：

### 4.1. 拉取上游变更
```bash
git fetch upstream main
```

### 4.2. 查看上游的新增提交与差异
```bash
# 查看自上次分支点以来上游的新增提交
git log --oneline 70348ec0a50b84544003cb119550778652e0d7d8..upstream/main

# 查看上游与分支点的代码改动明细
git diff 70348ec0a50b84544003cb119550778652e0d7d8..upstream/main --stat
```

### 4.3. 推荐同步方案

#### 方案 A: 基于变基 (Rebase)
适合保持提交历史整洁（如果未强依赖原有 merge commit）：
```bash
# 切换并确保工作区干净
git checkout main
git status

# 变基到最新的上游 main 分支
git rebase upstream/main
```
> **处理冲突重点**:
> - 上游若修改了已被移除的 provider（如 `maki-providers/src/providers/anthropic` 等），在 rebase 冲突时直接选择丢弃或接受本地删除。
> - 若上游更新了 agent 核心、UI、解释器、配置系统等通用模块，保留本地对于 `list_dir` 或 Responses 协议的相关改动。

#### 方案 B: 选择性合并 / Cherry-pick
若只需引入 upstream 的特定 bugfix 或功能模块，可使用 cherry-pick：
```bash
git cherry-pick <upstream-commit-sha>
```

### 4.4. 同步完成后更新此文档
同步完成后，重新运行：
```bash
git merge-base main upstream/main
```
并将得到的最新的 commit SHA、日期和提交信息更新至本文档第 1 节中，以便下一次追踪。
