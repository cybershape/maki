-- Policy for the Python interpreter: which tools it may call, what the model
-- sees (via the `describe(dctx)` callback), and the preamble. The sandbox and
-- dispatch live in Rust, which exposes primitives only (`maki.api.get_tools`,
-- `maki.agent.call_tool`); orchestration policy is here.

local ToolView = require("maki.tool_view")
local output_limits = require("maki.output_limits")
local partial = require("maki.partial")

local ENGINE_PYTHON = "python"
local ENGINE_JS = "js"
local MAX_SCRIPT_LINES = 2000
local OPEN_READ_CHECK_LINES = 1
local READ_INPUT_SLOTS = { "tool.read.input", "tool.*.input" }
local LIST_INPUT_SLOTS = { "tool.list_dir.input", "tool.*.input" }
local OPEN_READ_HOOKED_ERR = "open() cannot read while a plugin hooks the read tool, call read() instead"
local INSPECT_HOOKED_ERR =
  "os and pathlib cannot look at files while a plugin hooks the list_dir tool, call list_dir() instead"
local MKDIR_BASH_HINT = ", create the directory with the bash tool instead"
local MKDIR_POWERSHELL_HINT = ", create the directory with the powershell tool instead"
local REMOVAL_BASH = "use the bash tool"
local REMOVAL_POWERSHELL = "use the powershell tool"
local OPEN_APPEND_UNSUPPORTED_ERR = "open() cannot append because the write tool has no append option"
local NO_OUTPUT = "(no output)"
local CANCELLED_ERR = "cancelled"
local TIME_LIMIT_SUBSTR = "time limit exceeded"
-- Same marker the batch tool uses for a failed child, so a failure reads the
-- same wherever the model meets it.
local ERROR_PREFIX = "[ERROR] "
local CALL_RUNNING = "running"
local CALL_SUCCESS = "success"
local CALL_ERROR = "error"
local NANOS_PER_MILLISECOND = 1000000
local ANNOTATION_SEP = " · "
local ASYNCIO_GATHER = "asyncio.gather"
local GATHER_HINT = "\n\nHint: `gather(...)` keeps the other results, returning `"
  .. ERROR_PREFIX
  .. "...` for the failed call."
-- Mistakes the description leaves out to save tokens on every request: the
-- hint costs tokens only when the model actually makes one. Each key is a
-- pattern for the start of the exception line, the first unindented line under
-- the traceback header. A source preview is indented and a tool's error text
-- sits behind `RuntimeError: <tool>: `, so neither can match, and neither can
-- the later lines of a multi-line message.
local EXCEPTION_LINE = "\n(%S[^\n]*)"
local TOOLS_MODULE_HINT = "\n\nHint: tools are globals, call them directly. There is no `tools` module."
local ASYNCIO_HINT = "\n\nHint: asyncio here has only `gather`, `run` and `sleep`."
local COMPOUND_STMT_HINT =
  "\n\nHint: In Python, compound statements (`for`, `if`, `while`, `def`, `try`, `with`) cannot follow another statement on the same line after a semicolon (`;`). Write standard multi-line Python with newlines and proper indentation:\n\n# Put each statement on its own line:\nx = await gather(...)\nfor item in x:\n    print(item)"
local POSITIONAL_ARG_HINT =
  "\n\nHint: All tool arguments in the sandbox must use keywords. Never place a positional argument after a keyword argument, e.g. use `read(path='...', offset=1, limit=10)`."
local MISSING_MODULE_HINT =
  "\n\nHint: the sandbox has only part of the stdlib, and what is there can lack arguments. For more, run Python through bash."
local ERROR_HINTS = {
  { "^ModuleNotFoundError: No module named 'tools'", TOOLS_MODULE_HINT },
  { "^ModuleNotFoundError: No module named", MISSING_MODULE_HINT },
  { "^NameError: name 'tools' is not defined", TOOLS_MODULE_HINT },
  { "^AttributeError: module 'asyncio' has no attribute", ASYNCIO_HINT },
  { "^ImportError: cannot import name '[^']*' from 'asyncio'", ASYNCIO_HINT },
  { "^SyntaxError: Compound statements are not allowed on the same line as simple statements", COMPOUND_STMT_HINT },
  { "^SyntaxError: Positional argument cannot follow keyword argument", POSITIONAL_ARG_HINT },
}
-- `asyncio.gather` cancels its siblings the moment one call raises, throwing away
-- results the model already paid for, so `gather` awaits each call in its own
-- `try` instead. Awaiting one at a time is still concurrent: every call was made
-- before the first await, so they all sit pending and the host dispatches them in
-- one batch. Tasks look like the obvious fix, but monty 0.0.21 fails the whole
-- gather on an external call error rather than raising inside the awaiting task,
-- and a coroutine wrapper is lazy, so a call handed over that way would run alone.
-- Only `RuntimeError` is caught, the shape a failed tool call arrives in; a
-- TypeError from the script itself must still stop the run.
local PREAMBLE = [[
import re
import asyncio
import sys
import os
import json
import urllib.parse
import pathlib
from pathlib import Path
async def gather(*calls):
    if len(calls) == 1 and isinstance(calls[0], list):
        calls = calls[0]
    results = []
    for c in calls:
        try:
            results.append(await c)
        except RuntimeError as e:
            results.append(']] .. ERROR_PREFIX .. [[' + str(e))
    return results
_orig_open = open
def open(file, mode='r', buffering=-1, encoding=None, errors=None, newline=None, closefd=True, opener=None):
    if encoding is not None:
        enc_norm = str(encoding).lower().replace('-', '').replace('_', '')
        if enc_norm in ('utf8', 'cp65001'):
            return _orig_open(file, mode, encoding='utf-8')
        raise ValueError("Monty sandbox only supports UTF-8 encoding. For other encodings (" + repr(encoding) + "), execute a script via the shell tool on the host system.")
    return _orig_open(file, mode)
]]
local TOOLS_HEADER = "\n\nAvailable tools (async Python functions, keyword args only):\n"
local JS_TOOLS_HEADER = "\n\nAvailable tools (async JavaScript functions taking an options object):\n"
local WORKFLOW_TOOLS_NOTE =
  "\nWorkflow mode: orchestrate subagents from this script. Await every `task(...)` call and use `gather(task(...), task(...))` for parallel fan-out. Pass `output_schema` to task for machine-readable results (a JSON string, parse with `json.loads`).\n"
local JS_WORKFLOW_TOOLS_NOTE =
  "\nWorkflow mode: orchestrate subagents from this script. Await every `task(...)` call and use `gather(task(...), task(...))` for parallel fan-out. Pass `output_schema` to task for machine-readable results (a JSON string, parse with `JSON.parse`).\n"
local WORKFLOW_OFF_NOTE = "\nNot callable: %s\n"
local CALLABLE_TOOLS_ERR = "cannot list callable tools: "
local JS_IMPORT_HINT =
  "\n\nHint: ES module `import` statements are not supported in script mode. Built-ins (`fs`, `path`, `process`, `env`, `gather`, `sleep`), tools, and external libraries are already pre-bound globals; use them directly without importing."
local JS_PROMISE_ALL = "Promise.all("
-- `alias` covers hyphens; what is left is a name no substitution can fix, like
-- a leading digit. Binding it would raise a SyntaxError the model cannot act on.
local PY_IDENTIFIER = "^[%a_][%w_]*$"
local PY_TYPES = { string = "str", integer = "int", boolean = "bool", array = "list" }
local JS_TYPES = {
  string = "string",
  integer = "number",
  number = "number",
  boolean = "boolean",
  array = "any[]",
  object = "object",
}
local PYTHON_KEYWORDS = {
  ["False"] = true,
  ["None"] = true,
  ["True"] = true,
  ["and"] = true,
  ["as"] = true,
  ["assert"] = true,
  ["async"] = true,
  ["await"] = true,
  ["break"] = true,
  ["class"] = true,
  ["continue"] = true,
  ["def"] = true,
  ["del"] = true,
  ["elif"] = true,
  ["else"] = true,
  ["except"] = true,
  ["finally"] = true,
  ["for"] = true,
  ["from"] = true,
  ["global"] = true,
  ["if"] = true,
  ["import"] = true,
  ["in"] = true,
  ["is"] = true,
  ["lambda"] = true,
  ["nonlocal"] = true,
  ["not"] = true,
  ["or"] = true,
  ["pass"] = true,
  ["raise"] = true,
  ["return"] = true,
  ["try"] = true,
  ["while"] = true,
  ["with"] = true,
  ["yield"] = true,
}

local function to_py_identifier(name)
  if not name or #name == 0 then
    return nil
  end
  local clean = name:gsub("[^%w_]", "_")
  if clean:match("^%d") then
    clean = "_" .. clean
  end
  if PYTHON_KEYWORDS[clean] then
    clean = clean .. "_"
  end
  return clean:match(PY_IDENTIFIER) and clean or nil
end

local function identifier_alias(name)
  if not name or #name == 0 or name:match("^%d") then
    return nil
  end
  return to_py_identifier(name)
end

local function build_param_map(schema)
  if not schema or not schema.properties then
    return nil
  end
  local map = nil
  local seen = {}
  for raw_name in pairs(schema.properties) do
    seen[raw_name] = true
  end
  for raw_name in pairs(schema.properties) do
    local py_name = to_py_identifier(raw_name)
    if py_name then
      while seen[py_name] and py_name ~= raw_name do
        py_name = py_name .. "_"
      end
      if py_name ~= raw_name then
        map = map or {}
        map[py_name] = raw_name
        seen[py_name] = true
      end
    end
  end
  return map
end

local function apply_param_map(tool_input, param_map)
  if not param_map or type(tool_input) ~= "table" then
    return tool_input
  end
  for py_name, raw_name in pairs(param_map) do
    if tool_input[py_name] ~= nil then
      if tool_input[raw_name] == nil then
        tool_input[raw_name] = tool_input[py_name]
      end
      tool_input[py_name] = nil
    end
  end
  return tool_input
end

local function mcp_library_hint(libs, code, err, is_js)
  if not libs or #libs == 0 then
    return ""
  end
  local function help_call(lib_name, fn_name)
    if is_js then
      return string.format('library_help({ library_name: "%s", function_name: "%s" })', lib_name, fn_name)
    end
    return string.format('library_help(library_name="%s", function_name="%s")', lib_name, fn_name)
  end
  for _, lib in ipairs(libs) do
    local lib_name = identifier_alias(lib.name)
    if lib_name then
      local tried_import = code:match("import%s+" .. lib_name)
        or err:find("No module named '" .. lib_name .. "'", 1, true)
        or (
          is_js
          and (code:find("require('" .. lib_name .. "'", 1, true) or code:find('require("' .. lib_name .. '"', 1, true))
        )
      if tried_import then
        if is_js then
          return string.format(
            "\n\nHint: '%s' is an external library already injected as a global object in this sandbox. DO NOT import or require it; call its functions directly via `await %s.<function>({ ... })`. Use `%s` to inspect its available functions.",
            lib_name,
            lib_name,
            help_call(lib_name, "...")
          )
        end
        return string.format(
          "\n\nHint: '%s' is an external library already injected as a global object in this sandbox. DO NOT write `import %s`; call its functions directly via `await %s.<function>(...)`. Use `%s` to inspect its available functions.",
          lib_name,
          lib_name,
          lib_name,
          help_call(lib_name, "...")
        )
      end
      if not err:find("^parse error:") and not err:find("SyntaxError", 1, true) then
        local is_call_err = err:find(lib_name, 1, true)
          or err:find("RuntimeError", 1, true)
          or err:find("TypeError", 1, true)
          or (is_js and err:find("Error:", 1, true))
        for _, fn in ipairs(lib.functions or {}) do
          local name_str = type(fn) == "table" and fn.name or fn
          local fn_name = identifier_alias(name_str) or name_str
          if fn_name then
            local wire = lib_name .. "__" .. fn_name
            local dot_call = lib_name .. "." .. fn_name
            if err:find(wire, 1, true) or (code:find(dot_call, 1, true) and is_call_err) then
              return string.format(
                "\n\nHint: Parameter or runtime error in external library '%s'. Please inspect its schema using `%s` before retrying.",
                lib_name,
                help_call(lib_name, fn_name)
              )
            end
          end
        end
        local fn_call = code:match(lib_name .. "%.([%a_][%w_]*)")
        if fn_call and is_call_err then
          return string.format(
            "\n\nHint: Parameter or runtime error in external library '%s'. Please inspect its schema using `%s` before retrying.",
            lib_name,
            help_call(lib_name, fn_call)
          )
        end
      end
    end
  end
  return ""
end

local opts = maki.api.register_options(output_limits.extend({
  engine = {
    default = ENGINE_PYTHON,
    desc = 'Sandbox engine: "python" (Monty) or "js" (QuickJS-NG).',
  },
  timeout_secs = {
    default = 30,
    min = 5,
    desc = "Script execution time budget in seconds; waiting on tool calls does not count. A call's `timeout` param overrides it.",
  },
  max_memory_mb = { default = 50, min = 10, desc = "Memory limit for the Python sandbox (MB)." },
}))

if opts.engine ~= ENGINE_PYTHON and opts.engine ~= ENGINE_JS then
  error(
    string.format(
      'plugins.code_execution.engine: unknown value "%s" (expected "%s" or "%s")',
      tostring(opts.engine),
      ENGINE_PYTHON,
      ENGINE_JS
    )
  )
end
local IS_JS = opts.engine == ENGINE_JS
maki.api.set_slot("library_help.sandbox_engine", function(_next)
  return opts.engine
end)

local function new_view(ctx, buf)
  return ToolView.new(buf, { max_lines = ctx:tool_output_lines().code_execution or 3 })
end

-- One body builder for every path (start preview, handler, restore), so the
-- script renders the same no matter which lifecycle callbacks ran. The
-- header is always rebuilt from scratch; nothing mutates existing lines.
local function build_body(ctx, code)
  local lines = maki.split(code:gsub("\n+$", ""), "\n")
  local hl
  local buf = maki.ui.buf()
  local view = new_view(ctx, buf)

  local function header()
    local total = #lines
    local shown = view.expanded and total or math.min(total, MAX_SCRIPT_LINES)
    local fmt = ToolView.line_nr_fmt(shown) .. " "
    local out = {}
    for i = 1, shown do
      local spans = { { string.format(fmt, i), "line_nr" } }
      for _, seg in ipairs(hl and hl[i] or { { lines[i] } }) do
        spans[#spans + 1] = seg
      end
      out[#out + 1] = spans
    end
    if shown < total then
      out[#out + 1] = { { "... (" .. (total - shown) .. " lines) (click to expand)", "dim" } }
    end
    return out
  end

  view:set_header(header())
  -- Flip the flag directly: `toggle` would flush the old header first, and
  -- each flush walks every line of a script this long.
  buf:on("click", function()
    view.expanded = not view.expanded
    view:set_header(header())
  end)

  local function highlight()
    local highlighted = maki.ui.highlight(table.concat(lines, "\n"), IS_JS and "js" or "py")
    if highlighted then
      hl = highlighted
      view:set_header(header())
    end
  end
  return buf, view, highlight
end

local function build_description(batch_disabled, mcp_lib_names)
  local use_prefix = batch_disabled and "Use for parallel/batch calls (the batch tool is disabled; use gather), "
    or "Use for "
  local concurrency = batch_disabled
      and "- Concurrency / Batch execution: `a, b = await gather(read(path='a.py', offset=1, limit=100), grep(pattern='x'))`. Pass calls directly, never wrapped in `async def`. A failed call returns `[ERROR] ...` in its slot. The batch tool is disabled; use code_execution with `await gather(...)` to run multiple independent tool calls concurrently (replacing batch)."
    or "- Concurrency: `a, b = await gather(read(path='a.py', offset=1, limit=100), grep(pattern='x'))`. Pass calls directly, never wrapped in `async def`. A failed call returns `[ERROR] ...` in its slot."
  local single_call = batch_disabled
      and "- NEVER use as a proxy or wrapper for a single tool call. If no code logic, chaining, or output filtering is needed, invoke the tool directly (batching 2+ independent calls via gather is allowed when batch is disabled)."
    or "- NEVER use as a proxy or wrapper for a single tool call. If no code logic, chaining, or output filtering is needed, invoke the tool directly."

  local mcp_line = ""
  if mcp_lib_names and #mcp_lib_names > 0 then
    mcp_line = string.format(
      "\n- Pre-bound external libraries: %s (injected as global objects — DO NOT write `import <lib>`, call directly via `await <lib>.<fn>(...)`). Functions that declare an output schema return a dict/list directly (no `json.loads` needed); others return strings.",
      table.concat(mcp_lib_names, ", ")
    )
  end

  return ([[Execute a minimal embedded Python subset (Monty sandbox) — NOT a full standard Python runtime.

Purpose:
- %schained/dependent tool calls and filtering/processing results, e.g. filtering tool output. **DRAMATICALLY** cheaper than sequential tool calls!
- Exclusively for orchestrating and chaining the available async tool functions listed below (such as `read`, `grep`, `bash`, `edit`).
- For data analysis, third-party packages (e.g. pandas, openpyxl, scipy), or standard Python runtime features: DO NOT execute them here. Run scripts on the host system via the `bash` tool (using the host's `python3` or virtual environment) instead.

Available Modules & Global Objects:
- All tools are async and return strings: `result = await read(path='file.txt', offset=1, limit=100)`. Parse output yourself.
- Tool results are complete. Only what you print/return is capped.
- Pass arguments by KEYWORD only: always specify parameter names (e.g. `await edit(path='...', old_string='...', new_string='...')`). NEVER pass positional arguments after keywords (e.g. `edit(path=p, old_string=s, 'b')` causes `SyntaxError: Positional argument cannot follow keyword argument`).
%s
- Importable modules ONLY: re, asyncio, sys, os, json, pathlib, urllib.parse, glob. No other standard or third-party packages can be imported.%s
- Pre-imported globals (no import needed): re, asyncio, sys, os, json, pathlib, Path (= pathlib.Path), urllib.parse, gather.
- `open()` supports basic text read/write (UTF-8 only; defensive parameters like `errors=`, `buffering=`, `newline=` are absorbed). For non-UTF-8 encodings or binary data, run via the host shell instead. `os`/`pathlib` can stat, listdir, walk, and mkdir. For file matching, prefer `Path.glob()`.
- Fresh sandbox each run: no state persists between executions.
- 30s script timeout (`timeout` param); time awaiting tool calls doesn't count.
%s
- NOT a thinking scratchpad. Reason in your response text.

Syntax & Execution Limitations (Frequent Failure Points):
- Formatting: Write standard multi-line Python with proper indentation. DO NOT chain compound statements (`for`, `if`, `while`, `def`) on the same line with semicolons (`;`) — this causes `SyntaxError: Compound statements are not allowed on the same line as simple statements`. Avoid long single-line semicolon chains.
- NO JavaScript / TypeScript syntax: DO NOT use JS template backticks (`), Promise methods (`.catch()`, `Promise.all`), or arrow functions (`() => ...`) inside Python code. Use standard Python triple quotes `'''...'''` for multi-line strings.
- Shell commands & Regex quotes: For Shell commands or regex patterns containing quotes or backslashes, use raw triple-quoted strings `r'''...'''` to avoid multi-level quote escaping failures.
- NO generator iterators: Generator expressions `(x for ...)` materialize as `list`, and `iter()` is unavailable: DO NOT use `next(x for ...)`; use a `for` loop or `[...][0]` instead.
- NO suspension inside sync callbacks: DO NOT call async tool functions or OS operations (like `stat()`, `read_text()`) inside `sorted(key=...)`, `max(key=...)`, or `map()` (cannot suspend); extract attributes beforehand via list comprehension.
- Builtins NOT implemented: `iter`, `callable`, `dir`, `globals`, `locals`, `eval`, `exec`.
- Unsupported syntax: No `:=` (walrus), no `del` (use `d.pop(k, None)`), no `*args` unpacking in calls (`f(*args)`), no `match/case`, no decorators (`@...`).
- `pathlib.Path` does NOT support `.relative_to()`.
- `re.sub()` replacement must be a string: callable/lambda replacements are NOT supported.
]]):format(use_prefix, concurrency, mcp_line, single_call)
end

local function build_js_description(batch_disabled, mcp_lib_names)
  local use_prefix = batch_disabled and "Use for parallel/batch calls (the batch tool is disabled; use gather), "
    or "Use for "
  local concurrency = batch_disabled
      and "- Concurrency / Batch execution: `const [a, b] = await gather(read({ path: 'a.rs', offset: 1, limit: 100 }), grep({ pattern: 'x' }))` (or `Promise.all` / `Promise.allSettled`). `gather(...)` keeps sibling results, returning `[ERROR] ...` for any failed slot. The batch tool is disabled; use code_execution with `await gather(...)` to run multiple independent tool calls concurrently (replacing batch)."
    or "- Concurrency: `const [a, b] = await gather(read({ path: 'a.rs', offset: 1, limit: 100 }), grep({ pattern: 'x' }))` (or `Promise.all` / `Promise.allSettled`). `gather(...)` keeps sibling results, returning `[ERROR] ...` for any failed slot."
  local single_call = batch_disabled
      and "- NEVER use as a proxy or wrapper for a single tool call. If no code logic, chaining, or output filtering is needed, invoke the tool directly (batching 2+ independent calls via gather is allowed when batch is disabled)."
    or "- NEVER use as a proxy or wrapper for a single tool call. If no code logic, chaining, or output filtering is needed, invoke the tool directly."

  local mcp_line = ""
  if mcp_lib_names and #mcp_lib_names > 0 then
    mcp_line = string.format(
      "\n- Pre-bound external libraries: %s (injected as global objects — DO NOT import or require, call directly via `await <lib>.<fn>({ ... })`). Functions that declare an output schema return an object/array directly (no `JSON.parse` needed); others return strings.",
      table.concat(mcp_lib_names, ", ")
    )
  end

  return ([[Execute JavaScript in an embedded QuickJS sandbox — NOT a full Node.js runtime.

Purpose:
- %schained/dependent tool calls and filtering/processing results, e.g. filtering tool output. **DRAMATICALLY** cheaper than sequential tool calls!
- Exclusively for orchestrating and chaining the available async tool functions listed below (such as `read`, `grep`, `bash`, `edit`).
- For heavy data analysis, npm/Python packages, or full runtime features: DO NOT execute them here. Run scripts on the host system via the `bash` tool instead.

Execution Model & Globals:
- Write standard JavaScript with top-level `await`. Output results via `console.log(...)`, `return`, or a final expression.
- All tools are async globals taking a single options object and returning strings: `const result = await read({ path: 'file.txt', offset: 1, limit: 100 })`.
- Tool results are complete. Only what you print/return is capped.
%s
- Built-in globals (no import needed): `fs` (`readFileSync`, `writeFileSync`, `appendFileSync`, `readdirSync`, `statSync`, `existsSync`, `mkdirSync` — UTF-8 only), `path` (`join`, `resolve`, `dirname`, `basename`, `extname`, `normalize`, `relative`, `isAbsolute`), `env` / `process.env` (safe subset), `process.cwd()`, `gather`, `sleep(ms)`, `console.log` / `print`. `require('fs')` and `require('path')` also work.%s
- Fresh sandbox each run: no state persists between executions.
- 30s script timeout (`timeout` param); time awaiting tool calls does not count.
%s
- NOT a thinking scratchpad. Reason in your response text.]]):format(use_prefix, concurrency, mcp_line, single_call)
end

local function build_active_description(batch_disabled, mcp_lib_names)
  if IS_JS then
    return build_js_description(batch_disabled, mcp_lib_names)
  end
  return build_description(batch_disabled, mcp_lib_names)
end

local description = build_active_description(false, nil)

local schema = {
  type = "object",
  required = { "code" },
  additionalProperties = false,
  properties = {
    code = {
      type = "string",
      description = IS_JS
          and "JavaScript code with top-level await. Tools return strings, take an options object, and MUST be awaited: `const result = await read({ path: '/file', offset: 1, limit: 100 })`."
        or "Python code. Tools return strings, not objects, and you MUST await every call: `result = await read(path='/file', offset=1, limit=100)`.",
    },
    timeout = {
      type = "integer",
      description = "Script execution timeout in seconds (default 30)",
    },
  },
}

local examples = IS_JS
    and {
      {
        code = [[const files = (await glob({ pattern: '**/*.rs' })).trim().split('\n').filter(Boolean);
const results = await gather(files.map((f) => read({ path: f, offset: 1, limit: 0 })));
for (let i = 0; i < files.length; i++) {
  if (results[i].includes('fn main')) console.log(files[i]);
}]],
      },
      {
        code = [[const result = await grep({ pattern: 'TODO', include: '*.rs' });
console.log(`${result.trim().split('\n').filter(Boolean).length} TODOs found`);]],
      },
      {
        code = [[const content = await webfetch({ url: 'https://example.com/docs' });
for (const line of content.split('\n')) {
  if (line.toLowerCase().includes('auth')) console.log(line);
}]],
      },
    }
  or {
    {
      code = [[files = (await glob(pattern='**/*.rs')).strip().split('\n')
results = await gather([read(path=f, offset=1, limit=0) for f in files if f.strip()])
for f, c in zip(files, results):
    if 'fn main' in c: print(f)]],
    },
    {
      code = [[result = await grep(pattern='TODO', include='*.rs')
print(f"{len(result.strip().splitlines())} TODOs found")]],
    },
    {
      code = [[content = await webfetch(url='https://example.com/docs')
for line in content.splitlines():
    if 'auth' in line.lower(): print(line)]],
    },
  }

-- Shared predicate for describe and handler so advertised == callable.
-- The interpreter is a calling convention, not a capability grant: a read-only
-- subagent must not reach edit/write through Python.
local function interpreter_tools(tools, audience, workflow)
  local out = {}
  for _, t in ipairs(tools) do
    local aud = {}
    for _, a in ipairs(t.audiences) do
      aud[a] = true
    end
    if aud[audience] and (aud.interpreter or (workflow and aud.workflow)) then
      t.workflow_only = not aud.interpreter
      out[#out + 1] = t
    end
  end
  return out
end

local function matches_filter(name, dctx, alias)
  if dctx.only then
    for _, n in ipairs(dctx.only) do
      if n == name or (alias and n == alias) then
        return true
      end
    end
    return false
  end
  if dctx.except then
    for _, n in ipairs(dctx.except) do
      if n == name or (alias and n == alias) then
        return false
      end
    end
  end
  return true
end

local function signature(t)
  local schema_props = (t.schema and t.schema.properties) or {}
  local required = {}
  for _, r in ipairs((t.schema and t.schema.required) or {}) do
    required[r] = true
  end
  local names = {}
  for pname in pairs(schema_props) do
    names[#names + 1] = pname
  end
  table.sort(names, function(a, b)
    local ra, rb = required[a] or false, required[b] or false
    if ra ~= rb then
      return ra
    end
    return a < b
  end)
  local params = {}
  if IS_JS then
    for _, pname in ipairs(names) do
      local ptype = JS_TYPES[schema_props[pname].type] or "any"
      params[#params + 1] = required[pname] and (pname .. ": " .. ptype) or (pname .. "?: " .. ptype)
    end
    local arg_sig = #params > 0 and ("{ " .. table.concat(params, ", ") .. " }") or ""
    return "- " .. (t.alias or t.name) .. "(" .. arg_sig .. ") -> Promise<string>"
  end
  for _, pname in ipairs(names) do
    local py_pname = to_py_identifier(pname) or pname
    local ptype = PY_TYPES[schema_props[pname].type] or "any"
    params[#params + 1] = required[pname] and (py_pname .. ": " .. ptype) or (py_pname .. ": " .. ptype .. " = None")
  end
  return "- " .. (t.alias or t.name) .. "(" .. table.concat(params, ", ") .. ") -> str"
end

-- Keep cheap: runs on every request build. get_tools skips descriptions
-- to avoid recursion from describe callbacks.
local function describe(dctx)
  local mcp_lib_names = {}
  if dctx.mcp and dctx.mcp_libraries and #dctx.mcp_libraries > 0 then
    for _, lib in ipairs(dctx.mcp_libraries) do
      local lib_name = identifier_alias(lib.name) or lib.name
      table.insert(mcp_lib_names, lib_name)
    end
  end
  local batch_disabled = not matches_filter("batch", dctx)
  local base_desc = build_active_description(batch_disabled, mcp_lib_names)
  local parts = { base_desc, IS_JS and JS_TOOLS_HEADER or TOOLS_HEADER }
  local has_workflow_only, gated = false, {}
  -- Ask as if workflow were on, then hold back what it would unlock: those
  -- names are listed as not callable, so the model stops trying them.
  for _, t in ipairs(interpreter_tools(maki.api.get_tools(), dctx.audience, true)) do
    if matches_filter(t.name, dctx, t.alias) then
      if t.workflow_only and not dctx.workflow then
        gated[#gated + 1] = t.name
      else
        has_workflow_only = has_workflow_only or t.workflow_only
        parts[#parts + 1] = signature(t) .. "\n"
      end
    end
  end
  if has_workflow_only then
    parts[#parts + 1] = IS_JS and JS_WORKFLOW_TOOLS_NOTE or WORKFLOW_TOOLS_NOTE
  end
  if #gated > 0 then
    parts[#parts + 1] = WORKFLOW_OFF_NOTE:format(table.concat(gated, ", "))
  end
  if dctx.mcp and dctx.mcp_libraries and #dctx.mcp_libraries > 0 then
    local lines = {
      IS_JS
          and "\nExternal libraries (pre-bound global objects, callable in JavaScript via `await <library_name>.<function_name>({ ... })`; DO NOT import or require):"
        or "\nExternal libraries (pre-bound global objects, callable in Python via `await <library_name>.<function_name>(...)`; DO NOT import):",
    }
    for _, lib in ipairs(dctx.mcp_libraries) do
      local lib_name = identifier_alias(lib.name) or lib.name
      local desc = (lib.description and #lib.description > 0) and lib.description or "(no description)"
      local fn_names = {}
      local seen = {}
      for _, fn in ipairs(lib.functions or {}) do
        local fn_name = identifier_alias(fn) or fn
        if not seen[fn_name] then
          seen[fn_name] = true
          fn_names[#fn_names + 1] = fn_name
        end
      end
      local fns = #fn_names > 0 and table.concat(fn_names, ", ") or "(none)"
      table.insert(lines, string.format("- %s: %s\n  functions: %s", lib_name, desc, fns))
    end
    table.insert(
      lines,
      IS_JS
          and 'Use `library_help({ library_name: "...", function_name: "..." })` to inspect parameter schemas and function documentation. Always inspect unfamiliar external library functions with library_help before calling them for the first time.\n'
        or 'Use `library_help(library_name="...", function_name="...")` to inspect parameter schemas and function documentation. Always inspect unfamiliar external library functions with library_help before calling them for the first time.\n'
    )
    parts[#parts + 1] = table.concat(lines, "\n")
  end
  return table.concat(parts)
end

-- Publishes the script before the permission prompt paints. Highlight is
-- awaited inline: an async task from here could outlive ToolDone on fast
-- auto-allowed runs and bake a stale script-only snapshot.
local function start(input, ctx)
  local buf, _, highlight = build_body(ctx, input.code)
  ctx:live_buf(buf)
  highlight()
end

local function hooked(slot_names)
  local slots = maki.api.get_slots()
  for _, name in ipairs(slot_names) do
    if slots[name] and #slots[name].fillers > 0 then
      return true
    end
  end
  return false
end

-- `open()` rides on the read and write tools, so hooks, plugins that replace
-- them, permission prompts, the file lock and plan mode all still apply. The
-- read tool numbers and caps lines, so we ask it for one line just to get its
-- yes and record the read, then take the real content straight from disk.
-- A read hook may have pointed that check at another file, so while one is
-- installed we refuse rather than read the path it steered away from.
-- A replacement write tool without `append` would drop the flag and overwrite
-- the file, so appends are refused there.
-- Metadata and listings (inspect) remain available to the sandbox even when
-- the list_dir tool itself is disabled or hidden from the model, since
-- os/pathlib inspection is a core interpreter capability. mkdir looks before
-- it creates, so mkdir also needs `inspect`.
local function shell_hint(tools, bash_text, powershell_text)
  if not tools.bash and tools.powershell then
    return powershell_text
  end
  return bash_text
end

local function unsupported_module_hint(tools, err)
  local mod = err:match("No module named ['\"]([%w_]+)['\"]")
  if mod then
    local shell_tool = shell_hint(tools, "bash", "powershell")
    return string.format(
      "\n\nHint: '%s' is not available in this minimal sandbox (importable modules are only: re, asyncio, sys, os, json, pathlib, urllib.parse, glob). For external packages or full Python capabilities, execute a Python script via the `%s` tool on the host system instead.",
      mod,
      shell_tool
    )
  end
  return ""
end

local function file_access(tools, schemas, ctx)
  local files = {}
  if tools.read then
    files.read = function(path)
      if hooked(READ_INPUT_SLOTS) then
        return nil, OPEN_READ_HOOKED_ERR
      end
      local _, err = tools.read({ path = path, offset = 1, limit = OPEN_READ_CHECK_LINES })
      if err then
        return nil, err
      end
      return maki.fs.read(maki.fs.abspath(path))
    end
  end
  if tools.write then
    local can_append = (schemas.write.properties or {}).append ~= nil
    files.write = function(path, content, append)
      if append and not can_append then
        return nil, OPEN_APPEND_UNSUPPORTED_ERR
      end
      return tools.write({ path = path, content = content, append = append })
    end
    files.mkdir = function(path, parents)
      local _, err = ctx:can_write(path)
      if err then
        return nil, err .. shell_hint(tools, MKDIR_BASH_HINT, MKDIR_POWERSHELL_HINT)
      end
      return maki.fs.mkdir(path, { parents = parents })
    end
  end
  files.inspect = function()
    if hooked(LIST_INPUT_SLOTS) then
      return nil, INSPECT_HOOKED_ERR
    end
    return true
  end
  return files
end

local function handler(input, ctx)
  local started_at = os.time()
  local timeout = input.timeout or opts.timeout_secs

  local buf, view, highlight = build_body(ctx, input.code)
  ctx:live_buf(buf)
  maki.async.run(highlight)

  view:append({ { "Waiting for output...", "dim" } })

  local max_lines, max_bytes = output_limits.resolve(opts, ctx)

  local waiting = true
  local collected = output_limits.collector(max_lines, max_bytes)
  local function show(line)
    if waiting then
      waiting = false
      view:clear()
    end
    collected:append(line)
    view:append(line)
  end

  -- Every sandbox tool call, so the compact row can show one child per call
  -- the way batch does. Concurrent calls append in dispatch order, which need
  -- not be the script's. The body keeps the script and stdout; these records
  -- travel in `state` only.
  local calls = {}
  local state = { calls = calls }

  -- Memoized, because a cancel reaches us twice: once through the hook and
  -- again as the interpreter's error, and the view is painted only once.
  local cut_reply
  local function cut(reason)
    if not cut_reply then
      cut_reply = partial.cut(view, collected:result(), reason, timeout, started_at)
      cut_reply.state = state
    end
    return cut_reply
  end

  -- Only for a handler still parked when the host gives up on it: normally
  -- the interpreter sees the cancel and we return the partial reply below.
  maki.async.on_cancel(function(reason)
    ctx:finish(cut(reason))
  end)

  -- Registry, MCP and host tools in one list, already filtered by
  -- `disabled_tools`, each entry carrying the audience of the tool a call to
  -- that name would really reach.
  local callable, callable_err = maki.agent.callable_tools(ctx)
  if callable_err then
    return { llm_output = CALLABLE_TOOLS_ERR .. callable_err, is_error = true }
  end

  local tools, schemas = {}, {}
  local param_maps = {}
  for _, t in ipairs(interpreter_tools(callable, ctx:audience(), ctx:workflow())) do
    local bind, name = t.alias or t.name, t.name
    if bind:match(PY_IDENTIFIER) then
      schemas[bind] = t.schema or {}
      if not IS_JS then
        param_maps[bind] = build_param_map(t.schema)
      end
      tools[bind] = function(tool_input)
        if param_maps[bind] then
          tool_input = apply_param_map(tool_input, param_maps[bind])
        end
        local record = {
          tool = name,
          params = type(tool_input) == "table" and tool_input or nil,
          status = CALL_RUNNING,
        }
        calls[#calls + 1] = record
        local call_started = maki.uv.hrtime()
        local function annotate(ann)
          local sep = ANNOTATION_SEP
          if record.annotation and (sep .. record.annotation .. sep):find(sep .. ann .. sep, 1, true) then
            return
          end
          record.annotation = record.annotation and (record.annotation .. sep .. ann) or ann
        end
        local function settle(is_error)
          record.status = is_error and CALL_ERROR or CALL_SUCCESS
          record.duration_ms = math.floor((maki.uv.hrtime() - call_started) / NANOS_PER_MILLISECOND)
        end
        local call_opts
        if t.workflow_only then
          call_opts = {}
        else
          -- The script clock stops while a tool call is awaited. Child tools keep
          -- their own default timeout unless overridden explicitly or relaxed by
          -- an outer script timeout.
          local explicit = type(tool_input) == "table" and tonumber(tool_input.timeout) or nil
          if explicit then
            call_opts = { timeout = explicit }
          elseif input.timeout and input.timeout > opts.timeout_secs then
            call_opts = { timeout = input.timeout }
          else
            call_opts = {}
          end
        end
        call_opts.annotation_marks_caller = true
        call_opts.on_annotation = annotate
        call_opts.on_usage = function(usage)
          record.usage = usage
        end
        call_opts.on_done = function(done)
          settle(done.is_error)
        end
        local out, call_err = maki.agent.call_tool(ctx, name, tool_input, call_opts)
        if record.status == CALL_RUNNING then
          settle(call_err ~= nil)
        end
        return out, call_err
      end
    end
  end

  local preamble_parts = IS_JS and {} or { PREAMBLE }
  local libs = maki.agent.mcp_libraries(ctx)
  if libs and #libs > 0 then
    local ns_lines = IS_JS and {} or { "class _McpNamespace:\n    pass\n" }
    for _, lib in ipairs(libs) do
      local lib_name = identifier_alias(lib.name)
      if lib_name then
        if IS_JS then
          table.insert(ns_lines, string.format("globalThis.%s = {};\n", lib_name))
        else
          table.insert(ns_lines, string.format("%s = _McpNamespace()\n", lib_name))
        end
        for _, fn in ipairs(lib.functions or {}) do
          local raw_fn_name = fn.name
          local fn_name = identifier_alias(raw_fn_name) or raw_fn_name
          if fn_name then
            local raw_wire = lib.name .. "__" .. raw_fn_name
            local wire_alias = identifier_alias(raw_wire)
            local wire = (tools[raw_wire] and raw_wire)
              or (wire_alias and tools[wire_alias] and wire_alias)
              or (lib_name .. "__" .. fn_name)
            if tools[wire] then
              if fn.schema then
                schemas[wire] = fn.schema
                if not IS_JS then
                  param_maps[wire] = build_param_map(fn.schema)
                end
              end
              -- A declared output schema means the result is JSON: decode it in
              -- the sandbox so scripts work with objects, not strings. The raw
              -- tool keeps its string contract for the host.
              if fn.output_schema then
                local raw_alias = "_raw_" .. wire
                if IS_JS then
                  table.insert(ns_lines, string.format("const %s = %s;\n", raw_alias, wire))
                  table.insert(
                    ns_lines,
                    string.format(
                      "globalThis.%s = async (o) => { const s = await %s(o); try { return JSON.parse(s); } catch { return s; } };\n",
                      wire,
                      raw_alias
                    )
                  )
                else
                  table.insert(ns_lines, string.format("%s = %s\n", raw_alias, wire))
                  table.insert(
                    ns_lines,
                    string.format(
                      "async def %s(**kwargs):\n    out = await %s(**kwargs)\n    try:\n        return json.loads(out)\n    except Exception:\n        return out\n",
                      wire,
                      raw_alias
                    )
                  )
                end
              end
              if IS_JS then
                table.insert(ns_lines, string.format("globalThis.%s.%s = %s;\n", lib_name, fn_name, wire))
                if raw_fn_name ~= fn_name then
                  table.insert(ns_lines, string.format("globalThis.%s[%q] = %s;\n", lib_name, raw_fn_name, wire))
                end
              else
                table.insert(ns_lines, string.format("%s.%s = %s\n", lib_name, fn_name, wire))
              end
            end
          end
        end
      end
    end
    table.insert(preamble_parts, table.concat(ns_lines))
  end

  local result, err = maki.interpreter.run(input.code, {
    engine = opts.engine,
    timeout = timeout,
    max_memory_mb = opts.max_memory_mb,
    preamble = table.concat(preamble_parts),
    on_output = show,
    tools = tools,
    files = file_access(tools, schemas, ctx),
  })

  if err then
    if err == CANCELLED_ERR then
      return cut("cancelled")
    end
    if err:find(TIME_LIMIT_SUBSTR, 1, true) then
      return cut("timeout")
    end
    if waiting then
      view:clear()
    end
    err = shell_hint(tools, err, (err:gsub(REMOVAL_BASH, REMOVAL_POWERSHELL, 1)))
    view:append_text(err)
    view:finish()
    local hint = ""
    if IS_JS then
      if input.code:find(JS_PROMISE_ALL, 1, true) then
        hint = hint .. GATHER_HINT
      end
      if
        err:find("SyntaxError:", 1, true)
        and (input.code:match("^%s*import%s") or input.code:match("\n%s*import%s"))
      then
        hint = hint .. JS_IMPORT_HINT
      end
    else
      -- The run is already paid for, so point a script that reached for
      -- `asyncio.gather` at the wrapper that would have kept its other results.
      hint = input.code:find(ASYNCIO_GATHER, 1, true) and GATHER_HINT or ""
      local exception = err:match(EXCEPTION_LINE) or ""
      for _, entry in ipairs(ERROR_HINTS) do
        if exception:find(entry[1]) then
          hint = hint .. entry[2]
          break
        end
      end
    end
    local mcp_hint = mcp_library_hint(libs, input.code, err, IS_JS)
    local mod_hint = (not IS_JS and mcp_hint == "") and unsupported_module_hint(tools, err) or ""
    return {
      llm_output = output_limits.cut_middle(err .. hint .. mcp_hint .. mod_hint, max_lines, max_bytes),
      is_error = true,
      body = buf,
      state = state,
    }
  end

  if result.output then
    show("return: " .. result.output)
  end

  -- Every line already went through `show`, so the collector holds the
  -- reply and a finished run keeps its tail just like a cut one. Full mode
  -- promises the whole output, so a cut there is an error.
  local over_full_limit = not max_bytes and collected:truncated()
  local llm_output = collected:result()
  if llm_output == "" then
    llm_output = NO_OUTPUT
    view:clear()
    view:append({ { "No output", "dim" } })
  elseif over_full_limit then
    llm_output = llm_output .. "\n" .. output_limits.FULL_LIMIT_NOTE
  end
  view:finish()

  return { llm_output = llm_output, is_error = over_full_limit, body = buf, state = state }
end

local function header(input)
  local lines = select(2, input.code:gsub("\n", "\n")) + 1
  return lines .. " lines"
end

local function restore(input, output, is_error, ctx)
  local buf, view, highlight = build_body(ctx, input.code)
  local st = ctx:state()
  if st and type(st.calls) == "table" then
    for _, record in ipairs(st.calls) do
      if record.status == CALL_RUNNING then
        record.status = CALL_ERROR
      end
    end
  end
  if is_error then
    view:append(output)
  elseif output == NO_OUTPUT then
    view:append({ { "No output", "dim" } })
  else
    view:append_text(output)
  end
  view:finish()
  highlight()
  return buf
end

maki.api.register_tool({
  name = "code_execution",
  description = description,
  describe = describe,
  schema = schema,
  examples = examples,
  kind = "execute",
  audiences = { "main", "research_sub", "general_sub" },
  start_annotation = { field = "timeout", kind = "timeout" },
  start = start,
  handler = handler,
  header = header,
  restore = restore,
})

maki.api.register_prompt_hint({
  slot = "efficient_tools",
  content = "code_execution",
})
