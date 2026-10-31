use crate::theme;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Clear};
use unicode_width::UnicodeWidthStr;

/// The border, in cells, on each axis.
pub const BORDER: u16 = 2;

fn percent_of(total: u16, percent: u16) -> u16 {
    (u32::from(total) * u32::from(percent) / 100) as u16
}

/// The rect inside `rect` after `pad` on each side.
pub fn inset(rect: Rect, pad: u16) -> Rect {
    Rect {
        x: rect.x + pad,
        width: rect.width.saturating_sub(pad * 2),
        ..rect
    }
}

pub fn clean_left_wide_chars(buf: &mut Buffer, area: Rect) {
    if area.x == 0 {
        return;
    }
    let left = area.x - 1;
    if left < buf.area.left() || left >= buf.area.right() {
        return;
    }
    let top = area.top().max(buf.area.top());
    let bottom = area.bottom().min(buf.area.bottom());
    for y in top..bottom {
        let cell = &mut buf[(left, y)];
        if cell.symbol().width() > 1 {
            cell.set_symbol(" ");
        }
    }
}

pub fn clear_area(frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
    clean_left_wide_chars(frame.buffer_mut(), area);
}

pub struct Modal<'a> {
    pub title: &'a str,
    /// A floor, not the size: the content and `min_width` can widen the popup.
    pub width_percent: u16,
    pub min_width: u16,
    /// A ceiling, not the size: `render` shrinks the popup to the content.
    pub max_height_percent: u16,
}

impl Modal<'_> {
    pub fn width(&self, screen: u16) -> u16 {
        percent_of(screen, self.width_percent)
            .max(self.min_width)
            .min(screen)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, content_height: u16) -> (Rect, Rect) {
        self.render_with_footer(frame, area, content_height, None)
    }

    /// Keybind hints go on the bottom border, like the Lua floats draw them,
    /// so they cost no content row.
    pub fn render_with_footer(
        &self,
        frame: &mut Frame,
        area: Rect,
        content_height: u16,
        footer: Option<Line<'_>>,
    ) -> (Rect, Rect) {
        let total_h = (content_height + BORDER)
            .min(percent_of(area.height, self.max_height_percent))
            .max(BORDER + 1);

        let [popup] = Layout::vertical([Constraint::Length(total_h)])
            .flex(Flex::Center)
            .areas(area);
        let [popup] = Layout::horizontal([Constraint::Length(self.width(area.width))])
            .flex(Flex::Center)
            .areas(popup);

        clear_area(frame, popup);

        let mut block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::current().panel_border)
            .title(self.title)
            .title_style(theme::current().panel_title)
            .style(Style::new().bg(theme::current().background));
        if let Some(footer) = footer {
            block = block.title_bottom(footer);
        }

        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        (popup, inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn test_modal_occludes_wide_chars() {
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).unwrap();

        let modal = Modal {
            title: " test ",
            width_percent: 50,
            min_width: 10,
            max_height_percent: 100,
        };

        terminal
            .draw(|f| {
                f.buffer_mut()
                    .set_string(0, 1, "更新上游", Style::default());
                f.buffer_mut()
                    .set_string(0, 2, "更新上游", Style::default());
                modal.render(f, f.area(), 1);
            })
            .unwrap();

        let buf = terminal.backend().buffer();
        // popup starts at x=5, y=1
        // col 4 was '上' (width 2), which would bleed into col 5
        assert_eq!(buf[(4, 1)].symbol(), " ");
        assert_eq!(buf[(5, 1)].symbol(), "╭");
        assert_eq!(buf[(4, 2)].symbol(), " ");
        assert_eq!(buf[(5, 2)].symbol(), "│");
    }
}
