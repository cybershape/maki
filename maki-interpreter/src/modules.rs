//! Built-in Python modules provided by the sandbox host (`urllib.parse`, `glob`).
//! These modules provide standard Python APIs for pure-logic operations without
//! requiring external dependencies or full CPython runtime.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use monty_types::{ExcType, MontyException, MontyObject, MontyUuid};
use serde_json::Value;
use url::Url;

use crate::runner::OsHost;

const MODULE_URLLIB_ID: u128 = 0x1001;
const MODULE_URLLIB_PARSE_ID: u128 = 0x1002;
const MODULE_GLOB_ID: u128 = 0x1003;

const PARSE_RESULT_FIELDS: &[&str] = &["scheme", "netloc", "path", "params", "query", "fragment"];
const SPLIT_RESULT_FIELDS: &[&str] = &["scheme", "netloc", "path", "query", "fragment"];

/// Responds to Python `import <name>` calls with host-provided modules.
pub(crate) fn serve_import(name: &str) -> Option<MontyObject> {
    match name {
        "urllib" => Some(urllib_module()),
        "urllib.parse" => Some(urllib_parse_module()),
        "glob" => Some(glob_module()),
        _ => None,
    }
}

/// Whether the function call is routed to host-provided modules.
pub(crate) fn is_module_fn(name: &str) -> bool {
    name.starts_with("urllib.parse.") || name.starts_with("glob.")
}

/// Dispatches calls to functions in host-provided modules.
pub(crate) fn call_module_fn(
    name: &str,
    args: &[Value],
    kwargs: &[(String, Value)],
    host: &mut OsHost<'_>,
) -> Result<MontyObject, MontyException> {
    match name {
        "urllib.parse.urlparse" => parse_url_fn(args, kwargs, false),
        "urllib.parse.urlsplit" => parse_url_fn(args, kwargs, true),
        "urllib.parse.urlunparse" => unparse_url_fn(args, false),
        "urllib.parse.urlunsplit" => unparse_url_fn(args, true),
        "urllib.parse.urljoin" => urljoin_fn(args, kwargs),
        "urllib.parse.quote" => quote_fn(args, kwargs, false),
        "urllib.parse.quote_plus" => quote_fn(args, kwargs, true),
        "urllib.parse.unquote" => unquote_fn(args, false),
        "urllib.parse.unquote_plus" => unquote_fn(args, true),
        "urllib.parse.parse_qs" => parse_qs_fn(args, kwargs),
        "urllib.parse.parse_qsl" => parse_qsl_fn(args, kwargs),
        "urllib.parse.urlencode" => urlencode_fn(args, kwargs),
        "glob.glob" | "glob.iglob" => glob_fn(args, kwargs, host),
        "glob.escape" => glob_escape_fn(args),
        _ => Err(type_error(format!("unknown module function: {name}"))),
    }
}

fn urllib_module() -> MontyObject {
    let parse = urllib_parse_module();
    MontyObject::module(
        "urllib",
        MontyUuid::from_u128(MODULE_URLLIB_ID),
        vec![(MontyObject::string("parse"), parse)],
    )
}

fn urllib_parse_module() -> MontyObject {
    let funcs = [
        "urlparse",
        "urlsplit",
        "urlunparse",
        "urlunsplit",
        "urljoin",
        "quote",
        "quote_plus",
        "unquote",
        "unquote_plus",
        "parse_qs",
        "parse_qsl",
        "urlencode",
    ];
    let attrs = funcs.into_iter().map(|name| {
        (
            MontyObject::string(name),
            MontyObject::function(format!("urllib.parse.{name}"), None),
        )
    });
    MontyObject::module(
        "urllib.parse",
        MontyUuid::from_u128(MODULE_URLLIB_PARSE_ID),
        attrs,
    )
}

fn glob_module() -> MontyObject {
    let funcs = ["glob", "iglob", "escape"];
    let attrs = funcs.into_iter().map(|name| {
        (
            MontyObject::string(name),
            MontyObject::function(format!("glob.{name}"), None),
        )
    });
    MontyObject::module("glob", MontyUuid::from_u128(MODULE_GLOB_ID), attrs)
}

fn type_error(msg: impl Into<String>) -> MontyException {
    MontyException::new(ExcType::TypeError, Some(msg.into()))
}

fn value_error(msg: impl Into<String>) -> MontyException {
    MontyException::new(ExcType::ValueError, Some(msg.into()))
}

fn get_kwarg<'a>(kwargs: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    kwargs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn parse_url_fn(
    args: &[Value],
    kwargs: &[(String, Value)],
    split_mode: bool,
) -> Result<MontyObject, MontyException> {
    let url_str = args
        .first()
        .or_else(|| get_kwarg(kwargs, "urlstring"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            type_error("urlparse() missing 1 required positional argument: 'urlstring'")
        })?;

    let default_scheme = args
        .get(1)
        .or_else(|| get_kwarg(kwargs, "scheme"))
        .and_then(Value::as_str)
        .unwrap_or("");

    let allow_fragments = args
        .get(2)
        .or_else(|| get_kwarg(kwargs, "allow_fragments"))
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let parsed = parse_url_parts(url_str, default_scheme, allow_fragments);

    if split_mode {
        Ok(MontyObject::named_tuple(
            "SplitResult",
            SPLIT_RESULT_FIELDS.iter().copied(),
            [
                MontyObject::string(parsed.scheme),
                MontyObject::string(parsed.netloc),
                MontyObject::string(parsed.path),
                MontyObject::string(parsed.query),
                MontyObject::string(parsed.fragment),
            ],
        ))
    } else {
        Ok(MontyObject::named_tuple(
            "ParseResult",
            PARSE_RESULT_FIELDS.iter().copied(),
            [
                MontyObject::string(parsed.scheme),
                MontyObject::string(parsed.netloc),
                MontyObject::string(parsed.path),
                MontyObject::string(parsed.params),
                MontyObject::string(parsed.query),
                MontyObject::string(parsed.fragment),
            ],
        ))
    }
}

struct ParsedUrlParts {
    scheme: String,
    netloc: String,
    path: String,
    params: String,
    query: String,
    fragment: String,
}

fn parse_url_parts(url_str: &str, default_scheme: &str, allow_fragments: bool) -> ParsedUrlParts {
    let mut rest = url_str;
    let mut scheme = String::new();
    let mut netloc = String::new();
    let path;
    let mut params = String::new();
    let mut query = String::new();
    let mut fragment = String::new();

    // 1. Fragment
    if allow_fragments && let Some((r, f)) = rest.split_once('#') {
        rest = r;
        fragment = f.to_owned();
    }

    // 2. Query
    if let Some((r, q)) = rest.split_once('?') {
        rest = r;
        query = q.to_owned();
    }

    // 3. Scheme
    if let Some((s, r)) = rest.split_once(':')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
        && !s.is_empty()
    {
        scheme = s.to_ascii_lowercase();
        rest = r;
    }
    if scheme.is_empty() && !default_scheme.is_empty() {
        scheme = default_scheme.to_ascii_lowercase();
    }

    // 4. Netloc
    if let Some(after_slashes) = rest.strip_prefix("//") {
        let (nl, p) = match after_slashes.find(['/', '?', '#']) {
            Some(idx) => (&after_slashes[..idx], &after_slashes[idx..]),
            None => (after_slashes, ""),
        };
        netloc = nl.to_owned();
        rest = p;
    }

    // 5. Params
    if let Some((p, par)) = rest.split_once(';') {
        path = p.to_owned();
        params = par.to_owned();
    } else {
        path = rest.to_owned();
    }

    ParsedUrlParts {
        scheme,
        netloc,
        path,
        params,
        query,
        fragment,
    }
}

fn unparse_url_fn(args: &[Value], split_mode: bool) -> Result<MontyObject, MontyException> {
    let parts = args
        .first()
        .and_then(Value::as_array)
        .ok_or_else(|| type_error("urlunparse() takes exactly 1 sequence argument"))?;

    let expected_len = if split_mode { 5 } else { 6 };
    if parts.len() < expected_len {
        return Err(value_error(format!(
            "urlunparse() argument sequence must have at least {expected_len} items"
        )));
    }

    let str_part = |idx: usize| -> &str { parts.get(idx).and_then(Value::as_str).unwrap_or("") };

    let scheme = str_part(0);
    let netloc = str_part(1);
    let path = str_part(2);
    let (params, query, fragment) = if split_mode {
        ("", str_part(3), str_part(4))
    } else {
        (str_part(3), str_part(4), str_part(5))
    };

    let mut result = String::new();
    if !scheme.is_empty() {
        result.push_str(scheme);
        result.push(':');
    }
    if !netloc.is_empty() || (!scheme.is_empty() && path.starts_with('/')) {
        result.push_str("//");
        result.push_str(netloc);
    }
    result.push_str(path);
    if !params.is_empty() {
        result.push(';');
        result.push_str(params);
    }
    if !query.is_empty() {
        result.push('?');
        result.push_str(query);
    }
    if !fragment.is_empty() {
        result.push('#');
        result.push_str(fragment);
    }

    Ok(MontyObject::string(result))
}

fn urljoin_fn(args: &[Value], kwargs: &[(String, Value)]) -> Result<MontyObject, MontyException> {
    let base = args
        .first()
        .or_else(|| get_kwarg(kwargs, "base"))
        .and_then(Value::as_str)
        .ok_or_else(|| type_error("urljoin() missing required argument: 'base'"))?;
    let url = args
        .get(1)
        .or_else(|| get_kwarg(kwargs, "url"))
        .and_then(Value::as_str)
        .ok_or_else(|| type_error("urljoin() missing required argument: 'url'"))?;

    if let Ok(base_url) = Url::parse(base)
        && let Ok(joined) = base_url.join(url)
    {
        return Ok(MontyObject::string(joined.to_string()));
    }

    // Fallback simple join
    let joined = if url.starts_with('/') {
        url.to_owned()
    } else if base.ends_with('/') {
        format!("{base}{url}")
    } else {
        match base.rfind('/') {
            Some(idx) => format!("{}{url}", &base[..=idx]),
            None => url.to_owned(),
        }
    };
    Ok(MontyObject::string(joined))
}

fn quote_fn(
    args: &[Value],
    kwargs: &[(String, Value)],
    plus: bool,
) -> Result<MontyObject, MontyException> {
    let string = args
        .first()
        .or_else(|| get_kwarg(kwargs, "string"))
        .and_then(Value::as_str)
        .ok_or_else(|| type_error("quote() missing 1 required positional argument: 'string'"))?;

    let default_safe = if plus { "" } else { "/" };
    let safe = args
        .get(1)
        .or_else(|| get_kwarg(kwargs, "safe"))
        .and_then(Value::as_str)
        .unwrap_or(default_safe);

    let mut out = String::with_capacity(string.len() * 3);
    for b in string.as_bytes() {
        let ch = *b as char;
        if is_unreserved(*b) || safe.contains(ch) {
            out.push(ch);
        } else if plus && ch == ' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    Ok(MontyObject::string(out))
}

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-' || b == b'~'
}

fn unquote_fn(args: &[Value], plus: bool) -> Result<MontyObject, MontyException> {
    let string = args
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| type_error("unquote() missing 1 required positional argument: 'string'"))?;

    let replaced = if plus {
        string.replace('+', " ")
    } else {
        string.to_owned()
    };

    let bytes = percent_decode_bytes(replaced.as_bytes());
    let s = String::from_utf8_lossy(&bytes);
    Ok(MontyObject::string(s.into_owned()))
}

fn percent_decode_bytes(mut input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    while !input.is_empty() {
        if input[0] == b'%'
            && input.len() >= 3
            && let (Some(h1), Some(h2)) = (from_hex(input[1]), from_hex(input[2]))
        {
            out.push((h1 << 4) | h2);
            input = &input[3..];
            continue;
        }
        out.push(input[0]);
        input = &input[1..];
    }
    out
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_qs_fn(args: &[Value], kwargs: &[(String, Value)]) -> Result<MontyObject, MontyException> {
    let pairs = parse_qsl_internal(args, kwargs)?;
    let mut map: BTreeMap<String, Vec<MontyObject>> = BTreeMap::new();
    for (k, v) in pairs {
        map.entry(k).or_default().push(MontyObject::string(v));
    }
    let dict_items = map
        .into_iter()
        .map(|(k, v)| (MontyObject::string(k), MontyObject::list(v)));
    Ok(MontyObject::dict(dict_items))
}

fn parse_qsl_fn(args: &[Value], kwargs: &[(String, Value)]) -> Result<MontyObject, MontyException> {
    let pairs = parse_qsl_internal(args, kwargs)?;
    let list_items = pairs
        .into_iter()
        .map(|(k, v)| MontyObject::tuple([MontyObject::string(k), MontyObject::string(v)]));
    Ok(MontyObject::list(list_items))
}

fn parse_qsl_internal(
    args: &[Value],
    kwargs: &[(String, Value)],
) -> Result<Vec<(String, String)>, MontyException> {
    let qs = args
        .first()
        .or_else(|| get_kwarg(kwargs, "qs"))
        .and_then(Value::as_str)
        .ok_or_else(|| type_error("parse_qs() missing 1 required positional argument: 'qs'"))?;

    let keep_blank = args
        .get(1)
        .or_else(|| get_kwarg(kwargs, "keep_blank_values"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut out = Vec::new();
    for item in qs.split(['&', ';']) {
        if item.is_empty() {
            continue;
        }
        let (raw_k, raw_v) = match item.split_once('=') {
            Some((k, v)) => (k, v),
            None => (item, ""),
        };
        if raw_v.is_empty() && !keep_blank {
            continue;
        }
        let k = unquote_str(raw_k, true);
        let v = unquote_str(raw_v, true);
        out.push((k, v));
    }
    Ok(out)
}

fn unquote_str(s: &str, plus: bool) -> String {
    let replaced = if plus {
        s.replace('+', " ")
    } else {
        s.to_owned()
    };
    let bytes = percent_decode_bytes(replaced.as_bytes());
    String::from_utf8_lossy(&bytes).into_owned()
}

fn urlencode_fn(args: &[Value], kwargs: &[(String, Value)]) -> Result<MontyObject, MontyException> {
    let query = args
        .first()
        .or_else(|| get_kwarg(kwargs, "query"))
        .ok_or_else(|| type_error("urlencode() missing 1 required positional argument: 'query'"))?;

    let doseq = args
        .get(1)
        .or_else(|| get_kwarg(kwargs, "doseq"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut pairs = Vec::new();

    match query {
        Value::Object(map) => {
            for (k, v) in map {
                if doseq && v.is_array() {
                    for item in v.as_array().unwrap() {
                        pairs.push((k.clone(), val_to_str(item)));
                    }
                } else {
                    pairs.push((k.clone(), val_to_str(v)));
                }
            }
        }
        Value::Array(arr) => {
            for item in arr {
                if let Some(sub_arr) = item.as_array()
                    && sub_arr.len() == 2
                {
                    let k = val_to_str(&sub_arr[0]);
                    let v = &sub_arr[1];
                    if doseq && v.is_array() {
                        for sub_v in v.as_array().unwrap() {
                            pairs.push((k.clone(), val_to_str(sub_v)));
                        }
                    } else {
                        pairs.push((k, val_to_str(v)));
                    }
                }
            }
        }
        _ => {
            return Err(type_error(
                "urlencode() requires a dict or sequence of two-element tuples",
            ));
        }
    }

    let encoded: Vec<String> = pairs
        .into_iter()
        .map(|(k, v)| {
            format!("{}=\n{}", quote_str(&k, "", true), quote_str(&v, "", true)).replace("=\n", "=")
        })
        .collect();

    Ok(MontyObject::string(encoded.join("&")))
}

fn quote_str(s: &str, safe: &str, plus: bool) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        let ch = *b as char;
        if is_unreserved(*b) || safe.contains(ch) {
            out.push(ch);
        } else if plus && ch == ' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn val_to_str(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "None".to_owned(),
        other => other.to_string(),
    }
}

fn glob_escape_fn(args: &[Value]) -> Result<MontyObject, MontyException> {
    let s = args
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| type_error("escape() missing 1 required positional argument: 'pathname'"))?;

    let mut out = String::with_capacity(s.len() * 2);
    for ch in s.chars() {
        match ch {
            '*' => out.push_str("[*]"),
            '?' => out.push_str("[?]"),
            '[' => out.push_str("[[]"),
            c => out.push(c),
        }
    }
    Ok(MontyObject::string(out))
}

fn glob_fn(
    args: &[Value],
    kwargs: &[(String, Value)],
    host: &mut OsHost<'_>,
) -> Result<MontyObject, MontyException> {
    let pattern = args
        .first()
        .or_else(|| get_kwarg(kwargs, "pathname"))
        .and_then(Value::as_str)
        .ok_or_else(|| type_error("glob() missing 1 required positional argument: 'pathname'"))?;

    let matches = scan_glob(pattern, host)?;
    Ok(MontyObject::list(
        matches.into_iter().map(MontyObject::string),
    ))
}

fn scan_glob(pattern: &str, host: &mut OsHost<'_>) -> Result<Vec<String>, MontyException> {
    let (prefix, sub_pat) = split_literal_prefix(pattern);
    let is_absolute = pattern.starts_with('/');

    let base_dir = if is_absolute {
        if prefix.is_empty() {
            PathBuf::from("/")
        } else {
            PathBuf::from(prefix)
        }
    } else if prefix.is_empty() {
        PathBuf::from(&host.cwd)
    } else {
        Path::new(&host.cwd).join(prefix)
    };

    let search_dir = base_dir.to_string_lossy().to_string();
    host.query(&search_dir, |cleared| {
        let disk_dir = cleared.on_disk();
        if sub_pat.is_empty() {
            return Ok(if disk_dir.exists() {
                vec![pattern.to_owned()]
            } else {
                Vec::new()
            });
        }

        let mut results = Vec::new();
        walk_glob_dir(disk_dir, sub_pat, disk_dir, &mut results)?;
        results.sort_unstable();

        let out = results
            .into_iter()
            .map(|rel| {
                if is_absolute {
                    if prefix == "/" {
                        format!("/{rel}")
                    } else {
                        format!("{prefix}/{rel}")
                    }
                } else if prefix.is_empty() {
                    rel
                } else {
                    format!("{prefix}/{rel}")
                }
            })
            .collect();
        Ok(out)
    })
    .map_err(|fault| match fault {
        crate::runner::Fault::Raise(exc) => exc,
        crate::runner::Fault::Abort(err) => {
            MontyException::new(ExcType::RuntimeError, Some(err.to_string()))
        }
    })
}

fn split_literal_prefix(pattern: &str) -> (&str, &str) {
    let Some(wildcard_idx) = pattern.find(['*', '?', '[']) else {
        return (pattern, "");
    };
    match pattern[..wildcard_idx].rfind('/') {
        Some(slash_idx) => {
            let base = if slash_idx == 0 {
                "/"
            } else {
                &pattern[..slash_idx]
            };
            (base, &pattern[slash_idx + 1..])
        }
        None => ("", pattern),
    }
}

fn walk_glob_dir(
    dir: &Path,
    pattern: &str,
    base: &Path,
    results: &mut Vec<String>,
) -> Result<(), MontyException> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };

    let is_recursive = pattern.contains("**");

    for entry in entries.flatten() {
        let path = entry.path();
        let rel = path.strip_prefix(base).unwrap_or(&path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        if glob_match(pattern, &rel_str) {
            results.push(rel_str.clone());
        }

        // CPython's recursive glob does not descend into symlinked
        // directories, and following them here could cycle forever.
        let is_real_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        if is_real_dir && (is_recursive || pattern.contains('/')) {
            let _ = walk_glob_dir(&path, pattern, base, results);
        }
    }
    Ok(())
}

fn glob_match(pattern: &str, candidate: &str) -> bool {
    let pat_parts: Vec<&str> = pattern.split('/').collect();
    let cand_parts: Vec<&str> = candidate.split('/').collect();
    match_parts(&pat_parts, &cand_parts)
}

fn match_parts(pattern: &[&str], candidate: &[&str]) -> bool {
    if pattern.is_empty() {
        return candidate.is_empty();
    }
    if pattern[0] == "**" {
        if pattern.len() == 1 {
            return !candidate.iter().any(|c| is_hidden(c));
        }
        for i in 0..=candidate.len() {
            if candidate[..i].iter().any(|c| is_hidden(c)) {
                break;
            }
            if match_parts(&pattern[1..], &candidate[i..]) {
                return true;
            }
        }
        return false;
    }
    if candidate.is_empty() {
        return false;
    }
    if simple_component_match(pattern[0], candidate[0]) {
        match_parts(&pattern[1..], &candidate[1..])
    } else {
        false
    }
}

/// CPython's glob never matches a dotfile with a component that does not
/// start with a dot itself.
fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

fn simple_component_match(pat: &str, cand: &str) -> bool {
    if is_hidden(cand) && !is_hidden(pat) {
        return false;
    }
    if pat == "*" {
        return true;
    }
    let p_chars: Vec<char> = pat.chars().collect();
    let c_chars: Vec<char> = cand.chars().collect();
    match_chars(&p_chars, &c_chars)
}

fn match_chars(pat: &[char], cand: &[char]) -> bool {
    match (pat.first(), cand.first()) {
        (None, None) => true,
        (Some('*'), _) => {
            match_chars(&pat[1..], cand) || (!cand.is_empty() && match_chars(pat, &cand[1..]))
        }
        (Some('?'), Some(_)) => match_chars(&pat[1..], &cand[1..]),
        (Some(p), Some(c)) if p == c => match_chars(&pat[1..], &cand[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_urlparse_basic() {
        let parsed = parse_url_parts("https://example.com:8080/path?q=1#frag", "", true);
        assert_eq!(parsed.scheme, "https");
        assert_eq!(parsed.netloc, "example.com:8080");
        assert_eq!(parsed.path, "/path");
        assert_eq!(parsed.query, "q=1");
        assert_eq!(parsed.fragment, "frag");
    }

    #[test]
    fn test_urlparse_relative() {
        let parsed = parse_url_parts("/path?q=1", "", true);
        assert_eq!(parsed.scheme, "");
        assert_eq!(parsed.netloc, "");
        assert_eq!(parsed.path, "/path");
        assert_eq!(parsed.query, "q=1");
    }

    #[test]
    fn test_quote_unquote() {
        let q = quote_str("hello world/foo", "/", false);
        assert_eq!(q, "hello%20world/foo");
        let u = unquote_str(&q, false);
        assert_eq!(u, "hello world/foo");
    }

    #[test]
    fn test_quote_plus_unquote_plus() {
        let q = quote_str("hello world", "", true);
        assert_eq!(q, "hello+world");
        let u = unquote_str(&q, true);
        assert_eq!(u, "hello world");
    }

    #[test]
    fn test_parse_qs() {
        let pairs = parse_qsl_internal(&[Value::String("a=1&b=2&a=3".into())], &[]).unwrap();
        assert_eq!(
            pairs,
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "2".into()),
                ("a".into(), "3".into())
            ]
        );
    }

    #[test]
    fn test_parse_qsl_blank_and_bare() {
        let pairs = parse_qsl_internal(&[Value::String("a&b=1".into())], &[]).unwrap();
        assert_eq!(pairs, vec![("b".into(), "1".into())]);
        let pairs = parse_qsl_internal(
            &[Value::String("a&b=1".into())],
            &[("keep_blank_values".to_owned(), Value::Bool(true))],
        )
        .unwrap();
        assert_eq!(
            pairs,
            vec![("a".into(), "".into()), ("b".into(), "1".into())]
        );
    }

    #[test]
    fn test_glob_escape() {
        let res = glob_escape_fn(&[Value::String("test*file?.txt".into())]).unwrap();
        assert_eq!(res.as_ref().as_str(), Some("test[*]file[?].txt"));
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("src/**/*.rs", "src/modules/url.rs"));
        assert!(!glob_match("*.rs", "main.py"));
    }

    #[test]
    fn test_glob_dotfiles() {
        assert!(!glob_match("*", ".env"));
        assert!(!glob_match("*.md", ".hidden.md"));
        assert!(glob_match(".env", ".env"));
        assert!(glob_match(".*", ".env"));
        assert!(!glob_match("**", "a/.git/config"));
        assert!(glob_match(".git/**", ".git/config"));
        assert!(!glob_match("src/**/*.rs", "src/.private/mod.rs"));
        assert!(glob_match("*", "visible.txt"));
    }
}
