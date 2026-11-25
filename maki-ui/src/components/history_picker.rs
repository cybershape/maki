use std::collections::HashSet;

use crate::components::Overlay;
use crate::components::list_picker::{ListPicker, PickerAction, PickerItem};
use crate::repaint::Cadence;

use crossterm::event::KeyEvent;
use maki_storage::input_history::InputHistory;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

const TITLE: &str = " Input History ";
pub(crate) const NO_HISTORY_MSG: &str = "No input history";
const NEWLINE_MARK: &str = " ↵ ";
const FOOTER: &[(&str, &str)] = &[("Enter", "select")];

fn preview_line(text: &str) -> String {
    let mut preview = String::with_capacity(text.len());
    for line in text.split('\n') {
        if !preview.is_empty() {
            preview.push_str(NEWLINE_MARK);
        }
        for ch in line.trim_end_matches('\r').chars() {
            preview.push(if ch == '\t' { ' ' } else { ch });
        }
    }
    preview
}

pub enum HistoryPickerAction {
    Consumed,
    Select(HistoryEntry),
    Close,
}

pub struct HistoryEntry {
    pub index: usize,
    pub preview: String,
    pub text: String,
}

impl PickerItem for HistoryEntry {
    fn label(&self) -> &str {
        &self.preview
    }

    fn search_keys(&self) -> Vec<&str> {
        vec![&self.text]
    }
}

pub struct HistoryPicker {
    picker: ListPicker<HistoryEntry>,
}

impl HistoryPicker {
    pub fn new() -> Self {
        Self {
            picker: ListPicker::new().with_footer(FOOTER),
        }
    }

    pub fn open(&mut self, history: &InputHistory) -> Result<(), String> {
        let mut seen = HashSet::new();
        let entries: Vec<HistoryEntry> = history
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, text)| seen.insert(*text))
            .map(|(index, text)| HistoryEntry {
                index,
                preview: preview_line(text),
                text: text.to_owned(),
            })
            .collect();
        if entries.is_empty() {
            return Err(NO_HISTORY_MSG.into());
        }
        self.picker.open(entries, TITLE);
        Ok(())
    }

    pub fn is_open(&self) -> bool {
        self.picker.is_open()
    }

    pub fn close(&mut self) {
        self.picker.close();
    }

    pub fn contains(&self, pos: Position) -> bool {
        self.picker.contains(pos)
    }

    pub fn scroll(&mut self, delta: i32) {
        self.picker.scroll(delta);
    }

    pub fn handle_paste(&mut self, text: &str) {
        self.picker.handle_paste(text);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> HistoryPickerAction {
        match self.picker.handle_key(key) {
            PickerAction::Consumed => HistoryPickerAction::Consumed,
            PickerAction::Select(entry) => HistoryPickerAction::Select(entry),
            PickerAction::Close => HistoryPickerAction::Close,
            PickerAction::Toggle(..) => HistoryPickerAction::Consumed,
        }
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) -> (Rect, Option<Position>) {
        self.picker.view(frame, area)
    }
}

impl Overlay for HistoryPicker {
    fn is_prompting(&self) -> bool {
        false
    }

    fn is_open(&self) -> bool {
        self.is_open()
    }

    fn close(&mut self) {
        self.close();
    }

    fn cadence(&self) -> Cadence {
        self.picker.cadence()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::key;
    use crate::components::keybindings::key as kb;
    use crossterm::event::KeyCode;
    use test_case::test_case;

    const FIRST: &str = "first prompt";
    const SECOND: &str = "second prompt";
    const MULTILINE: &str = "line one\nline\ttwo";
    const MULTILINE_PREVIEW: &str = "line one ↵ line two";
    const SECOND_LINE_QUERY: &str = "two";

    fn seeded_history(entries: &[&str]) -> InputHistory {
        let mut history = InputHistory::default();
        for entry in entries {
            history.push((*entry).into());
        }
        history
    }

    #[test]
    fn empty_history_returns_error() {
        let mut picker = HistoryPicker::new();
        let err = picker.open(&InputHistory::default()).unwrap_err();
        assert_eq!(err, NO_HISTORY_MSG);
        assert!(!picker.is_open());
    }

    #[test]
    fn opens_newest_first_and_deduplicates_keeping_latest_index() {
        let mut picker = HistoryPicker::new();
        let history = seeded_history(&[FIRST, SECOND, FIRST]);
        picker.open(&history).unwrap();

        let action = picker.handle_key(key(KeyCode::Enter));
        let HistoryPickerAction::Select(entry) = action else {
            panic!("expected Select");
        };
        assert_eq!(entry.text, FIRST);
        assert_eq!(entry.index, 2);
        assert!(!picker.is_open());
    }

    #[test]
    fn multiline_entry_formats_preview_and_filters_by_full_text() {
        let mut picker = HistoryPicker::new();
        let history = seeded_history(&[FIRST, MULTILINE]);
        picker.open(&history).unwrap();

        assert_eq!(
            picker.picker.selected_item().unwrap().label(),
            MULTILINE_PREVIEW
        );

        for c in SECOND_LINE_QUERY.chars() {
            picker.handle_key(key(KeyCode::Char(c)));
        }
        let HistoryPickerAction::Select(entry) = picker.handle_key(key(KeyCode::Enter)) else {
            panic!("expected Select");
        };
        assert_eq!(entry.text, MULTILINE);
        assert_eq!(entry.index, 1);
    }

    #[test_case(key(KeyCode::Esc) ; "esc")]
    #[test_case(kb::QUIT.to_key_event() ; "ctrl_c")]
    fn cancel_closes_picker(cancel_key: KeyEvent) {
        let mut picker = HistoryPicker::new();
        picker.open(&seeded_history(&[FIRST])).unwrap();

        let action = picker.handle_key(cancel_key);
        assert!(matches!(action, HistoryPickerAction::Close));
        assert!(!picker.is_open());
    }
}
