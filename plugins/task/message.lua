-- task_message is one name with two directions, picked by who calls it:
-- the parent steers a background subagent by id, a subagent reports to its
-- parent. Keeping the name leaves room for subagent-to-subagent messages.

local PARENT_DESCRIPTION =
  [[Message a running background subagent by id, e.g. a new fact, a changed scope or "wrap up now". It reads it after its current step.
Set interrupt only when it must act now: it cuts the current step, running tools included.]]
local SUB_DESCRIPTION =
  [[Report to the parent agent mid-task: a key finding that changes the plan, a blocker needing direction, or a risk.
The parent reads it at its next step, or wakes to read it when idle. Keep working after reporting unless blocked;
the parent replies with a message if needed. Never report routine progress; your final answer carries the full result.]]
local ID_DESCRIPTION = "Parent only: task id from its receipt. A batch's id reaches every subagent in it"
local MESSAGE_DESCRIPTION = "What the subagent should know or do, or what the parent must know"
local INTERRUPT_DESCRIPTION = "Parent only: cut the current step short"
local QUEUED = "Message queued for %s."
local INTERRUPTED = "Interrupted %s with the message."
local NO_SESSION_ERR = "no parent session to report to"
local REPORT_HEAD = "[Report from subagent %s]"
local DISPLAY = "Subagent %s reported"
local DELIVERED = "Report delivered to the parent agent."

local function to_parent(input, ctx)
  local session = ctx:session_id()
  if not session then
    return { llm_output = NO_SESSION_ERR, is_error = true }
  end
  local task = ctx:task_id()
  local text = REPORT_HEAD:format(task) .. "\n" .. input.message
  local ok, err = maki.session.notify(text, {
    session = session,
    display = DISPLAY:format(task),
    wake = true,
  })
  if not ok then
    return { llm_output = err, is_error = true }
  end
  return DELIVERED
end

local function to_subagent(input, ctx)
  local interrupt = input.interrupt == true
  local sent, err = maki.task.send(ctx, input.id, input.message, { interrupt = interrupt })
  if not sent then
    return { llm_output = err, is_error = true }
  end
  return (interrupt and INTERRUPTED or QUEUED):format(sent.name)
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
    },
  },
  header = function(input)
    return input.message
  end,
  -- A restore hook that returns nothing would also drop the restored header.
  restore = function()
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
  content = "- A `[Report from subagent <id>]` observation is a mid-task report. Absorb plan-changing facts, answer a blocked subagent with **task_message** using that id, or adjust sibling tasks. Never send acknowledgements; if nothing needs action, end your turn.",
})
