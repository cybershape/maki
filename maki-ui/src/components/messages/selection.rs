use super::segment::SegmentCache;
use crate::selection::{self, LineBreaks, ScreenSelection, Selection};
use crate::wrap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};

/// Rows re-rendered per pass of the copy path. A `Cell` is 40 bytes, so at a
/// usual terminal width this holds the scratch buffer near a megabyte however
/// tall the selected segment is.
const COPY_CHUNK_ROWS: u16 = 256;

/// Copies the segments the selection spans.
pub(super) fn extract_selection_text(
    cache: &SegmentCache,
    width: u16,
    sel: &Selection,
    msg_area: Rect,
) -> String {
    let (start, end) = sel.normalized();

    let mut out = String::new();
    for i in start.seg..end.seg.saturating_add(1).min(cache.len()) {
        let Some(seg) = cache.get(i) else { continue };
        if seg.lines().is_empty() {
            continue;
        }
        // The rows we copy come from this wrap at the real width, so clamp
        // against it: a reflow can shrink a segment under a stored row.
        let seg_rows = seg.height(width);
        let cols = seg.gutter_cols(width);
        let body = width - cols;
        let body_origin = msg_area.x + cols;

        let (rel_start, start_col) = if i == start.seg {
            (
                start.row.min(seg_rows),
                start.col.saturating_sub(body_origin),
            )
        } else {
            (0, 0)
        };
        let last_col = body.saturating_sub(1);
        let (rel_end, end_col) = if i != end.seg {
            (seg_rows, last_col)
        } else if end.col < body_origin {
            (end.row.min(seg_rows), last_col)
        } else {
            (
                end.row.saturating_add(1).min(seg_rows),
                (end.col - body_origin).min(last_col),
            )
        };
        if rel_end <= rel_start {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }

        let ss = ScreenSelection {
            start_row: rel_start,
            start_col,
            end_row: rel_end.saturating_sub(1),
            end_col,
        };

        // Only the selected rows are re-rendered, a chunk at a time. The
        // cursor measures the segment once for all the chunks it hands out.
        let mut carry = selection::RowCarry::default();
        let mut walk = seg.rows_from(rel_start, width);
        while let Some((lines, chunk)) = walk.next_chunk(COPY_CHUNK_ROWS) {
            if chunk.start >= rel_end {
                break;
            }
            // The buffer sits at the rows it draws, so `ss` keeps talking in
            // segment rows across every chunk.
            let area = Rect::new(0, chunk.start, body, chunk.end - chunk.start);
            let mut tmp = Buffer::empty(area);
            Paragraph::new(wrap::visible_lines(lines, body, 0, area.height))
                .render(area, &mut tmp);

            // The chunk runs to a line boundary, so it can reach past the
            // selection on either side. `append_rows` clips to the rows this
            // buffer actually holds.
            selection::append_rows(
                &tmp,
                area,
                &ss,
                rel_start..rel_end,
                &mut out,
                &LineBreaks::from_lines(lines, body),
                &mut carry,
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::segment::{GUTTER_COLS, Segment};
    use super::{COPY_CHUNK_ROWS, SegmentCache, extract_selection_text};
    use crate::selection::{DocPos, Selection, SelectionZone};
    use ratatui::layout::Rect;
    use ratatui::text::{Line, Span};
    use test_case::test_case;

    const MARKER: &str = "\u{25cf} ";
    const WIDTH: u16 = 20;
    const MSG_X: u16 = 1;
    const BODY_X: u16 = MSG_X + GUTTER_COLS;

    /// What dragging the mouse over one whole segment copies.
    fn copy_whole_segment(lines: Vec<Line<'static>>, width: u16) -> String {
        let area = Rect::new(0, 0, width, 24);
        let mut cache = SegmentCache::new();
        cache.push(Segment::with_lines(lines, None));
        let last_row = cache
            .get(0)
            .expect("segment")
            .height(width)
            .saturating_sub(1);

        let corner = DocPos {
            seg: 0,
            row: 0,
            col: 0,
        };
        let mut sel = Selection::start(corner, area, SelectionZone::Messages);
        sel.update(DocPos {
            seg: 0,
            row: last_row,
            col: width,
        });
        extract_selection_text(&cache, width, &sel, area)
    }

    fn gutter_segment(text: &str) -> Segment {
        let mut seg = Segment::with_lines(vec![Line::from(text.to_owned())], None);
        seg.gutter = Some(Span::raw(MARKER));
        seg
    }

    fn pos(seg: usize, row: u16, col: u16) -> DocPos {
        DocPos { seg, row, col }
    }

    fn copy_range(cache: &SegmentCache, from: DocPos, to: DocPos) -> String {
        let area = Rect::new(MSG_X, 0, WIDTH, 24);
        let mut sel = Selection::start(from, area, SelectionZone::Messages);
        sel.update(to);
        extract_selection_text(cache, WIDTH, &sel, area)
    }

    #[test_case(&["hello"], pos(0, 0, 0), pos(0, 0, WIDTH), "hello" ; "marker_is_not_copied")]
    #[test_case(&["hello"], pos(0, 0, BODY_X + 3), pos(0, 0, WIDTH), "lo" ; "columns_count_from_the_body")]
    #[test_case(&["hello", "world"], pos(0, 0, 0), pos(1, 0, MSG_X), "hello" ; "ending_in_the_gutter_leaves_that_row_out")]
    fn copy_skips_the_gutter(texts: &[&str], from: DocPos, to: DocPos, expected: &str) {
        let mut cache = SegmentCache::new();
        for text in texts {
            cache.push(gutter_segment(text));
        }
        assert_eq!(copy_range(&cache, from, to), expected);
    }

    #[test]
    fn copy_wrapped_gutter_prose_joins_with_one_space() {
        let text = "alpha beta gamma delta epsilon zeta";
        let mut cache = SegmentCache::new();
        cache.push(gutter_segment(text));
        let rows = cache.get(0).expect("segment").height(WIDTH);
        assert!(rows > 1);
        assert_eq!(
            copy_range(&cache, pos(0, 0, 0), pos(0, rows - 1, WIDTH)),
            text
        );
    }

    #[test]
    fn copy_across_two_gutter_segments() {
        let mut cache = SegmentCache::new();
        cache.push(gutter_segment("user"));
        cache.push(Segment::with_lines(vec![Line::default()], None));
        cache.push(gutter_segment("assistant"));
        assert_eq!(
            copy_range(&cache, pos(0, 0, 0), pos(2, 0, WIDTH)),
            "user\n\nassistant"
        );
    }
    #[test]
    fn copying_into_two_columns_gives_back_what_is_on_screen() {
        assert_eq!(
            copy_whole_segment(vec![Line::from("a\u{4f60}\u{597d}")], 2),
            "a\u{4f60}\u{597d}"
        );
    }

    #[test]
    fn spaces_survive_a_chunk_boundary_and_a_wide_character_gap() {
        const WIDTH: u16 = 3;
        let body = format!(
            "{}   你好 a    b",
            "x".repeat(usize::from(COPY_CHUNK_ROWS * WIDTH - 1))
        );
        let lines = vec![Line::from(body.clone())];
        assert_eq!(copy_whole_segment(lines, WIDTH), body);
    }

    #[test]
    fn blank_rows_survive_a_chunk_boundary() {
        const WIDTH: u16 = 40;
        const LEAD: usize = COPY_CHUNK_ROWS as usize - 1;

        let mut lines: Vec<Line<'static>> =
            (0..LEAD).map(|i| Line::from(format!("l{i}"))).collect();
        lines.push(Line::default());
        lines.push(Line::default());
        lines.push(Line::from("tail"));

        let body = (0..LEAD)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            copy_whole_segment(lines, WIDTH),
            format!("{body}\n\n\ntail")
        );
    }
}
