use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::animation::spinner_frame;
use crate::theme;

use super::code_view::SectionFlags;
use super::tool_display::ToolLines;
use super::{GroupEntry, GroupThinkingEntry, GroupToolEntry, ToolGroupRole, ToolStatus};

pub const MAX_COMPACT_ENTRIES: usize = 10;
const RAIL_CONTINUE: &str = "│  ";
const RAIL_LAST: &str = "└  ";

pub fn format_tool_duration(dur: Duration) -> String {
    let millis = dur.as_millis();
    if millis < 1000 {
        format!("{millis}ms")
    } else {
        let secs = dur.as_secs_f64();
        if secs < 10.0 {
            format!("{secs:.2}s")
        } else {
            format!("{secs:.1}s")
        }
    }
}

pub fn format_thinking_tokens(tokens: usize) -> String {
    if tokens < 1000 {
        format!("({tokens}t)")
    } else {
        let k = tokens as f64 / 1000.0;
        if k < 100.0 {
            format!("({k:.1}k)")
        } else {
            format!("({:.0}k)", k.round())
        }
    }
}

fn single_line(s: &str) -> String {
    let trimmed = s.trim();
    let mut result = String::with_capacity(trimmed.len());
    let mut prev_space = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                result.push(' ');
                prev_space = true;
            }
        } else {
            result.push(ch);
            prev_space = false;
        }
    }
    result
}

fn truncate_to_width(s: &str, max_w: usize) -> String {
    if s.width() <= max_w {
        return s.to_string();
    }
    if max_w <= 3 {
        return "...".chars().take(max_w).collect();
    }
    let target = max_w - 3;
    let mut cur_w = 0;
    let mut end_idx = 0;
    for (idx, ch) in s.char_indices() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if cur_w + cw > target {
            break;
        }
        cur_w += cw;
        end_idx = idx + ch.len_utf8();
    }
    format!("{}...", &s[..end_idx])
}

fn render_tool_row(
    rail: &str,
    tool: &GroupToolEntry,
    spinner_frame_char: char,
    width: u16,
    is_dim: bool,
    spinner_lines: &mut Vec<(usize, usize)>,
    line_idx: usize,
) -> Line<'static> {
    let theme = theme::current();
    let is_in_progress = tool.status == ToolStatus::InProgress;
    let (icon, icon_style) = match tool.status {
        ToolStatus::InProgress => (spinner_frame_char.to_string(), theme.spinner),
        ToolStatus::Success => (
            "✓".to_string(),
            if is_dim {
                theme.tool_dim
            } else {
                theme.tool_success
            },
        ),
        ToolStatus::Error => (
            "✗".to_string(),
            if is_dim {
                theme.tool_dim
            } else {
                theme.tool_error
            },
        ),
    };

    let mut spans = Vec::new();
    let mut span_idx = 0;
    if !rail.is_empty() {
        spans.push(Span::styled(rail.to_string(), theme.tool_dim));
        span_idx += 1;
    }

    if is_in_progress {
        spinner_lines.push((line_idx, span_idx));
    }
    spans.push(Span::styled(icon, icon_style));
    spans.push(Span::raw(" "));

    let name_style = if is_dim {
        theme.tool_dim
    } else {
        theme.tool.add_modifier(Modifier::BOLD)
    };
    spans.push(Span::styled(tool.name.to_string(), name_style));

    let duration_str = tool
        .duration
        .or_else(|| tool.started_at.map(|st| st.elapsed()))
        .map(|d| format!("({})", format_tool_duration(d)));

    let right_str = duration_str.unwrap_or_default();
    let right_len = right_str.width();
    let right_style = if is_dim {
        theme.tool_dim
    } else {
        theme.status_dim
    };

    let left_len = rail.width() + 1 + 1 + tool.name.width();
    let summary_clean = single_line(&tool.summary);

    if !summary_clean.is_empty() {
        let max_summary_len = (width as usize).saturating_sub(left_len + 1 + right_len + 1);
        if max_summary_len > 3 {
            let truncated = truncate_to_width(&summary_clean, max_summary_len);
            let trunc_w = truncated.width();
            let summary_style = if is_dim {
                theme.tool_dim
            } else {
                theme.status_dim
            };
            spans.push(Span::raw(" "));
            spans.push(Span::styled(truncated, summary_style));

            let current_len = left_len + 1 + trunc_w;
            let gap = (width as usize)
                .saturating_sub(current_len + right_len)
                .max(1);
            spans.push(Span::raw(" ".repeat(gap)));
            if !right_str.is_empty() {
                spans.push(Span::styled(right_str, right_style));
            }
            return Line::from(spans);
        }
    }

    let gap = (width as usize).saturating_sub(left_len + right_len).max(1);
    spans.push(Span::raw(" ".repeat(gap)));
    if !right_str.is_empty() {
        spans.push(Span::styled(right_str, right_style));
    }
    Line::from(spans)
}

fn render_thinking_row(
    rail: &str,
    thinking: &GroupThinkingEntry,
    spinner_frame_char: char,
    width: u16,
    is_dim: bool,
    spinner_lines: &mut Vec<(usize, usize)>,
    line_idx: usize,
) -> Line<'static> {
    let theme = theme::current();
    let (icon, icon_style) = if thinking.active {
        (spinner_frame_char.to_string(), theme.spinner)
    } else {
        (
            "✓".to_string(),
            if is_dim {
                theme.tool_dim
            } else {
                theme.tool_success
            },
        )
    };

    let mut spans = Vec::new();
    let mut span_idx = 0;
    if !rail.is_empty() {
        spans.push(Span::styled(rail.to_string(), theme.tool_dim));
        span_idx += 1;
    }

    if thinking.active {
        spinner_lines.push((line_idx, span_idx));
    }
    spans.push(Span::styled(icon, icon_style));
    spans.push(Span::raw(" "));

    let preview = single_line(&thinking.text.replace(['*', '_', '#', '`', '>'], ""));
    let label = if preview.is_empty() {
        if thinking.active {
            "thinking..."
        } else {
            "thinking"
        }
    } else {
        "thinking"
    };

    let label_style = if is_dim {
        theme.tool_dim
    } else {
        theme.tool.add_modifier(Modifier::BOLD)
    };
    spans.push(Span::styled(label.to_string(), label_style));

    let right_str = if thinking.tokens > 0 {
        format_thinking_tokens(thinking.tokens)
    } else {
        String::new()
    };
    let right_len = right_str.width();
    let right_style = if is_dim {
        theme.tool_dim
    } else {
        theme.status_dim
    };

    let left_len = rail.width() + 1 + 1 + label.width();

    if !preview.is_empty() {
        let max_preview_len = (width as usize).saturating_sub(left_len + 1 + right_len + 1);
        if max_preview_len > 3 {
            let truncated = truncate_to_width(&preview, max_preview_len);
            let trunc_w = truncated.width();
            let content_style = if is_dim {
                theme.tool_dim
            } else {
                theme.thinking
            };
            spans.push(Span::raw(" "));
            spans.push(Span::styled(truncated, content_style));

            let current_len = left_len + 1 + trunc_w;
            let gap = (width as usize)
                .saturating_sub(current_len + right_len)
                .max(1);
            spans.push(Span::raw(" ".repeat(gap)));
            if !right_str.is_empty() {
                spans.push(Span::styled(right_str, right_style));
            }
            return Line::from(spans);
        }
    }

    let gap = (width as usize).saturating_sub(left_len + right_len).max(1);
    spans.push(Span::raw(" ".repeat(gap)));
    if !right_str.is_empty() {
        spans.push(Span::styled(right_str, right_style));
    }
    Line::from(spans)
}

pub fn build_tool_group_lines(
    group: &ToolGroupRole,
    width: u16,
    spinner_millis: u128,
) -> ToolLines {
    let mut lines = Vec::new();
    let mut spinner_lines = Vec::new();
    let frame_char = spinner_frame(spinner_millis);

    let has_pending_tool = group
        .entries
        .iter()
        .any(|e| matches!(e, GroupEntry::Tool(t) if t.status == ToolStatus::InProgress));
    let has_active_thinking = group
        .entries
        .iter()
        .any(|e| matches!(e, GroupEntry::Thinking(th) if th.active));
    let has_tools = group
        .entries
        .iter()
        .any(|e| matches!(e, GroupEntry::Tool(_)));
    let has_thinking = group
        .entries
        .iter()
        .any(|e| matches!(e, GroupEntry::Thinking(_)));
    let working =
        has_pending_tool || has_active_thinking || (!group.sealed && group.entries.is_empty());

    if group.entries.len() == 1 && !working {
        let entry = &group.entries[0];
        let line = match entry {
            GroupEntry::Tool(t) => {
                render_tool_row("", t, frame_char, width, false, &mut spinner_lines, 0)
            }
            GroupEntry::Thinking(th) => {
                render_thinking_row("", th, frame_char, width, false, &mut spinner_lines, 0)
            }
        };
        lines.push(line);
        return ToolLines {
            lines,
            highlight: None,
            spinner_lines,
            snapshot_base: None,
            content_indent: "",
            truncation: SectionFlags::default(),
        };
    }

    let theme = theme::current();
    let (icon, icon_style) = if working {
        (frame_char.to_string(), theme.spinner)
    } else {
        ("✓".to_string(), theme.tool_success)
    };

    let (state_text, state_style) = if has_pending_tool {
        ("tool calling...", theme.accent)
    } else if !has_tools {
        if working {
            ("thinking...", theme.thinking)
        } else {
            ("thinking", theme.thinking)
        }
    } else if has_active_thinking {
        ("thinking...", theme.thinking)
    } else {
        let text = if has_thinking { "done" } else { "tools done" };
        (text, theme.tool_success)
    };

    let hidden_count = group.entries.len().saturating_sub(MAX_COMPACT_ENTRIES);
    let hidden_suffix = if hidden_count > 0 {
        format!(" ({hidden_count} hidden)")
    } else {
        String::new()
    };

    let mut header_spans = Vec::new();
    if working {
        spinner_lines.push((0, 0));
    }
    header_spans.push(Span::styled(format!("{icon} "), icon_style));
    header_spans.push(Span::styled(state_text, state_style));
    if !hidden_suffix.is_empty() {
        header_spans.push(Span::styled(hidden_suffix, theme.status_dim));
    }
    lines.push(Line::from(header_spans));

    let visible_start = if hidden_count > 0 { hidden_count } else { 0 };
    let visible_entries = &group.entries[visible_start..];
    let total_visible = visible_entries.len();

    for (idx, entry) in visible_entries.iter().enumerate() {
        let is_last = idx == total_visible - 1;
        let rail = if is_last { RAIL_LAST } else { RAIL_CONTINUE };
        let is_dim = hidden_count > 0 && idx == 0;
        let line_idx = lines.len();

        let line = match entry {
            GroupEntry::Tool(t) => render_tool_row(
                rail,
                t,
                frame_char,
                width,
                is_dim,
                &mut spinner_lines,
                line_idx,
            ),
            GroupEntry::Thinking(th) => render_thinking_row(
                rail,
                th,
                frame_char,
                width,
                is_dim,
                &mut spinner_lines,
                line_idx,
            ),
        };
        lines.push(line);
    }

    ToolLines {
        lines,
        highlight: None,
        spinner_lines,
        snapshot_base: None,
        content_indent: "",
        truncation: SectionFlags::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn format_durations() {
        assert_eq!(format_tool_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_tool_duration(Duration::from_millis(7430)), "7.43s");
        assert_eq!(format_tool_duration(Duration::from_millis(15900)), "15.9s");
    }

    #[test]
    fn format_tokens() {
        assert_eq!(format_thinking_tokens(350), "(350t)");
        assert_eq!(format_thinking_tokens(1200), "(1.2k)");
        assert_eq!(format_thinking_tokens(150_000), "(150k)");
    }

    #[test]
    fn single_completed_tool_renders_single_line_without_header() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: true,
            entries: vec![GroupEntry::Tool(GroupToolEntry {
                id: "call-1".into(),
                name: Arc::from("bash"),
                summary: "cargo check".into(),
                status: ToolStatus::Success,
                started_at: None,
                duration: Some(Duration::from_millis(1200)),
            })],
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        assert_eq!(tl.lines.len(), 1);
        let text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("✓"));
        assert!(text.contains("bash"));
        assert!(text.contains("cargo check"));
        assert!(text.contains("(1.20s)"));
        assert!(!text.contains("tools done"));
    }

    #[test]
    fn multiple_tools_render_header_and_tree() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: true,
            entries: vec![
                GroupEntry::Tool(GroupToolEntry {
                    id: "call-1".into(),
                    name: Arc::from("edit"),
                    summary: "foo.rs".into(),
                    status: ToolStatus::Success,
                    started_at: None,
                    duration: Some(Duration::from_millis(7430)),
                }),
                GroupEntry::Tool(GroupToolEntry {
                    id: "call-2".into(),
                    name: Arc::from("bash"),
                    summary: "cargo test".into(),
                    status: ToolStatus::Error,
                    started_at: None,
                    duration: Some(Duration::from_millis(15900)),
                }),
            ],
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        assert_eq!(tl.lines.len(), 3);
        let header_text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header_text.contains("tools done"));

        let row1: String = tl.lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(row1.contains(RAIL_CONTINUE));
        assert!(row1.contains("edit"));
        assert!(row1.contains("(7.43s)"));

        let row2: String = tl.lines[2]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(row2.contains(RAIL_LAST));
        assert!(row2.contains("✗"));
        assert!(row2.contains("bash"));
        assert!(row2.contains("(15.9s)"));
    }

    #[test]
    fn hides_beyond_max_entries() {
        let mut entries = Vec::new();
        for i in 0..15 {
            entries.push(GroupEntry::Tool(GroupToolEntry {
                id: format!("call-{i}"),
                name: Arc::from("read"),
                summary: format!("file_{i}.rs"),
                status: ToolStatus::Success,
                started_at: None,
                duration: Some(Duration::from_millis(100)),
            }));
        }

        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: true,
            entries,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        // Header + 10 visible entries
        assert_eq!(tl.lines.len(), 11);
        let header_text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header_text.contains("(5 hidden)"));
    }
}
