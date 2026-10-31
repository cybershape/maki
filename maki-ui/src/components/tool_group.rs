use std::path::Path;
use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use maki_agent::tools::schema::TOOLCALL_REASON_FIELD;
use maki_agent::{BufferSnapshot, SpanStyle};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::theme;

use super::code_view::SectionFlags;
use super::tool_display::ToolLines;
use super::{GroupEntry, GroupThinkingEntry, GroupToolEntry, ToolGroupRole, ToolStatus};

pub const MAX_COMPACT_ENTRIES: usize = 10;
const RAIL_CONTINUE: &str = "│  ";
const RAIL_LAST: &str = "└  ";
const CHILD_RAIL_CONTINUE: &str = "├  ";
const CHILD_INDENT: &str = "   ";
pub(super) const BATCH_TOOL: &str = "batch";
const CHILD_PREFIX_STYLE: &str = "tool_prefix";
const CHILD_SUCCESS_STYLE: &str = "tool_success";
const CHILD_ERROR_STYLE: &str = "tool_error";
const CHILD_DURATION_STYLE: &str = "batch_duration_ms";
const CHILD_DURATION_PREFIX: &str = " (";
const CHILD_DURATION_SUFFIX: &str = "ms)";
const REASON_SEP: &str = " · ";

const BUSY_FRAMES: [&str; 4] = ["·", "•", "●", "•"];
const BUSY_INTERVAL_MS: u128 = 140;

pub fn busy_spinner_frame(elapsed_ms: u128) -> (&'static str, Style) {
    let theme = theme::current();
    let idx = ((elapsed_ms / BUSY_INTERVAL_MS) % 4) as usize;
    let frame = BUSY_FRAMES[idx];
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

pub fn format_group_summary(tools: usize, thoughts: usize) -> String {
    let tool_part = match tools {
        0 => None,
        1 => Some("calling one tool".to_string()),
        n => Some(format!("calling {n} tools")),
    };
    let thought_part = match thoughts {
        0 => None,
        1 => Some("thinking once".to_string()),
        n => Some(format!("thinking {n} times")),
    };
    match (tool_part, thought_part) {
        (Some(tool), Some(thought)) => format!(" ({tool}, {thought})"),
        (Some(tool), None) => format!(" ({tool})"),
        (None, Some(thought)) => format!(" ({thought})"),
        (None, None) => String::new(),
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

fn shorten_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let normalized = path.replace('\\', "/");
    let p = Path::new(&normalized);
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_norm = cwd.to_string_lossy().replace('\\', "/");
        if let Ok(rel) = p.strip_prefix(Path::new(&cwd_norm)) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            return if rel_str.is_empty() {
                ".".to_string()
            } else {
                rel_str
            };
        }
    }
    if let Some(home) = maki_storage::paths::home() {
        let home_norm = home.to_string_lossy().replace('\\', "/");
        if let Ok(rel) = p.strip_prefix(Path::new(&home_norm)) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            return if rel_str.is_empty() {
                "~".to_string()
            } else {
                format!("~/{rel_str}")
            };
        }
    }
    normalized
}

pub fn format_compact_summary(
    tool_name: &str,
    raw_input: Option<&Value>,
    default_summary: &str,
) -> String {
    let reason = raw_input
        .and_then(|v| v.get(TOOLCALL_REASON_FIELD))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let Some(reason) = reason else {
        return default_summary.to_string();
    };

    let Some(input) = raw_input else {
        return reason.to_string();
    };

    match tool_name {
        "read" => {
            let path = input.get("path").and_then(Value::as_str).map(shorten_path);
            if let Some(p) = path.filter(|s| !s.is_empty()) {
                let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(1);
                let orig_summary = if let Some(limit) = input
                    .get("limit")
                    .and_then(Value::as_u64)
                    .filter(|&l| l > 0)
                {
                    format!("{p}:{offset}-{}", offset + limit - 1)
                } else {
                    format!("{p}:{offset}")
                };
                format!("{reason}{REASON_SEP}{orig_summary}")
            } else if !default_summary.is_empty() && default_summary != reason {
                format!("{reason}{REASON_SEP}{default_summary}")
            } else {
                reason.to_string()
            }
        }
        "write" => {
            let path = input.get("path").and_then(Value::as_str).map(shorten_path);
            if let Some(p) = path.filter(|s| !s.is_empty()) {
                format!("{reason}{REASON_SEP}{p}")
            } else {
                reason.to_string()
            }
        }
        "edit" | "multiedit" | "edit_lines" | "insert_lines" => {
            let path = input.get("path").and_then(Value::as_str).map(shorten_path);
            if let Some(p) = path.filter(|s| !s.is_empty()) {
                format!("{reason}{REASON_SEP}{p}")
            } else {
                reason.to_string()
            }
        }
        "index" => {
            let path = input.get("path").and_then(Value::as_str).map(shorten_path);
            if let Some(p) = path.filter(|s| !s.is_empty()) {
                format!("{reason}{REASON_SEP}{p}")
            } else {
                reason.to_string()
            }
        }
        "grep" => {
            let path = input.get("path").and_then(Value::as_str).map(shorten_path);
            if let Some(p) = path.filter(|s| !s.is_empty() && s != ".") {
                format!("{reason}{REASON_SEP}{p}")
            } else {
                reason.to_string()
            }
        }
        "glob" => {
            let pattern = input.get("pattern").and_then(Value::as_str).map(str::trim);
            if let Some(p) = pattern.filter(|s| !s.is_empty()) {
                format!("{reason}{REASON_SEP}{p}")
            } else {
                reason.to_string()
            }
        }
        "list_dir" => {
            let path = input.get("path").and_then(Value::as_str).map(shorten_path);
            if let Some(p) = path.filter(|s| !s.is_empty()) {
                format!("{reason}{REASON_SEP}{p}")
            } else {
                reason.to_string()
            }
        }
        "view_image" => {
            let path = input
                .get("path")
                .or_else(|| input.get("file_path"))
                .and_then(Value::as_str)
                .map(shorten_path);
            if let Some(p) = path.filter(|s| !s.is_empty()) {
                format!("{reason}{REASON_SEP}{p}")
            } else {
                reason.to_string()
            }
        }
        "batch" => {
            let count = input
                .get("tool_calls")
                .and_then(Value::as_array)
                .map(|arr| arr.len());
            if let Some(n) = count {
                format!("{reason}{REASON_SEP}{n} tools")
            } else {
                reason.to_string()
            }
        }
        "websearch" => {
            let query = input.get("query").and_then(Value::as_str).map(str::trim);
            if let Some(q) = query.filter(|s| !s.is_empty()) {
                format!("{reason}{REASON_SEP}{q}")
            } else {
                reason.to_string()
            }
        }
        _ => reason.to_string(),
    }
}

pub fn update_batch_compact_summary(current_summary: &str, count: usize) -> String {
    match current_summary.split_once(REASON_SEP) {
        Some((reason, _)) => format!("{reason}{REASON_SEP}{count} tools"),
        None => format!("{count} tools"),
    }
}

pub fn extract_batch_child_input(batch_raw_input: &Value, index: usize) -> Option<Value> {
    let tool_calls = batch_raw_input.get("tool_calls")?.as_array()?;
    let entry = tool_calls.get(index)?.as_object()?;
    let mut map = serde_json::Map::new();
    if let Some(nested) = entry.get("parameters").and_then(Value::as_object) {
        for (k, v) in nested {
            map.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in entry {
        if k != "tool" && k != "parameters" {
            map.insert(k.clone(), v.clone());
        }
    }
    Some(Value::Object(map))
}

pub(super) fn batch_children(
    snapshot: &BufferSnapshot,
    batch_raw_input: Option<&Value>,
) -> Vec<GroupToolEntry> {
    let mut child_idx = 0;
    snapshot
        .lines
        .iter()
        .filter_map(|line| {
            let [indicator, prefix, header @ ..] = line.spans.as_slice() else {
                return None;
            };
            if !matches!(&prefix.style, SpanStyle::Named(style) if style == CHILD_PREFIX_STYLE) {
                return None;
            }
            let name = prefix.text.strip_suffix("> ")?;
            let status = match &indicator.style {
                SpanStyle::Named(style) if style == CHILD_SUCCESS_STYLE => ToolStatus::Success,
                SpanStyle::Named(style) if style == CHILD_ERROR_STYLE => ToolStatus::Error,
                _ => ToolStatus::InProgress,
            };
            let is_duration = |style: &SpanStyle| {
                matches!(style, SpanStyle::Named(name) if name == CHILD_DURATION_STYLE)
            };
            let duration = header
                .iter()
                .find(|span| is_duration(&span.style))
                .and_then(|span| span.text.strip_prefix(CHILD_DURATION_PREFIX))
                .and_then(|text| text.strip_suffix(CHILD_DURATION_SUFFIX))
                .and_then(|text| text.parse().ok())
                .map(Duration::from_millis);
            let idx = child_idx;
            child_idx += 1;
            let default_summary: String = header
                .iter()
                .filter(|span| !is_duration(&span.style))
                .map(|span| span.text.as_str())
                .collect();
            let child_input =
                batch_raw_input.and_then(|input| extract_batch_child_input(input, idx));
            let summary = format_compact_summary(name, child_input.as_ref(), &default_summary);
            Some(GroupToolEntry {
                id: String::new(),
                name: name.into(),
                summary,
                raw_input: child_input,
                children: Vec::new(),
                status,
                receiving_args: false,
                started_at: None,
                duration,
            })
        })
        .collect()
}

fn push_tool_rows(
    lines: &mut Vec<Line<'static>>,
    spinner_lines: &mut Vec<(usize, usize)>,
    tool: &GroupToolEntry,
    rail: &str,
    frame: (&str, Style),
    width: u16,
) {
    lines.push(render_tool_row(
        rail,
        tool,
        frame,
        width,
        spinner_lines,
        lines.len(),
    ));
    let parent_rail = match rail {
        RAIL_LAST => CHILD_INDENT,
        _ => rail,
    };
    for (idx, child) in tool.children.iter().enumerate() {
        let branch = if idx + 1 == tool.children.len() {
            RAIL_LAST
        } else {
            CHILD_RAIL_CONTINUE
        };
        let child_rail = format!("{parent_rail}{branch}");
        lines.push(render_tool_row(
            &child_rail,
            child,
            frame,
            width,
            spinner_lines,
            lines.len(),
        ));
    }
}

fn render_tool_row(
    rail: &str,
    tool: &GroupToolEntry,
    frame: (&str, Style),
    width: u16,
    spinner_lines: &mut Vec<(usize, usize)>,
    line_idx: usize,
) -> Line<'static> {
    let theme = theme::current();
    let is_in_progress = tool.status == ToolStatus::InProgress;
    let (icon, icon_style) = match tool.status {
        ToolStatus::InProgress => (format!("{} ", frame.0), frame.1),
        ToolStatus::Success => ("✓ ".to_string(), theme.tool_success),
        ToolStatus::Error => ("✗ ".to_string(), theme.tool_error),
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
    let icon_width = icon.width();
    spans.push(Span::styled(icon, icon_style));

    let name_style = theme.tool.add_modifier(Modifier::BOLD);
    spans.push(Span::styled(tool.name.to_string(), name_style));

    let duration_str = tool
        .duration
        .or_else(|| tool.started_at.map(|st| st.elapsed()))
        .map(|d| format!("({})", format_tool_duration(d)));

    let right_str = duration_str.unwrap_or_default();
    let right_len = right_str.width();
    let right_style = theme.status_dim;

    let left_len = rail.width() + icon_width + tool.name.width();
    let summary_clean = single_line(&tool.summary);

    if !summary_clean.is_empty() {
        let max_summary_len = (width as usize).saturating_sub(left_len + 1 + right_len + 1);
        if max_summary_len > 3 {
            let truncated = truncate_to_width(&summary_clean, max_summary_len);
            let trunc_w = truncated.width();
            let summary_style = theme.status_dim;
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
    _line_idx: usize,
) -> Line<'static> {
    let theme = theme::current();
    let (icon, icon_style) = if thinking.active {
        let (f, s) = busy_spinner_frame(spinner_millis);
        (format!("{f} "), s)
    } else {
        ("✓ ".to_string(), theme.tool_success)
    };

    let mut spans = Vec::new();

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

    let label_style = theme.tool.add_modifier(Modifier::BOLD);
    spans.push(Span::styled(label.to_string(), label_style));

    let duration_str = thinking
        .duration
        .or_else(|| thinking.started_at.map(|st| st.elapsed()))
        .map(|d| format!("({})", format_tool_duration(d)));
    let right_str = duration_str.unwrap_or_default();
    let right_len = right_str.width();
    let right_style = theme.status_dim;

    let left_len = rail.width() + icon_width + label.width();

    if !preview.is_empty() {
        let max_preview_len = (width as usize).saturating_sub(left_len + 1 + right_len + 1);
        if max_preview_len > 3 {
            let truncated = truncate_to_width(&preview, max_preview_len);
            let trunc_w = truncated.width();
            let content_style = theme.thinking.remove_modifier(Modifier::ITALIC);
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
    let frame = busy_spinner_frame(spinner_millis);

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
            elapsed: None,
        };
    }

    let theme = theme::current();
    let state_text = if has_pending_tool {
        if receiving_tool_args {
            "Tool call receiving...".to_string()
        } else {
            "Tool calling...".to_string()
        }
    } else if is_thinking {
        "Thinking...".to_string()
    } else if working {
        let timer = group
            .waiting_duration()
            .map(|dur| format!(" ({})", format_tool_duration(dur)))
            .unwrap_or_default();
        format!("Waiting for response...{timer}")
    } else {
        format!(
            "Worked for {}",
            format_group_duration(group.total_duration())
        )
    };

    let (left_icon, left_style) = if has_pending_tool || working {
        (format!("{} ", frame.0), frame.1)
    } else {
        ("✓ ".to_string(), theme.tool_success)
    };

    let mut total_tools = 0;
    let mut total_thoughts = 0;
    for entry in &group.entries {
        match entry {
            GroupEntry::Tool(_) => total_tools += 1,
            GroupEntry::Thinking(_) => total_thoughts += 1,
        }
    }
    let summary_suffix = format_group_summary(total_tools, total_thoughts);

    let mut header_spans = Vec::new();
    header_spans.push(Span::styled(left_icon, left_style));
    if working && (has_pending_tool || (!is_thinking && working)) {
        spinner_lines.push((0, 0));
    }
    header_spans.push(Span::styled(state_text, Style::default()));
    if !summary_suffix.is_empty() {
        header_spans.push(Span::styled(summary_suffix, theme.status_dim));
    }
    lines.push(Line::from(header_spans));

    let hidden_count = group.entries.len().saturating_sub(MAX_COMPACT_ENTRIES);
    if hidden_count > 0 {
        lines.push(Line::from(vec![
            Span::styled(RAIL_CONTINUE, theme.tool_dim),
            Span::styled(format!("... ({hidden_count} hidden)"), theme.tool_dim),
        ]));
    }

    let visible_start = if hidden_count > 0 { hidden_count } else { 0 };
    let visible_entries = &group.entries[visible_start..];
    let total_visible = visible_entries.len();

    for (idx, entry) in visible_entries.iter().enumerate() {
        let is_last = idx == total_visible - 1;
        let rail = if is_last { RAIL_LAST } else { RAIL_CONTINUE };
        let line_idx = lines.len();

        match entry {
            GroupEntry::Tool(t) => {
                push_tool_rows(&mut lines, &mut spinner_lines, t, rail, frame, width)
            }
            GroupEntry::Thinking(th) => lines.push(render_thinking_row(
                rail,
                th,
                spinner_millis,
                width,
                line_idx,
            )),
        }
    }

    ToolLines {
        lines,
        highlight: None,
        spinner_lines,
        snapshot_base: None,
        content_indent: "",
        truncation: SectionFlags::default(),
        elapsed: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;
    use test_case::test_case;

    #[test_case(false, false; "single_batch")]
    #[test_case(true, false; "batch_followed_by_tool")]
    #[test_case(false, true; "batch_last_in_group")]
    fn batch_renders_child_tree(followed_by_tool: bool, preceded_by_tool: bool) {
        let child = GroupToolEntry {
            id: String::new(),
            name: Arc::from("read"),
            summary: "src/main.rs".into(),
            raw_input: None,
            children: Vec::new(),
            status: ToolStatus::Success,
            receiving_args: false,
            started_at: None,
            duration: None,
        };
        let mut batch = child.clone();
        batch.name = Arc::from(BATCH_TOOL);
        batch.summary = "2 tools".into();
        batch.duration = Some(Duration::from_secs(1));
        batch.children = vec![child.clone(), child.clone()];
        batch.children[1].name = Arc::from("bash");
        batch.children[1].summary = "cargo check".into();
        batch.children[1].status = ToolStatus::Error;
        let mut group = ToolGroupRole {
            id: "group".into(),
            sealed: true,
            entries: vec![GroupEntry::Tool(batch)],
            started_at: None,
            duration: None,
            waiting_since: None,
        };
        if followed_by_tool {
            group.entries.push(GroupEntry::Tool(child.clone()));
        }
        if preceded_by_tool {
            group.entries.insert(0, GroupEntry::Tool(child));
        }
        let rendered = build_tool_group_lines(&group, 60, 0);
        let parent_idx = 1 + usize::from(preceded_by_tool);
        let parent = rendered.lines[parent_idx].to_string();
        assert!(parent.contains("batch 2 tools"), "{parent}");
        assert!(parent.ends_with("(1.00s)"), "{parent}");
        let rail = if followed_by_tool {
            RAIL_CONTINUE
        } else {
            CHILD_INDENT
        };
        assert_eq!(
            rendered.lines[parent_idx + 1].to_string().trim_end(),
            format!("{rail}├  ✓ read src/main.rs")
        );
        assert_eq!(
            rendered.lines[parent_idx + 2].to_string().trim_end(),
            format!("{rail}└  ✗ bash cargo check")
        );
        assert!(rendered.lines.iter().all(|line| line.width() <= 60));
    }

    #[test]
    fn format_durations() {
        assert_eq!(format_tool_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_tool_duration(Duration::from_millis(7430)), "7.43s");
        assert_eq!(format_tool_duration(Duration::from_millis(15900)), "15.9s");
    }

    #[test]
    fn busy_spinner_frames_and_styles() {
        assert_eq!(busy_spinner_frame(0).0, "·");
        assert_eq!(busy_spinner_frame(140).0, "•");
        assert_eq!(busy_spinner_frame(280).0, "●");
        assert_eq!(busy_spinner_frame(420).0, "•");
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
    fn single_completed_tool_renders_group_header_and_row() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: true,
            entries: vec![GroupEntry::Tool(GroupToolEntry {
                id: "call-1".into(),
                name: Arc::from("bash"),
                summary: "cargo check".into(),
                raw_input: None,
                children: Vec::new(),
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
        assert_eq!(tl.lines.len(), 2);
        let header_text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header_text.contains("Worked for 1s"));

        let row_text: String = tl.lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(row_text.starts_with(RAIL_LAST));
        assert!(row_text.contains("✓"));
        assert!(row_text.contains("bash"));
        assert!(row_text.contains("cargo check"));
        assert!(row_text.contains("(1.20s)"));
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
        assert!(header_text.contains("Waiting for response..."));
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
                raw_input: None,
                children: Vec::new(),
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
        assert!(header_text.contains("Waiting for response..."));
        assert!(!header_text.contains("Worked for"));
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
        assert!(header_text.contains("Waiting for response... (1.52s)"));
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
                raw_input: None,
                children: Vec::new(),
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
        assert!(header_text.contains("Tool calling..."));
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
                raw_input: None,
                children: Vec::new(),
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
        assert!(header_text.contains("Tool call receiving..."));
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
        assert!(header_text.contains("Thinking..."));
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
                    raw_input: None,
                    children: Vec::new(),
                    status: ToolStatus::Success,
                    receiving_args: false,
                    started_at: None,
                    duration: Some(Duration::from_millis(7430)),
                }),
                GroupEntry::Tool(GroupToolEntry {
                    id: "call-2".into(),
                    name: Arc::from("bash"),
                    summary: "cargo test".into(),
                    raw_input: None,
                    children: Vec::new(),
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
        assert!(header_text.starts_with("✓ "));
        assert!(header_text.contains("Worked for 23s"));

        let row1: String = tl.lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(row1.starts_with(RAIL_CONTINUE));
        assert!(row1.contains("edit"));
        assert!(row1.contains("(7.43s)"));

        let row2: String = tl.lines[2]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(row2.starts_with(RAIL_LAST));
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
                raw_input: None,
                children: Vec::new(),
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
        assert_eq!(tl.lines.len(), 12);
        let header_text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header_text.contains("(calling 15 tools)"));
        let hidden_line: String = tl.lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(hidden_line.contains("... (5 hidden)"));
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
                    raw_input: None,
                    children: Vec::new(),
                    status: ToolStatus::Success,
                    receiving_args: false,
                    started_at: None,
                    duration: Some(Duration::from_millis(100)),
                }),
                GroupEntry::Tool(GroupToolEntry {
                    id: "call-2".into(),
                    name: Arc::from("bash"),
                    summary: "Run compact_test".into(),
                    raw_input: None,
                    children: Vec::new(),
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
            busy_line.contains("· bash"),
            "busy row should have 1 space: {busy_line}"
        );
        assert!(!busy_line.contains("·  bash"));

        // When updated by update_spinners
        let (frame, style) = busy_spinner_frame(BUSY_INTERVAL_MS);
        let spinner_span = Span::styled(format!("{frame} "), style);
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
            updated_busy_line.contains("• bash"),
            "updated busy row should still have 1 space: {updated_busy_line}"
        );
        assert!(!updated_busy_line.contains("•  bash"));
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
                raw_input: None,
                children: Vec::new(),
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
        let title_span = &tl.lines[0].spans[1];
        assert_eq!(title_span.content.as_ref(), "Tool calling...");
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
                started_at: None,
                duration: None,
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

    #[test_case(1, 0 => " (calling one tool)".to_string() ; "one_tool")]
    #[test_case(2, 0 => " (calling 2 tools)".to_string() ; "two_tools")]
    #[test_case(10, 0 => " (calling 10 tools)".to_string() ; "ten_tools")]
    #[test_case(0, 1 => " (thinking once)".to_string() ; "one_thinking")]
    #[test_case(0, 2 => " (thinking 2 times)".to_string() ; "two_thinking")]
    #[test_case(1, 1 => " (calling one tool, thinking once)".to_string() ; "one_tool_one_thinking")]
    #[test_case(10, 2 => " (calling 10 tools, thinking 2 times)".to_string() ; "ten_tools_two_thinking")]
    #[test_case(1, 2 => " (calling one tool, thinking 2 times)".to_string() ; "one_tool_two_thinking")]
    #[test_case(10, 1 => " (calling 10 tools, thinking once)".to_string() ; "ten_tools_one_thinking")]
    #[test_case(0, 0 => "".to_string() ; "empty")]
    fn group_summary_formatting(tools: usize, thoughts: usize) -> String {
        format_group_summary(tools, thoughts)
    }

    #[test]
    fn group_summary_shows_all_entries() {
        let entries = vec![
            GroupEntry::Tool(GroupToolEntry {
                id: "t0".into(),
                name: Arc::from("tool"),
                summary: "op 0".into(),
                raw_input: None,
                children: Vec::new(),
                status: ToolStatus::Success,
                receiving_args: false,
                started_at: None,
                duration: None,
            }),
            GroupEntry::Thinking(GroupThinkingEntry {
                id: 0,
                text: "thinking".into(),
                tokens: 50,
                active: false,
                started_at: None,
                duration: None,
            }),
        ];
        let group = ToolGroupRole {
            id: "group-small".into(),
            sealed: true,
            entries,
            started_at: None,
            duration: Some(Duration::from_secs(10)),
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        assert_eq!(tl.lines.len(), 3);
        let header: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header.contains("(calling one tool, thinking once)"));
    }

    #[test]
    fn sealed_group_title_shows_worked_for_and_hidden() {
        let mut entries = Vec::new();
        for i in 0..22 {
            entries.push(GroupEntry::Tool(GroupToolEntry {
                id: format!("c{i}"),
                name: Arc::from("tool"),
                summary: format!("op {i}"),
                raw_input: None,
                children: Vec::new(),
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
        assert_eq!(tl.lines.len(), 12);
        let header: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header.contains("Worked for 1m35s (calling 22 tools)"));
        let hidden_line: String = tl.lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(hidden_line.contains("... (12 hidden)"));
    }

    #[test]
    fn sealed_group_title_shows_tools_and_thoughts_hidden() {
        let mut entries = Vec::new();
        for i in 0..10 {
            entries.push(GroupEntry::Tool(GroupToolEntry {
                id: format!("t{i}"),
                name: Arc::from("tool"),
                summary: format!("op {i}"),
                raw_input: None,
                children: Vec::new(),
                status: ToolStatus::Success,
                receiving_args: false,
                started_at: None,
                duration: None,
            }));
        }
        for i in 0..2 {
            entries.push(GroupEntry::Thinking(GroupThinkingEntry {
                id: i,
                text: "thinking".into(),
                tokens: 50,
                active: false,
                started_at: None,
                duration: None,
            }));
        }
        let group = ToolGroupRole {
            id: "group-mixed".into(),
            sealed: true,
            entries,
            started_at: None,
            duration: Some(Duration::from_secs(10)),
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        assert_eq!(tl.lines.len(), 12);
        let header: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header.contains("(calling 10 tools, thinking 2 times)"));
        let hidden_line: String = tl.lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(hidden_line.contains("... (2 hidden)"));
    }

    #[test]
    fn thinking_row_displays_duration_on_the_right() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: true,
            entries: vec![GroupEntry::Thinking(GroupThinkingEntry {
                id: 0,
                text: "thinking about something".into(),
                tokens: 350,
                active: false,
                started_at: None,
                duration: Some(Duration::from_millis(1500)),
            })],
            started_at: None,
            duration: None,
            waiting_since: None,
        };

        let tl = build_tool_group_lines(&group, 80, 0);
        assert_eq!(tl.lines.len(), 2);
        assert!(
            tl.lines[1]
                .spans
                .iter()
                .any(|s| s.content.as_ref() == "thinking")
        );
        let thinking_line = tl.lines[1].to_string();
        assert!(thinking_line.contains("(1.50s)"));
        assert!(!thinking_line.contains("350t"));
    }

    #[test_case("read", Some(serde_json::json!({"path": "src/main.rs", "offset": 10, "limit": 20, "toolcall_reason": "reading main"})), "fallback", "reading main · src/main.rs:10-29" ; "read_with_offset_and_limit")]
    #[test_case("read", Some(serde_json::json!({"path": "src/main.rs", "offset": 5, "toolcall_reason": "reading main"})), "fallback", "reading main · src/main.rs:5" ; "read_with_offset_only")]
    #[test_case("write", Some(serde_json::json!({"path": "src/lib.rs", "toolcall_reason": "writing lib"})), "fallback", "writing lib · src/lib.rs" ; "write_with_path")]
    #[test_case("edit", Some(serde_json::json!({"path": "src/app.rs", "toolcall_reason": "editing app"})), "fallback", "editing app · src/app.rs" ; "edit_with_path")]
    #[test_case("multiedit", Some(serde_json::json!({"path": "src/app.rs", "toolcall_reason": "multiediting app"})), "fallback", "multiediting app · src/app.rs" ; "multiedit_with_path")]
    #[test_case("index", Some(serde_json::json!({"path": "src/lib.rs", "toolcall_reason": "indexing lib"})), "fallback", "indexing lib · src/lib.rs" ; "index_with_path")]
    #[test_case("grep", Some(serde_json::json!({"path": "src", "pattern": "TODO", "toolcall_reason": "searching todo"})), "fallback", "searching todo · src" ; "grep_with_path")]
    #[test_case("grep", Some(serde_json::json!({"pattern": "TODO", "toolcall_reason": "searching todo"})), "fallback", "searching todo" ; "grep_without_path")]
    #[test_case("glob", Some(serde_json::json!({"pattern": "**/*.rs", "toolcall_reason": "finding files"})), "fallback", "finding files · **/*.rs" ; "glob_with_pattern")]
    #[test_case("list_dir", Some(serde_json::json!({"path": "src", "toolcall_reason": "listing dir"})), "fallback", "listing dir · src" ; "list_dir_with_path")]
    #[test_case("view_image", Some(serde_json::json!({"path": "logo.png", "toolcall_reason": "inspecting logo"})), "fallback", "inspecting logo · logo.png" ; "view_image_with_path")]
    #[test_case("view_image", Some(serde_json::json!({"file_path": "logo.png", "toolcall_reason": "inspecting logo"})), "fallback", "inspecting logo · logo.png" ; "view_image_with_file_path")]
    #[test_case("bash", Some(serde_json::json!({"command": "cargo test", "toolcall_reason": "running tests"})), "fallback", "running tests" ; "bash_reason_only")]
    #[test_case("code_execution", Some(serde_json::json!({"code": "print(1)", "toolcall_reason": "running script"})), "fallback", "running script" ; "code_execution_reason_only")]
    #[test_case("batch", Some(serde_json::json!({"tool_calls": [{}, {}], "toolcall_reason": "running batch"})), "fallback", "running batch · 2 tools" ; "batch_with_tool_calls")]
    #[test_case("task", Some(serde_json::json!({"description": "subtask", "toolcall_reason": "starting subtask"})), "fallback", "starting subtask" ; "task_reason_only")]
    #[test_case("websearch", Some(serde_json::json!({"query": "rust tokio", "toolcall_reason": "searching web"})), "fallback", "searching web · rust tokio" ; "websearch_with_query")]
    #[test_case("webfetch", Some(serde_json::json!({"url": "https://example.com", "toolcall_reason": "fetching page"})), "fallback", "fetching page" ; "webfetch_reason_only")]
    #[test_case("question", Some(serde_json::json!({"questions": [], "toolcall_reason": "asking question"})), "fallback", "asking question" ; "question_reason_only")]
    #[test_case("memory", Some(serde_json::json!({"path": "notes.md", "toolcall_reason": "saving memory"})), "fallback", "saving memory" ; "memory_reason_only")]
    #[test_case("todo_write", Some(serde_json::json!({"todos": [], "toolcall_reason": "updating todos"})), "fallback", "updating todos" ; "todo_write_reason_only")]
    #[test_case("read", Some(serde_json::json!({"path": "src/main.rs"})), "default summary", "default summary" ; "fallback_when_no_reason")]
    #[test_case("read", Some(serde_json::json!({"path": "src/main.rs", "toolcall_reason": "   "})), "default summary", "default summary" ; "fallback_when_empty_reason")]
    #[test_case("read", None, "default summary", "default summary" ; "fallback_when_no_input")]
    fn compact_summary_formatting(
        tool: &str,
        input: Option<serde_json::Value>,
        default_summary: &str,
        expected: &str,
    ) {
        let actual = format_compact_summary(tool, input.as_ref(), default_summary);
        assert_eq!(actual, expected);
    }

    #[test_case("running batch · 2 tools", 3, "running batch · 3 tools" ; "preserves_reason_prefix")]
    #[test_case("2 tools", 3, "3 tools" ; "plain_count_without_reason")]
    fn update_batch_summary(current: &str, count: usize, expected: &str) {
        assert_eq!(update_batch_compact_summary(current, count), expected);
    }

    #[test]
    fn batch_children_formats_toolcall_reason_like_top_level() {
        let snapshot = BufferSnapshot::from_arc(std::sync::Arc::new(vec![
            maki_agent::SnapshotLine {
                spans: vec![
                    maki_agent::SnapshotSpan {
                        text: "● ".into(),
                        style: SpanStyle::Named("tool_success".into()),
                    },
                    maki_agent::SnapshotSpan {
                        text: "read> ".into(),
                        style: SpanStyle::Named("tool_prefix".into()),
                    },
                    maki_agent::SnapshotSpan {
                        text: "src/main.rs:1-50".into(),
                        style: SpanStyle::Default,
                    },
                    maki_agent::SnapshotSpan {
                        text: " (120ms)".into(),
                        style: SpanStyle::Named("batch_duration_ms".into()),
                    },
                ],
            },
            maki_agent::SnapshotLine {
                spans: vec![
                    maki_agent::SnapshotSpan {
                        text: "● ".into(),
                        style: SpanStyle::Named("tool_success".into()),
                    },
                    maki_agent::SnapshotSpan {
                        text: "grep> ".into(),
                        style: SpanStyle::Named("tool_prefix".into()),
                    },
                    maki_agent::SnapshotSpan {
                        text: "TODO (src)".into(),
                        style: SpanStyle::Default,
                    },
                ],
            },
            maki_agent::SnapshotLine {
                spans: vec![
                    maki_agent::SnapshotSpan {
                        text: "● ".into(),
                        style: SpanStyle::Named("tool_success".into()),
                    },
                    maki_agent::SnapshotSpan {
                        text: "write> ".into(),
                        style: SpanStyle::Named("tool_prefix".into()),
                    },
                    maki_agent::SnapshotSpan {
                        text: "src/lib.rs".into(),
                        style: SpanStyle::Default,
                    },
                ],
            },
        ]));

        let batch_input = serde_json::json!({
            "tool_calls": [
                {
                    "tool": "read",
                    "parameters": {
                        "path": "src/main.rs",
                        "offset": 1,
                        "limit": 50,
                        "toolcall_reason": "reading entry point"
                    }
                },
                {
                    "tool": "grep",
                    "path": "src",
                    "pattern": "TODO",
                    "toolcall_reason": "searching todo items"
                },
                {
                    "tool": "write",
                    "parameters": {
                        "path": "src/lib.rs",
                        "content": "pub fn foo() {}"
                    }
                }
            ],
            "toolcall_reason": "inspecting and updating code"
        });

        let children = batch_children(&snapshot, Some(&batch_input));
        assert_eq!(children.len(), 3);
        assert_eq!(children[0].name.as_ref(), "read");
        assert_eq!(
            children[0].summary,
            "reading entry point · src/main.rs:1-50"
        );
        assert_eq!(children[0].duration, Some(Duration::from_millis(120)));

        assert_eq!(children[1].name.as_ref(), "grep");
        assert_eq!(children[1].summary, "searching todo items · src");

        assert_eq!(children[2].name.as_ref(), "write");
        assert_eq!(children[2].summary, "src/lib.rs");
    }
}
