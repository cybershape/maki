use unicode_width::UnicodeWidthStr;

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

    // Replace escaped underscores inside \text{...} or general math
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

pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Linear representation of LaTeX fractions and commands for constrained width.
pub(crate) fn linearize_latex(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut pos = 0;
    let chars: Vec<char> = src.chars().collect();
    let len = chars.len();

    while pos < len {
        if chars[pos] == '\\' {
            // Check commands
            let rest: String = chars[pos..].iter().collect();
            if rest.starts_with("\\frac") {
                pos += 5;
                if let Some((num, next_pos)) = extract_brace_group(&chars, pos)
                    && let Some((den, final_pos)) = extract_brace_group(&chars, next_pos)
                {
                    out.push_str(&linearize_latex(&num));
                    out.push_str(" / ");
                    let den_lin = linearize_latex(&den);
                    if den_lin.contains(' ') || den_lin.contains('+') || den_lin.contains('-') {
                        out.push('(');
                        out.push_str(&den_lin);
                        out.push(')');
                    } else {
                        out.push_str(&den_lin);
                    }
                    pos = final_pos;
                    continue;
                }
            } else if rest.starts_with("\\text{") || rest.starts_with("\\mathrm{") {
                let prefix_len = if rest.starts_with("\\text{") { 6 } else { 8 };
                pos += prefix_len - 1;
                if let Some((inner, next_pos)) = extract_brace_group(&chars, pos) {
                    out.push_str(&inner);
                    pos = next_pos;
                    continue;
                }
            } else if rest.starts_with("\\cdot") {
                out.push('·');
                pos += 5;
                continue;
            } else if rest.starts_with("\\times") {
                out.push('×');
                pos += 6;
                continue;
            } else if rest.starts_with("\\le") || rest.starts_with("\\leq") {
                out.push('≤');
                pos += if rest.starts_with("\\leq") { 4 } else { 3 };
                continue;
            } else if rest.starts_with("\\ge") || rest.starts_with("\\geq") {
                out.push('≥');
                pos += if rest.starts_with("\\geq") { 4 } else { 3 };
                continue;
            } else if rest.starts_with("\\ne") || rest.starts_with("\\neq") {
                out.push('≠');
                pos += if rest.starts_with("\\neq") { 4 } else { 3 };
                continue;
            } else if rest.starts_with("\\approx") {
                out.push('≈');
                pos += 7;
                continue;
            }
        }
        out.push(chars[pos]);
        pos += 1;
    }
    out
}

fn extract_brace_group(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut i = start;
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    if i >= chars.len() || chars[i] != '{' {
        return None;
    }
    i += 1;
    let mut depth = 1;
    let mut content = String::new();
    while i < chars.len() {
        if chars[i] == '{' {
            depth += 1;
            content.push('{');
        } else if chars[i] == '}' {
            depth -= 1;
            if depth == 0 {
                return Some((content, i + 1));
            }
            content.push('}');
        } else {
            content.push(chars[i]);
        }
        i += 1;
    }
    None
}

/// Renders display math into multiple lines of text.
pub(crate) fn render_math_lines(source: &str, max_width: usize) -> Vec<String> {
    let clean = preprocess_latex(source);
    if clean.is_empty() {
        return Vec::new();
    }

    if let Ok(tree) = txm::layout(&clean)
        && (max_width == 0 || tree.width <= max_width)
        && let Ok(rendered) = txm::render(&clean)
    {
        let lines: Vec<String> = rendered
            .split('\n')
            .map(|l| strip_ansi(l).trim_end().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if !lines.is_empty() {
            return lines;
        }
    }

    // Fallback: linearized math wrapped to max_width
    let linear = linearize_latex(&clean);
    if max_width > 0 && linear.width() > max_width {
        wrap_text(&linear, max_width)
    } else {
        vec![linear]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_rate_formula_renders_2d_lines() {
        let formula = r"\text{Cache Hit Rate} = \frac{\text{cache\_read}}{\text{total\_input}} = \frac{\text{cache\_read}}{\text{input} + \text{cache\_read} + \text{cache\_creation}}";
        let lines = render_math_lines(formula, 120);
        assert_eq!(
            lines.len(),
            3,
            "Expected 3 lines: numerator, bar, denominator"
        );
        assert!(lines[0].contains("cache_read"));
        assert!(lines[1].contains("Cache Hit Rate ="));
        assert!(lines[1].contains("─"));
        assert!(lines[2].contains("total_input"));
        assert!(lines[2].contains("cache_creation"));
    }

    #[test]
    fn narrow_width_falls_back_to_linear() {
        let formula = r"\text{Cache Hit Rate} = \frac{\text{cache\_read}}{\text{total\_input}} = \frac{\text{cache\_read}}{\text{input} + \text{cache\_read} + \text{cache\_creation}}";
        let lines = render_math_lines(formula, 30);
        assert!(!lines.is_empty());
        assert!(lines[0].contains("Cache Hit Rate"));
        assert!(lines.iter().any(|l| l.contains("/")));
    }

    #[test]
    fn malformed_formula_never_panics() {
        let malformed = r"\frac{incomplete";
        let lines = render_math_lines(malformed, 80);
        assert!(!lines.is_empty());
    }
}
