# Upstream Tracking & Synchronization Record

This document records the synchronization point between this repository (`origin/main`) and upstream (`upstream/main`), summarizes changes specific to this fork, and provides guidance for future upstream synchronization.

---

## 1. Upstream Repository and Baseline (Merge Base / Sync Point)

- **Upstream repository**: `https://github.com/tontinton/maki.git`
- **Upstream tracking branch**: `upstream/main`
- **Current local branch**: `main` (tracking `origin/main`: `git@github.com:cybershape/maki.git`)
- **Latest synchronized upstream commit**:
  - **Commit SHA**: `e9ca885c272154ce6714bb83ee6cab9d9128f293`
  - **Commit date**: `Sun Nov 8 00:26:51 2026 +0200`
  - **Commit author**: `Tony Solomonik <tony.solomonik@gmail.com>`
  - **Commit message**: `fix(mcp): find OAuth metadata appended to a path issuer`
- **Initial fork point (Initial Merge Base)**:
  - **Commit SHA**: `70348ec0a50b84544003cb119550778652e0d7d8`
  - **Commit date**: `Sun Oct 25 11:34:26 2026 +0100`
  - **Commit message**: `chore(deps): move monty to crates.io 1.x`

> `upstream/main` has been successfully merged through commit `e9ca885c`.

---

## 2. Local Commit History Since the Fork Point

The current branch includes the following commits since fork point `70348ec0a50b84544003cb119550778652e0d7d8`, listed from oldest to newest:

| Commit SHA | Commit summary | Notes |
|---|---|---|
| `d144281a` | `feat(acp): use on-disk model cache and wait for discovery on first run` | Use an on-disk model cache for ACP and wait for model discovery on first run |
| `676f0f5e` | `fix(code_execution): alias list tool to list_dir to avoid Python name collision` | Alias the built-in `list` tool to `list_dir` to avoid a collision with Python's built-in `list` |
| `af42e76b` | `feat(providers): retain only OpenAI Responses API and remove all built-in/third-party providers` | **Core customization**: Retain only the OpenAI Responses API and remove all other built-in and third-party provider modules and plugins |
| `0960239a` | `Merge branch 'feat/acp-wait-for-models-discovery' into main` | Merge the ACP model discovery waiting feature branch |
| `d6e7f018` | `Merge branch 'fix/code-execution-builtin-alias' into main` | Merge the code execution built-in alias fix branch |
| `aafeca09` | `refactor(tools): rename built-in tool list to list_dir` | Rename the built-in `list` tool to `list_dir` |
| `afe978f9` | `feat(providers): support thinking level and full model discovery fields for responses` | **Core customization**: Add OpenAI-standard thinking level (reasoning effort) handling and complete `/models` discovery field parsing to the Responses provider |
| `7da1e933` | `feat(acp): ensure discovered reasoning models send reasoning effort` | Ensure reasoning models discovered through ACP send the reasoning effort parameter |
| `5297e50f` | `test(lua): match reserved slug error message for responses provider` | Fix the test to match the Responses provider's reserved slug error message |
| `06b8b8ee` | `feat(acp): support thought_level session config option for reasoning effort` | Support selecting reasoning effort through the ACP `thought_level` session option |
| `6e81fa86` | `ci: run build and release only on tags for windows, linux, mac-arm64` | Build and release only on tags, targeting Windows, Linux, and macOS arm64 |
| `89d6d5c0` | `feat(ui): support model name in picker and status bar via prefer_model_name` | Add `prefer_model_name` to show friendly model names in the model picker and status bar |
| `cc5c037d` | `feat(ui): display context window usage as progress bar in status bar` | Visualize context window usage with a character-based progress bar in the status bar |
| `9fbeeaff` | `fix(code_execution): clarify description to discourage proxying single tool calls` | Clarify the code_execution prompt to discourage proxying individual tool calls |
| `5e429368` | `feat(ui): add show_cost configuration to hide cost and pricing in UI` | Add `show_cost` to hide all cost and pricing information in the UI |
| `c206adff` | `Merge branch 'upstream/main' into main` | Merge upstream through `08793b9f`, including the official OTel SDK exporter, multiline input and syntax highlighting refactoring, and sandbox filesystem access |
| `341da1ef` | `docs: update upstream tracking record to 08793b9f` | Update the upstream tracking record to `08793b9f` |
| `546567d2` | `feat(ui): add compact_tools mode for grouped tool and thinking view` | Add compact mode for grouped tool calls and thinking |
| `ce9f9fc7` | `fix(ui): align compact tool group header, status text and thinking animation with pi-my-compact-ui` | Align compact tool group headers, status text, and thinking animation styles |
| `e50c81e1` | `fix(ui): add waiting states for compact tool groups and support late tool names` | Improve compact tool group waiting states and support tool names that arrive later |
| `0b3b7569` | `fix(ui): remove redundant space after in-progress spinner in compact tool rows` | Remove the extra space after the spinner in compact tool rows |
| `5effe64b` | `fix(ui): separate compact tool groups by text output and discard empty groups` | Split compact tool groups at text output and discard empty groups |
| `42732f86` | `feat(config): control agent compatibility sources with per-agent and all switches` | Add per-agent and global switches for agent compatibility sources |
| `58af112a` | `fix(ui): reflow compact tool and thinking groups on resize` | Reflow compact tool and thinking groups when the terminal is resized |
| `7896ade1` | `fix(ui): wrap chat text at display column boundaries` | Wrap chat text at display column boundaries |
| `d32d13cb` | `feat(ui): add max_width config and cli option to center UI` | Add the `max_width` configuration and CLI option to center the UI |
| `2897baf9` | `feat(ui): refine compact tool group styles and waiting state titles` | Refine compact tool group styles and waiting state titles |
| `44cc5ef8` | `feat(acp): return model display names in config options` | Return friendly model display names in ACP configuration options |
| `45ed00a4` | `fix(acp): include enum values and map header to title for question tool` | Fix question tool enum values and header-to-title mapping in ACP |
| `4df8ab3d` | `feat(acp): emit usage_update on session/new and session/load` | Emit `usage_update` when ACP sessions are created or loaded |
| `cf98b38d` | `feat(acp): emit usage_update on model switch, model refresh, and compaction` | Emit `usage_update` on ACP model switches, model refreshes, and compaction |
| `1a6f0151` | `fix(ui): re-open plan form after refinement and queue implement while streaming` | Reopen the plan form after refinement and queue implementation while streaming |
| `26d5a75b` | `fix(acp): cache model metadata and push updates on display name resolution` | Cache ACP model metadata and push updates when display names are resolved |
| `401b5406` | `Merge branch 'upstream/main' into main` | Merge upstream through `ec1c1a62`, including model tier override precedence, release fat LTO build optimization, and numbered Kagi websearch results |
| `d6094fa8` | `docs: update upstream tracking record to ec1c1a62` | Update the upstream tracking record to `ec1c1a62` |
| `2dfe0846` | `feat(ui): show batch child tools and durations in compact groups` | Show batch child tools and their durations in compact tool groups |
| `15b7359e` | `feat(ui): unify compact group busy animations as pulsing dots` | Use pulsing dots for all compact group busy animations |
| `5bcd27ee` | `feat(ui): show per-turn stats line when a turn completes (#1)` | Show a stats line with duration and token usage when a user turn completes |
| `87dd0f13` | `docs: add personal fork notice and upstream diff summary to README (#2)` | Add a personal fork notice and upstream differences summary to README |
| `3e3cc101` | `fix(ui): preserve compact tool groups, headers, and durations on session restore` | Preserve compact tool groups, headers, and durations when restoring sessions |
| `4e5a3f45` | `fix(ui): show turn stats once when the user turn finishes (#3)` | Show turn stats only once, when the user turn finishes |
| `4265d676` | `fix(ui): fix compilation errors and dead code in chat turn stats` | Fix compilation errors and dead code in turn stats |
| `0227856d` | `feat(markdown): render LaTeX math expressions using txm` | Render LaTeX math expressions in Markdown using txm |
| `f70d08c6` | `refactor(code_execution): drop legacy list tool compatibility` | Remove the legacy `list` tool compatibility shim from the code execution sandbox |
| `2bfb89fc` | `feat(ui): add left padding to chat messages panel` | Add left padding to the chat messages panel |
| `b7d57572` | `feat(ui): capitalize tool group titles and show thinking duration` | Capitalize tool group titles and display thinking duration |
| `c72e8fa1` | `Merge branch 'upstream/main' into main` | Merge upstream through `1a8b897b`, including replacement of deprecated serde_yaml with serde-saphyr, agent-client-protocol-schema 1.13.0, `sess:send` messages to running subagents, and project identifiers in logs |
| `6069e830` | `docs: update upstream tracking record to 1a8b897b` | Update the upstream tracking record to `1a8b897b` |
| `bcaa1d21` | `feat(storage): record timestamps in session log records` | Persist Unix timestamps in session log messages |
| `9104593f` | `fix(ui): remove leading padding in compact tool groups` | Remove extra leading padding inside compact tool groups |
| `137a821e` | `feat(ui): include waiting time in compact thinking and tool call durations` | Include time spent waiting for model responses in compact thinking and first tool call durations |
| `da932b30` | `feat(ui): summarize tool calls and thoughts in tool group header` | Summarize tool calls and thinking in compact tool group headers |
| `24382572` | `fix(ui): use "thinking" instead of "thought" in group summary` | Use the term "thinking" consistently in compact tool group summaries |
| `25eb4d73` | `feat(ui): support true queue execution, steer shortcut, and recall editing` | Support queued execution, the Alt+S steer shortcut, and recalling previous input for editing |
| `4d8dcb46` | `fix(ui): use theme colors for context window progress bar in status bar` | Apply theme colors to the context window progress bar in the status bar |
| `7771692d` | `fix(ui): dim context percentage and window size in status bar` | Dim the context usage percentage and window size in the status bar |
| `19e40f75` | `feat(ui): support OSC 7501 Program Status Protocol` | Support OSC 7501 Program Status Protocol notifications (Working, Idle, Done, Error) |
| `7f136ab7` | `feat(ui): display user messages with chevron and background, remove maki prefix` | Display user messages with a background and chevron, removing the `maki> ` prefix |
| `2c3a9d79` | `feat: show a model-written reason on every tool call` | Support and display model-written `toolcall_reason` explanations on tool calls |
| `7b40aa5e` | `feat: store how long each thinking block took` | Record each thinking block's duration in sessions and the data model |
| `e6679ed4` | `feat: encourage models to fill toolcall_reason` | Encourage models to fill in `toolcall_reason` through the system prompt |
| `29a7e495` | `fix: tell an injected toolcall_reason by its description` | Identify injected `toolcall_reason` properties by their description |
| `7b20b515` | `build: leave LTO off unless a release asks for it` | Disable LTO by default for faster builds, enabling it only for the release-lto profile |
| `f26eff25` | `feat: adjust prompt tool usage when batch, code_execution, or task are disabled` | Adjust the system prompt dynamically when batch, code_execution, or task are disabled |
| `91af6fff` | `feat(ui): refine compact tool summaries with toolcall_reason` | Prefer `toolcall_reason` when rendering compact tool row summaries |
| `df344898` | `feat: tell models not to mention file names in toolcall_reason` | Explicitly instruct models to omit redundant file names from `toolcall_reason` |
| `cc535772` | `docs: update fork changes in README and add maintenance rule to AGENTS` | Update the fork differences summary in README and add a fork maintenance rule to AGENTS.md |
| `5b5dae02` | `feat(mcp): replace tool_search with code_execution external libraries` | Replace tool_search with Python external libraries in `code_execution` for MCP tool discovery and execution |
| `4a93166c` | `feat(config): enable library_help in DEFAULT_BUILTINS` | Add `library_help` to the default built-in tool list |
| `265a90e5` | `feat(ui): format batch child tool summaries with toolcall_reason in compact mode` | Format batch child tool summaries with `toolcall_reason` in compact mode |
| `cf8e3c13` | `feat(ui): enable inline image rendering for Rex terminal` | Support inline image rendering in the Rex terminal |
| `686e9a76` | `fix(mcp): await mcp ready before building frame tools and compaction` | Wait for MCP initialization before building frame tools and compacting context |
| `c2e04192` | `feat(ui): support ratio and negative offset in max_width` | Support ratios and negative offsets in `max_width` |
| `da15dc8d` | `fix(mcp): normalize hyphenated identifiers for python sandbox and library_help` | Normalize hyphens in MCP tool and parameter names to underscores for valid Python identifiers |
| `fb49721e` | `fix(ui): apply max_width once when laying out the frame` | Apply `max_width` only once at the top level during UI layout |
| `67e6fec3` | `feat(ui): sum compact thinking durations in turn completion stats line` | Sum compact thinking durations in the turn completion stats line |
| `51d9955f` | `docs: describe MCP-through-code_execution and old-session incompatibility` | Document MCP access through code execution and incompatibility with older sessions |
| `0e1f3227` | `fix(session): keep message timestamps across a headless resume` | Preserve message timestamps when resuming sessions in headless mode |
| `ce0c8984` | `Merge branch 'upstream/main' into main` | Merge upstream through `7ae4c5d1`, including slow tool call background task management (Ctrl+B), nested `/tasks` display and interaction improvements, bash output line and size limits with full logs saved to disk, and isolated Lua plugin task scheduling |
| `e289e9e1` | `docs: update upstream tracking record to 7ae4c5d1` | Update the upstream tracking record to `7ae4c5d1` |
| `502c0545` | `build: configure release profile with thin LTO, strip, and panic abort` | Configure the release profile with thin LTO, stripping, and panic abort |
| `c33902f9` | `feat: simplify toolcall_reason description and add usage rule` | Simplify the `toolcall_reason` description and add a usage rule |
| `33f1dcd3` | `feat(ui): open compact tool rows in a popup` | Open compact tool row details in a popup |
| `924c6de0` | `fix(code_execution): preserve files.inspect capability when list_dir is disabled` | Preserve the Python sandbox's `files.inspect` capability when `list_dir` is disabled |
| `55e0b38e` | `fix(ui): occlude wide characters at popup left edge to prevent border corruption` | Clip wide characters at the popup's left edge to prevent border corruption |
| `2d9d0864` | `feat(ui): improve compact group expansion and detail popup interactions` | Improve compact group expansion and detail popup interactions |
| `798e20aa` | `feat(agent): improve standby and sleep recovery resilience` | Improve network and task recovery after system standby and wake from sleep |
| `ddf287bb` | `feat(ui): add compact_lines config to control visible entries in compact groups` | Add `compact_lines` to control the default number of visible entries in compact groups |
| `161a4c53` | `feat(ui): preselect recent entry in model picker when present` | Prefer selecting the recently used model in the model picker |
| `53be365d` | `feat(ui): debounce viewport resize rendering` | Debounce rendering during terminal viewport resizing |
| `d1f3bd7d` | `fix(code_execution): document monty syntax and iterator limitations in description` | Document Monty syntax and iterator limitations in the sandbox tool description |
| `f4910029` | `feat(ui): use compact single-width unicode symbols in turn stats line` | Use compact single-width Unicode symbols in the turn stats line |
| `2a669f8e` | `feat(ui): use ↵ newline separator for compact thinking preview` | Use `↵` to separate lines in compact thinking previews |
| `48ca0d2f` | `feat(ui): merge total and thinking time in turn stats line` | Combine total and thinking time in the turn stats line |
| `f11423b7` | `Merge branch 'upstream/main' into main` | Merge upstream through `1a5d0fe8`, including migration from `smol` to `tokio`, migration from `isahc` to `reqwest`, the version bump to 0.7.0, and an async event loop, while preserving core fork customizations (the standalone Responses API provider, toolcall_reason, code execution sandbox, and others) |
| `82362790` | `docs: update upstream tracking record to 1a5d0fe8` | Update the upstream tracking record to `1a5d0fe8` |
| `f0f81c83` | `feat(tools): merge view_image into read tool` | Merge view_image into read tool |
| `98d56206` | `Revert "feat(tools): merge view_image into read tool"` | Revert merging view_image into read tool |
| `25a5e1fa` | `feat(index): add Vue SFC language support` | Add Vue SFC language support to the index tool |
| `85a0338d` | `docs: clarify fork changes and translate upstream tracking to English` | Clarify fork changes in README and translate upstream tracking record to English |
| `74865de1` | `feat(ui): add 50% min height watermark for compact detail and highlight code_execution input` | Add 50% min height watermark for compact detail popup and highlight Python code in code_execution tool calls |
| `36f4a01f` | `Merge branch 'upstream/main' into main` | Merge upstream through `e18f54a0`, including non-blocking models.dev catalog loading, OAuth loopback refactoring, background tasks UI & `/tasks` improvements, headless turn cancel handling, and other upstream fixes while preserving core fork customizations |
| `a0dd8398` | `fix(lua): prefer Git Bash over cmd.exe on Windows` | Prefer Git Bash over cmd.exe on Windows |
| `7104c7dd` | `Merge branch 'fix/windows-git-bash'` | Merge Windows Git Bash fix branch |
| `63359d71` | `docs: update upstream tracking record to e18f54a0` | Update upstream tracking record to e18f54a0 |
| `e08bd8c0` | `Merge branch 'main' of https://github.com/cybershape/maki` | Merge remote-tracking branch |
| `79358270` | `feat(tools): ship powershell and disable bash on Windows` | Ship PowerShell tool and disable bash on Windows |
| `d238a2c7` | `fix(tools): force PowerShell 5.1 pipe output to UTF-8` | Force PowerShell 5.1 pipe output to UTF-8 |
| `e9364301` | `fix(ui): drop split SGR mouse reports on Windows` | Drop split SGR mouse reports on Windows ConPTY |
| `bf30b237` | `feat(tools): prefer pwsh and describe its syntax` | Prefer pwsh and describe syntax based on PowerShell version |
| `fad38ace` | `docs: note powershell utf-8 fix and split SGR mouse swallow` | Note PowerShell UTF-8 fix and split SGR mouse swallow in README |
| `5290d387` | `fix(index): recurse into C# namespace bodies` | Recurse into C# block-scoped and file-scoped namespace bodies |
| `10a4d42e` | `Merge branch 'upstream/main' into main` | Merge upstream through `936726a5`, including u64 session token counters, restore snapshot scoping to chats with the tool, current cwd adoption on session resume, dependency updates, cargo-deny workflow gate, and other upstream improvements while preserving core fork customizations |
| `f5393a88` | `docs: update upstream tracking record to 936726a5` | Update upstream tracking record to `936726a5` |
| `399206b8` | `feat(storage): remember last thinking level per model per project` | Persist and restore the last thinking level per model per project |
| `5a1e2d5f` | `Merge branch 'upstream/main' into main` | Merge upstream through `ef3032d9`, including prompt cache reuse for `/btw`, model picker recent retired model filtering, tree-sitter-dart 0.3.0 update, lute nightly/renovate CI updates, Lua job pump wakeup refactor, headless first-turn provider discovery, and question prefix fixes while preserving core fork customizations |
| `d987ed04` | `docs: update upstream tracking record to ef3032d9` | Update upstream tracking record to `ef3032d9` |
| `a32eb9d7` | `feat(ui): close compact detail popup when clicking outside` | Close compact detail popup when clicking outside its bounds |
| `2fd55f84` | `feat(storage): remember last used model per project` | Persist and restore the last used model per project |
| `53a3ac7a` | `test(acp): use dedicated provider slug in display name resolution test` | Prevent model registry collisions in parallel tests by using an isolated provider slug |
| `b1dfd196` | `docs: update upstream tracking record with recent local commits` | Update the upstream tracking record with recent local commits |
| `168f982f` | `feat(index): add F# indexing support` | Add F# indexing support to the index tool using tree-sitter-fsharp |
| `c5f51b44` | `feat(ui): strip markdown formatting from turn response previews` | Strip markdown formatting from OSC 7501 turn response previews |
| `821c8599` | `feat(ui): activate Kitty keyboard protocol on Windows for compatible terminals` | Activate Kitty keyboard protocol on Windows for compatible terminals |
| `5bcf5517` | `feat(code_execution): hint batch execution when batch tool is disabled` | Hint parallel tool execution via `gather()` in `code_execution` description when batch tool is disabled |
| `9d786b19` | `fix(ui): silence unused warnings for kitty keyboard helpers on non-windows` | Silence unused warnings for Windows Kitty keyboard helpers on non-Windows platforms |
| `fac8c88b` | `feat(bash): strip ANSI escape sequences and inject NO_COLOR in output capture` | Strip ANSI escape sequences and inject `NO_COLOR=1` in bash output capture |
| `1d5e1764` | `fix(ui): dual-write clipboard copy to native and terminal OSC 52` | Dual-write clipboard copy to native desktop clipboard and terminal OSC 52 sequence |
| `870ea011` | `Merge branch 'upstream/main' into main` | Merge upstream through `b9897ee1`, including removing MCP from ToolRegistry, light/dark theme auto-detection via terminal background luminance probe, community plugin directory site page, and live child reporting for moved background batches while preserving core fork customizations |
| `b5209d24` | `fix(sessions): handle CJK display width and clamp titles in session picker` | Handle CJK character display width and clamp titles in session picker |
| `cf209dfd` | `feat(agent): content hash fast-path and opt-out for stale read check` | Optimize FileAccess with SHA-256 hash fast-path and opt-out for write tool |
| `d1da689c` | `feat(code_execution): builtin urllib.parse/glob, clarify sandbox limits, and preserve child tool timeouts` | Built-in urllib.parse/glob modules, document sandbox limits, and preserve child tool timeouts |
| `9adc3330` | `Merge branch 'upstream/main' into main` | Merge upstream through `66cb4f98`, including version 0.7.1 bump, XDG layout unification, `app/mod.rs` decomposition and `KeyboardOwner` architecture, subagent `SessionControl` refactoring, derive(`Merge`) for config, default `@` completion plugin, UI collapsing and background task output dimming, and site restyling, while preserving core fork customizations |
| `9f3ffaee` | `docs: update upstream tracking record to 66cb4f98` | Update the upstream tracking record to `66cb4f98` |
| `e66df122` | `fix(agent): realign message timestamps on history replace and truncate` | Realign message timestamps on history replace and truncate |
| `9b660863` | `fix(code_execution): glob against the native disk path, not the sandbox spelling` | Glob against the native disk path rather than sandbox path |
| `d342ad78` | `fix(code_execution): match CPython glob dotfile rule and skip symlinked dirs` | Match CPython glob dotfile rule and avoid descending into symlinked directories |
| `743ecc19` | `fix(code_execution): parse_qsl drops bare keys like CPython` | Drop bare keys in parse_qsl like CPython |
| `3643d0c6` | `build: drop panic=abort from the release profile` | Drop panic=abort from the release profile |
| `f0a1dd65` | `fix(headless): gate OSC program-status on a real terminal` | Gate OSC program-status on a real terminal |
| `58638ddd` | `fix(acp): sync the usage cost snapshot when a turn ends in Error` | Sync usage cost snapshot in ACP when a turn ends in Error |
| `48d0e973` | `fix(mcp): stop stealing a server-declared toolcall_reason parameter` | Avoid stealing server-declared toolcall_reason parameter |
| `3d08126f` | `fix(code_execution): stop preamble import glob from shadowing the glob tool` | Stop preamble import glob from shadowing the glob tool |
| `65c7959d` | `test(ui): make waiting timer assertion robust to elapsed-time drift` | Make waiting timer assertion robust to elapsed-time drift |
| `6e1715d0` | `fix(ui): defer plan form popup until agent turn completes` | Defer plan form popup until agent turn completes |
| `4fb237f2` | `Merge branch 'upstream/main' into main` | Merge upstream through `1748888f`, including plugin session data root (`data/<id>/`) and lifecycle sweep on session delete, Unix domain socket support in `maki.net`, OSC 7501 Program Status Protocol reporting, theme scrollbar thumb styling, session archive persistence until session deletion, prompt background guidance, and gpt-6.1-terra support, while preserving core fork customizations |
| `72481cf3` | `docs: update upstream tracking record to 1748888f` | Update the upstream tracking record to `1748888f` |
| `0390dd4c` | `fix(lua): protect async highlight tasks and lua thread against unhandled errors` | Protect async syntax highlighting tasks and Lua thread execution against unhandled errors |
| `5f92fe19` | `Merge branch 'upstream/main' into main` | Merge upstream through `e9ca885c`, including OAuth metadata discovery on path issuers, host system prompt overrides across all run paths, trusting empty folders without `.maki`, declared compaction model selection and empty summary retry, headless registry re-reads, inert tool permissions declaration, monty 1.5.0 update, read empty window explanation, OpenAI wire empty tool result marker, and tokio oneshot host requests, while preserving core fork customizations |

---

## 3. Core Fork Customizations and Affected Areas

Pay particular attention to the following potential conflicts and integration requirements when synchronizing with `upstream/main`:

### 3.1. Provider Consolidation Around the OpenAI Responses API

- **Affected files**: `maki-providers/`, `plugins/`, `site/docs/content/providers/`
- **Key changes**:
  - Remove all vendor-specific client implementations and plugins, including Anthropic, Google, Copilot, Z.AI, and xAI, consolidating them into `ResponsesProvider`.
  - Implement reasoning effort dialect mappings for OpenAI-family models (such as GPT-5.5, GPT-5.6, GPT-6, and Codex) in `maki-providers/src/providers/responses.rs`, using `dialect::GPT_5_6`, `dialect::GPT_6`, `dialect::CODEX`, and related dialects.
  - Support complete field parsing in dynamic `/models` discovery responses, including normalization between per-token and per-million-token pricing, vision support, reasoning flags and effort levels, and accelerated service tiers.

### 3.2. Built-in Tool Renaming

- **Affected files**: `maki-agent/src/tools/`, `plugins/`
- **Key changes**:
  - Rename the built-in directory listing tool from `list` to `list_dir` to avoid shadowing Python's built-in `list` in the sandbox.

### 3.3. ACP Model Caching and Discovery

- **Affected files**: `maki-acp/`
- **Key changes**:
  - Add a persistent local model discovery cache and wait for discovery on cold starts.
  - Support the `thought_level` session configuration option to select reasoning effort levels.

### 3.4. UI Customization and Display Enhancements

- **Affected files**: `maki-ui/`, `maki-config/`
- **Key changes**:
  - `show_cost`: Set `show_cost = false` in the configuration to hide pricing and billing information from the status bar, usage statistics popup, and model picker.
  - `prefer_model_name`: Prefer friendly model names (such as "GPT-6.1 Sol") over raw slugs in the status bar and model picker.
  - Context usage progress bar: Visualize the current session's context window usage in the status bar, for example, `[████░░░░░░] 40.0%/1.0M`.
  - Adaptive space for long notifications: Prioritize displaying the full flash notification in the status bar, automatically shrinking or hiding the persistent path and model displays when necessary.

### 3.5. CI and Release Workflow Simplification

- **Affected files**: `.github/workflows/`
- **Key changes**:
  - Limit builds and releases to `windows`, `linux`, and `mac-arm64`, removing targets such as NetBSD; trigger releases only on Git tags.

### 3.6. Interpreter and Sandbox Filesystem Access Adaptation

- **Affected files**: `plugins/code_execution/`, `maki-lua/`
- **Key changes**:
  - Update sandbox filesystem operations to use the renamed built-in `list_dir` tool.

### 3.7. Storage & Per-Project Model Memory

- **Affected files**: `maki-storage/`, `maki-ui/`, `src/`
- **Key changes**:
  - Persist and restore the last used model and thinking level per model per project under `<state>/projects/<project-id>/`.

---

## 4. Upstream Tracking and Synchronization Guide

Use the following workflow when fetching and integrating the latest commits from `upstream/main`:

### 4.1. Fetch Upstream Changes

```bash
git fetch upstream main
```

### 4.2. Review New Upstream Commits and Changes

```bash
# List upstream commits since the fork point
git log --oneline 70348ec0a50b84544003cb119550778652e0d7d8..upstream/main

# Show a summary of upstream changes since the fork point
git diff 70348ec0a50b84544003cb119550778652e0d7d8..upstream/main --stat
```

### 4.3. Recommended Synchronization Options

#### Option A: Rebase

Use this option to maintain a clean commit history when preserving existing merge commits is not required:

```bash
# Switch branches and verify that the working tree is clean
git checkout main
git status

# Rebase onto the latest upstream main branch
git rebase upstream/main
```

> **Conflict resolution priorities**:
>
> - If upstream modifies a removed provider (such as `maki-providers/src/providers/anthropic`), discard those changes or retain the local deletion when resolving rebase conflicts.
> - If upstream updates shared modules such as the agent core, UI, interpreter, or configuration system, preserve local changes related to `list_dir` and the Responses API.

#### Option B: Selective Integration / Cherry-pick

Use cherry-pick when only specific upstream bug fixes or features are needed:

```bash
git cherry-pick <upstream-commit-sha>
```

### 4.4. Update This Document After Synchronization

After synchronization, run:

```bash
git merge-base main upstream/main
```

Update Section 1 with the resulting commit SHA, date, and commit message to provide a reference for the next synchronization.
