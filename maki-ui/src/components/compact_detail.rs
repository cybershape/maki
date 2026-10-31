use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};
use maki_agent::{BufferSnapshot, SnapshotLine, ToolInput, ToolOutput};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use super::keybindings::key;
use super::modal::Modal;
use super::scrollbar::render_vertical_scrollbar;
use super::tool_display::resolve_span_style;
use super::{
    CompactDetail, GroupThinkingEntry, GroupToolEntry, ModalScroll, Overlay, ToolStatus, hint_footer,
};
use crate::theme;

const TITLE_PAD: &str = " ";
const SECTION_GAP: &str = "";
const INPUT_LABEL: &str = "Input";
const OUTPUT_LABEL: &str = "Output";
const ERROR_LABEL: &str = "Error";
const EMPTY: &str = "(empty)";
const RECEIVING: &str = "Receiving arguments…";
const RUNNING: &str = "Running…";
/// Of the centered display column, not the terminal. `max_width` already
/// picked that column out of the screen.
const WIDTH_PERCENT: u16 = 90;
const MIN_WIDTH: u16 = 48;
const MAX_HEIGHT_PERCENT: u16 = 80;
const H_PAD: u16 = 1;
const MULTI_HINTS: &[(&str, &str)] = &[("↑/↓", "scroll"), ("j/k", "prev/next"), ("Esc", "close")];
const SINGLE_HINTS: &[(&str, &str)] = &[("↑/↓", "scroll"), ("Esc", "close")];

/// Which row of a compact group a click opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactTarget {
    pub group_id: String,
    pub entry: usize,
    /// `Some` names a child of a `batch` row; `None` is the row itself.
    pub child: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Shown {
    Tool(Box<GroupToolEntry>),
    Thinking(GroupThinkingEntry),
}

/// The call behind one compact row, opened over the transcript.
pub struct CompactDetailModal {
    items: Vec<Shown>,
    current: usize,
    scroll: ModalScroll,
}

impl CompactDetailModal {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            current: 0,
            scroll: ModalScroll::new_top(),
        }
    }

    #[cfg(test)]
    pub fn open_tool(&mut self, entry: GroupToolEntry) {
        self.open_group(vec![Shown::Tool(Box::new(entry))], 0);
    }

    #[cfg(test)]
    pub fn open_thinking(&mut self, entry: GroupThinkingEntry) {
        self.open_group(vec![Shown::Thinking(entry)], 0);
    }

    pub fn open_group(&mut self, items: Vec<Shown>, current: usize) {
        self.scroll.reset();
        self.items = items;
        self.current = if self.items.is_empty() {
            0
        } else {
            current.min(self.items.len() - 1)
        };
    }

    pub fn is_open(&self) -> bool {
        !self.items.is_empty()
    }

    pub fn close(&mut self) {
        self.items.clear();
        self.current = 0;
        self.scroll.reset();
    }

    pub fn next_item(&mut self) {
        if self.current + 1 < self.items.len() {
            self.current += 1;
            self.scroll.reset();
        }
    }

    pub fn prev_item(&mut self) {
        if self.current > 0 {
            self.current -= 1;
            self.scroll.reset();
        }
    }

    pub fn scroll(&mut self, delta: i32) {
        self.scroll.scroll(delta);
    }

    pub fn handle_key(&mut self, key_event: KeyEvent) -> bool {
        if key_event.code == KeyCode::Esc
            || key_event.code == KeyCode::Enter
            || key::QUIT.matches(key_event)
        {
            self.close();
            return true;
        }
        if key_event.modifiers.is_empty() {
            match key_event.code {
                KeyCode::Char('j') => {
                    self.next_item();
                    return true;
                }
                KeyCode::Char('k') => {
                    self.prev_item();
                    return true;
                }
                _ => {}
            }
        }
        self.scroll.handle_key(key_event);
        true
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) -> Rect {
        let Some(shown) = self.items.get(self.current) else {
            return Rect::default();
        };
        let lines = match shown {
            Shown::Tool(entry) => tool_lines(entry),
            Shown::Thinking(entry) => thinking_lines(entry),
        };
        let title = match shown {
            Shown::Tool(entry) => {
                if self.items.len() > 1 {
                    format!(
                        "{TITLE_PAD}{}{TITLE_PAD}({}/{})",
                        entry.name,
                        self.current + 1,
                        self.items.len()
                    )
                } else {
                    format!("{TITLE_PAD}{}{TITLE_PAD}", entry.name)
                }
            }
            Shown::Thinking(_) => {
                if self.items.len() > 1 {
                    format!(
                        "{TITLE_PAD}thinking{TITLE_PAD}({}/{})",
                        self.current + 1,
                        self.items.len()
                    )
                } else {
                    format!("{TITLE_PAD}thinking{TITLE_PAD}")
                }
            }
        };
        let modal = Modal {
            title: &title,
            width_percent: WIDTH_PERCENT,
            min_width: MIN_WIDTH,
            max_height_percent: MAX_HEIGHT_PERCENT,
        };
        let hints = if self.items.len() > 1 {
            MULTI_HINTS
        } else {
            SINGLE_HINTS
        };
        let padded_width = modal
            .width(area.width)
            .saturating_sub(super::modal::BORDER + H_PAD * 2);
        let wrapped = Paragraph::new(lines).wrap(Wrap { trim: false });
        let total = wrapped.line_count(padded_width) as u16;
        let (popup, inner) = modal.render_with_footer(frame, area, total, Some(hint_footer(hints)));
        let padded = super::modal::inset(inner, H_PAD);
        let total = wrapped.line_count(padded.width) as u16;
        self.scroll.update_dimensions(total, padded.height);
        frame.render_widget(wrapped.scroll((self.scroll.offset(), 0)), padded);
        if total > padded.height {
            render_vertical_scrollbar(
                frame,
                inner,
                u32::from(total),
                u32::from(self.scroll.offset()),
            );
        }
        popup
    }
}

impl Overlay for CompactDetailModal {
    fn is_prompting(&self) -> bool {
        self.is_open()
    }

    fn is_open(&self) -> bool {
        self.is_open()
    }

    fn close(&mut self) {
        self.close()
    }
}

fn tool_lines(entry: &GroupToolEntry) -> Vec<Line<'static>> {
    let theme = theme::current();
    let mut lines = Vec::new();
    let detail = entry.detail.as_ref();

    if let Some(note) = detail.and_then(|d| d.annotation.as_deref()) {
        lines.push(Line::from(Span::styled(note.to_owned(), theme.tool_dim)));
        lines.push(Line::from(SECTION_GAP));
    }

    lines.push(section_label(INPUT_LABEL));
    match detail.and_then(|d| d.input.as_ref()) {
        Some(ToolInput::Code { code, .. } | ToolInput::Script { code, .. }) => {
            push_text(&mut lines, code)
        }
        None if entry.receiving_args => {
            lines.push(Line::from(Span::styled(RECEIVING, theme.tool_dim)))
        }
        None => push_json(&mut lines, detail.and_then(|d| d.raw_input.as_ref()).or(entry.raw_input.as_ref())),
    }

    lines.push(Line::from(SECTION_GAP));
    lines.push(section_label(if entry.status == ToolStatus::Error {
        ERROR_LABEL
    } else {
        OUTPUT_LABEL
    }));
    push_output(&mut lines, entry, detail);
    lines
}

fn thinking_lines(entry: &GroupThinkingEntry) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if entry.text.is_empty() {
        lines.push(Line::from(Span::styled(
            if entry.active { RUNNING } else { EMPTY },
            theme::current().tool_dim,
        )));
    } else {
        push_text(&mut lines, &entry.text);
    }
    lines
}

fn push_output(lines: &mut Vec<Line<'static>>, entry: &GroupToolEntry, detail: Option<&CompactDetail>) {
    let Some(detail) = detail else {
        lines.push(Line::from(Span::styled(EMPTY, theme::current().tool_dim)));
        return;
    };
    if let Some(output) = &detail.output {
        let text = output.as_display_text();
        if !text.is_empty() {
            push_text(lines, &text);
            return;
        }
    }
    if let Some(snapshot) = &detail.render_snapshot {
        push_snapshot(lines, snapshot);
        return;
    }
    if let Some(output) = &detail.output {
        push_output_value(lines, output);
        return;
    }
    if let Some(live) = &detail.live_output {
        push_text(lines, live);
        return;
    }
    if entry.status == ToolStatus::InProgress {
        lines.push(Line::from(Span::styled(RUNNING, theme::current().tool_dim)));
        return;
    }
    lines.push(Line::from(Span::styled(EMPTY, theme::current().tool_dim)));
}

fn push_output_value(lines: &mut Vec<Line<'static>>, output: &Arc<ToolOutput>) {
    let text = output.as_display_text();
    if text.is_empty() {
        lines.push(Line::from(Span::styled(EMPTY, theme::current().tool_dim)));
    } else {
        push_text(lines, &text);
    }
}

fn push_json(lines: &mut Vec<Line<'static>>, value: Option<&serde_json::Value>) {
    match value {
        Some(value) => match serde_json::to_string_pretty(value) {
            Ok(text) => push_text(lines, &text),
            Err(_) => lines.push(Line::from(Span::styled(EMPTY, theme::current().tool_dim))),
        },
        None => lines.push(Line::from(Span::styled(EMPTY, theme::current().tool_dim))),
    }
}

fn push_snapshot(lines: &mut Vec<Line<'static>>, snapshot: &BufferSnapshot) {
    if snapshot.lines.is_empty() {
        lines.push(Line::from(Span::styled(EMPTY, theme::current().tool_dim)));
        return;
    }
    for line in snapshot.lines.iter() {
        lines.push(snapshot_line(line));
    }
}

fn snapshot_line(line: &SnapshotLine) -> Line<'static> {
    Line::from(
        line.spans
            .iter()
            .map(|span| Span::styled(span.text.clone(), resolve_span_style(&span.style)))
            .collect::<Vec<_>>(),
    )
}

fn push_text(lines: &mut Vec<Line<'static>>, text: &str) {
    let style = theme::current().tool;
    if text.is_empty() {
        lines.push(Line::from(Span::styled(EMPTY, theme::current().tool_dim)));
        return;
    }
    for line in text.lines() {
        lines.push(Line::from(Span::styled(line.to_owned(), style)));
    }
}

fn section_label(label: &str) -> Line<'static> {
    Line::from(Span::styled(label.to_owned(), theme::current().panel_title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::key as key_ev;
    use maki_agent::{BufferSnapshot, ToolOutput};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    const CLOSE: &str = "esc closes the popup";
    const OUTPUT_TEXT: &str = "compiled 12 crates";
    const LONG_LINE: &str = "abcdefghijklmnopqrstuvwxyz";
    const SCREEN: u16 = 60;

    fn tool(detail: CompactDetail) -> GroupToolEntry {
        GroupToolEntry {
            id: "call-1".into(),
            name: "bash".into(),
            summary: "cargo build".into(),
            raw_input: None,
            children: Vec::new(),
            status: ToolStatus::Success,
            receiving_args: false,
            started_at: None,
            duration: None,
            detail: Some(detail),
        }
    }

    fn draw(modal: &mut CompactDetailModal, height: u16) -> String {
        let backend = TestBackend::new(SCREEN, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                modal.view(frame, frame.area());
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn popup_shows_the_call_and_shrinks_to_it() {
        let mut modal = CompactDetailModal::new();
        modal.open_tool(tool(CompactDetail {
            raw_input: Some(serde_json::json!({ "command": "cargo build" })),
            output: Some(Arc::new(ToolOutput::Plain(OUTPUT_TEXT.into()))),
            ..CompactDetail::default()
        }));
        let text = draw(&mut modal, 30);
        assert!(text.contains("bash"), "{text}");
        assert!(text.contains("cargo build"), "{text}");
        assert!(text.contains(OUTPUT_TEXT), "{text}");
        assert!(text.contains("close"), "{text}");
        assert!(text.lines().any(|line| line.trim().is_empty()));
    }

    #[test]
    fn popup_scrolls_a_snapshot_taller_than_the_screen() {
        let lines = (0..80)
            .map(|n| maki_agent::SnapshotLine::plain(format!("row {n}")))
            .collect();
        let mut modal = CompactDetailModal::new();
        modal.open_tool(tool(CompactDetail {
            render_snapshot: Some(BufferSnapshot::from_arc(Arc::new(lines))),
            ..CompactDetail::default()
        }));
        let top = draw(&mut modal, 16);
        assert!(top.contains("row 0"), "{top}");
        assert!(!top.contains("row 30"), "{top}");
        for _ in 0..6 {
            modal.scroll.scroll(-1);
        }
        let scrolled = draw(&mut modal, 16);
        assert!(scrolled.contains("row 2"), "{scrolled}");
        assert!(!scrolled.contains("row 0"), "{scrolled}");
    }

    #[test]
    fn popup_takes_ninety_percent_of_the_display_column() {
        let mut modal = CompactDetailModal::new();
        modal.open_tool(tool(CompactDetail::default()));
        let text = draw(&mut modal, 20);
        let widest = text
            .lines()
            .map(|line| line.trim().chars().count())
            .max()
            .unwrap_or(0);
        assert_eq!(widest, SCREEN as usize * 90 / 100, "{text}");
    }

    #[test]
    fn popup_wraps_a_line_wider_than_the_screen() {
        let mut modal = CompactDetailModal::new();
        modal.open_tool(tool(CompactDetail {
            live_output: Some(LONG_LINE.repeat(6)),
            ..CompactDetail::default()
        }));
        let text = draw(&mut modal, 20);
        assert!(text.lines().any(|line| line.contains("abcdefghij")));
        assert!(text.lines().all(|line| line.chars().count() <= SCREEN as usize));
    }

    #[test]
    fn esc_closes() {
        let mut modal = CompactDetailModal::new();
        modal.open_thinking(GroupThinkingEntry {
            id: 0,
            text: "reasoning".into(),
            tokens: 1,
            active: false,
            started_at: None,
            duration: None,
        });
        assert!(modal.is_open());
        assert!(modal.handle_key(key_ev(KeyCode::Esc)), "{CLOSE}");
        assert!(!modal.is_open(), "{CLOSE}");
    }

    #[test]
    fn j_and_k_navigate_between_items() {
        let mut modal = CompactDetailModal::new();
        let mut t1 = tool(CompactDetail::default());
        t1.name = Arc::from("tool1");
        let mut t2 = tool(CompactDetail::default());
        t2.name = Arc::from("tool2");
        let th = GroupThinkingEntry {
            id: 0,
            text: "thinking block".into(),
            tokens: 1,
            active: false,
            started_at: None,
            duration: None,
        };

        modal.open_group(
            vec![
                Shown::Tool(Box::new(t1)),
                Shown::Tool(Box::new(t2)),
                Shown::Thinking(th),
            ],
            0,
        );

        let view1 = draw(&mut modal, 20);
        assert!(view1.contains("tool1 (1/3)"), "{view1}");

        // j goes to next item
        assert!(modal.handle_key(key_ev(KeyCode::Char('j'))));
        let view2 = draw(&mut modal, 20);
        assert!(view2.contains("tool2 (2/3)"), "{view2}");

        // j goes to thinking
        assert!(modal.handle_key(key_ev(KeyCode::Char('j'))));
        let view3 = draw(&mut modal, 20);
        assert!(view3.contains("thinking (3/3)"), "{view3}");

        // j at end stays at end
        assert!(modal.handle_key(key_ev(KeyCode::Char('j'))));
        assert_eq!(modal.current, 2);

        // k goes to previous item
        assert!(modal.handle_key(key_ev(KeyCode::Char('k'))));
        let view4 = draw(&mut modal, 20);
        assert!(view4.contains("tool2 (2/3)"), "{view4}");

        // k goes back to first item
        assert!(modal.handle_key(key_ev(KeyCode::Char('k'))));
        let view5 = draw(&mut modal, 20);
        assert!(view5.contains("tool1 (1/3)"), "{view5}");

        // k at beginning stays at beginning
        assert!(modal.handle_key(key_ev(KeyCode::Char('k'))));
        assert_eq!(modal.current, 0);
    }

    #[test]
    fn content_has_horizontal_padding() {
        let mut modal = CompactDetailModal::new();
        let mut t = tool(CompactDetail {
            raw_input: Some(serde_json::json!({ "command": "cargo check" })),
            ..CompactDetail::default()
        });
        t.name = Arc::from("bash");
        modal.open_tool(t);

        let text = draw(&mut modal, 20);
        // Find a line that has borders
        let border_line = text.lines().find(|l| l.contains("Input")).expect("Input line");
        // With H_PAD = 1, inside border there is a space before "Input"
        assert!(border_line.contains("│ Input") || border_line.contains("│  Input"), "{border_line}");
    }

    #[test]
    fn modal_defaults_to_top_when_scroll_needed() {
        let lines: Vec<_> = (0..100)
            .map(|n| format!("output line {n}"))
            .collect();
        let full_text = lines.join("\n");
        let mut modal = CompactDetailModal::new();
        modal.open_tool(tool(CompactDetail {
            output: Some(Arc::new(ToolOutput::Plain(full_text.into()))),
            ..CompactDetail::default()
        }));

        let text = draw(&mut modal, 20);
        assert!(text.contains("output line 0"), "{text}");
        assert_eq!(modal.scroll.offset(), 0);

        // Drawing a second time must still stay at top
        let text2 = draw(&mut modal, 20);
        assert!(text2.contains("output line 0"), "{text2}");
        assert_eq!(modal.scroll.offset(), 0);
    }

    #[test]
    fn output_prefers_full_output_over_truncated_snapshot() {
        let mut modal = CompactDetailModal::new();
        let snapshot_lines = vec![
            SnapshotLine::plain("... (5 hidden)".to_string()),
            SnapshotLine::plain("last visible line".to_string()),
        ];
        modal.open_tool(tool(CompactDetail {
            output: Some(Arc::new(ToolOutput::Plain("line 1\nline 2\nline 3\nline 4\nline 5\nlast line".into()))),
            render_snapshot: Some(BufferSnapshot::from_arc(Arc::new(snapshot_lines))),
            ..CompactDetail::default()
        }));

        let text = draw(&mut modal, 25);
        assert!(!text.contains("hidden"), "{text}");
        assert!(text.contains("line 1"), "{text}");
        assert!(text.contains("last line"), "{text}");
    }
}
