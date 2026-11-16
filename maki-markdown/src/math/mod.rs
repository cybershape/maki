//! LaTeX math to Unicode conversion, ported from xai-org/grok-build
//! `xai-grok-markdown/src/latex` (Apache-2.0). Best-effort and total: unknown
//! commands degrade to their bare name, oversized or deeply nested input
//! degrades instead of panicking, so streaming partial formulas are safe.

mod commands;
mod cursor;
mod environments;
mod math_box;
mod symbols;

#[cfg(test)]
mod tests;

use commands::render_sequence;
use cursor::Cursor;
use math_box::MathBox;
use unicode_width::UnicodeWidthStr;

/// Inputs larger than this are not converted (callers fall back to raw display).
/// Guards the streaming hot path: the tail is re-rendered on every chunk, so conversion cost must stay trivially small.
const MAX_MATH_SOURCE_LEN: usize = 4096;

/// Hard cap on group-nesting recursion.
/// Inputs deeper than this render their remaining content flatly rather than recursing further.
const MAX_DEPTH: usize = 32;

/// Convert inline math to a single-line Unicode string.
/// Row separators (`\\`) collapse to `; ` and multi-row environments render single-row, so inline math never introduces a line break mid-paragraph.
/// Returns `None` when the source is too large to convert (see [`MAX_MATH_SOURCE_LEN`]).
pub(crate) fn latex_to_unicode_inline(src: &str) -> Option<String> {
    if src.len() > MAX_MATH_SOURCE_LEN {
        return None;
    }
    let lines = convert(src, true);
    let joined = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    Some(joined)
}

/// Convert display math to one or more Unicode lines.
/// Leading whitespace is structural (box alignment) and preserved; only line ends are trimmed.
/// Returns `None` when the source is too large to convert (see [`MAX_MATH_SOURCE_LEN`]).
pub(crate) fn latex_to_unicode_display(src: &str) -> Option<Vec<String>> {
    if src.len() > MAX_MATH_SOURCE_LEN {
        return None;
    }
    let lines: Vec<String> = convert(src, false)
        .into_iter()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    Some(lines)
}

/// Run the converter and return the output lines.
fn convert(src: &str, flat: bool) -> Vec<String> {
    let mut cursor = Cursor::new(src);
    let mut out = MathBox::new(flat);
    render_sequence(&mut cursor, &mut out, 0, Mode::Math, None);
    out.into_lines()
}

/// Rendering mode: math mode applies typographic substitutions (`-` → `−`, `'` → `′`) that text fragments (`\text{...}`) must not receive.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Mode {
    Math,
    Text,
}

/// Cleans raw LaTeX formula by stripping outer delimiters and normalizing common LLM artifacts.
pub(crate) fn preprocess_latex(raw: &str) -> String {
    let trimmed = raw.trim();
    let inner = if let Some(stripped) = trimmed
        .strip_prefix("$$")
        .and_then(|s| s.strip_suffix("$$"))
    {
        stripped.trim()
    } else if let Some(stripped) = trimmed
        .strip_prefix("\\[")
        .and_then(|s| s.strip_suffix("\\]"))
    {
        stripped.trim()
    } else if let Some(stripped) = trimmed
        .strip_prefix("\\(")
        .and_then(|s| s.strip_suffix("\\)"))
    {
        stripped.trim()
    } else if let Some(stripped) = trimmed.strip_prefix('$').and_then(|s| s.strip_suffix('$')) {
        stripped.trim()
    } else {
        trimmed
    };

    normalize_escapes(inner)
}

fn normalize_escapes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek()
                && next == '_'
            {
                chars.next();
                out.push('_');
                continue;
            }
            out.push('\\');
        } else {
            out.push(c);
        }
    }
    out
}

/// Renders display math into multiple lines of text, falling back to the raw
/// source when the converter declines (oversized input) or produces nothing.
pub(crate) fn render_math_lines(source: &str, max_width: usize) -> Vec<String> {
    let clean = preprocess_latex(source);
    if clean.is_empty() {
        return Vec::new();
    }

    if let Some(lines) = latex_to_unicode_display(&clean)
        && !lines.is_empty()
    {
        if max_width == 0 || lines.iter().all(|l| l.width() <= max_width) {
            return lines;
        }
        let mut wrapped = Vec::new();
        for line in lines {
            if line.width() > max_width {
                wrapped.extend(wrap_text(&line, max_width));
            } else {
                wrapped.push(line);
            }
        }
        return wrapped;
    }

    if max_width > 0 && clean.width() > max_width {
        wrap_text(&clean, max_width)
    } else {
        vec![clean]
    }
}

fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;

    for word in text.split_whitespace() {
        let w_width = word.width();
        if current.is_empty() {
            current.push_str(word);
            current_width = w_width;
        } else if current_width + 1 + w_width <= max_width {
            current.push(' ');
            current.push_str(word);
            current_width += 1 + w_width;
        } else {
            lines.push(current);
            current = word.to_string();
            current_width = w_width;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}