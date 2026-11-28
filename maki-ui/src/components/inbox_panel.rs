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

use super::truncate_to_width;
use crate::theme;

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
            let flat = preview_text(preview).replace(['\n', '\r', '\t'], " ");
            Line::from(Span::styled(truncate_to_width(&flat, content_width), style))
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

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::buffer::Buffer;
    use test_case::test_case;
    use unicode_width::UnicodeWidthStr;

    use crate::components::ELLIPSIS;

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

    fn row_text(buffer: &Buffer, y: u16, width: u16) -> String {
        let mut out = String::new();
        let mut x = 1;
        while x < width - 1 {
            let sym = buffer[(x, y)].symbol();
            out.push_str(sym);
            x += sym.width().max(1) as u16;
        }
        out.trim_end().to_owned()
    }

    #[test_case("abcdefghijklmnopqrstuvwxyz", "abcdefg...", "h" ; "ascii_uses_full_width")]
    #[test_case("一二三四五六七八九十", "一二三...", "四" ; "cjk_uses_display_width_not_bytes")]
    fn view_truncates_to_display_width(input: &str, expected_row: &str, excluded: &str) {
        const WIDTH: u16 = 12;
        const HEIGHT: u16 = 3;
        let backend = ratatui::backend::TestBackend::new(WIDTH, HEIGHT);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let previews = [preview(None, input)];

        terminal
            .draw(|f| view(f, Rect::new(0, 0, WIDTH, HEIGHT), &previews))
            .unwrap();

        let row = row_text(terminal.backend().buffer(), 1, WIDTH);
        assert_eq!(row, expected_row);
        assert!(!row.contains(excluded), "row={row:?}");
    }

    #[test]
    fn view_truncates_cjk_and_ascii_lines_to_matching_visual_width() {
        const WIDTH: u16 = 32;
        const HEIGHT: u16 = 4;
        const CJK_DISPLAY: &str = "Vega reported: 一二三四五六七八九十甲乙丙丁";
        const ASCII_DISPLAY: &str = "Atlas reported: abcdefghijklmnopqrstuvwxyz";
        let backend = ratatui::backend::TestBackend::new(WIDTH, HEIGHT);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let previews = [
            preview(Some(CJK_DISPLAY), CJK_DISPLAY),
            preview(Some(ASCII_DISPLAY), ASCII_DISPLAY),
        ];

        terminal
            .draw(|f| view(f, Rect::new(0, 0, WIDTH, HEIGHT), &previews))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let cjk_row = row_text(buffer, 1, WIDTH);
        let ascii_row = row_text(buffer, 2, WIDTH);
        assert!(cjk_row.ends_with(ELLIPSIS), "{cjk_row:?}");
        assert!(ascii_row.ends_with(ELLIPSIS), "{ascii_row:?}");
        let content_width = usize::from(WIDTH - 2);
        assert_eq!(ascii_row.width(), content_width);
        assert!(cjk_row.width() >= content_width - 1, "{cjk_row:?}");
    }
}
