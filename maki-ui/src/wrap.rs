use ratatui::buffer::CellWidth;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

pub fn total_rows(lines: &[Line<'_>], width: u16) -> u16 {
    let mut measure = Measure::new(width);
    lines
        .iter()
        .fold(0, |rows, line| rows.saturating_add(measure.rows(line)))
}

pub(crate) struct Measure {
    width: u16,
}

impl Measure {
    pub(crate) fn new(width: u16) -> Self {
        Self { width }
    }

    pub(crate) fn rows(&mut self, line: &Line<'_>) -> u16 {
        scan(line, self.width, |_, _, _| {}, |_| {})
    }
}

pub(crate) fn row_widths(line: &Line<'_>, width: u16, on_row: impl FnMut(u16)) {
    scan(line, width, |_, _, _| {}, on_row);
}

pub(crate) fn visible_lines<'a>(
    lines: &'a [Line<'_>],
    width: u16,
    mut skip: u16,
    height: u16,
) -> Vec<Line<'a>> {
    let mut result = Vec::new();
    for line in lines {
        if result.len() >= usize::from(height) {
            break;
        }
        let available = usize::from(height) - result.len();
        let start = result.len();
        let rows = scan(
            line,
            width,
            |row, symbol, style| {
                if row < skip || usize::from(row - skip) >= available {
                    return;
                }
                let index = start + usize::from(row - skip);
                while result.len() <= index {
                    result.push(Line {
                        style: line.style,
                        alignment: line.alignment,
                        ..Line::default()
                    });
                }
                let spans = &mut result[index].spans;
                if let Some(last) = spans.last_mut()
                    && last.style == style
                {
                    last.content.to_mut().push_str(symbol);
                } else {
                    spans.push(Span::styled(symbol.to_owned(), style));
                }
            },
            |_| {},
        );
        let visible = usize::from(rows.saturating_sub(skip)).min(available);
        while result.len() < start + visible {
            result.push(Line {
                style: line.style,
                alignment: line.alignment,
                ..Line::default()
            });
        }
        skip = skip.saturating_sub(rows);
    }
    result
}

fn scan(
    line: &Line<'_>,
    width: u16,
    mut on_symbol: impl FnMut(u16, &str, Style),
    mut on_row: impl FnMut(u16),
) -> u16 {
    if width == 0 {
        on_row(0);
        return 1;
    }
    let mut row = 0u16;
    let mut col = 0u16;
    let mut push = |symbol: &str, symbol_width: u16, style| {
        if symbol_width > width {
            return;
        }
        if symbol_width > width - col {
            on_row(col);
            row = row.saturating_add(1);
            col = 0;
        }
        on_symbol(row, symbol, style);
        col += symbol_width;
    };
    for span in &line.spans {
        let content = span.content.as_ref();
        if content.is_ascii() {
            for (i, byte) in content.bytes().enumerate() {
                if !byte.is_ascii_control() {
                    push(&content[i..i + 1], 1, span.style);
                }
            }
        } else {
            for grapheme in span.styled_graphemes(Style::default()) {
                push(
                    grapheme.symbol,
                    grapheme.symbol.cell_width(),
                    grapheme.style,
                );
            }
        }
    }
    on_row(col);
    row.saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::{row_widths, total_rows, visible_lines};
    use ratatui::buffer::{Buffer, CellWidth};
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Paragraph, Widget};
    use test_case::test_case;

    const SCREENSHOT: &str = "我会新增 progress.md，在 AGENTS.md 中加入修改前回顾决策、提交时记录决策的要求，并补记本次扫码登录调整，然后一起提交并推送当前改动。";
    const SCREENSHOT_FIRST_ROW: &str = "maki> 我会新增 progress.md，在 AGENTS.md 中加入修改前回顾决策、提交时记录决策的要求，并补记本次扫码";

    #[test_case("你好世界", 4, &["你好", "世界"] ; "chinese")]
    #[test_case("a你好", 2, &["a", "你", "好"] ; "wide_character_moves_to_next_row")]
    #[test_case("漢字漢字", 3, &["漢", "字", "漢", "字"] ; "odd_width")]
    #[test_case("hello world", 5, &["hello", " worl", "d"] ; "english_splits_at_columns")]
    #[test_case("AGENTS.md", 4, &["AGEN", "TS.m", "d"] ; "filename")]
    #[test_case("a    b", 4, &["a   ", " b"] ; "spaces_preserved")]
    #[test_case("ab\tcd", 3, &["abc", "d"] ; "controls_filtered")]
    #[test_case("⚠\u{fe0f}你", 3, &["⚠\u{fe0f}", "你"] ; "variation_selector")]
    #[test_case("👨‍👩‍👧‍👦a", 2, &["👨‍👩‍👧‍👦", "a"] ; "zwj_cluster")]
    #[test_case("e\u{301}ab", 2, &["e\u{301}a", "b"] ; "combining_mark")]
    #[test_case("你a好b", 1, &["a", "b"] ; "overwide_symbols_omitted")]
    #[test_case("你好", 1, &[""] ; "only_overwide_symbols")]
    #[test_case("abc", 0, &[""] ; "zero_width")]
    #[test_case("", 5, &[""] ; "empty_line")]
    #[test_case("abcd", 4, &["abcd"] ; "exact_width")]
    fn wraps_at_display_columns(input: &str, width: u16, expected: &[&str]) {
        let lines = [Line::from(input)];
        let wrapped = visible_lines(&lines, width, 0, u16::MAX);
        let text: Vec<String> = wrapped.iter().map(ToString::to_string).collect();
        assert_eq!(text, expected);
        assert_eq!(usize::from(total_rows(&lines, width)), expected.len());
        let mut widths = Vec::new();
        row_widths(&lines[0], width, |cols| widths.push(cols));
        assert_eq!(widths.len(), expected.len());
        assert!(widths.iter().all(|&cols| cols <= width));
        if width > 0 {
            let area = Rect::new(1, 1, width, wrapped.len() as u16);
            let mut buffer = Buffer::empty(Rect::new(0, 0, width + 2, area.height + 2));
            Paragraph::new(wrapped).render(area, &mut buffer);
            for (row, expected) in expected.iter().enumerate() {
                let mut actual = String::new();
                let mut col = area.x;
                while col < area.right() {
                    let cell = &buffer[(col, area.y + row as u16)];
                    actual.push_str(cell.symbol());
                    col += cell.symbol().cell_width().max(1);
                }
                assert_eq!(actual.trim_end(), expected.trim_end());
            }
            for y in 0..buffer.area.bottom() {
                assert_eq!(buffer[(0, y)].symbol(), " ");
                assert_eq!(buffer[(width + 1, y)].symbol(), " ");
            }
        }
    }

    #[test]
    fn screenshot_uses_the_whole_first_row_and_keeps_styles() {
        let code = Style::default().fg(Color::Magenta);
        let base = Style::default().bg(Color::Black);
        let line = Line::from(vec![
            Span::raw("maki> 我会新增 "),
            Span::styled("progress.md", code),
            Span::raw("，在 "),
            Span::styled("AGENTS.md", code),
            Span::raw(SCREENSHOT.split_once("AGENTS.md").unwrap().1),
        ])
        .style(base);
        let lines = [line];
        let wrapped = visible_lines(&lines, 100, 0, 10);
        assert_eq!(wrapped[0].to_string(), SCREENSHOT_FIRST_ROW);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].style, base);
        assert_eq!(wrapped[0].spans[1].style, code);
        let joined: String = wrapped.iter().map(ToString::to_string).collect();
        assert_eq!(joined, format!("maki> {SCREENSHOT}"));
    }

    #[test_case(0, 2, &["abc", "def"] ; "first_rows")]
    #[test_case(1, 2, &["def", "ghi"] ; "scrolled_inside_line")]
    #[test_case(2, 2, &["ghi", ""] ; "explicit_empty_line")]
    #[test_case(3, 2, &["", "末"] ; "next_source_line")]
    #[test_case(0, 0, &[] ; "zero_height")]
    fn visible_rows_keep_span_and_source_boundaries(skip: u16, height: u16, expected: &[&str]) {
        let lines = [
            Line::from(vec![Span::raw("ab"), Span::raw("cdefghi")]),
            Line::default(),
            Line::from("末尾"),
        ];
        let actual: Vec<String> = visible_lines(&lines, 3, skip, height)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(actual, expected);
    }
}
