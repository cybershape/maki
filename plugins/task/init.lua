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
local WAITING_FOR_SLOT = "waiting for a free subagent slot"
local MOVED_RECEIPT = "Subagent %s"
local FOREGROUND_NOTE = "[Could not start in the background (%s). Ran in the foreground.]"
local MAX_CODENAME_LEN = 40
local BAD_CODENAME = "name must be a non-empty single line of at most 40 characters"
local TAKEN_CODENAME = "codename '%s' is already taken by a running task (taken: %s). Pick another."

-- Codenames a live task of this session holds. The Lua thread is single, so a
-- check and the reserve that follows it are atomic between awaits: two task
-- calls of one turn cannot both claim a name. Released when the task closes.
local reserved = {}

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

Notes:
1. Launch multiple tasks concurrently when possible.
2. The agent's result is not visible to the user. Summarize it in your response.
3. Each invocation starts fresh - inline any needed context into the prompt.
4. Tell it to return concise summaries with file:line refs, not full file contents.
5. A synchronous task (`background = false`) blocks your turn until it finishes: you cannot send or receive messages while you wait, and it cannot report to you mid-task. If any task you launch is synchronous, no subagent can message you until it returns. Prefer background tasks so reports and steering keep flowing.
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
  local name = input.name and input.name:match("^%s*(.-)%s*$")
  if not name or name == "" or name:find("\n") or #name > MAX_CODENAME_LEN then
    return { llm_output = BAD_CODENAME, is_error = true }
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

  -- Background by default: a foreground task holds the model's turn until it
  -- finishes, so only an explicit `background = false` blocks. Where moving is
  -- impossible (nested call, no live chat, background off) the task runs
  -- blocking with a note, the same as an explicit ask that failed.
  local run_in_background = Runner.allow_background and input.background ~= false
  local prepared, prepare_err = Runner.prepare(input, ctx, run_in_background)
  if prepare_err then
    if sid then release_codename(sid, name) end
    return { llm_output = prepare_err, is_error = true }
  end

  -- Opened before the slot wait, so a moved task waiting for a slot can
  -- already take messages: they ride with its first prompt.
  local sess, sess_err = maki.agent.session(ctx, prepared.options)
  if sess_err then
    if sid then release_codename(sid, name) end
    return { llm_output = prepared.error_context .. sess_err, is_error = true }
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

    -- Shown only while every slot is taken, moved subagents included. A live
    -- body outlives the wait as a snapshot, so the final reply carries its own.
    permit = Runner.acquire_slot(function()
      waiting = maki.ui.buf()
      waiting:line({ { WAITING_FOR_SLOT, "dim" } })
      ctx:live_buf(waiting)
    end)
    if waiting then
      waiting:set_lines({})
    end

    if not run_in_background then
      ctx:on_background(receipt)
    end
    return Runner.run(sess, prepared)
  end)

  sess:close(ok and out or { llm_output = tostring(out), is_error = true })
  if sid then release_codename(sid, name) end
  if permit then
    permit:release()
  end
  if not ok then
    error(out, 0)
  end
  if waiting then
    out.body = result_body(out.llm_output, out.is_error, ctx)
  end
  if foreground_note then
    out.llm_output = foreground_note .. "\n\n" .. out.llm_output
  end
  return out
end

local function header(input)
  return input.name
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
