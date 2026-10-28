# Upstream Tracking & Synchronization Record

本文档用于记录当前仓库（`origin/main`）与上游仓库（`upstream/main`）的分支基准点、本地分支特有修改摘要，以及后续同步上游变更的指引。

---

## 1. 上游仓库与基准分支点 (Merge Base / Sync Point)

- **上游仓库地址**: `https://github.com/tontinton/maki.git`
- **上游跟踪分支**: `upstream/main`
- **当前本地分支**: `main`（跟踪 `origin/main`: `git@github.com:cybershape/maki.git`）
- **最新同步的上游 Commit**:
  - **Commit SHA**: `08793b9f1972f38aa59148f7d98d99caaf7e5a98`
  - **Commit 日期**: `Mon Oct 26 13:49:17 2026 +0100`
  - **Commit 作者**: `Falk Woldmann Lu <52786457+FalkWoldmann@users.noreply.github.com>`
  - **Commit 信息**: `refactor(otel): export through the official OpenTelemetry SDK`
- **初始分叉点 (Initial Merge Base)**:
  - **Commit SHA**: `70348ec0a50b84544003cb119550778652e0d7d8`
  - **Commit 日期**: `Sun Oct 25 11:34:26 2026 +0100`
  - **Commit 信息**: `chore(deps): move monty to crates.io 1.x`

> 本次已成功合并 `upstream/main`（至 commit `08793b9f`）。

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
  - 沙箱文件系统操作全面适配内置工具重命名为 `list_dir` 的逻辑，同时兼容 `list` 钩子与槽位。

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
