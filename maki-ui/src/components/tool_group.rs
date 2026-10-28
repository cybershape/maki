use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::animation::spinner_frame;
use crate::theme;

use super::code_view::SectionFlags;
use super::tool_display::ToolLines;
use super::{GroupEntry, GroupThinkingEntry, GroupToolEntry, ToolGroupRole, ToolStatus};

pub const MAX_COMPACT_ENTRIES: usize = 10;
const GROUP_PADDING: &str = " ";
const RAIL_CONTINUE: &str = "│  ";
const RAIL_LAST: &str = "└  ";

const THINKING_FRAMES: [&str; 4] = ["·", "•", "●", "•"];
const THINKING_INTERVAL_MS: u128 = 140;

pub fn thinking_spinner_frame(elapsed_ms: u128) -> (&'static str, Style) {
    let theme = theme::current();
    let idx = ((elapsed_ms / THINKING_INTERVAL_MS) % 4) as usize;
    let frame = THINKING_FRAMES[idx];
    let style = match idx {
        0 => theme.tool_dim,
        1 | 3 => theme.status_dim,
        2 => theme.accent,
        _ => theme.tool_dim,
    };
    (frame, style)
}

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

pub fn format_group_duration(dur: Duration) -> String {
    let total_secs = dur.as_secs();
    if total_secs >= 3600 {
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        let secs = total_secs % 60;
        format!("{hours}h{mins}m{secs}s")
    } else if total_secs >= 60 {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{mins}m{secs}s")
    } else if total_secs > 0 {
        format!("{total_secs}s")
    } else {
        let millis = dur.as_millis();
        if millis > 0 {
            format!("{millis}ms")
        } else {
            "0s".to_string()
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
        ToolStatus::InProgress => (format!("{spinner_frame_char} "), theme.spinner),
        ToolStatus::Success => (
            "✓ ".to_string(),
            if is_dim {
                theme.tool_dim
            } else {
                theme.tool_success
            },
        ),
        ToolStatus::Error => (
            "✗ ".to_string(),
            if is_dim {
                theme.tool_dim
            } else {
                theme.tool_error
            },
        ),
    };

    let mut spans = Vec::new();
    spans.push(Span::raw(GROUP_PADDING));
    let mut span_idx = 1;

    if !rail.is_empty() {
        spans.push(Span::styled(rail.to_string(), theme.tool_dim));
        span_idx += 1;
    }

    if is_in_progress {
        spinner_lines.push((line_idx, span_idx));
    }
    let icon_width = icon.width();
    spans.push(Span::styled(icon, icon_style));

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

    let left_len = GROUP_PADDING.width() + rail.width() + icon_width + tool.name.width();
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
    spinner_millis: u128,
    width: u16,
    is_dim: bool,
    _line_idx: usize,
) -> Line<'static> {
    let theme = theme::current();
    let (icon, icon_style) = if thinking.active {
        let (f, s) = thinking_spinner_frame(spinner_millis);
        (format!("{f} "), s)
    } else {
        (
            "✓ ".to_string(),
            if is_dim {
                theme.tool_dim
            } else {
                theme.tool_success
            },
        )
    };

    let mut spans = Vec::new();
    spans.push(Span::raw(GROUP_PADDING));

    if !rail.is_empty() {
        spans.push(Span::styled(rail.to_string(), theme.tool_dim));
    }

    let icon_width = icon.width();
    spans.push(Span::styled(icon, icon_style));

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

    let left_len = GROUP_PADDING.width() + rail.width() + icon_width + label.width();

    if !preview.is_empty() {
        let max_preview_len = (width as usize).saturating_sub(left_len + 1 + right_len + 1);
        if max_preview_len > 3 {
            let truncated = truncate_to_width(&preview, max_preview_len);
            let trunc_w = truncated.width();
            let content_style = if is_dim {
                theme.tool_dim
            } else {
                theme.thinking.remove_modifier(Modifier::ITALIC)
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

    let pending_tools: Vec<&GroupToolEntry> = group
        .entries
        .iter()
        .filter_map(|e| match e {
            GroupEntry::Tool(t) if t.status == ToolStatus::InProgress => Some(t),
            _ => None,
        })
        .collect();
    let has_pending_tool = !pending_tools.is_empty();
    let receiving_tool_args = has_pending_tool && pending_tools.iter().all(|t| t.receiving_args);

    let is_thinking = !group.sealed
        && group
            .entries
            .iter()
            .any(|e| matches!(e, GroupEntry::Thinking(th) if th.active));
    let working = !group.sealed;

    if group.sealed && group.entries.is_empty() {
        return ToolLines {
            lines: Vec::new(),
            highlight: None,
            spinner_lines: Vec::new(),
            snapshot_base: None,
            content_indent: "",
            truncation: SectionFlags::default(),
        };
    }

    if group.sealed && group.entries.len() == 1 {
        let entry = &group.entries[0];
        let line = match entry {
            GroupEntry::Tool(t) => {
                render_tool_row("", t, frame_char, width, false, &mut spinner_lines, 0)
            }
            GroupEntry::Thinking(th) => {
                render_thinking_row("", th, spinner_millis, width, false, 0)
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
    let state_text = if has_pending_tool {
        if receiving_tool_args {
            "tool call receiving...".to_string()
        } else {
            "tool calling...".to_string()
        }
    } else if is_thinking {
        "thinking...".to_string()
    } else if working {
        let timer = group
            .waiting_duration()
            .map(|dur| format!(" ({})", format_tool_duration(dur)))
            .unwrap_or_default();
        format!("waiting for response...{timer}")
    } else {
        format!(
            "worked for {}",
            format_group_duration(group.total_duration())
        )
    };

    let (left_icon, left_style) = if has_pending_tool {
        (format!("{frame_char} "), theme.accent)
    } else if is_thinking {
        let (frame, style) = thinking_spinner_frame(spinner_millis);
        (format!("{frame} "), style)
    } else if working {
        (format!("{frame_char} "), theme.thinking)
    } else {
        ("✓ ".to_string(), theme.tool_success)
    };

    let hidden_count = group.entries.len().saturating_sub(MAX_COMPACT_ENTRIES);
    let hidden_suffix = if hidden_count > 0 {
        format!(" ({hidden_count} hidden)")
    } else {
        String::new()
    };

    let mut header_spans = Vec::new();
    header_spans.push(Span::raw(GROUP_PADDING));
    header_spans.push(Span::styled(left_icon, left_style));
    if working && (has_pending_tool || (!is_thinking && working)) {
        spinner_lines.push((0, 1));
    }
    header_spans.push(Span::styled(state_text, Style::default()));
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
            GroupEntry::Thinking(th) => {
                render_thinking_row(rail, th, spinner_millis, width, is_dim, line_idx)
            }
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
    use std::time::Instant;

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
    fn thinking_spinner_frames_and_styles() {
        assert_eq!(thinking_spinner_frame(0).0, "·");
        assert_eq!(thinking_spinner_frame(140).0, "•");
        assert_eq!(thinking_spinner_frame(280).0, "●");
        assert_eq!(thinking_spinner_frame(420).0, "•");
    }

    #[test]
    fn format_group_durations() {
        assert_eq!(format_group_duration(Duration::from_millis(0)), "0s");
        assert_eq!(format_group_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_group_duration(Duration::from_secs(5)), "5s");
        assert_eq!(format_group_duration(Duration::from_secs(95)), "1m35s");
        assert_eq!(format_group_duration(Duration::from_secs(3665)), "1h1m5s");
    }

    #[test]
    fn empty_sealed_group_renders_empty_lines() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: true,
            entries: vec![],
            started_at: None,
            duration: None,
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        assert!(tl.lines.is_empty());
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
                receiving_args: false,
                started_at: None,
                duration: Some(Duration::from_millis(1200)),
            })],
            started_at: None,
            duration: None,
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        assert_eq!(tl.lines.len(), 1);
        let text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.starts_with(" "));
        assert!(text.contains("✓"));
        assert!(text.contains("bash"));
        assert!(text.contains("cargo check"));
        assert!(text.contains("(1.20s)"));
        assert!(!text.contains("worked for"));
    }

    #[test]
    fn waiting_for_response_when_group_empty_and_unsealed() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: false,
            entries: vec![],
            started_at: None,
            duration: None,
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        let header_text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header_text.contains("waiting for response..."));
    }

    #[test]
    fn waiting_for_response_when_all_tools_done_but_unsealed() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: false,
            entries: vec![GroupEntry::Tool(GroupToolEntry {
                id: "call-1".into(),
                name: Arc::from("bash"),
                summary: "ls".into(),
                status: ToolStatus::Success,
                receiving_args: false,
                started_at: None,
                duration: Some(Duration::from_millis(100)),
            })],
            started_at: None,
            duration: None,
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        let header_text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header_text.contains("waiting for response..."));
        assert!(!header_text.contains("worked for"));
    }

    #[test]
    fn waiting_for_response_shows_timer() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: false,
            entries: vec![],
            started_at: None,
            duration: None,
            waiting_since: Some(Instant::now() - Duration::from_millis(1520)),
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        let header_text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header_text.contains("waiting for response... (1.52s)"));
    }

    #[test]
    fn single_tool_unsealed_renders_header_with_calling_state() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: false,
            entries: vec![GroupEntry::Tool(GroupToolEntry {
                id: "call-1".into(),
                name: Arc::from("bash"),
                summary: "cargo check".into(),
                status: ToolStatus::InProgress,
                receiving_args: false,
                started_at: None,
                duration: None,
            })],
            started_at: None,
            duration: None,
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        // Header + 1 tool row
        assert_eq!(tl.lines.len(), 2);
        let header_text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header_text.contains("tool calling..."));
    }

    #[test]
    fn tool_call_receiving_state_when_all_pending_tools_receiving() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: false,
            entries: vec![GroupEntry::Tool(GroupToolEntry {
                id: "call-1".into(),
                name: Arc::from("bash"),
                summary: String::new(),
                status: ToolStatus::InProgress,
                receiving_args: true,
                started_at: None,
                duration: None,
            })],
            started_at: None,
            duration: None,
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        let header_text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header_text.contains("tool call receiving..."));
    }

    #[test]
    fn waiting_for_first_token_when_thinking_empty() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: false,
            entries: vec![GroupEntry::Thinking(GroupThinkingEntry {
                id: 0,
                text: String::new(),
                tokens: 0,
                active: true,
            })],
            started_at: None,
            duration: None,
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        let header_text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header_text.contains("thinking..."));
        assert!(!header_text.contains("waiting for"));
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
                    receiving_args: false,
                    started_at: None,
                    duration: Some(Duration::from_millis(7430)),
                }),
                GroupEntry::Tool(GroupToolEntry {
                    id: "call-2".into(),
                    name: Arc::from("bash"),
                    summary: "cargo test".into(),
                    status: ToolStatus::Error,
                    receiving_args: false,
                    started_at: None,
                    duration: Some(Duration::from_millis(15900)),
                }),
            ],
            started_at: None,
            duration: None,
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        assert_eq!(tl.lines.len(), 3);
        let header_text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header_text.starts_with(" "));
        assert!(header_text.contains("worked for 23s"));

        let row1: String = tl.lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(row1.starts_with(" "));
        assert!(row1.contains(RAIL_CONTINUE));
        assert!(row1.contains("edit"));
        assert!(row1.contains("(7.43s)"));

        let row2: String = tl.lines[2]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(row2.starts_with(" "));
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
                receiving_args: false,
                started_at: None,
                duration: Some(Duration::from_millis(100)),
            }));
        }

        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: true,
            entries,
            started_at: None,
            duration: None,
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        assert_eq!(tl.lines.len(), 11);
        let header_text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header_text.contains("(5 hidden)"));
    }

    #[test]
    fn single_space_after_busy_and_done_icons() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: false,
            entries: vec![
                GroupEntry::Tool(GroupToolEntry {
                    id: "call-1".into(),
                    name: Arc::from("bash"),
                    summary: "git status".into(),
                    status: ToolStatus::Success,
                    receiving_args: false,
                    started_at: None,
                    duration: Some(Duration::from_millis(100)),
                }),
                GroupEntry::Tool(GroupToolEntry {
                    id: "call-2".into(),
                    name: Arc::from("bash"),
                    summary: "Run compact_test".into(),
                    status: ToolStatus::InProgress,
                    receiving_args: false,
                    started_at: None,
                    duration: None,
                }),
            ],
            started_at: None,
            duration: None,
            waiting_since: None,
        };

        let mut tl = build_tool_group_lines(&group, 80, 0);
        let done_line: String = tl.lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            done_line.contains("✓ bash"),
            "done row should have 1 space: {done_line}"
        );
        assert!(!done_line.contains("✓  bash"));

        let busy_line: String = tl.lines[2]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            busy_line.contains("⠋ bash"),
            "busy row should have 1 space: {busy_line}"
        );
        assert!(!busy_line.contains("⠋  bash"));

        // When updated by update_spinners
        let spinner_span = Span::styled("⠙ ", theme::current().spinner);
        for &(line_idx, span_idx) in &tl.spinner_lines {
            if let Some(line) = tl.lines.get_mut(line_idx)
                && line.spans.len() > span_idx
            {
                line.spans[span_idx] = spinner_span.clone();
            }
        }
        let updated_busy_line: String = tl.lines[2]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            updated_busy_line.contains("⠙ bash"),
            "updated busy row should still have 1 space: {updated_busy_line}"
        );
        assert!(!updated_busy_line.contains("⠙  bash"));
    }

    #[test]
    fn group_title_has_default_style() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: false,
            entries: vec![GroupEntry::Tool(GroupToolEntry {
                id: "call-1".into(),
                name: Arc::from("bash"),
                summary: "ls".into(),
                status: ToolStatus::InProgress,
                receiving_args: false,
                started_at: None,
                duration: None,
            })],
            started_at: None,
            duration: None,
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        let title_span = &tl.lines[0].spans[2];
        assert_eq!(title_span.content.as_ref(), "tool calling...");
        assert_eq!(title_span.style, Style::default());
    }

    #[test]
    fn thinking_preview_has_no_italic_modifier() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: false,
            entries: vec![GroupEntry::Thinking(GroupThinkingEntry {
                id: 0,
                text: "analyzing requirements".into(),
                tokens: 10,
                active: false,
            })],
            started_at: None,
            duration: None,
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        let preview_span = tl.lines[1]
            .spans
            .iter()
            .find(|s| s.content.contains("analyzing requirements"))
            .expect("preview span found");
        assert!(!preview_span.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn sealed_group_title_shows_worked_for_and_hidden() {
        let mut entries = Vec::new();
        for i in 0..22 {
            entries.push(GroupEntry::Tool(GroupToolEntry {
                id: format!("c{i}"),
                name: Arc::from("tool"),
                summary: format!("op {i}"),
                status: ToolStatus::Success,
                receiving_args: false,
                started_at: None,
                duration: None,
            }));
        }
        let group = ToolGroupRole {
            id: "group-done".into(),
            sealed: true,
            entries,
            started_at: None,
            duration: Some(Duration::from_secs(95)),
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        let header: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header.contains("worked for 1m35s (12 hidden)"));
    }
}
