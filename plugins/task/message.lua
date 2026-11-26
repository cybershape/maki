-- task_message is one name with two directions, picked by who calls it:
-- the parent steers a background subagent by id, a subagent reports to its
-- parent. Keeping the name leaves room for subagent-to-subagent messages.

local PARENT_DESCRIPTION =
  [[Message a running background subagent by codename, e.g. a new fact, a changed scope or "wrap up now". It reads it after its current step.
Set interrupt only when it must act now: it cuts the current step, running tools included.]]
local SUB_DESCRIPTION =
  [[Report to the parent agent mid-task: a key finding that changes the plan, a blocker needing direction, or a risk.
The parent reads it at its next step, or wakes to read it when idle. Keep working after reporting unless blocked;
the parent replies with a message if needed. Never report routine progress; your final answer carries the full result.]]
local ID_DESCRIPTION = "Parent only: task codename from its receipt (its call id also works). A batch's id reaches every subagent in it"
local MESSAGE_DESCRIPTION = "What the subagent should know or do, or what the parent must know"
local INTERRUPT_DESCRIPTION = "Parent only: cut the current step short"
local REASON_DESCRIPTION = "Why you are calling this tool and what you expect."
local QUEUED = "Message queued for %s."
local INTERRUPTED = "Interrupted %s with the message."
local NO_SESSION_ERR = "no parent session to report to"
local REPORT_HEAD = "[Report from %s]"
local DISPLAY = "%s reported: %s"
local DELIVERED = "Report delivered to the parent agent."
local PARENT_RECIPIENT = "parent"
local SEND_ARROW = "→ "
local HEADER_MESSAGE = "%s%s: %s"
local HEADER_REASON = "%s%s · %s"

-- The recipient stays visible even when a toolcall_reason takes the message
-- text's place, matching the compact rows' `reason · detail` shape.
local function header_line(recipient, input)
  local reason = input.toolcall_reason
  if reason and reason:find("%S") then
    return HEADER_REASON:format(SEND_ARROW, recipient, reason)
  end
  return HEADER_MESSAGE:format(SEND_ARROW, recipient, input.message)
end

local function header_buf(recipient, input)
  local buf = maki.ui.buf()
  buf:line({ { header_line(recipient, input) } })
  return buf
end

-- The handler's header is not saved with the session, so the resolved
-- recipient also goes in the state for a restore to show.
local function sent_reply(text, recipient, input)
  return {
    llm_output = text,
    header = header_buf(recipient, input),
    state = { name = recipient },
  }
end

local function to_parent(input, ctx)
  local session = ctx:session_id()
  if not session then
    return { llm_output = NO_SESSION_ERR, is_error = true }
  end
  local task = ctx:task_name() or ctx:task_id()
  local text = REPORT_HEAD:format(task) .. "\n" .. input.message
  local ok, err = maki.session.notify(text, {
    session = session,
    display = DISPLAY:format(task, input.message),
    wake = true,
  })
  if not ok then
    return { llm_output = err, is_error = true }
  end
  return sent_reply(DELIVERED, PARENT_RECIPIENT, input)
end

local function to_subagent(input, ctx)
  local interrupt = input.interrupt == true
  local sent, err = maki.task.send(ctx, input.id, input.message, { interrupt = interrupt })
  if not sent then
    return { llm_output = err, is_error = true }
  end
  return sent_reply((interrupt and INTERRUPTED or QUEUED):format(sent.name), sent.name, input)
end

maki.api.register_tool({
  name = "task_message",
  description = PARENT_DESCRIPTION,
  audiences = { "main", "research_sub", "general_sub" },
  describe = function(dctx)
    return dctx.audience == "main" and PARENT_DESCRIPTION or SUB_DESCRIPTION
  end,
  schema = {
    type = "object",
    required = { "message" },
    additionalProperties = false,
    properties = {
      id = { type = "string", description = ID_DESCRIPTION, needs_background = true },
      message = { type = "string", description = MESSAGE_DESCRIPTION },
      interrupt = {
        type = "boolean",
        description = INTERRUPT_DESCRIPTION,
        needs_background = true,
      },
      -- Declared here so the host does not lift it into the header on its
      -- own: the recipient must stay visible even with a reason.
      toolcall_reason = { type = "string", description = REASON_DESCRIPTION },
    },
  },
  header = function(input)
    return header_line(input.id or PARENT_RECIPIENT, input)
  end,
  restore = function(input, _, _, ctx)
    local state = ctx:state()
    if state and state.name then
      return { header = header_buf(state.name, input) }
    end
    return {}
  end,
  handler = function(input, ctx)
    if ctx:audience() == "main" then
      return to_subagent(input, ctx)
    end
    return to_parent(input, ctx)
  end,
})

maki.api.register_prompt_hint({
  prompt = { "research", "general" },
  slot = "tool_usage",
  content = "- Use **task_message** only for what changes the parent's work: a plan-changing finding, a blocker needing direction, or a risk. Never for routine progress. Keep working after reporting unless blocked.",
})

maki.api.register_prompt_hint({
  prompt = "system",
  slot = "tool_usage",
  content = "- A `[Report from <codename>]` observation is a mid-task report. Absorb plan-changing facts, answer a blocked subagent with **task_message** using that codename, or adjust sibling tasks. Never send acknowledgements; if nothing needs action, end your turn.",
})
