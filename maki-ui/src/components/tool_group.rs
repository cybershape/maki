use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

use maki_agent::agent::tool_dispatch::MCP_SUMMARY_PREFIX;
use maki_agent::mcp::{internal_tool_name, is_wire_name};
use maki_agent::tools::BASH_TOOL_NAME;
use maki_agent::tools::schema::TOOLCALL_REASON_FIELD;
use maki_agent::{BufferSnapshot, SpanStyle, ToolOutput};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::theme;

use super::code_view::SectionFlags;
use super::compact_detail::CompactTarget;
use super::tool_display::ToolLines;
use super::{
    CompactDetail, GroupEntry, GroupThinkingEntry, GroupToolEntry, ToolGroupRole, ToolStatus,
    truncate_to_width,
};

#[cfg(test)]
pub const DEFAULT_COMPACT_LINES: usize = maki_config::DEFAULT_COMPACT_LINES;
pub const COMPACT_OPEN_ICON: &str = "↗ ";
const RAIL_CONTINUE: &str = "│  ";
const RAIL_LAST: &str = "└  ";
const CHILD_RAIL_CONTINUE: &str = "├  ";
const CHILD_INDENT: &str = "   ";
pub(super) const BATCH_TOOL: &str = "batch";
pub(super) const CODE_EXECUTION_TOOL: &str = "code_execution";
pub const QUESTION_TOOL: &str = "question";
const NO_ANSWER: &str = "(no answer)";
const DISMISSED_OUTPUT: &str = "(question dismissed by user)";
const CHILD_PREFIX_STYLE: &str = "tool_prefix";
const CHILD_SUCCESS_STYLE: &str = "tool_success";
const CHILD_ERROR_STYLE: &str = "tool_error";
const CHILD_DURATION_STYLE: &str = "batch_duration_ms";
const CHILD_DURATION_PREFIX: &str = " (";
const CHILD_DURATION_SUFFIX: &str = "ms)";
const REASON_SEP: &str = " · ";
const MESSAGE_SEND_ARROW: &str = "→ ";
const MESSAGE_PARENT_RECIPIENT: &str = "parent";
const THINKING_NEWLINE_SEP: &str = " ↵ ";
const MERGED_COUNT_PREFIX: &str = " x ";
/// Annotation notes whose numbers are statuses or budgets, not sums.
const OPAQUE_NOTE_PREFIXES: [&str; 2] = ["exit ", "timeout "];
/// Formatted token usage ("1.2k↑ 345↓") is rounded text, not a summand.
const USAGE_NOTE_MARKERS: [char; 2] = ['↑', '↓'];

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

fn format_thinking_preview(text: &str) -> String {
    let cleaned = text.replace(['*', '_', '#', '`', '>'], "");
    let lines: Vec<String> = cleaned
        .lines()
        .map(single_line)
        .filter(|line| !line.is_empty())
        .collect();
    lines.join(THINKING_NEWLINE_SEP)
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
        "task_message" => {
            let recipient = input
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(MESSAGE_PARENT_RECIPIENT);
            format!("{MESSAGE_SEND_ARROW}{recipient}{REASON_SEP}{reason}")
        }
        _ => reason.to_string(),
    }
}

pub fn is_background_bash(tool: &str, raw_input: Option<&Value>) -> bool {
    if tool != BASH_TOOL_NAME {
        return false;
    }
    raw_input.is_some_and(|input| {
        input
            .get("background")
            .is_some_and(|val| val.as_bool() == Some(true) || val.as_str() == Some("true"))
    })
}

pub fn update_batch_compact_summary(current_summary: &str, count: usize) -> String {
    match current_summary.split_once(REASON_SEP) {
        Some((reason, _)) => format!("{reason}{REASON_SEP}{count} tools"),
        None => format!("{count} tools"),
    }
}

/// Keeps the script's line count and appends the sandbox call count, so the
/// compact row says both what ran and how many calls it made. The count always
/// goes last; everything before a previous count is kept as is.
pub fn update_code_execution_compact_summary(current_summary: &str, count: usize) -> String {
    let base = match current_summary.rsplit_once(REASON_SEP) {
        Some((head, tail)) if tail.ends_with(" tools") => head,
        _ => current_summary,
    };
    format!("{base}{REASON_SEP}{count} tools")
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

pub fn extract_batch_child_output(batch_output: &str, child_idx: usize) -> Option<String> {
    let mut sections = Vec::new();
    let mut current_section: Option<Vec<&str>> = None;
    for line in batch_output.lines() {
        if line.starts_with("## ") {
            if let Some(sec) = current_section.take() {
                sections.push(sec.join("\n"));
            }
            current_section = Some(Vec::new());
        } else if let Some(sec) = &mut current_section {
            sec.push(line);
        }
    }
    if let Some(sec) = current_section {
        sections.push(sec.join("\n"));
    }
    let section = sections.get(child_idx)?;
    let trimmed = section.trim();
    let clean = if let Some((content, _)) = trimmed.rsplit_once("\n\nAll ") {
        content
    } else if let Some((content, _)) = trimmed.rsplit_once("\n\nExecuted ") {
        content
    } else {
        trimmed
    };
    Some(clean.strip_prefix("[ERROR] ").unwrap_or(clean).to_string())
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
                count: 1,
                status,
                receiving_args: false,
                started_at: None,
                duration,
                detail: None,
            })
        })
        .collect()
}

/// One child per sandbox tool call, from the `state` the plugin saves with
/// the result. Unlike batch, the calls never render into the body, so the
/// records are the only source. Calls without a `toolcall_reason` merge into
/// one row per tool (`bash x 2`), with their statistics summed.
pub(super) fn code_execution_children(output: Option<&ToolOutput>) -> Vec<GroupToolEntry> {
    let Some(calls) = output
        .and_then(ToolOutput::state)
        .and_then(|state| state.get("calls"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let children: Vec<GroupToolEntry> = calls
        .iter()
        .filter_map(|call| {
            let name = call.get("tool").and_then(Value::as_str)?;
            let params = call.get("params").filter(|v| v.is_object()).cloned();
            let mut summary = format_compact_summary(name, params.as_ref(), "");
            for field in ["annotation", "usage"] {
                if let Some(extra) = call
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    if summary.is_empty() {
                        summary = extra.to_string();
                    } else {
                        summary = format!("{summary}{REASON_SEP}{extra}");
                    }
                }
            }
            let status = match call.get("status").and_then(Value::as_str) {
                Some("success") => ToolStatus::Success,
                Some("error") => ToolStatus::Error,
                _ => ToolStatus::InProgress,
            };
            let duration = call
                .get("duration_ms")
                .and_then(Value::as_u64)
                .map(Duration::from_millis);
            Some(GroupToolEntry {
                id: String::new(),
                name: name.into(),
                summary,
                raw_input: params,
                children: Vec::new(),
                count: 1,
                status,
                receiving_args: false,
                started_at: None,
                duration,
                detail: None,
            })
        })
        .collect();
    merge_code_execution_children(children)
}

pub(super) fn child_reason(entry: &GroupToolEntry) -> Option<&str> {
    entry
        .raw_input
        .as_ref()
        .and_then(|v| v.get(TOOLCALL_REASON_FIELD))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Collapses reason-less calls of the same tool into the first row of that
/// tool, keeping first-seen order. Rows with a reason stay separate: the
/// reason explains one specific call.
fn merge_code_execution_children(children: Vec<GroupToolEntry>) -> Vec<GroupToolEntry> {
    let mut merged: Vec<GroupToolEntry> = Vec::new();
    for child in children {
        let target = (child_reason(&child).is_none()).then(|| {
            merged
                .iter_mut()
                .find(|row| row.name == child.name && child_reason(row).is_none())
        });
        match target {
            Some(Some(row)) => merge_call_into(row, child),
            _ => merged.push(child),
        }
    }
    merged
}

fn merge_call_into(row: &mut GroupToolEntry, call: GroupToolEntry) {
    row.count += call.count;
    row.status = match (row.status, call.status) {
        (ToolStatus::Error, _) | (_, ToolStatus::Error) => ToolStatus::Error,
        (ToolStatus::InProgress, _) | (_, ToolStatus::InProgress) => ToolStatus::InProgress,
        _ => ToolStatus::Success,
    };
    row.duration = match (row.duration, call.duration) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    };
    row.summary = merge_notes(&row.summary, &call.summary);
    row.raw_input = None;
}

/// Merges the ` · `-separated annotation/usage notes of two rows: identical
/// text is kept once, numeric notes with the same shape are summed per
/// position ("50 lines" + "30 lines" -> "80 lines"), everything else is kept.
fn merge_notes(a: &str, b: &str) -> String {
    let mut notes: Vec<String> = a
        .split(REASON_SEP)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    for note in b.split(REASON_SEP).filter(|s| !s.is_empty()) {
        if notes.iter().any(|kept| kept == note) {
            continue;
        }
        let summed = numeric_note(note).and_then(|(template, numbers)| {
            notes.iter_mut().find_map(|kept| {
                let (kept_template, kept_numbers) = numeric_note(kept)?;
                (kept_template == template && kept_numbers.len() == numbers.len()).then(|| {
                    *kept = render_numeric_note(
                        &template,
                        &kept_numbers
                            .iter()
                            .zip(&numbers)
                            .map(|(x, y)| x + y)
                            .collect::<Vec<_>>(),
                    );
                })
            })
        });
        if summed.is_none() {
            notes.push(note.to_owned());
        }
    }
    notes.join(REASON_SEP)
}

/// Splits a note into digit-free gaps and the numbers between them, so two
/// notes of the same shape can be summed per position. Notes with no digits,
/// and status or budget notes ("exit 1", "timeout 30s"), have no shape.
fn numeric_note(note: &str) -> Option<(Vec<String>, Vec<u64>)> {
    if OPAQUE_NOTE_PREFIXES
        .iter()
        .any(|prefix| note.starts_with(prefix))
        || note.contains(USAGE_NOTE_MARKERS)
    {
        return None;
    }
    let mut gaps = Vec::new();
    let mut numbers = Vec::new();
    let mut rest = note;
    while let Some(start) = rest.find(|c: char| c.is_ascii_digit()) {
        gaps.push(rest[..start].to_owned());
        rest = &rest[start..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        numbers.push(rest[..end].parse().ok()?);
        rest = &rest[end..];
    }
    gaps.push(rest.to_owned());
    (!numbers.is_empty()).then_some((gaps, numbers))
}

fn render_numeric_note(gaps: &[String], numbers: &[u64]) -> String {
    let mut out = String::new();
    for (gap, number) in gaps.iter().zip(numbers) {
        out.push_str(gap);
        out.push_str(&number.to_string());
    }
    if let Some(last) = gaps.last() {
        out.push_str(last);
    }
    out
}

pub fn question_children(
    raw_input: Option<&Value>,
    output: Option<&ToolOutput>,
) -> Vec<GroupToolEntry> {
    let Some(output) = output else {
        return Vec::new();
    };
    if output
        .state()
        .and_then(|s| s.get("dismissed"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return Vec::new();
    }
    let display_text = output.as_display_text();
    if display_text.trim() == DISMISSED_OUTPUT {
        return Vec::new();
    }

    let state_answers = output
        .state()
        .and_then(|s| s.get("answers"))
        .and_then(Value::as_array);

    let fallback_lines: Vec<&str> = display_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    let questions = raw_input
        .and_then(|v| v.get("questions"))
        .and_then(Value::as_array);

    if let Some(questions) = questions
        && !questions.is_empty()
    {
        questions
            .iter()
            .enumerate()
            .map(|(idx, q)| {
                let header = q
                    .get("header")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let label = match header {
                    Some(h) => {
                        if h.ends_with(':') {
                            h.to_string()
                        } else {
                            format!("{h}:")
                        }
                    }
                    None => format!("Q{}:", idx + 1),
                };

                let answer = if let Some(answers) = state_answers {
                    match answers.get(idx) {
                        Some(Value::Array(arr)) => {
                            let items: Vec<&str> = arr
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .collect();
                            if items.is_empty() {
                                NO_ANSWER.to_string()
                            } else {
                                items.join(", ")
                            }
                        }
                        Some(Value::String(s)) => {
                            let trimmed = s.trim();
                            if trimmed.is_empty() {
                                NO_ANSWER.to_string()
                            } else {
                                trimmed.to_string()
                            }
                        }
                        _ => NO_ANSWER.to_string(),
                    }
                } else if let Some(line) = fallback_lines.get(idx) {
                    if let Some((_, ans)) = line.split_once(": ") {
                        let trimmed = ans.trim();
                        if trimmed.is_empty() {
                            NO_ANSWER.to_string()
                        } else {
                            trimmed.to_string()
                        }
                    } else {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            NO_ANSWER.to_string()
                        } else {
                            trimmed.to_string()
                        }
                    }
                } else {
                    NO_ANSWER.to_string()
                };

                GroupToolEntry {
                    id: String::new(),
                    name: Arc::from(label),
                    summary: answer.clone(),
                    raw_input: Some(q.clone()),
                    children: Vec::new(),
                    count: 1,
                    status: ToolStatus::Success,
                    receiving_args: false,
                    started_at: None,
                    duration: None,
                    detail: Some(CompactDetail {
                        raw_input: Some(q.clone()),
                        output: Some(Arc::new(ToolOutput::Plain(answer.into()))),
                        ..CompactDetail::default()
                    }),
                }
            })
            .collect()
    } else {
        fallback_lines
            .iter()
            .filter_map(|line| {
                let (lbl, ans) = line.split_once(": ")?;
                let label = if lbl.ends_with(':') {
                    lbl.to_string()
                } else {
                    format!("{lbl}:")
                };
                let trimmed = ans.trim();
                let answer = if trimmed.is_empty() {
                    NO_ANSWER.to_string()
                } else {
                    trimmed.to_string()
                };
                Some(GroupToolEntry {
                    id: String::new(),
                    name: Arc::from(label),
                    summary: answer.clone(),
                    raw_input: None,
                    children: Vec::new(),
                    count: 1,
                    status: ToolStatus::Success,
                    receiving_args: false,
                    started_at: None,
                    duration: None,
                    detail: Some(CompactDetail {
                        raw_input: None,
                        output: Some(Arc::new(ToolOutput::Plain(answer.into()))),
                        ..CompactDetail::default()
                    }),
                })
            })
            .collect()
    }
}

/// Whether clicking a child row can show anything: its own detail, or a batch
/// parent whose output sections the click handler splits per child.
pub fn child_has_detail(parent: &GroupToolEntry, child: &GroupToolEntry) -> bool {
    child.detail.is_some() || parent.name.as_ref() == BATCH_TOOL
}

pub fn compact_icon_columns(line: &Line<'_>) -> Option<Range<u16>> {
    if line.spans.len() >= 2 {
        let rail_w = line.spans[0].width() as u16;
        let icon_w = line.spans[1].width() as u16;
        Some(rail_w..rail_w + icon_w)
    } else {
        line.spans.first().map(|first| 0..first.width() as u16)
    }
}

pub fn compact_interactive_columns(line: &Line<'_>) -> Option<Range<u16>> {
    if line.spans.len() >= 3 {
        let rail_w = line.spans[0].width() as u16;
        let icon_w = line.spans[1].width() as u16;
        let name_w = line.spans[2].width() as u16;
        Some(rail_w..rail_w + icon_w + name_w)
    } else {
        compact_icon_columns(line)
    }
}

struct ToolHover {
    parent: bool,
    child: Option<usize>,
}

struct RowHover {
    hovered: bool,
    openable: bool,
}

fn push_tool_rows(
    lines: &mut Vec<Line<'static>>,
    spinner_lines: &mut Vec<(usize, usize)>,
    tool: &GroupToolEntry,
    rail: &str,
    frame: (&str, Style),
    width: u16,
    hover: ToolHover,
) {
    lines.push(render_tool_row(
        rail,
        tool,
        frame,
        width,
        spinner_lines,
        lines.len(),
        RowHover {
            hovered: hover.parent,
            openable: true,
        },
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
            RowHover {
                hovered: hover.child == Some(idx),
                openable: child_has_detail(tool, child),
            },
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
    hover: RowHover,
) -> Line<'static> {
    let theme = theme::current();
    let is_in_progress = tool.status == ToolStatus::InProgress;
    let show_open = hover.hovered && hover.openable;
    let (icon, icon_style) = if show_open {
        let style = if tool.status == ToolStatus::Error {
            theme.tool_error
        } else {
            theme.accent
        };
        (COMPACT_OPEN_ICON.to_string(), style)
    } else {
        match tool.status {
            ToolStatus::InProgress => (format!("{} ", frame.0), frame.1),
            ToolStatus::Success => ("✓ ".to_string(), theme.tool_success),
            ToolStatus::Error => ("✗ ".to_string(), theme.tool_error),
        }
    };

    let mut spans = Vec::new();
    let mut span_idx = 0;

    if !rail.is_empty() {
        spans.push(Span::styled(rail.to_string(), theme.tool_dim));
        span_idx += 1;
    }

    if is_in_progress && !show_open {
        spinner_lines.push((line_idx, span_idx));
    }
    let icon_width = icon.width();
    spans.push(Span::styled(icon, icon_style));

    let mut name_style = theme.tool.add_modifier(Modifier::BOLD);
    if show_open {
        name_style = name_style.add_modifier(Modifier::UNDERLINED);
    }
    let display_name = if is_wire_name(&tool.name) {
        format!("{MCP_SUMMARY_PREFIX}{}", internal_tool_name(&tool.name))
    } else {
        tool.name.to_string()
    };
    spans.push(Span::styled(display_name.clone(), name_style));

    let count_str = if tool.count > 1 {
        format!("{MERGED_COUNT_PREFIX}{}", tool.count)
    } else {
        String::new()
    };
    if !count_str.is_empty() {
        spans.push(Span::styled(count_str.clone(), theme.status_dim));
    }

    let right_str = if is_in_progress || hover.hovered {
        tool.duration
            .or_else(|| tool.started_at.map(|st| st.elapsed()))
            .map(|d| format!("({})", format_tool_duration(d)))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let right_len = right_str.width();
    let right_style = theme.status_dim;

    let left_len = rail.width() + icon_width + display_name.width() + count_str.width();
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
    is_hovered: bool,
) -> Line<'static> {
    let theme = theme::current();
    let (icon, icon_style) = if is_hovered {
        (COMPACT_OPEN_ICON.to_string(), theme.accent)
    } else if thinking.active {
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

    let preview = format_thinking_preview(&thinking.text);
    let label = if preview.is_empty() {
        if thinking.active {
            "thinking..."
        } else {
            "thinking"
        }
    } else {
        "thinking"
    };

    let mut label_style = theme.tool.add_modifier(Modifier::BOLD);
    if is_hovered {
        label_style = label_style.add_modifier(Modifier::UNDERLINED);
    }
    spans.push(Span::styled(label.to_string(), label_style));

    let right_str = if thinking.active || is_hovered {
        thinking
            .duration
            .or_else(|| thinking.started_at.map(|st| st.elapsed()))
            .map(|d| format!("({})", format_tool_duration(d)))
            .unwrap_or_default()
    } else {
        String::new()
    };
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
    max_entries: usize,
    hovered: Option<&CompactTarget>,
) -> ToolLines {
    let mut lines = Vec::new();
    let mut spinner_lines = Vec::new();
    let frame = busy_spinner_frame(spinner_millis);

    let pending_tools: Vec<&GroupToolEntry> = group
        .entries
        .iter()
        .filter_map(|e| match e {
            GroupEntry::Tool(t) if t.status == ToolStatus::InProgress => Some(t.as_ref()),
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
            gutter: Span::default(),
            gutter_spins: false,
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

    let hidden_count = if group.expanded {
        0
    } else {
        group.entries.len().saturating_sub(max_entries)
    };

    let visible_start = if hidden_count > 0 { hidden_count } else { 0 };
    let visible_entries = &group.entries[visible_start..];
    let total_visible = visible_entries.len();

    if hidden_count > 0 {
        let rail = if total_visible == 0 {
            RAIL_LAST
        } else {
            RAIL_CONTINUE
        };
        lines.push(Line::from(vec![
            Span::styled(rail, theme.tool_dim),
            Span::styled(format!("... ({hidden_count} hidden)"), theme.tool_dim),
        ]));
    }

    for (idx, entry) in visible_entries.iter().enumerate() {
        let is_last = idx == total_visible - 1;
        let rail = if is_last { RAIL_LAST } else { RAIL_CONTINUE };
        let line_idx = lines.len();
        let entry_idx = visible_start + idx;
        let is_this_entry_hovered =
            hovered.is_some_and(|h| h.group_id == group.id && h.entry == entry_idx);
        let is_parent_hovered = is_this_entry_hovered && hovered.and_then(|h| h.child).is_none();
        let hovered_child = if is_this_entry_hovered {
            hovered.and_then(|h| h.child)
        } else {
            None
        };

        match entry {
            GroupEntry::Tool(t) => push_tool_rows(
                &mut lines,
                &mut spinner_lines,
                t,
                rail,
                frame,
                width,
                ToolHover {
                    parent: is_parent_hovered,
                    child: hovered_child,
                },
            ),
            GroupEntry::Thinking(th) => lines.push(render_thinking_row(
                rail,
                th,
                spinner_millis,
                width,
                line_idx,
                is_parent_hovered,
            )),
        }
    }

    ToolLines {
        lines,
        highlight: None,
        spinner_lines,
        snapshot_base: None,
        gutter: Span::default(),
        gutter_spins: false,
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

    #[test_case(CODE_EXECUTION_TOOL, false, false; "code_execution_child_without_detail")]
    #[test_case(BATCH_TOOL, false, true; "batch_child_splits_parent_output")]
    #[test_case(CODE_EXECUTION_TOOL, true, true; "child_with_own_detail")]
    fn child_has_detail_matrix(parent_name: &str, own_detail: bool, expected: bool) {
        let mut child = GroupToolEntry {
            id: String::new(),
            name: Arc::from("read"),
            summary: "src/main.rs".into(),
            raw_input: None,
            children: Vec::new(),
            count: 1,
            status: ToolStatus::Success,
            receiving_args: false,
            started_at: None,
            duration: None,
            detail: None,
        };
        if own_detail {
            child.detail = Some(CompactDetail::default());
        }
        let mut parent = child.clone();
        parent.name = Arc::from(parent_name);
        parent.detail = None;
        assert_eq!(child_has_detail(&parent, &child), expected);
    }

    #[test_case(ToolStatus::Success; "success_uses_accent")]
    #[test_case(ToolStatus::Error; "error_uses_tool_error")]
    #[test_case(ToolStatus::InProgress; "in_progress_uses_accent")]
    fn hovered_entry_open_icon_style_follows_status(status: ToolStatus) {
        const DURATION_TEXT: &str = "(500ms)";
        let child = GroupToolEntry {
            id: String::new(),
            name: Arc::from("read"),
            summary: "src/main.rs".into(),
            raw_input: None,
            children: Vec::new(),
            count: 1,
            status,
            receiving_args: false,
            started_at: None,
            duration: Some(Duration::from_millis(500)),
            detail: None,
        };
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: true,
            entries: vec![GroupEntry::Tool(Box::new(child))],
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };

        let hover_target = CompactTarget {
            group_id: "group-1".into(),
            entry: 0,
            child: None,
        };
        let hovered_tl =
            build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, Some(&hover_target));
        let hovered_row = &hovered_tl.lines[1];
        assert_eq!(hovered_row.spans[1].content, COMPACT_OPEN_ICON);
        assert!(hovered_row.to_string().ends_with(DURATION_TEXT));
        let expected_style = match status {
            ToolStatus::Error => theme::current().tool_error,
            _ => theme::current().accent,
        };
        assert_eq!(hovered_row.spans[1].style, expected_style);

        let cols = compact_icon_columns(hovered_row).expect("icon columns");
        assert_eq!(cols, 3..5);
        let interactive_cols =
            compact_interactive_columns(hovered_row).expect("interactive columns");
        assert_eq!(interactive_cols, 3..9);

        assert!(
            hovered_row.spans[2]
                .style
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );

        let unhovered_tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
        let unhovered_row = &unhovered_tl.lines[1];
        assert_ne!(unhovered_row.spans[1].content, COMPACT_OPEN_ICON);
        assert_eq!(
            unhovered_row.to_string().ends_with(DURATION_TEXT),
            status == ToolStatus::InProgress
        );
        assert!(
            !unhovered_row.spans[2]
                .style
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
    }

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
            count: 1,
            status: ToolStatus::Success,
            receiving_args: false,
            started_at: None,
            duration: None,
            detail: None,
        };
        let mut batch = child.clone();
        batch.name = Arc::from(BATCH_TOOL);
        batch.summary = "2 tools".into();
        batch.children = vec![child.clone(), child.clone()];
        batch.children[1].name = Arc::from("bash");
        batch.children[1].summary = "cargo check".into();
        batch.children[1].status = ToolStatus::Error;
        let mut group = ToolGroupRole {
            id: "group".into(),
            sealed: true,
            entries: vec![GroupEntry::Tool(Box::new(batch))],
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };
        if followed_by_tool {
            group
                .entries
                .push(GroupEntry::Tool(Box::new(child.clone())));
        }
        if preceded_by_tool {
            group.entries.insert(0, GroupEntry::Tool(Box::new(child)));
        }
        let rendered = build_tool_group_lines(&group, 60, 0, DEFAULT_COMPACT_LINES, None);
        let parent_idx = 1 + usize::from(preceded_by_tool);
        let parent = rendered.lines[parent_idx].to_string();
        assert!(parent.contains("batch 2 tools"), "{parent}");
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
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
        assert!(tl.lines.is_empty());
    }

    #[test]
    fn single_completed_tool_renders_group_header_and_row() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: true,
            entries: vec![GroupEntry::Tool(Box::new(GroupToolEntry {
                id: "call-1".into(),
                name: Arc::from("bash"),
                summary: "cargo check".into(),
                raw_input: None,
                children: Vec::new(),
                count: 1,
                status: ToolStatus::Success,
                receiving_args: false,
                started_at: None,
                duration: Some(Duration::from_millis(1200)),
                detail: None,
            }))],
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
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
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
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
            entries: vec![GroupEntry::Tool(Box::new(GroupToolEntry {
                id: "call-1".into(),
                name: Arc::from("bash"),
                summary: "ls".into(),
                raw_input: None,
                children: Vec::new(),
                count: 1,
                status: ToolStatus::Success,
                receiving_args: false,
                started_at: None,
                duration: Some(Duration::from_millis(100)),
                detail: None,
            }))],
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
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
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
        let header_text: String = tl.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        // The elapsed time grows between building `waiting_since` and the
        // render, so assert the timer's shape, not a rounded instant.
        assert!(header_text.contains("Waiting for response... (1."));
        assert!(header_text.ends_with("s)"));
    }

    #[test]
    fn single_tool_unsealed_renders_header_with_calling_state() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: false,
            entries: vec![GroupEntry::Tool(Box::new(GroupToolEntry {
                id: "call-1".into(),
                name: Arc::from("bash"),
                summary: "cargo check".into(),
                raw_input: None,
                children: Vec::new(),
                count: 1,
                status: ToolStatus::InProgress,
                receiving_args: false,
                started_at: None,
                duration: None,
                detail: None,
            }))],
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
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
            entries: vec![GroupEntry::Tool(Box::new(GroupToolEntry {
                id: "call-1".into(),
                name: Arc::from("bash"),
                summary: String::new(),
                raw_input: None,
                children: Vec::new(),
                count: 1,
                status: ToolStatus::InProgress,
                receiving_args: true,
                started_at: None,
                duration: None,
                detail: None,
            }))],
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
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
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
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
                GroupEntry::Tool(Box::new(GroupToolEntry {
                    id: "call-1".into(),
                    name: Arc::from("edit"),
                    summary: "foo.rs".into(),
                    raw_input: None,
                    children: Vec::new(),
                    count: 1,
                    status: ToolStatus::Success,
                    receiving_args: false,
                    started_at: None,
                    duration: Some(Duration::from_millis(7430)),
                    detail: None,
                })),
                GroupEntry::Tool(Box::new(GroupToolEntry {
                    id: "call-2".into(),
                    name: Arc::from("bash"),
                    summary: "cargo test".into(),
                    raw_input: None,
                    children: Vec::new(),
                    count: 1,
                    status: ToolStatus::Error,
                    receiving_args: false,
                    started_at: None,
                    duration: Some(Duration::from_millis(15900)),
                    detail: None,
                })),
            ],
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
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

        let row2: String = tl.lines[2]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(row2.starts_with(RAIL_LAST));
        assert!(row2.contains("✗"));
        assert!(row2.contains("bash"));
    }

    #[test]
    fn hides_beyond_max_entries() {
        let mut entries = Vec::new();
        for i in 0..15 {
            entries.push(GroupEntry::Tool(Box::new(GroupToolEntry {
                id: format!("call-{i}"),
                name: Arc::from("read"),
                summary: format!("file_{i}.rs"),
                raw_input: None,
                children: Vec::new(),
                count: 1,
                status: ToolStatus::Success,
                receiving_args: false,
                started_at: None,
                duration: Some(Duration::from_millis(100)),
                detail: None,
            })));
        }

        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: true,
            entries,
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
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
                GroupEntry::Tool(Box::new(GroupToolEntry {
                    id: "call-1".into(),
                    name: Arc::from("bash"),
                    summary: "git status".into(),
                    raw_input: None,
                    children: Vec::new(),
                    count: 1,
                    status: ToolStatus::Success,
                    receiving_args: false,
                    started_at: None,
                    duration: Some(Duration::from_millis(100)),
                    detail: None,
                })),
                GroupEntry::Tool(Box::new(GroupToolEntry {
                    id: "call-2".into(),
                    name: Arc::from("bash"),
                    summary: "Run compact_test".into(),
                    raw_input: None,
                    children: Vec::new(),
                    count: 1,
                    status: ToolStatus::InProgress,
                    receiving_args: false,
                    started_at: None,
                    duration: None,
                    detail: None,
                })),
            ],
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };

        let mut tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
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
            entries: vec![GroupEntry::Tool(Box::new(GroupToolEntry {
                id: "call-1".into(),
                name: Arc::from("bash"),
                summary: "ls".into(),
                raw_input: None,
                children: Vec::new(),
                count: 1,
                status: ToolStatus::InProgress,
                receiving_args: false,
                started_at: None,
                duration: None,
                detail: None,
            }))],
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
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
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
        let preview_span = tl.lines[1]
            .spans
            .iter()
            .find(|s| s.content.contains("analyzing requirements"))
            .expect("preview span found");
        assert!(!preview_span.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn format_thinking_preview_single_line() {
        assert_eq!(
            format_thinking_preview("analyzing requirements"),
            "analyzing requirements"
        );
    }

    #[test]
    fn format_thinking_preview_multiline_joins_with_separator() {
        let text = "Building the AST conversion script\nDesigning the AST converter\nDesigning";
        assert_eq!(
            format_thinking_preview(text),
            "Building the AST conversion script ↵ Designing the AST converter ↵ Designing"
        );
    }

    #[test]
    fn format_thinking_preview_collapses_empty_and_whitespace_lines() {
        let text = "Building script\n\n   \n\nDesigning converter\n\n";
        assert_eq!(
            format_thinking_preview(text),
            "Building script ↵ Designing converter"
        );
    }

    #[test]
    fn format_thinking_preview_strips_markdown_and_trims() {
        let text = "### Building script\n- Designing converter\n> Note";
        assert_eq!(
            format_thinking_preview(text),
            "Building script ↵ - Designing converter ↵ Note"
        );
    }

    #[test]
    fn thinking_row_displays_multiline_preview_with_newline_separator() {
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: false,
            entries: vec![GroupEntry::Thinking(GroupThinkingEntry {
                id: 0,
                text: "Building script\nDesigning converter".into(),
                tokens: 10,
                active: false,
                started_at: None,
                duration: None,
            })],
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
        let preview_span = tl.lines[1]
            .spans
            .iter()
            .find(|s| s.content.contains("Building script ↵ Designing converter"))
            .expect("multiline preview span found");
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
            GroupEntry::Tool(Box::new(GroupToolEntry {
                id: "t0".into(),
                name: Arc::from("tool"),
                summary: "op 0".into(),
                raw_input: None,
                children: Vec::new(),
                count: 1,
                status: ToolStatus::Success,
                receiving_args: false,
                started_at: None,
                duration: None,
                detail: None,
            })),
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
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
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
            entries.push(GroupEntry::Tool(Box::new(GroupToolEntry {
                id: format!("c{i}"),
                name: Arc::from("tool"),
                summary: format!("op {i}"),
                raw_input: None,
                children: Vec::new(),
                count: 1,
                status: ToolStatus::Success,
                receiving_args: false,
                started_at: None,
                duration: None,
                detail: None,
            })));
        }
        let group = ToolGroupRole {
            id: "group-done".into(),
            sealed: true,
            entries,
            started_at: None,
            duration: Some(Duration::from_secs(95)),
            waiting_since: None,
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
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
            entries.push(GroupEntry::Tool(Box::new(GroupToolEntry {
                id: format!("t{i}"),
                name: Arc::from("tool"),
                summary: format!("op {i}"),
                raw_input: None,
                children: Vec::new(),
                count: 1,
                status: ToolStatus::Success,
                receiving_args: false,
                started_at: None,
                duration: None,
                detail: None,
            })));
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
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
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

    #[test_case(true; "active")]
    #[test_case(false; "completed")]
    fn thinking_row_displays_duration_when_active_or_hovered(active: bool) {
        const DURATION_TEXT: &str = "(1.50s)";
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: !active,
            entries: vec![GroupEntry::Thinking(GroupThinkingEntry {
                id: 0,
                text: "thinking about something".into(),
                tokens: 350,
                active,
                started_at: None,
                duration: Some(Duration::from_millis(1500)),
            })],
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
        assert_eq!(tl.lines.len(), 2);
        assert!(
            tl.lines[1]
                .spans
                .iter()
                .any(|s| s.content.as_ref() == "thinking")
        );
        let thinking_line = tl.lines[1].to_string();
        assert_eq!(thinking_line.ends_with(DURATION_TEXT), active);
        assert!(!thinking_line.contains("350t"));

        let target = CompactTarget {
            group_id: group.id.clone(),
            entry: 0,
            child: None,
        };
        let hovered = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, Some(&target));
        assert!(hovered.lines[1].to_string().ends_with(DURATION_TEXT));
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
    #[test_case("task_message", Some(serde_json::json!({"id": "Hawk", "message": "wrap up", "toolcall_reason": "steering hawk"})), "fallback", "→ Hawk · steering hawk" ; "task_message_names_the_recipient")]
    #[test_case("task_message", Some(serde_json::json!({"message": "IOC list stale", "toolcall_reason": "reporting risk"})), "fallback", "→ parent · reporting risk" ; "task_message_report_names_the_parent")]
    #[test_case("task_message", Some(serde_json::json!({"id": "  ", "message": "wrap up", "toolcall_reason": "steering"})), "fallback", "→ parent · steering" ; "task_message_blank_id_falls_back_to_parent")]
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

    #[test_case("3 lines", 2, "3 lines · 2 tools" ; "keeps_line_count")]
    #[test_case("3 lines · 1 tools", 2, "3 lines · 2 tools" ; "replaces_previous_count")]
    #[test_case("run script · 3 lines", 2, "run script · 3 lines · 2 tools" ; "keeps_reason")]
    fn update_code_execution_summary(current: &str, count: usize, expected: &str) {
        assert_eq!(
            update_code_execution_compact_summary(current, count),
            expected
        );
    }

    #[test]
    fn code_execution_children_come_from_saved_call_state() {
        let output = ToolOutput::Plain(maki_agent::TextOutput {
            text: "script output".into(),
            instructions: None,
            state: Some(serde_json::json!({
                "calls": [
                    {
                        "tool": "read",
                        "params": {
                            "path": "src/main.rs",
                            "offset": 1,
                            "limit": 50,
                            "toolcall_reason": "reading entry point"
                        },
                        "status": "success",
                        "duration_ms": 120
                    },
                    {
                        "tool": "bash",
                        "params": { "command": "cargo check" },
                        "status": "error",
                        "duration_ms": 30,
                        "annotation": "exit 1"
                    },
                    { "tool": "grep", "status": "running" }
                ]
            })),
            loaded_tools: Vec::new(),
            duration_ms: None,
        });
        let children = code_execution_children(Some(&output));
        assert_eq!(children.len(), 3);
        assert!(children.iter().all(|child| child.count == 1));
        assert_eq!(children[0].name.as_ref(), "read");
        assert_eq!(
            children[0].summary,
            "reading entry point · src/main.rs:1-50"
        );
        assert_eq!(children[0].status, ToolStatus::Success);
        assert_eq!(children[0].duration, Some(Duration::from_millis(120)));
        assert_eq!(children[1].name.as_ref(), "bash");
        assert_eq!(children[1].summary, "exit 1");
        assert_eq!(children[1].status, ToolStatus::Error);
        assert_eq!(children[2].status, ToolStatus::InProgress);
        assert!(children[2].duration.is_none());
    }

    #[test]
    fn compact_row_shows_mcp_wire_name_with_prefix_and_dot() {
        const DISPLAY_NAME: &str = "mcp: mdserver.search_documents";
        let entry = GroupToolEntry {
            id: String::new(),
            name: Arc::from("mdserver__search_documents"),
            summary: "1 line".into(),
            raw_input: None,
            children: Vec::new(),
            count: 1,
            status: ToolStatus::Success,
            receiving_args: false,
            started_at: None,
            duration: None,
            detail: None,
        };
        let row = render_tool_row(
            "",
            &entry,
            ("", Style::default()),
            80,
            &mut Vec::new(),
            0,
            RowHover {
                hovered: false,
                openable: false,
            },
        );
        assert_eq!(row.spans[1].content, DISPLAY_NAME);
        assert!(row.to_string().starts_with(&format!("✓ {DISPLAY_NAME}")));
    }

    fn calls_output(calls: Value) -> ToolOutput {
        ToolOutput::Plain(maki_agent::TextOutput {
            text: String::new(),
            instructions: None,
            state: Some(serde_json::json!({ "calls": calls })),
            loaded_tools: Vec::new(),
            duration_ms: None,
        })
    }

    #[test]
    fn code_execution_children_merge_reasonless_calls_of_same_tool() {
        let output = calls_output(serde_json::json!([
            {
                "tool": "bash",
                "params": { "command": "cargo check" },
                "status": "success",
                "duration_ms": 120
            },
            {
                "tool": "read",
                "params": { "path": "src/main.rs", "toolcall_reason": "reading entry point" },
                "status": "success",
                "duration_ms": 10
            },
            {
                "tool": "bash",
                "params": { "command": "cargo test" },
                "status": "error",
                "duration_ms": 30,
                "annotation": "exit 1"
            }
        ]));
        let children = code_execution_children(Some(&output));
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].name.as_ref(), "bash");
        assert_eq!(children[0].count, 2);
        assert_eq!(children[0].status, ToolStatus::Error);
        assert_eq!(children[0].duration, Some(Duration::from_millis(150)));
        assert_eq!(children[0].summary, "exit 1");
        assert!(children[0].raw_input.is_none());
        assert_eq!(children[1].name.as_ref(), "read");
        assert_eq!(children[1].count, 1);
        assert_eq!(children[1].summary, "reading entry point · src/main.rs:1");
    }

    #[test]
    fn code_execution_children_keep_reasoned_calls_separate() {
        let output = calls_output(serde_json::json!([
            {
                "tool": "read",
                "params": { "path": "a.rs", "toolcall_reason": "first" },
                "status": "success"
            },
            {
                "tool": "read",
                "params": { "path": "b.rs", "toolcall_reason": "second" },
                "status": "success"
            }
        ]));
        let children = code_execution_children(Some(&output));
        assert_eq!(children.len(), 2);
        assert!(children.iter().all(|child| child.count == 1));
    }

    #[test]
    fn code_execution_merged_children_sum_numeric_annotations() {
        let output = calls_output(serde_json::json!([
            { "tool": "read", "status": "success", "annotation": "50 lines" },
            { "tool": "read", "status": "success", "annotation": "30 lines" },
            { "tool": "grep", "status": "success", "annotation": "4 matches in 2 files" },
            { "tool": "grep", "status": "success", "annotation": "3 matches in 1 files" },
            { "tool": "grep", "status": "success", "annotation": "2 matches in 1 file" }
        ]));
        let children = code_execution_children(Some(&output));
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].summary, "80 lines");
        // Same shape sums per position; a different shape (singular "file")
        // stays a separate note.
        assert_eq!(
            children[1].summary,
            "7 matches in 3 files · 2 matches in 1 file"
        );
    }

    #[test]
    fn code_execution_merged_children_dedupe_opaque_annotations() {
        let output = calls_output(serde_json::json!([
            { "tool": "bash", "status": "error", "annotation": "exit 1" },
            { "tool": "bash", "status": "error", "annotation": "exit 2" },
            { "tool": "bash", "status": "error", "annotation": "exit 1 · allowed by memory" }
        ]));
        let children = code_execution_children(Some(&output));
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].count, 3);
        assert_eq!(children[0].summary, "exit 1 · exit 2 · allowed by memory");
        assert!(children[0].duration.is_none());
    }

    #[test]
    fn code_execution_merged_children_keep_usage_notes_as_text() {
        let output = calls_output(serde_json::json!([
            { "tool": "task", "status": "success", "usage": "1.2k↑ 300↓" },
            { "tool": "task", "status": "success", "usage": "1.2k↑ 300↓" },
            { "tool": "task", "status": "success", "usage": "2.0k↑ 100↓" }
        ]));
        let children = code_execution_children(Some(&output));
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].count, 3);
        assert_eq!(children[0].summary, "1.2k↑ 300↓ · 2.0k↑ 100↓");
    }

    #[test_case(ToolStatus::Success, ToolStatus::Success, ToolStatus::Success ; "all_success")]
    #[test_case(ToolStatus::Success, ToolStatus::InProgress, ToolStatus::InProgress ; "running_wins_over_success")]
    #[test_case(ToolStatus::InProgress, ToolStatus::Error, ToolStatus::Error ; "error_wins_over_running")]
    #[test_case(ToolStatus::Error, ToolStatus::Success, ToolStatus::Error ; "error_wins_over_success")]
    fn code_execution_merged_status_is_most_severe(
        first: ToolStatus,
        second: ToolStatus,
        expected: ToolStatus,
    ) {
        let status_str = |status: ToolStatus| match status {
            ToolStatus::Success => "success",
            ToolStatus::Error => "error",
            ToolStatus::InProgress => "running",
        };
        let output = calls_output(serde_json::json!([
            { "tool": "bash", "status": status_str(first) },
            { "tool": "bash", "status": status_str(second) }
        ]));
        let children = code_execution_children(Some(&output));
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].status, expected);
    }

    #[test]
    fn merged_child_row_renders_count_suffix() {
        const MERGED_LABEL: &str = "bash x 2";
        let parent = GroupToolEntry {
            id: "call-1".into(),
            name: Arc::from(CODE_EXECUTION_TOOL),
            summary: "2 tools".into(),
            raw_input: None,
            children: vec![GroupToolEntry {
                id: String::new(),
                name: Arc::from("bash"),
                summary: String::new(),
                raw_input: None,
                children: Vec::new(),
                count: 2,
                status: ToolStatus::Success,
                receiving_args: false,
                started_at: None,
                duration: None,
                detail: None,
            }],
            count: 1,
            status: ToolStatus::Success,
            receiving_args: false,
            started_at: None,
            duration: None,
            detail: None,
        };
        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: true,
            entries: vec![GroupEntry::Tool(Box::new(parent))],
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };
        let tl = build_tool_group_lines(&group, 80, 0, DEFAULT_COMPACT_LINES, None);
        let child_row = tl
            .lines
            .iter()
            .find(|line| line.to_string().contains(MERGED_LABEL))
            .expect("merged child row");
        assert!(child_row.to_string().contains(MERGED_LABEL));
    }

    #[test_case(None ; "no_output")]
    #[test_case(Some(serde_json::json!({})) ; "no_calls")]
    fn code_execution_children_empty_without_calls(state: Option<Value>) {
        let output = ToolOutput::Plain(maki_agent::TextOutput {
            text: String::new(),
            instructions: None,
            state,
            loaded_tools: Vec::new(),
            duration_ms: None,
        });
        assert!(code_execution_children(Some(&output)).is_empty());
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

    #[test]
    fn expanded_group_renders_all_entries_without_hidden_notice() {
        let child = GroupToolEntry {
            id: String::new(),
            name: Arc::from("read"),
            summary: "src/main.rs".into(),
            raw_input: None,
            children: Vec::new(),
            count: 1,
            status: ToolStatus::Success,
            receiving_args: false,
            started_at: None,
            duration: None,
            detail: None,
        };
        let entries: Vec<GroupEntry> = (0..15)
            .map(|_| GroupEntry::Tool(Box::new(child.clone())))
            .collect();
        let group_collapsed = ToolGroupRole {
            id: "group".into(),
            sealed: true,
            entries: entries.clone(),
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };
        let tl_collapsed =
            build_tool_group_lines(&group_collapsed, 80, 0, DEFAULT_COMPACT_LINES, None);
        let collapsed_text = tl_collapsed
            .lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(collapsed_text.contains("... (5 hidden)"));

        let group_expanded = ToolGroupRole {
            id: "group".into(),
            sealed: true,
            entries,
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: true,
        };
        let tl_expanded =
            build_tool_group_lines(&group_expanded, 80, 0, DEFAULT_COMPACT_LINES, None);
        let expanded_text = tl_expanded
            .lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!expanded_text.contains("hidden"));
        // 1 header line + 15 entry lines = 16 lines
        assert_eq!(tl_expanded.lines.len(), 16);
    }

    #[test]
    fn hides_all_entries_when_max_entries_is_zero() {
        let entries: Vec<GroupEntry> = (0..3)
            .map(|i| {
                GroupEntry::Tool(Box::new(GroupToolEntry {
                    id: format!("call-{i}"),
                    name: Arc::from("read"),
                    summary: format!("file_{i}.rs"),
                    raw_input: None,
                    children: Vec::new(),
                    count: 1,
                    status: ToolStatus::Success,
                    receiving_args: false,
                    started_at: None,
                    duration: Some(Duration::from_millis(100)),
                    detail: None,
                }))
            })
            .collect();

        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: true,
            entries,
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, 0, None);
        // Header + hidden line using RAIL_LAST because no entries follow
        assert_eq!(tl.lines.len(), 2);
        let hidden_line: String = tl.lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(hidden_line.starts_with(RAIL_LAST));
        assert!(hidden_line.contains("... (3 hidden)"));
    }

    #[test]
    fn custom_max_entries_limits_visible_rows() {
        let entries: Vec<GroupEntry> = (0..6)
            .map(|i| {
                GroupEntry::Tool(Box::new(GroupToolEntry {
                    id: format!("call-{i}"),
                    name: Arc::from("read"),
                    summary: format!("file_{i}.rs"),
                    raw_input: None,
                    children: Vec::new(),
                    count: 1,
                    status: ToolStatus::Success,
                    receiving_args: false,
                    started_at: None,
                    duration: Some(Duration::from_millis(100)),
                    detail: None,
                }))
            })
            .collect();

        let group = ToolGroupRole {
            id: "group-1".into(),
            sealed: true,
            entries,
            started_at: None,
            duration: None,
            waiting_since: None,
            expanded: false,
        };

        let tl = build_tool_group_lines(&group, 80, 0, 2, None);
        // Header + 1 hidden line + 2 visible entries = 4 lines
        assert_eq!(tl.lines.len(), 4);
        let hidden_line: String = tl.lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(hidden_line.starts_with(RAIL_CONTINUE));
        assert!(hidden_line.contains("... (4 hidden)"));
    }

    #[test]
    fn extract_batch_child_output_parses_sections() {
        let batch_output = "## read\nfile contents line 1\nfile contents line 2\n\n## bash\n[ERROR] cargo error: unresolved\n\nAll 2 tools executed successfully.";
        assert_eq!(
            extract_batch_child_output(batch_output, 0),
            Some("file contents line 1\nfile contents line 2".to_string())
        );
        assert_eq!(
            extract_batch_child_output(batch_output, 1),
            Some("cargo error: unresolved".to_string())
        );
        assert_eq!(extract_batch_child_output(batch_output, 2), None);
    }

    #[test]
    fn test_is_background_bash() {
        assert!(is_background_bash(
            "bash",
            Some(&serde_json::json!({"command": "cargo check", "background": true}))
        ));
        assert!(is_background_bash(
            "bash",
            Some(&serde_json::json!({"command": "cargo check", "background": "true"}))
        ));
        assert!(!is_background_bash(
            "bash",
            Some(&serde_json::json!({"command": "cargo check", "background": false}))
        ));
        assert!(!is_background_bash(
            "bash",
            Some(&serde_json::json!({"command": "cargo check"}))
        ));
        assert!(!is_background_bash("bash", None));
        assert!(!is_background_bash(
            "read",
            Some(&serde_json::json!({"path": "foo.rs", "background": true}))
        ));
    }

    #[test]
    fn question_children_multiple_with_headers_and_state_answers() {
        let input = serde_json::json!({
            "questions": [
                {
                    "question": "Which breakfast items?",
                    "header": "Breakfast",
                    "options": [{ "label": "eggs" }, { "label": "toast" }]
                },
                {
                    "question": "Sweet or savory?",
                    "header": "Taste",
                    "options": [{ "label": "savory" }]
                }
            ]
        });
        let output = ToolOutput::Plain(maki_agent::TextOutput {
            text: "Breakfast: eggs, toast\nTaste: savory".into(),
            instructions: None,
            state: Some(serde_json::json!({
                "answers": [["eggs", "toast"], ["savory"]]
            })),
            loaded_tools: Vec::new(),
            duration_ms: None,
        });

        let children = question_children(Some(&input), Some(&output));
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].name.as_ref(), "Breakfast:");
        assert_eq!(children[0].summary, "eggs, toast");
        assert_eq!(children[0].status, ToolStatus::Success);
        assert!(children[0].detail.is_some());

        assert_eq!(children[1].name.as_ref(), "Taste:");
        assert_eq!(children[1].summary, "savory");
        assert_eq!(children[1].status, ToolStatus::Success);
        assert!(children[1].detail.is_some());
    }

    #[test]
    fn question_children_omitted_header_falls_back_to_q_index() {
        let input = serde_json::json!({
            "questions": [
                { "question": "First question?" },
                { "question": "Second question?" }
            ]
        });
        let output = ToolOutput::Plain(maki_agent::TextOutput {
            text: "Q1: yes\nQ2: no".into(),
            instructions: None,
            state: Some(serde_json::json!({
                "answers": [["yes"], ["no"]]
            })),
            loaded_tools: Vec::new(),
            duration_ms: None,
        });

        let children = question_children(Some(&input), Some(&output));
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].name.as_ref(), "Q1:");
        assert_eq!(children[0].summary, "yes");
        assert_eq!(children[1].name.as_ref(), "Q2:");
        assert_eq!(children[1].summary, "no");
    }

    #[test]
    fn question_children_dismissed_returns_empty() {
        let input = serde_json::json!({
            "questions": [{ "question": "Do you agree?", "header": "Confirm" }]
        });
        let output_state = ToolOutput::Plain(maki_agent::TextOutput {
            text: "(question dismissed by user)".into(),
            instructions: None,
            state: Some(serde_json::json!({ "dismissed": true })),
            loaded_tools: Vec::new(),
            duration_ms: None,
        });
        assert!(question_children(Some(&input), Some(&output_state)).is_empty());

        let output_text_only = ToolOutput::Plain(maki_agent::TextOutput {
            text: "(question dismissed by user)".into(),
            instructions: None,
            state: None,
            loaded_tools: Vec::new(),
            duration_ms: None,
        });
        assert!(question_children(Some(&input), Some(&output_text_only)).is_empty());
    }

    #[test]
    fn question_children_no_answers_formats_as_no_answer() {
        let input = serde_json::json!({
            "questions": [{ "question": "Skipped question?", "header": "Optional" }]
        });
        let output = ToolOutput::Plain(maki_agent::TextOutput {
            text: "Optional: (no answer)".into(),
            instructions: None,
            state: Some(serde_json::json!({ "answers": [[]] })),
            loaded_tools: Vec::new(),
            duration_ms: None,
        });

        let children = question_children(Some(&input), Some(&output));
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name.as_ref(), "Optional:");
        assert_eq!(children[0].summary, "(no answer)");
    }

    #[test]
    fn question_children_fallback_from_display_text_without_state() {
        let input = serde_json::json!({
            "questions": [
                { "question": "Item A?", "header": "A" },
                { "question": "Item B?", "header": "B" }
            ]
        });
        let output = ToolOutput::Plain(maki_agent::TextOutput {
            text: "A: alpha\nB: beta".into(),
            instructions: None,
            state: None,
            loaded_tools: Vec::new(),
            duration_ms: None,
        });

        let children = question_children(Some(&input), Some(&output));
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].name.as_ref(), "A:");
        assert_eq!(children[0].summary, "alpha");
        assert_eq!(children[1].name.as_ref(), "B:");
        assert_eq!(children[1].summary, "beta");
    }

    #[test]
    fn question_children_header_already_ending_with_colon() {
        let input = serde_json::json!({
            "questions": [{ "question": "Pick one", "header": "Choice:" }]
        });
        let output = ToolOutput::Plain(maki_agent::TextOutput {
            text: "Choice: x".into(),
            instructions: None,
            state: Some(serde_json::json!({ "answers": [["x"]] })),
            loaded_tools: Vec::new(),
            duration_ms: None,
        });

        let children = question_children(Some(&input), Some(&output));
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name.as_ref(), "Choice:");
    }
}
