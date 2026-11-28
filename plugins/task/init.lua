-- This plugin owns the /tasks picker over the subagents spawned here:
-- picker.lua registers the command and the keymap when this file is loaded, so
-- the two cannot be enabled apart and left pointing at each other's absence.

local ToolView = require("maki.tool_view")
local output_limits = require("maki.output_limits")
require("jobs")
require("picker")
require("stop")

local Runner = require("runner")
-- The parent's steering needs background delegation; a subagent's report
-- does not, so the tool loads either way and id/interrupt stay gated.
require("message")

local DEFAULT_OUTPUT_LINES = 3
local WAITING_FOR_DEPS = "waiting for %s"
local WAITING_FOR_SLOT = "waiting for a free subagent slot"
local MOVED_RECEIPT = "Subagent %s"
local FOREGROUND_NOTE = "[Could not start in the background (%s). Ran in the foreground.]"
local MAX_CODENAME_LEN = 40
local BAD_CODENAME = "name must be a non-empty single line of at most 40 characters"
local TAKEN_CODENAME = "codename '%s' is already taken by a running task (taken: %s). Pick another."
local DEFAULT_SCOPE = "__default__"
local AFTER_ARRAY_ERR = "after must be an array of task codenames"
local AFTER_EMPTY_ERR = "after entries must be non-empty task codenames"
local AFTER_SELF_ERR = "task '%s' cannot depend on itself"
local AFTER_UNKNOWN_ERR = "unknown dependency '%s' (must name an active or completed task in this session)"
local AFTER_CYCLE_ERR = "dependency cycle involving '%s'"
local AFTER_FAILED_ERR = "dependency '%s' failed: %s"
local AFTER_CANCELLED_ERR = "dependency '%s' failed"
local DEP_CONTEXT_HEADER = "Dependency results (completed before this task started):"
local DEP_EMPTY_OUTPUT = "(no output)"

-- Codenames a live task of this session holds. The Lua thread is single, so a
-- check and the reserve that follows it are atomic between awaits: two task
-- calls of one turn cannot both claim a name. Released when the task closes.
local reserved = {}
local active_tasks = {}
local completed_tasks = {}

local function scope_table(root, scope)
  local tbl = root[scope]
  if not tbl then
    tbl = {}
    root[scope] = tbl
  end
  return tbl
end

local function parse_after(raw, self_name)
  if raw == nil then
    return {}, nil
  end
  if type(raw) ~= "table" or (#raw == 0 and next(raw) ~= nil) then
    return nil, AFTER_ARRAY_ERR
  end
  local deps, seen = {}, {}
  for i = 1, #raw do
    local dep = raw[i]
    if type(dep) ~= "string" then
      return nil, AFTER_ARRAY_ERR
    end
    dep = dep:match("^%s*(.-)%s*$")
    if dep == "" then
      return nil, AFTER_EMPTY_ERR
    end
    if dep == self_name then
      return nil, string.format(AFTER_SELF_ERR, self_name)
    end
    if not seen[dep] then
      seen[dep] = true
      deps[#deps + 1] = dep
    end
  end
  return deps, nil
end

local function has_cycle(scope, start_name)
  local active = active_tasks[scope]
  if not active then
    return false
  end
  local visiting, visited = {}, {}
  local function dfs(name)
    if visiting[name] then
      return true
    end
    if visited[name] then
      return false
    end
    visiting[name] = true
    local node = active[name]
    if node then
      for _, dep in ipairs(node.deps) do
        if dfs(dep) then
          return true
        end
      end
    end
    visiting[name] = nil
    visited[name] = true
    return false
  end
  return dfs(start_name)
end

local function lookup_dep(scope, dep)
  return (active_tasks[scope] and active_tasks[scope][dep]) or (completed_tasks[scope] and completed_tasks[scope][dep])
end

local function settle_node(scope, node, output, is_error, err_msg)
  if node.done then
    return
  end
  node.done = true
  node.failed = is_error or err_msg ~= nil
  node.err = err_msg or (is_error and output) or nil
  node.output = output
  local active = active_tasks[scope]
  if active and active[node.name] == node then
    active[node.name] = nil
    if next(active) == nil then
      active_tasks[scope] = nil
    end
  end
  scope_table(completed_tasks, scope)[node.name] = node
  if node.permit then
    node.permit:release()
    node.permit = nil
  end
end

local function with_dep_context(prompt, dep_outputs)
  if #dep_outputs == 0 then
    return prompt
  end
  local sections = { DEP_CONTEXT_HEADER }
  for _, item in ipairs(dep_outputs) do
    local body = item.output and item.output ~= "" and item.output or DEP_EMPTY_OUTPUT
    sections[#sections + 1] = string.format("[%s]\n%s", item.name, body)
  end
  return table.concat(sections, "\n\n") .. "\n\n" .. prompt
end

maki.api.create_autocmd("SessionEnd", {
  callback = function(ev)
    local sid = ev.data and ev.data.session_id
    if sid then
      completed_tasks[sid] = nil
    end
  end,
})

local function taken_names(sid)
  local names = {}
  for name in pairs(reserved[sid] or {}) do
    names[#names + 1] = name
  end
  -- A reload drops the local table; the host still lists the live sessions.
  -- agent.list reads the registry directly, so it answers without a UI too.
  local live = maki.agent.list({ session = sid })
  if live then
    for _, t in ipairs(live) do
      if t.name and t.name ~= "" and (t.status == "working" or t.status == "idle") then
        names[#names + 1] = t.name
      end
    end
  end
  table.sort(names)
  return names
end

local function reserve_codename(sid, name)
  local held = reserved[sid]
  if not held then
    held = {}
    reserved[sid] = held
  end
  held[name] = true
end

local function release_codename(sid, name)
  local held = reserved[sid]
  if held then
    held[name] = nil
    if next(held) == nil then
      reserved[sid] = nil
    end
  end
end

local description = [[Launch an autonomous subagent to perform tasks independently. Best combined with batch.

Subagent types (set via `subagent_type`):
- `research` (default): Read-only tools. For codebase exploration or gathering context.
- `general`: Full tool access. For delegating implementation work.

Lifecycle & coordination:
- Subagents are run-to-completion: each runs until it finishes a turn with no tool calls, then closes and delivers its final output to you. A subagent cannot pause or idle-wait for you or a sibling.
- Use `after` when a task depends on another task's work or output: the dependent task stays queued (zero turns/tokens) until all `after` tasks succeed, receives their outputs in its prompt, and fails without running if any dependency fails. Never launch a dependent subagent early and tell it to wait or sleep.
- A background task (`background = true`, default) lets you keep working or end your turn. If you launch a task, do not use `sleep` to wait for it to finish: you are the main loop, and the task will naturally return to you when it finishes (mid-task `[Report from <codename>]` messages and final results wake you automatically). Steer a running task with `task_message` or stop it with `task_stop`.
- A synchronous task (`background = false`) blocks your turn until it finishes: no subagent can message you while you wait. Prefer background tasks so reports and steering keep flowing.

Notes:
1. Launch independent tasks concurrently in a batch.
2. The subagent's output is not visible to the user. Summarize it in your response.
3. Each invocation starts fresh - inline any needed context into the prompt (plus `after` for upstream task outputs).
4. Tell it to return concise summaries with file:line refs, not full file contents.
]]

local examples = {
  {
    name = "Hawk",
    description = "Find auth middleware",
    prompt = "Search the codebase for authentication middleware. Return file paths and a summary of how auth is implemented.",
  },
}

-- Standalone runs render markdown on the Rust side (format = "markdown").
-- This mirrors it for restore, batch children and a row that had to wait.
local function result_body(output, is_error, ctx)
  local tol = ctx:tool_output_lines()
  return ToolView.restore_markdown(output, is_error, {
    max_lines = (tol and tol.task) or DEFAULT_OUTPUT_LINES,
    keep = "head",
    max_line_bytes = output_limits.DEFAULT_MAX_LINE_BYTES,
  })
end

local function handler(input, ctx)
  local sid = ctx:session_id()
  local scope = sid or DEFAULT_SCOPE
  local name = input.name and input.name:match("^%s*(.-)%s*$")
  if not name or name == "" or name:find("\n") or #name > MAX_CODENAME_LEN then
    return { llm_output = BAD_CODENAME, is_error = true }
  end
  local deps, deps_err = parse_after(input.after, name)
  if deps_err then
    return { llm_output = deps_err, is_error = true }
  end
  if sid then
    for _, taken in ipairs(taken_names(sid)) do
      if taken == name then
        return {
          llm_output = string.format(TAKEN_CODENAME, name, table.concat(taken_names(sid), ", ")),
          is_error = true,
        }
      end
    end
    reserve_codename(sid, name)
  end

  local node = {
    name = name,
    deps = deps,
    done = false,
    failed = false,
  }
  scope_table(active_tasks, scope)[name] = node

  local function fail_early(msg)
    settle_node(scope, node, msg, true, msg)
    if sid then
      release_codename(sid, name)
    end
    return { llm_output = msg, is_error = true }
  end

  -- Background by default: a foreground task holds the model's turn until it
  -- finishes, so only an explicit `background = false` blocks. Where moving is
  -- impossible (nested call, no live chat, background off) the task runs
  -- blocking with a note, the same as an explicit ask that failed.
  local run_in_background = Runner.allow_background and input.background ~= false
  local prepared, prepare_err = Runner.prepare(input, ctx)
  if prepare_err then
    return fail_early(prepare_err)
  end

  if #deps > 0 then
    -- Yield briefly so sibling tasks launched in the same turn or batch register first.
    for _ = 1, 2 do
      local all_known = true
      for _, dep in ipairs(deps) do
        if not lookup_dep(scope, dep) then
          all_known = false
          break
        end
      end
      if all_known then
        break
      end
      maki.async.sleep(0)
    end
    for _, dep in ipairs(deps) do
      if not lookup_dep(scope, dep) then
        return fail_early(string.format(AFTER_UNKNOWN_ERR, dep))
      end
    end
    if has_cycle(scope, name) then
      return fail_early(string.format(AFTER_CYCLE_ERR, name))
    end
  end

  -- Opened before the slot wait, so a moved task waiting for a slot can
  -- already take messages: they ride with its first prompt.
  local sess, sess_err = maki.agent.session(ctx, prepared.options)
  if sess_err then
    return fail_early(prepared.error_context .. sess_err)
  end
  local function receipt()
    return string.format(MOVED_RECEIPT, prepared.options.name)
  end

  -- pcall so a raised error cannot leak the permit or the session: left to
  -- the garbage collector it keeps the subagent's event relay alive on an
  -- idle VM.
  local foreground_note, waiting, permit
  local ok, out = pcall(function()
    -- A background task moves before it waits for a slot, so a full house
    -- never holds the model's turn. Once moved it still queues for a slot
    -- like any other task. A blocking task only gets its hook once it holds a
    -- slot. Moving fails where background work is off (a zero
    -- --background-timeout), and then the task simply runs blocking.
    if run_in_background then
      ctx:on_background(receipt)
      local moved, move_err = ctx:background()
      if not moved then
        foreground_note = string.format(FOREGROUND_NOTE, tostring(move_err))
      end
    end

    if #deps > 0 then
      local pending_names = {}
      for _, dep in ipairs(deps) do
        local dep_node = lookup_dep(scope, dep)
        if dep_node and not dep_node.done then
          pending_names[#pending_names + 1] = dep
        end
      end
      if #pending_names > 0 then
        waiting = maki.ui.buf()
        waiting:line({ { string.format(WAITING_FOR_DEPS, table.concat(pending_names, ", ")), "dim" } })
        ctx:live_buf(waiting)
      end
      local dep_outputs = {}
      for _, dep in ipairs(deps) do
        local dep_node = lookup_dep(scope, dep)
        while dep_node and not dep_node.done do
          if not dep_node.sem then
            dep_node.sem = maki.async.semaphore(1)
            dep_node.permit = dep_node.sem:acquire()
          end
          local p = dep_node.sem:acquire()
          p:release()
          dep_node = lookup_dep(scope, dep)
        end
        if not dep_node then
          return { llm_output = string.format(AFTER_UNKNOWN_ERR, dep), is_error = true }
        end
        if dep_node.failed then
          local reason = dep_node.err or dep_node.output
          local msg = reason and reason ~= "" and string.format(AFTER_FAILED_ERR, dep, reason)
            or string.format(AFTER_CANCELLED_ERR, dep)
          return { llm_output = msg, is_error = true }
        end
        dep_outputs[#dep_outputs + 1] = { name = dep, output = dep_node.output }
      end
      prepared.message = with_dep_context(prepared.message, dep_outputs)
      if waiting then
        waiting:set_lines({})
      end
    end

    -- Shown only while every slot is taken, moved subagents included. A live
    -- body outlives the wait as a snapshot, so the final reply carries its own.
    permit = Runner.acquire_slot(function()
      if waiting then
        waiting:set_lines({ { WAITING_FOR_SLOT, "dim" } })
      else
        waiting = maki.ui.buf()
        waiting:line({ { WAITING_FOR_SLOT, "dim" } })
        ctx:live_buf(waiting)
      end
    end)
    if waiting then
      waiting:set_lines({})
    end

    if not run_in_background then
      ctx:on_background(receipt)
    end
    return Runner.run(sess, prepared, ctx)
  end)

  local final_out = ok and out or { llm_output = tostring(out), is_error = true }
  sess:close(final_out)
  settle_node(scope, node, final_out.llm_output, final_out.is_error == true, not ok and tostring(out) or nil)
  if sid then
    release_codename(sid, name)
  end
  if permit then
    permit:release()
  end
  if not ok then
    error(out, 0)
  end
  if waiting then
    out.body = result_body(out.llm_output, out.is_error, ctx)
  end
  if foreground_note and not ctx:backgrounded() then
    out.llm_output = foreground_note .. "\n\n" .. out.llm_output
  end
  return out
end

local function header(input)
  local buf = maki.ui.buf()
  local desc = input.description
  if desc and desc ~= "" then
    buf:line({ { input.name }, { ": " .. desc:gsub("%s+", " "), "dim" } })
  else
    buf:line({ { input.name } })
  end
  return buf
end

local function restore(_input, output, is_error, ctx)
  return result_body(output, is_error, ctx)
end

maki.api.register_tool({
  name = "task",
  description = description,
  kind = "execute",
  audiences = { "main", "workflow" },
  examples = examples,
  schema = Runner.schema,
  handler = handler,
  header = header,
  restore = restore,
})
