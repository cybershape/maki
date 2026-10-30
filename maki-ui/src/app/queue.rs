//! Queue for messages typed while the agent is busy, and the subagent
//! inbox a message typed in a subagent's chat goes to instead.

use std::borrow::Cow;

use maki_agent::{AgentInput, AgentMode, ImageSource, InputSource};
use maki_lua::agent_autocmd::HostEvent;

use super::mode::{Mode, PlanTrigger};
use super::{Action, App, InputWriter, Status};

use crate::agent::shared_queue::{Compaction, QueueItem, QueueSender, QueuedInput};
use crate::components::queue_panel::QueueEntry;
use crate::theme;

pub(crate) use crate::agent::shared_queue::QueuedMessage;

pub(crate) const EMPTY_PROMPT_ERR: &str = "prompt is empty";
pub(crate) const NO_QUEUE_ERR: &str = "session cannot queue messages";
pub(crate) const NO_INBOX_ERR: &str = "subagent is not taking messages";

pub(crate) enum SubmitOutcome {
    Started(Vec<Action>),
    Queued,
    Rejected(&'static str),
}

#[derive(Default)]
pub(crate) struct MessageQueue {
    shared: Option<QueueSender>,
    focus: Option<usize>,
}

impl MessageQueue {
    pub(crate) fn set_shared(&mut self, shared: QueueSender) {
        self.shared = Some(shared);
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.shared.as_ref().is_none_or(|s| s.is_empty())
    }

    pub(crate) fn len(&self) -> usize {
        self.shared.as_ref().map_or(0, |s| s.len())
    }

    pub(crate) fn remove(&mut self, index: usize) -> Option<QueueItem> {
        let item = self.shared.as_ref().and_then(|s| s.remove(index));
        if item.is_some() {
            self.clamp_focus();
        }
        item
    }

    pub(crate) fn convert_to_steer(&mut self, index: usize) -> bool {
        self.shared
            .as_ref()
            .is_some_and(|s| s.convert_to_steer(index))
    }

    pub(crate) fn convert_to_queue(&mut self, index: usize) -> bool {
        self.shared
            .as_ref()
            .is_some_and(|s| s.convert_to_queue(index))
    }

    pub(crate) fn clear(&mut self) {
        if let Some(ref shared) = self.shared {
            shared.clear();
        }
        self.focus = None;
    }

    pub(crate) fn focus(&self) -> Option<usize> {
        self.focus
    }

    pub(crate) fn set_focus(&mut self) {
        self.set_focus_at(0);
    }

    pub(crate) fn unfocus(&mut self) {
        self.focus = None;
    }

    pub(crate) fn move_focus_up(&mut self) {
        if let Some(sel) = self.focus
            && sel > 0
        {
            self.focus = Some(sel - 1);
        }
    }

    pub(crate) fn move_focus_down(&mut self) {
        if let Some(sel) = self.focus {
            let len = self.len();
            if sel + 1 < len {
                self.focus = Some(sel + 1);
            }
        }
    }

    pub(crate) fn remove_focused(&mut self) {
        if let Some(sel) = self.focus {
            self.remove(sel);
        }
    }

    pub(crate) fn panel_len(&self) -> usize {
        self.shared.as_ref().map_or(0, |s| s.panel_len())
    }

    pub(crate) fn panel_entries(&self) -> Vec<QueueEntry<'static>> {
        self.shared.as_ref().map_or(vec![], |s| s.panel_entries())
    }

    pub(crate) fn text_messages(&self) -> Vec<String> {
        self.shared.as_ref().map_or(vec![], |s| s.text_messages())
    }

    fn clamp_focus(&mut self) {
        let len = self.len();
        self.focus = match self.focus {
            Some(_) if len == 0 => None,
            Some(sel) if sel >= len => Some(len - 1),
            other => other,
        };
    }

    pub(crate) fn set_focus_at(&mut self, index: usize) {
        if index < self.len() {
            self.focus = Some(index);
        }
    }
}

impl App {
    /// The one queue-or-start decision, shared by the keyboard and Lua
    /// paths so they cannot drift. Expects raw text: interpretation (slash
    /// commands, `exit`, `!`) is the caller's job, or skipped on purpose.
    pub(crate) fn submit_prompt(&mut self, msg: QueuedMessage) -> SubmitOutcome {
        if msg.text.trim().is_empty() && msg.images.is_empty() {
            return SubmitOutcome::Rejected(EMPTY_PROMPT_ERR);
        }
        if self.state.mode == Mode::Plan {
            self.transition_plan(PlanTrigger::InteractivePrompt);
        }
        if self.status == Status::Streaming {
            if self.queue_and_notify(msg) {
                SubmitOutcome::Queued
            } else {
                SubmitOutcome::Rejected(NO_QUEUE_ERR)
            }
        } else {
            SubmitOutcome::Started(self.start_from_queue(&msg))
        }
    }

    /// Keyboard path: nobody is around to receive an `Err`, so
    /// rejections flash on screen instead.
    pub(super) fn submit_or_queue(&mut self, msg: QueuedMessage) -> Vec<Action> {
        match self.submit_prompt(msg) {
            SubmitOutcome::Started(actions) => actions,
            SubmitOutcome::Queued => vec![],
            SubmitOutcome::Rejected(e) => {
                self.flash(e.into());
                vec![]
            }
        }
    }

    /// A message typed in a subagent's chat is for that subagent: it waits in
    /// the subagent's inbox until its loop reaches a turn boundary, and the
    /// `QueueItemConsumed` relayed from there draws it in this chat. The
    /// subagent runs its own settings, so the input carries them.
    pub(super) fn queue_for_subagent(&mut self, msg: QueuedMessage) -> Vec<Action> {
        let chat = &self.chats[self.active_chat];
        let Some(inbox) = chat.inbox.as_ref() else {
            self.flash(NO_INBOX_ERR.into());
            return vec![];
        };
        let (thinking, fast) = chat
            .opts
            .map_or((self.state.thinking, self.state.fast), |o| {
                (o.thinking, o.fast)
            });
        inbox.push(AgentInput {
            message: msg.text,
            mode: AgentMode::Build,
            images: msg.images,
            preamble: Vec::new(),
            earlier: Vec::new(),
            thinking,
            fast,
            workflow: false,
            prompt: None,
            source: InputSource::Tui,
        });
        vec![]
    }

    /// The panel above the input shows the queue of the chat in front: the
    /// session's for the main chat, the subagent's inbox otherwise.
    pub(super) fn active_queue_len(&self) -> usize {
        if self.is_main_chat() {
            return self.queue.panel_len();
        }
        self.chats[self.active_chat]
            .inbox
            .as_ref()
            .map_or(0, |inbox| inbox.len())
    }

    pub(super) fn active_queue_entries(&self) -> Vec<QueueEntry<'static>> {
        if self.is_main_chat() {
            return self.queue.panel_entries();
        }
        let color = theme::current().foreground;
        self.chats[self.active_chat]
            .inbox
            .as_ref()
            .map_or_else(Vec::new, |inbox| inbox.texts())
            .into_iter()
            .map(|text| QueueEntry {
                text: Cow::Owned(text),
                color,
                is_steer: true,
            })
            .collect()
    }

    pub(super) fn pop_active_queue(&mut self) {
        let item = if self.is_main_chat() {
            self.queue.remove(0)
        } else if let Some(ref inbox) = self.chats[self.active_chat].inbox {
            inbox.remove(0).map(|input| {
                QueueItem::Message(QueuedInput {
                    text: input.message.clone(),
                    input,
                    run_id: 0,
                    displayed: false,
                })
            })
        } else {
            None
        };

        let Some(item) = item else {
            return;
        };
        let (text, images) = match item {
            QueueItem::Message(queued) | QueueItem::Steer(queued) => {
                (queued.text, queued.input.images)
            }
            QueueItem::Compact(compaction) => {
                (compaction.instructions.unwrap_or_default(), Vec::new())
            }
        };
        self.input_box.set_input(&text);
        for img in images {
            self.input_box.attach_image(img);
        }
        self.input_box.buffer.move_to_end();
        self.input_changed(InputWriter::Anyone);
    }

    pub(super) fn convert_active_to_steer(&mut self) -> bool {
        if self.is_main_chat() {
            let idx = self.queue.focus().unwrap_or(0);
            return self.queue.convert_to_steer(idx);
        }
        false
    }

    pub(super) fn convert_active_to_queue(&mut self) -> bool {
        if self.is_main_chat() {
            let idx = self.queue.focus().unwrap_or(0);
            return self.queue.convert_to_queue(idx);
        }
        false
    }

    pub(super) fn steer_and_notify(&mut self, msg: QueuedMessage) -> bool {
        let Some(ref shared) = self.queue.shared else {
            return false;
        };
        let input = self.build_agent_input(&msg);
        shared.push(QueueItem::Steer(QueuedInput {
            text: msg.text,
            input,
            run_id: self.run_id,
            displayed: false,
        }));
        true
    }

    /// Deferred path: the agent is busy, so park the message and let
    /// `QueueItemConsumed` draw it once the agent picks it up. Returns
    /// false when there is no shared queue, meaning the message was dropped.
    pub(super) fn queue_and_notify(&mut self, msg: QueuedMessage) -> bool {
        let Some(ref shared) = self.queue.shared else {
            return false;
        };
        let input = self.build_agent_input(&msg);
        shared.push(QueueItem::Message(QueuedInput {
            text: msg.text,
            input,
            run_id: self.run_id,
            displayed: false,
        }));
        true
    }

    /// Push restored queue items only here, never in `restore_display`: on
    /// load/rewind the display is restored before `respawn` swaps the shared
    /// queue, so pushing earlier would fill a queue that is about to die.
    pub(crate) fn flush_restored_queue(&mut self) {
        // The live queue owns the text from here on, so the recovery
        // snapshot must stop overriding it on save.
        self.recoverable_queue.clear();
        // Read, not taken: the live queue is what the next checkpoint mirrors
        // back into the session, so emptying it here changes nothing on disk.
        for text in self.state.session.meta.queued_messages.clone() {
            self.queue_and_notify(QueuedMessage {
                text,
                images: Vec::new(),
            });
        }
    }

    pub(super) fn queue_compact(&mut self, instructions: Option<String>) {
        let Some(ref shared) = self.queue.shared else {
            return;
        };
        shared.push(QueueItem::Compact(Compaction {
            run_id: self.run_id,
            instructions,
            workflow: self.state.workflow,
        }));
    }

    /// Agent reached a deferred message: time to draw the bubble. Restored
    /// queue items start runs without `start_run`, so this is where the app
    /// learns the agent is busy. Immediate-dispatch items skip this event,
    /// so no dedup needed.
    pub(super) fn on_queue_item_consumed(&mut self, text: String, images: Vec<ImageSource>) {
        self.status = Status::Streaming;
        self.main_chat().show_user_message(text, images);
    }

    /// Immediate path: kick off the agent and draw the bubble in the same
    /// frame, so the user sees their message land where it will stay.
    pub(super) fn start_from_queue(&mut self, msg: &QueuedMessage) -> Vec<Action> {
        let input = self.build_agent_input(msg);
        self.start_run(input, msg.text.clone())
    }

    pub(crate) fn start_mailbox_run(
        &mut self,
        preamble: Vec<maki_providers::Message>,
    ) -> Vec<Action> {
        let mut input = self.build_agent_input(&QueuedMessage {
            text: String::new(),
            images: Vec::new(),
        });
        input.preamble = preamble;
        self.start_run(input, String::new())
    }

    /// The one place a fresh run starts: every path that emits
    /// `Action::SendMessage` must go through here so `run_id` bumps exactly
    /// once per run.
    pub(super) fn start_run(&mut self, input: AgentInput, display: String) -> Vec<Action> {
        self.run_id += 1;
        self.clear_exit_request();
        // New work supersedes text held for recovery after an agent error.
        self.recoverable_queue.clear();
        self.status = Status::Streaming;
        self.fire_session_event(HostEvent::TurnStart { text: &display });
        if !display.is_empty() || !input.images.is_empty() {
            self.main_chat()
                .show_user_message(display, input.images.clone());
        } else {
            self.main_chat().flush();
            self.main_chat().ensure_active_group();
        }
        vec![Action::SendMessage(Box::new(input))]
    }
}
