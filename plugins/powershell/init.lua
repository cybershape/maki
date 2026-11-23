local output_limits = require("maki.output_limits")

local BASE = [[
- Commands run in the configured working directory or the current directory.
- Use `workdir` param instead of `cd` or `Set-Location`.
- Interactive commands are not supported.
- Use `head` or `tail` param to limit output lines instead of piping to `Select-Object -First`.
- Provide a short `toolcall_reason` (3-5 words).]]

local V5_NOTE =
  "Windows PowerShell 5.1 environment. Prefer one statement per call or separate statements with `;` (chained `&&` / `||` are automatically shimmed)."

local V7_NOTE = [[PowerShell 7 or newer. `&&` and `||` work (`$?` is the previous statement, not the previous pipeline).
- `&&` and `||` have lower precedence than `|`. Parenthesize a chain used as pipeline input: `(@(1) && @(2)) | Measure-Object`.
- Ternary `?:` and null-coalescing `??` / `??=` work.
- `ForEach-Object -Parallel` runs script blocks on other runspaces. It does not see variables unless you pass them with `$using:name`.]]

local function description_for(modern)
  local note = modern and V7_NOTE or V5_NOTE
  return "Execute a command through Windows PowerShell. " .. note .. "\n" .. BASE
end

local shell = "powershell.exe"
local modern = maki.fn.executable("pwsh.exe") == 1
if modern then
  shell = "pwsh.exe"
end

local opts = maki.api.register_options(output_limits.extend({
  timeout_secs = {
    default = 180,
    min = 5,
    desc = "Kill the command after this many seconds. A call's `timeout` param overrides it.",
  },
}))

local function unquote(s)
  local q = s:sub(1, 1)
  if (q == '"' or q == "'") and s:sub(-1) == q then
    return s:sub(2, -2)
  end
  return s
end

local function parse_cd_hint(cmd, workdir)
  if workdir and workdir ~= "" then
    return cmd, workdir
  end
  local dir, tail = cmd:match("^%s*cd%s+([^\r\n;&]+)%s*[;&]+%s*(.+)$")
  if not dir then
    dir, tail = cmd:match("^%s*Set%-Location%s+([^\r\n;&]+)%s*[;&]+%s*(.+)$")
  end
  if dir and dir ~= "" and tail and tail ~= "" then
    local s = dir:match("^%s*(.-)%s*$")
    s = s:gsub("%s+[%d%*]*>%s*%$null", "")
    s = s:gsub("%s+|%s*Out%-Null", "")
    s = s:match("^%s*(.-)%s*$")
    if s and s ~= "" and s ~= "-" then
      return tail, unquote(s)
    end
  end
  return cmd, nil
end

local function strip_ansi(s)
  if not s:find("\27", 1, true) and not s:find("\r", 1, true) then
    return s
  end
  if s:find("\r", 1, true) then
    s = s:gsub("\r\n", "\n"):gsub("\r", "\n")
  end
  s = s:gsub("\27%[[0-9;?<=>]*[@-~]", "")
  s = s:gsub("\27%]..-\7", "")
  s = s:gsub("\27%]..-\27\\", "")
  s = s:gsub("\27[@-Z\\%-_]", "")
  s = s:gsub("\27", "")
  return s
end

local function shim_v5_operators(cmd)
  local in_single = false
  local in_double = false
  local i = 1
  local n = #cmd
  local parts = {}
  local pending_closes = 0

  while i <= n do
    local c = cmd:sub(i, i)
    if c == "'" and not in_double then
      in_single = not in_single
      parts[#parts + 1] = c
      i = i + 1
    elseif c == '"' and not in_single then
      in_double = not in_double
      parts[#parts + 1] = c
      i = i + 1
    elseif not in_single and not in_double and c == "&" and cmd:sub(i + 1, i + 1) == "&" then
      parts[#parts + 1] = "; if ($?) { "
      pending_closes = pending_closes + 1
      i = i + 2
    elseif not in_single and not in_double and c == "|" and cmd:sub(i + 1, i + 1) == "|" then
      parts[#parts + 1] = "; if (-not $?) { "
      pending_closes = pending_closes + 1
      i = i + 2
    else
      parts[#parts + 1] = c
      i = i + 1
    end
  end

  if pending_closes > 0 then
    parts[#parts + 1] = string.rep(" }", pending_closes)
  end
  return table.concat(parts)
end

maki.api.register_tool({
  name = "powershell",
  kind = "execute",
  permission = "run",
  permission_scopes = function(input)
    if not input.command or input.command:match("^%s*$") then
      return nil
    end
    return { scopes = { input.command }, force_prompt = false }
  end,
  description = description_for(modern),
  schema = {
    type = "object",
    properties = {
      command = { type = "string", description = "The PowerShell command to execute", required = true },
      timeout = { type = "integer", description = "Timeout in seconds (default 180)" },
      workdir = { type = "string", description = "Working directory (default: current directory)" },
      head = { type = "integer", description = "Return only the first N lines" },
      tail = { type = "integer", description = "Return only the last N lines" },
      toolcall_reason = {
        type = "string",
        description = "Why you are calling this tool and what you expect.",
      },
    },
  },
  header = function(input)
    return input.toolcall_reason or input.command
  end,
  handler = function(input, ctx)
    if not input.command or input.command:match("^%s*$") then
      return { llm_output = "error: command is required", is_error = true }
    end
    if input.head and input.head < 1 then
      return { llm_output = "error: head must be >= 1", is_error = true }
    end
    if input.tail and input.tail < 1 then
      return { llm_output = "error: tail must be >= 1", is_error = true }
    end

    local command, workdir = parse_cd_hint(input.command, input.workdir)
    if not modern then
      command = shim_v5_operators(command)
    end

    local timeout_secs = input.timeout or opts.timeout_secs
    local output = {}
    local finished = false

    ctx:set_deadline(timeout_secs)

    local function append(line)
      if #output > 0 then
        output[#output + 1] = "\n"
      end
      output[#output + 1] = line
    end

    local function finish(code)
      if finished then
        return
      end
      finished = true

      local raw_result = table.concat(output)
      local result = strip_ansi(raw_result)

      if input.head or input.tail then
        local lines = {}
        for line in result:gmatch("[^\n]+") do
          lines[#lines + 1] = line
        end
        if input.head and #lines > input.head then
          local kept = {}
          for i = 1, math.min(input.head, #lines) do
            kept[#kept + 1] = lines[i]
          end
          result = table.concat(kept, "\n")
        elseif input.tail and #lines > input.tail then
          local first = math.max(1, #lines - input.tail + 1)
          local kept = {}
          for i = first, #lines do
            kept[#kept + 1] = lines[i]
          end
          result = table.concat(kept, "\n")
        end
      end

      local max_lines, max_bytes = output_limits.resolve(opts, ctx)
      if max_lines or max_bytes then
        result = output_limits.cut_middle(result, max_lines, max_bytes)
      end

      local is_err = code ~= 0

      -- Broken pipe tolerance when Select-Object was used downstream
      if is_err and (code == 1 or code == 141) and input.command:find("Select%-Object%s+%-First") then
        local has_syntax_err = result:find("ParserError") or result:find("ParseException")
        local has_fatal_err = result:find("fatal:") or result:find("Exception:")
        if not has_syntax_err and not has_fatal_err and #output > 0 then
          is_err = false
          code = 0
        end
      end

      if is_err then
        local hint = ""
        if result:find("ParserError") and (input.command:find("&&") or input.command:find("||")) then
          hint = "\nHint: Windows PowerShell 5.1 rejects `&&` and `||`. Separate statements with `;` or run one command per call."
        end
        result = (result == "" and "" or result .. "\n") .. "Exit code: " .. code .. hint
      elseif result == "" then
        result = "Exit code: 0"
      end

      ctx:finish({ llm_output = result, is_error = is_err })
    end

    local job_id = maki.fn.jobstart({
      shell,
      "-NoLogo",
      "-NoProfile",
      "-ExecutionPolicy",
      "Bypass",
      "-NonInteractive",
      "-Command",
      "[Console]::InputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); "
        .. "$OutputEncoding = [Console]::OutputEncoding; "
        .. "chcp 65001 >$null; "
        .. command,
    }, {
      cwd = workdir,
      on_stdout = function(_, line)
        append(line)
      end,
      on_stderr = function(_, line)
        append(line)
      end,
      on_exit = function(_, code)
        finish(code)
      end,
    })

    maki.async.on_cancel(function(reason)
      if finished then
        return
      end
      finished = true
      maki.fn.jobstop(job_id)
      local cancel_msg = reason or "cancelled"
      if cancel_msg:find("deadline") or cancel_msg:find("timeout") then
        cancel_msg = cancel_msg
          .. "\n[timed out after "
          .. timeout_secs
          .. "s; for long builds/tests, increase the 'timeout' parameter]"
      end
      ctx:finish({ llm_output = table.concat(output) .. "\n" .. cancel_msg, is_error = true })
    end)

    return nil
  end,
})
