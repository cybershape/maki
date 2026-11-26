//! The mailbox's waiting messages, shown before a run reads them. Read-only:
//! once the model takes a message it leaves here and draws as a notice line.

use maki_agent::MailboxPreview;
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};

use crate::theme;

const ELLIPSIS: &str = "...";
const INBOX_LABEL: &str = " Inbox ";
const MAX_ROWS: usize = 5;

pub fn height(inbox_len: usize) -> u16 {
    if inbox_len == 0 {
        0
    } else {
        (inbox_len.min(MAX_ROWS) + usize::from(inbox_len > MAX_ROWS)) as u16 + 2
    }
}

/// The one line a preview draws: its label, else the body's first line.
pub fn preview_text(preview: &MailboxPreview) -> &str {
    preview
        .display
        .as_deref()
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| {
            preview
                .body
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default()
        })
}

pub fn view(frame: &mut Frame, area: Rect, previews: &[MailboxPreview]) {
    if previews.is_empty() {
        return;
    }
    let content_width = area.width.saturating_sub(2) as usize;
    let style = theme::current().tool_dim;
    let mut lines: Vec<Line> = previews
        .iter()
        .take(MAX_ROWS)
        .map(|preview| {
            let flat = preview_text(preview).replace('\n', " ");
            truncate_line(&flat, content_width, style)
        })
        .collect();
    if previews.len() > MAX_ROWS {
        lines.push(Line::from(Span::styled(
            format!("+{} more", previews.len() - MAX_ROWS),
            style,
        )));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::current().panel_border)
        .title_top(Line::from(INBOX_LABEL).left_aligned())
        .title_style(theme::current().panel_title);

    let paragraph = Paragraph::new(lines)
        .style(Style::new().fg(theme::current().foreground))
        .block(block);

    frame.render_widget(paragraph, area);
}

fn truncate_line(text: &str, max_width: usize, style: Style) -> Line<'static> {
    if text.len() <= max_width {
        return Line::from(Span::styled(text.to_string(), style));
    }
    let truncated_len = text.floor_char_boundary(max_width.saturating_sub(ELLIPSIS.len()));
    Line::from(vec![
        Span::styled(text[..truncated_len].to_string(), style),
        Span::styled(ELLIPSIS, style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    use test_case::test_case;

    const BODY: &str = "the IOC list is stale";
    const LABEL: &str = "Task toolu_01 reported: the IOC list is stale";

    fn preview(display: Option<&str>, body: &str) -> MailboxPreview {
        MailboxPreview {
            display: display.map(str::to_owned),
            body: body.to_owned(),
        }
    }

    #[test_case(0, 0 ; "empty")]
    #[test_case(1, 3 ; "one")]
    #[test_case(MAX_ROWS, 7 ; "full")]
    #[test_case(MAX_ROWS + 1, 8 ; "overflow_adds_one_row")]
    #[test_case(MAX_ROWS + 20, 8 ; "overflow_stays_capped")]
    fn height_caps_rows_and_counts_borders(len: usize, expected: u16) {
        assert_eq!(height(len), expected);
    }

    #[test_case(Some(LABEL), BODY, LABEL ; "label_wins")]
    #[test_case(Some(""), BODY, BODY ; "blank_label_falls_back")]
    #[test_case(None, BODY, BODY ; "no_label")]
    #[test_case(None, "first\nsecond", "first" ; "body_first_line")]
    #[test_case(None, "\n \nsecond", "second" ; "body_skips_blank_lines")]
    fn preview_text_cases(display: Option<&str>, body: &str, expected: &str) {
        assert_eq!(preview_text(&preview(display, body)), expected);
    }

    #[test]
    fn view_renders_the_label_and_an_overflow_count() {
        const WIDTH: u16 = 40;
        const BORDERS: usize = 2;
        let backend = ratatui::backend::TestBackend::new(WIDTH, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let previews: Vec<_> = (0..MAX_ROWS + 2)
            .map(|i| preview(Some(LABEL), &format!("{BODY} {i}")))
            .collect();

        terminal
            .draw(|f| view(f, Rect::new(0, 0, WIDTH, 8), &previews))
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let content: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("Inbox"));
        let content_width = WIDTH as usize - BORDERS;
        let fits = &LABEL[..LABEL.floor_char_boundary(content_width - ELLIPSIS.len())];
        assert!(content.contains(fits), "{content}");
        assert!(content.contains("+2 more"));
    }

    #[test]
    fn view_truncates_a_long_line() {
        let backend = ratatui::backend::TestBackend::new(12, 3);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let previews = [preview(None, "abcdefghijklmnopqrstuvwxyz")];

        terminal
            .draw(|f| view(f, Rect::new(0, 0, 12, 3), &previews))
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let content: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains(ELLIPSIS));
        assert!(!content.contains("qrstuvwxyz"));
    }
}
