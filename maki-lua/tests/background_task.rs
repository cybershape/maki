//! Calls that move themselves with `ctx:background()`: the real task plugin
//! asked to run in the background, and the `agent` gate on the move.
//! Model selection inherits the test provider; the session APIs stay real.

mod common;

use std::iter;
use std::sync::Arc;
use std::sync::mpsc::sync_channel;
use std::thread;

use common::{
    Chat, Parked, WAIT, call_ctx, done_text, exec, next_event, next_task_event, poll_until,
    request_move,
};
use maki_agent::permissions::PermissionManager;
use maki_agent::tools::{ToolAudience, ToolRegistry};
use maki_agent::{
    AgentEvent, CancelToken, Envelope, EventSender, PermissionsConfig, SessionMailbox, ToolDoneEvent,
};
use maki_config::{ProjectConfig, ToolOutputLines};
use maki_lua::{
    MoveOutcome, Permission, PluginHost, PluginPermissions, RestoreItem, RestoreReason, UiAction,
};
use maki_providers::ContentBlock;
use maki_storage::id::MakiId;
use serde_json::{Value, json};
use tempfile::TempDir;
use test_case::test_case;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::unbounded_channel;

const PLUGIN: &str = "background_task_test";
const GUARDED_PLUGIN: &str = "guarded_tool_test";
const MOVER_PLUGIN: &str = "self_mover_test";
const TASK_TOOL: &str = "task";
const MOVER_TOOL: &str = "self_mover";
const DEADLINE_MOVER_TOOL: &str = "deadline_mover";
const CALL_ID: &str = "toolu_background_task";
const SECOND_CALL_ID: &str = "toolu_background_task_second";
const GUARDED_CALL_ID: &str = "toolu_guarded";
const GUARDED_TOOL: &str = "guarded";
const NAME: &str = "find the answer";
const CODENAME: &str = "Hawk";
const PROMPT: &str = "Find the answer";
const RESULT: &str = "background answer";
const MOVED_RECEIPT: &str = "Subagent Hawk";
const MOVER_RECEIPT: &str = "mover moved";
const MOVER_FINAL: &str = "true: nil";
const DEFAULT_SLOTS: u64 = 8;
const MESSAGE_TOOL: &str = "task_message";
const STOP_TOOL: &str = "task_stop";
const MESSAGE_CALL_ID: &str = "toolu_message";
const MESSAGE: &str = "the user added an IOC, check it too";
const QUEUED: &str = "Message queued for Hawk.";
const INTERRUPTED: &str = "Interrupted Hawk with the message.";
const NOT_RUNNING: &str = "no running background subagent with id";
const INTERRUPT_OPEN: &str = "<parent-interrupt>";
const INIT: &str = include_str!("../../plugins/task/init.lua");
const RUNNER: &str = include_str!("../../plugins/task/runner.lua");
const BATCH: &str = include_str!("../../plugins/batch/init.lua");
const BATCH_PLUGIN: &str = "batch";
const BATCH_TOOL: &str = "batch";
const SECOND_NAME: &str = "check the logs";
const SECOND_CODENAME: &str = "Nyx";
const LISTED_TASK: &str = "- task (task ";
const NAMED_SECTION: &str = "## task (task ";
const NESTED_STOPPED: &str = "Stopped. What it did so far arrives later as the result of task ";

// Mirrors of the runtime's wording.
const MOVED_AS_ASKED: &str = "Started in the background as asked";
const AGENT_DENIED: &str = "permission denied: 'agent' not granted";
const TIMED_OUT: &str = "timed out after";
const UNATTENDED: &str = "no one can answer a permission prompt";
const SUBAGENT_SELF_MOVE: &str =
    "background: a subagent's tool call only moves when the user moves it";
const SUBAGENT_TASK_ID: &str = "toolu_subagent";
const NESTED_CALL: &str = "background is ignored for a call inside batch or another tool; \
     call the tool directly to run it in the background";
const NO_LIVE_CHAT: &str = "no live chat";
const BACKGROUND_OFF: &str = "background tasks are turned off here (--background-timeout 0)";

/// A tool only a subagent sees, whose every call needs the user's answer.
const GUARDED_SRC: &str = r#"
maki.api.register_tool({
  name = "guarded",
  description = "needs a yes from the user",
  schema = { type = "object", properties = { target = { type = "string" } } },
  audiences = { "research_sub" },
  permission = "run",
  permission_scopes = "target",
  handler = function()
    return "ran"
  end,
})
"#;

const MOVER_SRC: &str = r#"
maki.api.register_tool({
  name = "self_mover",
  description = "moves itself to the background",
  schema = { type = "object", properties = {} },
  audiences = { "main" },
  handler = function(_, ctx)
    ctx:on_background(function()
      return "mover moved"
    end)
    local ok, err = ctx:background()
    return tostring(ok) .. ": " .. tostring(err)
  end,
})
maki.api.register_tool({
  name = "deadline_mover",
  description = "moves itself when its deadline passes",
  schema = { type = "object", properties = {} },
  audiences = { "main" },
  handler = function(_, ctx)
    ctx:on_background(function()
      return "mover moved"
    end)
    ctx:set_deadline(0, { expire = "background" })
    maki.async.await(1, function() end)
  end,
})
"#;

struct Host {
    reg: Arc<ToolRegistry>,
    host: PluginHost,
    ui: UnboundedReceiver<UiAction>,
}

impl Host {
    fn attached(reg: Arc<ToolRegistry>, mut host: PluginHost) -> Self {
        let ui = host.take_ui_action_rx().unwrap();
        Self { reg, host, ui }
    }

    /// The real task plugin with `background` exposed.
    fn task(reg: Arc<ToolRegistry>) -> Self {
        Self::task_with_slots(reg, DEFAULT_SLOTS)
    }

    fn task_with_slots(reg: Arc<ToolRegistry>, slots: u64) -> Self {
        Self::task_with_opts(reg, true, slots)
    }

    fn task_with_opts(reg: Arc<ToolRegistry>, allow_background: bool, slots: u64) -> Self {
        let host = PluginHost::new(Arc::clone(&reg)).unwrap();
        host.load_source_with_opts(
            PLUGIN,
            &task_source(),
            serde_json::Map::from_iter([
                ("allow_background".into(), json!(allow_background)),
                ("max_concurrent".into(), json!(slots)),
            ]),
        )
        .unwrap();
        Self::attached(reg, host)
    }

    fn mover(agent: bool) -> Self {
        let reg = Arc::new(ToolRegistry::new());
        let host = PluginHost::new(Arc::clone(&reg)).unwrap();
        let mut permissions = PluginPermissions::trusted();
        permissions.set(Permission::Agent, agent);
        host.load_source_with_permissions(MOVER_PLUGIN, MOVER_SRC, permissions)
            .unwrap();
        Self::attached(reg, host)
    }

    /// The final reply of the moved call, as its task tab gets it.
    fn task_done(&mut self) -> ToolDoneEvent {
        self.task_dones(&[CALL_ID]).remove(0)
    }

    /// The final replies of {ids}, in that order, whichever ends first.
    fn task_dones(&mut self, ids: &[&str]) -> Vec<ToolDoneEvent> {
        let mut found: Vec<Option<ToolDoneEvent>> = ids.iter().map(|_| None).collect();
        while found.iter().any(Option::is_none) {
            let envelope = next_task_event(&mut self.ui);
            if let (AgentEvent::ToolDone(done), None) = (envelope.event, envelope.subagent)
                && let Some(slot) = ids.iter().position(|id| *id == done.id)
            {
                found[slot] = Some(*done);
            }
        }
        found.into_iter().flatten().collect()
    }

    /// Waits for the moved subagent to ask the user, failing if it ends first.
    fn wait_permission_request(&mut self) {
        loop {
            let envelope = next_task_event(&mut self.ui);
            match (envelope.event, envelope.subagent) {
                (AgentEvent::PermissionRequest { .. }, _) => return,
                (AgentEvent::ToolDone(done), None) if done.id == CALL_ID => {
                    panic!("the task ended without asking: {}", done.output.as_text())
                }
                _ => {}
            }
        }
    }
}

fn task_source() -> String {
    format!(
        r#"
local builtin_require = require
local runner
local function require(name)
  if name == "runner" then
    if runner == nil then
      runner = (function()
{RUNNER}
      end)()
    end
    return runner
  end
  return builtin_require(name)
end
maki.agent.resolve_model = function() return {{}} end
{INIT}
"#
    )
}

fn task_input() -> Value {
    task_input_named(CODENAME)
}

fn task_input_named(codename: &str) -> Value {
    json!({ "name": codename, "description": NAME, "prompt": PROMPT, "background": true })
}

/// Sends {MESSAGE} to task {id} as a model call of chat {chat}.
fn message(host: &Host, chat: MakiId, id: &str, interrupt: bool) -> (String, bool) {
    let (_run, run_token) = CancelToken::new();
    let (ctx, _events) = call_ctx(&host.reg, chat, &run_token, MESSAGE_CALL_ID);
    done_text(&exec(
        &host.reg,
        MESSAGE_TOOL,
        json!({ "id": id, "message": MESSAGE, "interrupt": interrupt }),
        &ctx,
    ))
}

fn assert_refused(host: &Host, chat: MakiId, id: &str) {
    let (output, failed) = message(host, chat, id, false);
    assert!(failed && output.starts_with(NOT_RUNNING), "{output}");
}

/// The provider stays parked until released, so the receipt coming back at
/// all means the call did not wait for its subagent.
#[test]
fn a_background_task_returns_a_receipt_and_its_reply_arrives_as_the_notice() {
    let mut host = Host::task(Arc::new(ToolRegistry::new()));
    let chat = Chat::live();
    let parked = Parked::new(RESULT);
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    ctx.provider = Arc::clone(&parked.provider);

    let result = exec(&host.reg, TASK_TOOL, task_input(), &ctx);

    assert!(result.backgrounded);
    let (receipt, failed) = done_text(&result);
    assert!(!failed, "{receipt}");
    assert!(receipt.starts_with(MOVED_RECEIPT), "{receipt}");
    assert!(receipt.contains(MOVED_AS_ASKED), "{receipt}");

    parked.wait_started();
    parked.release.send(()).unwrap();
    assert_eq!(done_text(&host.task_done()), (RESULT.to_owned(), false));
    let notices = chat.mailbox.drain();
    assert_eq!(notices.len(), 1);
    let notice = notices[0].first_text_content().unwrap();
    assert!(notice.contains(RESULT), "{notice}");
}

/// A task with no `background` input moves by default: a foreground task
/// holds the model's turn until it finishes, which is almost never what a
/// delegation wants.
#[test]
fn a_task_without_a_background_input_moves_by_default() {
    let mut host = Host::task(Arc::new(ToolRegistry::new()));
    let chat = Chat::live();
    let parked = Parked::new(RESULT);
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    ctx.provider = Arc::clone(&parked.provider);
    let input = json!({ "name": CODENAME, "description": NAME, "prompt": PROMPT });

    let result = exec(&host.reg, TASK_TOOL, input, &ctx);

    assert!(result.backgrounded);
    let (receipt, failed) = done_text(&result);
    assert!(!failed, "{receipt}");
    assert!(receipt.starts_with(MOVED_RECEIPT), "{receipt}");

    parked.wait_started();
    parked.release.send(()).unwrap();
    assert_eq!(done_text(&host.task_done()), (RESULT.to_owned(), false));
}

/// The move comes before the wait for a slot, so a full house does not hold
/// the model's turn. The second task keeps waiting in the background.
#[test]
fn a_background_task_waiting_for_a_slot_returns_its_receipt_at_once() {
    let mut host = Host::task_with_slots(Arc::new(ToolRegistry::new()), 1);
    let chat = Chat::live();
    let parked = Parked::new(RESULT);
    let (_run, run_token) = CancelToken::new();
    let (mut first, _first_events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    first.provider = Arc::clone(&parked.provider);
    let (mut second, _second_events) = call_ctx(&host.reg, chat.id, &run_token, SECOND_CALL_ID);
    second.provider = Arc::clone(&parked.provider);

    assert!(exec(&host.reg, TASK_TOOL, task_input(), &first).backgrounded);
    parked.wait_started();

    let reg = Arc::clone(&host.reg);
    let (receipt_tx, receipt_rx) = sync_channel(1);
    let waiter = thread::spawn(move || {
        receipt_tx
            .send(exec(&reg, TASK_TOOL, task_input_named(SECOND_CODENAME), &second))
            .ok();
    });
    let result = receipt_rx
        .recv_timeout(WAIT)
        .expect("the receipt must not wait for a free slot");
    waiter.join().expect("waiter thread");

    assert!(result.backgrounded);
    let (receipt, failed) = done_text(&result);
    assert!(!failed, "{receipt}");
    assert!(receipt.starts_with(&format!("Subagent {SECOND_CODENAME}")), "{receipt}");
    assert!(parked.started.try_recv().is_err(), "the slot is still held");

    parked.release.send(()).unwrap();
    parked.release.send(()).unwrap();
    for done in host.task_dones(&[CALL_ID, SECOND_CALL_ID]) {
        assert_eq!(done_text(&done), (RESULT.to_owned(), false));
    }
}

/// A ctx session has its own answer channel, so its prompt reaches the user.
/// One opened inside another subagent is denied instead, because the answer
/// would go to the outer one. The subagent dispatches through the global
/// registry, so the tools live there.
#[test_case(false ; "top_level_session_asks_the_user")]
#[test_case(true ; "nested_session_is_denied_as_unattended")]
fn a_subagent_asks_for_permission_only_where_the_answer_comes_back(nested: bool) {
    let reg = Arc::clone(ToolRegistry::global_arc());
    let mut host = Host::task(Arc::clone(&reg));
    host.host.load_source(GUARDED_PLUGIN, GUARDED_SRC).unwrap();
    let chat = Chat::live();
    let project = TempDir::new().unwrap();
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, mut events) = call_ctx(&reg, chat.id, &run_token, CALL_ID);
    // The rest of `Parked` drops here, and without a release sender no turn
    // waits.
    ctx.provider = Parked::with_first_turn(
        RESULT,
        Some(ContentBlock::tool_use(
            GUARDED_CALL_ID,
            GUARDED_TOOL,
            json!({ "target": "x" }),
        )),
    )
    .provider;
    ctx.permissions = Arc::new(PermissionManager::new(
        PermissionsConfig::default(),
        project.path().to_owned(),
        ProjectConfig::discover(project.path()),
        Arc::default(),
    ));
    let (_answer_tx, answer_rx) = unbounded_channel::<String>();
    ctx.user_response_rx = Some(Arc::new(tokio::sync::Mutex::new(answer_rx)));
    if nested {
        ctx.task_id = Some(Arc::from(SUBAGENT_TASK_ID));
    }

    let result = exec(&reg, TASK_TOOL, task_input(), &ctx);

    if nested {
        let guarded = next_event(&mut events, |event| match event {
            AgentEvent::ToolDone(done) if done.id == GUARDED_CALL_ID => Some(done),
            _ => None,
        });
        let (reply, failed) = done_text(&guarded);
        assert!(failed && reply.contains(UNATTENDED), "{reply}");
    } else {
        assert!(result.backgrounded);
        host.wait_permission_request();
        host.host.event_handle().cancel_plugin_session(CALL_ID);
        host.task_done();
    }
}

/// Moving itself makes the result wake the chat, the price of
/// `notify{ wake = true }`, so it takes the same `agent` grant.
#[test_case(true ; "with_agent_it_moves")]
#[test_case(false ; "without_agent_it_stays")]
fn ctx_background_needs_the_agent_permission(agent: bool) {
    let mut host = Host::mover(agent);
    let chat = Chat::live();
    let (_run, run_token) = CancelToken::new();
    let (ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);

    let result = exec(&host.reg, MOVER_TOOL, json!({}), &ctx);

    assert_eq!(result.backgrounded, agent);
    let (reply, failed) = done_text(&result);
    assert!(!failed, "{reply}");
    if agent {
        assert!(reply.starts_with(MOVER_RECEIPT), "{reply}");
        assert_eq!(
            done_text(&host.task_done()),
            (MOVER_FINAL.to_owned(), false)
        );
    } else {
        assert!(reply.contains(AGENT_DENIED), "{reply}");
        assert!(chat.mailbox.drain().is_empty());
    }
}

/// A deadline that expires to the background is a self-move all the same.
#[test_case(true ; "with_agent_it_moves")]
#[test_case(false ; "without_agent_it_times_out")]
fn a_background_deadline_needs_the_agent_permission(agent: bool) {
    let host = Host::mover(agent);
    let chat = Chat::live();
    let (_run, run_token) = CancelToken::new();
    let (ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);

    let result = exec(&host.reg, DEADLINE_MOVER_TOOL, json!({}), &ctx);

    assert_eq!(result.backgrounded, agent);
    let (reply, failed) = done_text(&result);
    if agent {
        assert!(reply.starts_with(MOVER_RECEIPT), "{reply}");
    } else {
        assert!(failed && reply.contains(TIMED_OUT), "{reply}");
        assert!(chat.mailbox.unsettled().is_empty());
    }
}

/// A subagent waits for its moved calls, so only the user moves one.
#[test]
fn ctx_background_refuses_a_subagents_call() {
    let host = Host::mover(true);
    let chat = Chat::live();
    let subagent = SessionMailbox::register(MakiId::generate());
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    ctx.task_id = Some(Arc::from(SUBAGENT_TASK_ID));
    ctx.mailbox = Some(subagent.clone());

    let result = exec(&host.reg, MOVER_TOOL, json!({}), &ctx);

    assert!(!result.backgrounded);
    assert_eq!(
        done_text(&result),
        (format!("nil: {SUBAGENT_SELF_MOVE}"), false)
    );
    assert!(subagent.unsettled().is_empty());
}

/// A call nested in batch shares the batch's scope but has no id of its own,
/// and the model has to learn to make it a direct call instead.
#[test]
fn ctx_background_refuses_a_call_nested_in_another_tool() {
    let host = Host::mover(true);
    let chat = Chat::live();
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    ctx.tool_use_id = None;

    let result = exec(&host.reg, MOVER_TOOL, json!({}), &ctx);

    assert!(!result.backgrounded);
    assert_eq!(done_text(&result), (format!("nil: {NESTED_CALL}"), false));
}

/// A session that is up without a seed runs with background work off, which
/// the model must not read as a missing chat.
#[test_case(false, NO_LIVE_CHAT ; "no_live_chat")]
#[test_case(true, BACKGROUND_OFF ; "background_off")]
fn ctx_background_without_a_seed_names_the_cause(session_up: bool, reason: &str) {
    let host = Host::mover(true);
    let chat = MakiId::generate();
    let _mailbox = session_up.then(|| SessionMailbox::register(chat));
    let (_run, run_token) = CancelToken::new();
    let (ctx, _events) = call_ctx(&host.reg, chat, &run_token, CALL_ID);

    let result = exec(&host.reg, MOVER_TOOL, json!({}), &ctx);

    assert!(!result.backgrounded);
    assert_eq!(done_text(&result), (format!("nil: {reason}"), false));
}

/// The receipt names the id `task_message` takes, and the subagent reads the
/// message as an interrupt from its parent once its current turn ends.
#[test]
fn a_message_reaches_a_running_background_task_at_its_next_turn() {
    let mut host = Host::task(Arc::new(ToolRegistry::new()));
    let chat = Chat::live();
    let parked = Parked::new(RESULT);
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    ctx.provider = Arc::clone(&parked.provider);

    let (receipt, _) = done_text(&exec(&host.reg, TASK_TOOL, task_input(), &ctx));
    assert!(receipt.contains(CALL_ID), "{receipt}");
    parked.wait_started();
    assert_eq!(parked.user_turns.recv().unwrap(), [PROMPT]);

    assert_eq!(
        message(&host, chat.id, CALL_ID, false),
        (QUEUED.to_owned(), false)
    );
    parked.release.send(()).unwrap();
    parked.wait_started();
    let [interrupt] = &parked.user_turns.recv().unwrap()[..] else {
        panic!("the message should land alone");
    };
    assert!(
        interrupt.starts_with(INTERRUPT_OPEN) && interrupt.contains(MESSAGE),
        "{interrupt}"
    );
    parked.release.send(()).unwrap();
    assert_eq!(done_text(&host.task_done()), (RESULT.to_owned(), false));
    assert_refused(&host, chat.id, CALL_ID);
}

/// A task still waiting for a slot has opened its session but not prompted
/// it, so the message waits and the first request carries it just before the
/// prompt.
#[test]
fn a_message_to_a_task_waiting_for_a_slot_rides_with_its_first_prompt() {
    let mut host = Host::task_with_slots(Arc::new(ToolRegistry::new()), 1);
    let chat = Chat::live();
    let parked = Parked::new(RESULT);
    let (_run, run_token) = CancelToken::new();
    let (mut first, _first_events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    first.provider = Arc::clone(&parked.provider);
    let (mut second, _second_events) = call_ctx(&host.reg, chat.id, &run_token, SECOND_CALL_ID);
    second.provider = Arc::clone(&parked.provider);

    assert!(exec(&host.reg, TASK_TOOL, task_input(), &first).backgrounded);
    parked.wait_started();
    assert!(exec(&host.reg, TASK_TOOL, task_input_named(SECOND_CODENAME), &second).backgrounded);

    assert_eq!(
        message(&host, chat.id, SECOND_CALL_ID, false),
        (format!("Message queued for {SECOND_CODENAME}."), false)
    );
    parked.release.send(()).unwrap();
    parked.wait_started();
    parked.release.send(()).unwrap();
    let turns: Vec<_> = parked.user_turns.try_iter().collect();
    assert_eq!(turns, [vec![PROMPT], vec![MESSAGE, PROMPT]]);
    host.task_dones(&[CALL_ID, SECOND_CALL_ID]);
}

/// An interrupt does not wait for the turn to end: the parked reply is cut,
/// and the same prompt goes on with the message.
#[test]
fn an_interrupt_cuts_a_background_tasks_turn_and_it_goes_on() {
    let mut host = Host::task(Arc::new(ToolRegistry::new()));
    let chat = Chat::live();
    let parked = Parked::new(RESULT);
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    ctx.provider = Arc::clone(&parked.provider);

    assert!(exec(&host.reg, TASK_TOOL, task_input(), &ctx).backgrounded);
    parked.wait_started();
    assert_eq!(parked.user_turns.recv().unwrap(), [PROMPT]);

    assert_eq!(
        message(&host, chat.id, CALL_ID, true),
        (INTERRUPTED.to_owned(), false)
    );
    parked.wait_started();
    let turn = parked.user_turns.recv().unwrap();
    let interrupt = turn.last().expect("the request closes on the interrupt");
    assert!(
        interrupt.starts_with(INTERRUPT_OPEN) && interrupt.contains(MESSAGE),
        "{interrupt}"
    );
    parked.release.send(()).unwrap();
    assert_eq!(done_text(&host.task_done()), (RESULT.to_owned(), false));
}

/// A user stop does not wait for the task to end, and the message must not
/// queue for a prompt that is already unwinding.
#[test]
fn a_message_is_refused_by_a_task_the_user_just_stopped() {
    let mut host = Host::task(Arc::new(ToolRegistry::new()));
    let chat = Chat::live();
    let parked = Parked::new(RESULT);
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    ctx.provider = Arc::clone(&parked.provider);

    assert!(exec(&host.reg, TASK_TOOL, task_input(), &ctx).backgrounded);
    parked.wait_started();

    host.host.event_handle().cancel_plugin_session(CALL_ID);
    assert_refused(&host, chat.id, CALL_ID);
    host.task_done();
}

/// Only the chat that started a task reaches it.
#[test]
fn a_message_is_refused_by_tasks_the_caller_cannot_reach() {
    let host = Host::task(Arc::new(ToolRegistry::new()));
    let chat = Chat::live();
    let other = Chat::live();
    let parked = Parked::new(RESULT);
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    ctx.provider = Arc::clone(&parked.provider);

    assert!(exec(&host.reg, TASK_TOOL, task_input(), &ctx).backgrounded);
    parked.wait_started();

    assert_refused(&host, other.id, CALL_ID);
    assert_refused(&host, chat.id, SECOND_CALL_ID);
    let mut agent = chat.seed.tool_context(CancelToken::none());
    agent.mailbox = Some(chat.mailbox.clone());
    exec(&host.reg, STOP_TOOL, json!({ "id": CALL_ID }), &agent);
    assert_refused(&host, chat.id, CALL_ID);
}

/// Ctrl+B still moves a blocking task with `allow_background` off. The
/// parent's steering inputs stay hidden, while a subagent's report needs
/// no background delegation, so the name stays registered.
#[test]
fn a_task_moved_without_allow_background_has_no_steering_inputs_and_still_reports() {
    let mut host = Host::task_with_opts(Arc::new(ToolRegistry::new()), false, DEFAULT_SLOTS);
    let entry = host
        .reg
        .get(MESSAGE_TOOL)
        .expect("task_message stays registered");
    let gated = entry
        .tool
        .offered_schema(false)
        .expect("a subagent still sees the tool");
    for param in ["id", "interrupt"] {
        assert!(
            gated["properties"].get(param).is_none(),
            "{param} must stay gated"
        );
    }
    assert!(gated["properties"].get("message").is_some());
    let chat = Chat::live();
    let parked = Parked::new(RESULT);
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    ctx.provider = Arc::clone(&parked.provider);

    let reg = Arc::clone(&host.reg);
    let call = thread::spawn(move || {
        exec(
            &reg,
            TASK_TOOL,
            json!({ "name": CODENAME, "description": NAME, "prompt": PROMPT }),
            &ctx,
        )
    });
    parked.wait_started();
    assert_eq!(
        request_move(&host.host.event_handle(), CALL_ID),
        MoveOutcome::Moved
    );
    let result = call.join().expect("task call");
    assert!(result.backgrounded);
    assert!(done_text(&result).0.starts_with(MOVED_RECEIPT));

    parked.release.send(()).unwrap();
    assert_eq!(done_text(&host.task_done()), (RESULT.to_owned(), false));
}

/// The model sent its background tasks out in one batch, and the batch moved
/// as one call. The receipt lists each subagent's id. The batch's id reaches
/// all of them, a subagent's own id reaches just that one, and stopping one
/// leaves the rest and the batch running. Each result, early or in the final
/// reply, names the id the receipt gave it.
#[test]
fn the_tasks_of_a_moved_batch_are_reached_by_the_batchs_id_or_their_own() {
    let mut host = Host::task(Arc::new(ToolRegistry::new()));
    host.host
        .load_source_with_permissions(BATCH_PLUGIN, BATCH, PluginPermissions::trusted())
        .unwrap();
    let chat = Chat::live();
    let parked = Parked::new(RESULT);
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    ctx.provider = Arc::clone(&parked.provider);
    let tasks = [(NAME, CODENAME), (SECOND_NAME, SECOND_CODENAME)].map(|(desc, codename)| {
        json!({
            "tool": TASK_TOOL,
            "parameters": { "name": codename, "description": desc, "prompt": PROMPT, "background": true },
        })
    });

    let result = exec(&host.reg, BATCH_TOOL, json!({ "tool_calls": tasks }), &ctx);

    assert!(result.backgrounded);
    let (receipt, _) = done_text(&result);
    let ids: Vec<&str> = receipt
        .lines()
        .filter_map(|line| line.strip_prefix(LISTED_TASK)?.split_once(')'))
        .map(|(id, _)| id)
        .collect();
    let [first, second] = ids[..] else {
        panic!("the receipt lists both tasks:\n{receipt}");
    };
    for _ in &tasks {
        parked.wait_started();
    }

    let (sent, failed) = message(&host, chat.id, CALL_ID, false);
    assert!(
        !failed && sent.contains(CODENAME) && sent.contains(SECOND_CODENAME),
        "{sent}"
    );
    let mut agent = chat.seed.tool_context(CancelToken::none());
    agent.mailbox = Some(chat.mailbox.clone());
    assert_eq!(
        done_text(&exec(&host.reg, STOP_TOOL, json!({ "id": second }), &agent)),
        (format!("{NESTED_STOPPED}{second}."), false)
    );
    assert_refused(&host, chat.id, second);
    let notices = poll_until("the stopped subagent never reported", || {
        Some(chat.mailbox.claim_wake()).filter(|notices| !notices.is_empty())
    });
    let [notice] = &notices[..] else {
        panic!("one part notice: {notices:?}");
    };
    let notice = notice.first_text_content().unwrap();
    assert!(
        notice.contains(second) && !notice.contains(CALL_ID),
        "the part is named by the id the receipt gave it: {notice}"
    );
    assert_eq!(
        message(&host, chat.id, first, false),
        (QUEUED.to_owned(), false)
    );

    // Dropping the release sender lets every turn finish, however many the
    // queued messages add.
    drop(parked);
    let (last, _) = done_text(&host.task_done());
    assert!(
        last.contains(&format!("{NAMED_SECTION}{first})")),
        "the last subagent's section names its id: {last}"
    );
}

const REPORT: &str = "the IOC list is stale, need a fresh source";
const REPORT_DELIVERED: &str = "Report delivered to the parent agent.";
const NO_PARENT_SESSION: &str = "no parent session to report to";

/// A subagent's task_message goes the other way: the report lands in the
/// parent chat's mailbox as a waking observation named by the subagent's id.
#[test_case(ToolAudience::RESEARCH_SUB ; "research_sub")]
#[test_case(ToolAudience::GENERAL_SUB ; "general_sub")]
fn a_subagents_message_reports_to_the_parent_chat(audience: ToolAudience) {
    let host = Host::task(Arc::new(ToolRegistry::new()));
    let chat = Chat::live();
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, SUBAGENT_TASK_ID);
    ctx.audience = audience;
    ctx.task_id = Some(Arc::from(SUBAGENT_TASK_ID));
    ctx.task_name = Some(Arc::from(CODENAME));

    let (reply, failed) = done_text(&exec(
        &host.reg,
        MESSAGE_TOOL,
        json!({ "message": REPORT }),
        &ctx,
    ));

    assert!(!failed, "{reply}");
    assert_eq!(reply, REPORT_DELIVERED);
    let notices = chat.mailbox.claim_wake();
    let [notice] = &notices[..] else {
        panic!("one report: {notices:?}");
    };
    let body = notice.first_text_content().unwrap();
    assert!(body.contains(CODENAME) && body.contains(REPORT), "{body}");
    assert_eq!(
        notice.display_text.as_deref(),
        Some(format!("{CODENAME} reported: {REPORT}").as_str())
    );
}

/// Without a session there is no parent to report to.
#[test]
fn a_subagents_message_without_a_session_is_refused() {
    let host = Host::task(Arc::new(ToolRegistry::new()));
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, MakiId::generate(), &run_token, SUBAGENT_TASK_ID);
    ctx.audience = ToolAudience::GENERAL_SUB;
    ctx.session_id = None;

    let (reply, failed) = done_text(&exec(
        &host.reg,
        MESSAGE_TOOL,
        json!({ "message": REPORT }),
        &ctx,
    ));

    assert!(failed && reply.contains(NO_PARENT_SESSION), "{reply}");
}

const MESSAGE_REASON: &str = "steering the subagent";
const SEND_ARROW: &str = "→ ";
const PARENT_RECIPIENT: &str = "parent";
const REASON_SEP: &str = " · ";
const RESTORE_CALL_ID: &str = "toolu_message_restore";
const NO_HEADER: &str = "the call never named its recipient";

fn header_snapshot(events: &mut UnboundedReceiver<Envelope>) -> String {
    next_event(events, |event| match event {
        AgentEvent::ToolHeaderSnapshot { snapshot, .. } => Some(snapshot.text()),
        _ => None,
    })
}

/// The header names who the message went to, whichever handle addressed the
/// task, and a reason replaces the message text without hiding the recipient.
#[test_case(CODENAME, None, format!("{SEND_ARROW}{CODENAME}: {MESSAGE}") ; "by_codename")]
#[test_case(CALL_ID, None, format!("{SEND_ARROW}{CODENAME}: {MESSAGE}") ; "by_call_id_resolves_the_codename")]
#[test_case(CODENAME, Some(MESSAGE_REASON), format!("{SEND_ARROW}{CODENAME}{REASON_SEP}{MESSAGE_REASON}") ; "with_a_reason")]
fn a_message_header_names_its_recipient(id: &str, reason: Option<&str>, expected: String) {
    let mut host = Host::task(Arc::new(ToolRegistry::new()));
    let chat = Chat::live();
    let parked = Parked::new(RESULT);
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    ctx.provider = Arc::clone(&parked.provider);

    assert!(exec(&host.reg, TASK_TOOL, task_input(), &ctx).backgrounded);
    parked.wait_started();

    let (mut msg_ctx, mut events) = call_ctx(&host.reg, chat.id, &run_token, MESSAGE_CALL_ID);
    msg_ctx.provider = Arc::clone(&parked.provider);
    let mut input = json!({ "id": id, "message": MESSAGE });
    if let Some(reason) = reason {
        input["toolcall_reason"] = json!(reason);
    }
    let (reply, failed) = done_text(&exec(&host.reg, MESSAGE_TOOL, input, &msg_ctx));
    assert!(!failed, "{reply}");

    assert_eq!(header_snapshot(&mut events), expected);

    drop(parked);
    host.task_done();
}

/// A report's header names the parent as the recipient.
#[test]
fn a_report_header_names_the_parent() {
    let host = Host::task(Arc::new(ToolRegistry::new()));
    let chat = Chat::live();
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, mut events) = call_ctx(&host.reg, chat.id, &run_token, SUBAGENT_TASK_ID);
    ctx.audience = ToolAudience::GENERAL_SUB;
    ctx.task_id = Some(Arc::from(SUBAGENT_TASK_ID));
    ctx.task_name = Some(Arc::from(CODENAME));

    let (reply, failed) = done_text(&exec(
        &host.reg,
        MESSAGE_TOOL,
        json!({ "message": REPORT }),
        &ctx,
    ));
    assert!(!failed, "{reply}");

    assert_eq!(
        header_snapshot(&mut events),
        format!("{SEND_ARROW}{PARENT_RECIPIENT}: {REPORT}")
    );
}

/// The recipient survives a restore through the state the handler saved; an
/// old call without it falls back to the id the input still carries.
#[test_case(Some(json!({ "name": CODENAME })), format!("{SEND_ARROW}{CODENAME}: {MESSAGE}") ; "with_saved_name")]
#[test_case(None, format!("{SEND_ARROW}{CALL_ID}: {MESSAGE}") ; "without_state_falls_back_to_the_id")]
fn restore_shows_the_recipient_as_the_header(state: Option<Value>, expected: String) {
    let host = Host::task(Arc::new(ToolRegistry::new()));
    let (tx, mut rx) = unbounded_channel();
    let item = RestoreItem {
        tool: Arc::from(MESSAGE_TOOL),
        tool_use_id: RESTORE_CALL_ID.to_owned(),
        output: QUEUED.to_owned(),
        input: json!({ "id": CALL_ID, "message": MESSAGE }),
        is_error: false,
        tool_output_lines: ToolOutputLines::DEFAULT,
        theme_gen: None,
        clicks: Vec::new(),
        state,
        session_id: None,
        task_id: None,
        reason: RestoreReason::default(),
    };

    let handle = host.host.event_handle();
    handle.request_restore(item, EventSender::new(tx, 0));
    handle.wait_restore_complete_for_test();

    let header = iter::from_fn(|| rx.try_recv().ok())
        .find_map(|env| match env.event {
            AgentEvent::ToolHeaderSnapshot { snapshot, .. } => Some(snapshot.text()),
            _ => None,
        })
        .expect(NO_HEADER);
    assert_eq!(header, expected);
}

/// One name, two schemas: the parent keeps id and interrupt, a subagent's
/// array offers only the message, and each audience reads its own description.
#[test]
fn task_message_offers_each_audience_its_own_direction() {
    let host = Host::task(Arc::new(ToolRegistry::new()));
    let entry = host.reg.get(MESSAGE_TOOL).expect("task_message registered");
    let parent = entry
        .tool
        .offered_schema(true)
        .expect("the parent sees the tool");
    assert!(parent["properties"].get("id").is_some());
    assert!(parent["properties"].get("interrupt").is_some());
    let sub = entry
        .tool
        .offered_schema(false)
        .expect("a subagent sees the tool");
    assert!(sub["properties"].get("id").is_none());
    assert!(sub["properties"].get("interrupt").is_none());
    assert!(sub["properties"].get("message").is_some());
}

/// A codename is the handle the model uses: `task_message` and `task_stop`
/// reach a task by it just as by its call id.
#[test]
fn a_task_is_reached_by_its_codename() {
    let mut host = Host::task(Arc::new(ToolRegistry::new()));
    let chat = Chat::live();
    let parked = Parked::new(RESULT);
    let (_run, run_token) = CancelToken::new();
    let (mut ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    ctx.provider = Arc::clone(&parked.provider);

    assert!(exec(&host.reg, TASK_TOOL, task_input(), &ctx).backgrounded);
    parked.wait_started();

    assert_eq!(
        message(&host, chat.id, CODENAME, false),
        (QUEUED.to_owned(), false)
    );

    let mut agent = chat.seed.tool_context(CancelToken::none());
    agent.mailbox = Some(chat.mailbox.clone());
    let stopped = exec(&host.reg, STOP_TOOL, json!({ "id": CODENAME }), &agent);
    assert!(!stopped.is_error, "{}", stopped.output.as_text());

    drop(parked);
    host.task_done();
}

/// A blank or oversized codename is refused before any session opens.
#[test]
fn a_bad_codename_is_refused() {
    let host = Host::task(Arc::new(ToolRegistry::new()));
    let chat = Chat::live();
    let (_run, run_token) = CancelToken::new();
    let (ctx, _events) = call_ctx(&host.reg, chat.id, &run_token, CALL_ID);
    for bad in ["", "   ", "has\nnewline", &"x".repeat(41)] {
        let input = json!({ "name": bad, "description": NAME, "prompt": PROMPT });
        let result = exec(&host.reg, TASK_TOOL, input, &ctx);
        assert!(result.is_error, "codename {bad:?} must be refused");
    }
}
