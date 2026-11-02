local DESCRIPTION = [[Execute a command through Windows PowerShell.

- Commands run in the configured working directory or the current directory.
- Use `workdir` instead of `Set-Location`.
- Interactive commands are not supported.
- Use `tail` to return only the last N output lines.
- Provide a short `toolcall_reason` (3-5 words).]]

local opts = maki.api.register_options({
  timeout_secs = {
    default = 120,
    min = 5,
    desc = "Kill the command after this many seconds. A call's `timeout` param overrides it.",
  },
})

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
  description = DESCRIPTION,
  schema = {
    type = "object",
    properties = {
      command = { type = "string", description = "The PowerShell command to execute", required = true },
      timeout = { type = "integer", description = "Timeout in seconds (default 120)" },
      workdir = { type = "string", description = "Working directory (default: current directory)" },
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
    if input.tail and input.tail < 1 then
      return { llm_output = "error: tail must be >= 1", is_error = true }
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

      local result = table.concat(output)
      if input.tail then
        local lines = {}
        for line in result:gmatch("[^\n]+") do
          lines[#lines + 1] = line
        end
        local first = math.max(1, #lines - input.tail + 1)
        local kept = {}
        for i = first, #lines do
          kept[#kept + 1] = lines[i]
        end
        result = table.concat(kept, "\n")
      end

      if code ~= 0 then
        result = (result == "" and "" or result .. "\n") .. "Exit code: " .. code
      elseif result == "" then
        result = "Exit code: 0"
      end
      ctx:finish({ llm_output = result, is_error = code ~= 0 })
    end

    -- A pipe has no console code page, so PowerShell 5.1 emits the ANSI
    -- code page. The job reader decodes UTF-8, which turns that into mojibake.
    local job_id = maki.fn.jobstart({
      "powershell.exe",
      "-NoLogo",
      "-NoProfile",
      "-ExecutionPolicy",
      "Bypass",
      "-NonInteractive",
      "-Command",
      "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); "
        .. "$OutputEncoding = [Console]::OutputEncoding; "
        .. input.command,
    }, {
      cwd = input.workdir,
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
      ctx:finish({ llm_output = table.concat(output) .. "\n" .. reason, is_error = true })
    end)

    return nil
  end,
})
