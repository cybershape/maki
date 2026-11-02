use std::fmt;
use std::io::{Write, stdout};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};

const MAX_SEQUENCE_LEN: usize = 4096;
const MAX_KEY_LEN: usize = 16;
const MAX_MSG_ENCODED_LEN: usize = 2732;
const MAX_MSG_DECODED_LEN: usize = 2048;
const MAX_TITLE_ENCODED_LEN: usize = 256;
const MAX_TITLE_DECODED_LEN: usize = 192;
const MAX_APP_LEN: usize = 32;
const MAX_ID_LEN: usize = 128;
const MAX_SEGMENT_LEN: usize = 32;
const MAX_ID_DEPTH: usize = 8;
const MAX_RECORDS_PER_TERMINAL: usize = 256;

const OSC_PREFIX: &str = "\u{1b}]7501;";
const ST_ESCAPE: &str = "\u{1b}\\";
const ST_BEL: &str = "\u{7}";
const DEFAULT_APP: &str = "maki";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    Clear,
}

impl State {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
            Self::Clear => "clear",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "idle" => Some(Self::Idle),
            "working" => Some(Self::Working),
            "done" => Some(Self::Done),
            "blocked" => Some(Self::Blocked),
            "error" => Some(Self::Error),
            "clear" => Some(Self::Clear),
            _ => None,
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Permission,
    Question,
    Auth,
}

impl Kind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "permission" => Some(Self::Permission),
            "question" => Some(Self::Question),
            "auth" => Some(Self::Auth),
            _ => None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatusReport {
    pub state: State,
    pub id: Option<String>,
    pub kind: Option<Kind>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
}

impl ProgramStatusReport {
    pub fn new(state: State) -> Self {
        Self {
            state,
            id: None,
            kind: None,
            progress: None,
            app: Some(DEFAULT_APP.to_string()),
            title: None,
            msg: None,
        }
    }

    pub fn idle() -> Self {
        Self::new(State::Idle)
    }

    pub fn working(msg: Option<String>) -> Self {
        let mut report = Self::new(State::Working);
        report.msg = msg;
        report
    }

    pub fn blocked(kind: Kind, msg: Option<String>) -> Self {
        let mut report = Self::new(State::Blocked);
        report.kind = Some(kind);
        report.msg = msg;
        report
    }

    pub fn done(msg: Option<String>) -> Self {
        let mut report = Self::new(State::Done);
        report.msg = msg;
        report
    }

    pub fn error(msg: Option<String>) -> Self {
        let mut report = Self::new(State::Error);
        report.msg = msg;
        report
    }

    pub fn clear(id: Option<String>) -> Self {
        Self {
            state: State::Clear,
            id,
            kind: None,
            progress: None,
            app: None,
            title: None,
            msg: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramStatusMessage {
    Report(ProgramStatusReport),
    Query,
    Reply {
        states: Vec<State>,
        kinds: Vec<Kind>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramStatusError {
    SequenceTooLong(usize),
    InvalidId(String),
    InvalidApp(String),
    InvalidControlChar(&'static str),
    TextTooLong { field: &'static str, length: usize },
    Base64EncodeFailed,
}

impl fmt::Display for ProgramStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SequenceTooLong(len) => {
                write!(f, "sequence exceeds {MAX_SEQUENCE_LEN} bytes: {len}")
            }
            Self::InvalidId(id) => write!(f, "invalid id: {id:?}"),
            Self::InvalidApp(app) => write!(f, "invalid app: {app:?}"),
            Self::InvalidControlChar(field) => {
                write!(f, "{field} contains forbidden control character")
            }
            Self::TextTooLong { field, length } => {
                write!(f, "{field} exceeds length limit: {length}")
            }
            Self::Base64EncodeFailed => write!(f, "base64 encoding failed"),
        }
    }
}

impl std::error::Error for ProgramStatusError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    TooLong(usize),
    MissingPrefix,
    MissingTerminator,
    InvalidId,
    InvalidApp,
    MissingState,
    ControlCharInText(&'static str),
    Base64DecodeFailed(&'static str),
    TextExceedsLimit(&'static str),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong(len) => {
                write!(f, "sequence length {len} exceeds cap of {MAX_SEQUENCE_LEN}")
            }
            Self::MissingPrefix => write!(f, "missing OSC 7501 prefix"),
            Self::MissingTerminator => write!(f, "missing ST terminator"),
            Self::InvalidId => write!(f, "invalid id path"),
            Self::InvalidApp => write!(f, "invalid app identifier"),
            Self::MissingState => write!(f, "missing or invalid state"),
            Self::ControlCharInText(field) => write!(f, "control character found in {field}"),
            Self::Base64DecodeFailed(field) => write!(f, "failed to decode base64 for {field}"),
            Self::TextExceedsLimit(field) => write!(f, "decoded {field} exceeds length limit"),
        }
    }
}

impl std::error::Error for ParseError {}

fn is_valid_char_for_id_or_app(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-')
}

fn contains_control_char(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '\u{0000}'..='\u{001F}' | '\u{007F}' | '\u{0080}'..='\u{009F}'))
}

pub fn validate_id(id: &str) -> Result<(), ProgramStatusError> {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return Err(ProgramStatusError::InvalidId(id.to_string()));
    }
    let segments: Vec<&str> = id.split('/').collect();
    if segments.is_empty() || segments.len() > MAX_ID_DEPTH {
        return Err(ProgramStatusError::InvalidId(id.to_string()));
    }
    for seg in segments {
        if seg.is_empty()
            || seg.len() > MAX_SEGMENT_LEN
            || !seg.bytes().all(is_valid_char_for_id_or_app)
        {
            return Err(ProgramStatusError::InvalidId(id.to_string()));
        }
    }
    Ok(())
}

pub fn validate_app(app: &str) -> Result<(), ProgramStatusError> {
    if app.is_empty() || app.len() > MAX_APP_LEN || !app.bytes().all(is_valid_char_for_id_or_app) {
        return Err(ProgramStatusError::InvalidApp(app.to_string()));
    }
    Ok(())
}

fn is_legal_value_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b',' | b'+' | b'/' | b'=' | b'-')
}

pub fn format_report(report: &ProgramStatusReport) -> Result<String, ProgramStatusError> {
    if let Some(id) = &report.id {
        validate_id(id)?;
    }
    if let Some(app) = &report.app {
        validate_app(app)?;
    }

    let encoded_title = if let Some(title) = &report.title {
        if contains_control_char(title) {
            return Err(ProgramStatusError::InvalidControlChar("title"));
        }
        if title.len() > MAX_TITLE_DECODED_LEN {
            return Err(ProgramStatusError::TextTooLong {
                field: "title",
                length: title.len(),
            });
        }
        let encoded = STANDARD.encode(title.as_bytes());
        if encoded.len() > MAX_TITLE_ENCODED_LEN {
            return Err(ProgramStatusError::TextTooLong {
                field: "title (encoded)",
                length: encoded.len(),
            });
        }
        Some(encoded)
    } else {
        None
    };

    let encoded_msg = if let Some(msg) = &report.msg {
        if contains_control_char(msg) {
            return Err(ProgramStatusError::InvalidControlChar("msg"));
        }
        if msg.len() > MAX_MSG_DECODED_LEN {
            return Err(ProgramStatusError::TextTooLong {
                field: "msg",
                length: msg.len(),
            });
        }
        let encoded = STANDARD.encode(msg.as_bytes());
        if encoded.len() > MAX_MSG_ENCODED_LEN {
            return Err(ProgramStatusError::TextTooLong {
                field: "msg (encoded)",
                length: encoded.len(),
            });
        }
        Some(encoded)
    } else {
        None
    };

    let mut body = format!("state={}", report.state.as_str());

    if let Some(id) = &report.id {
        body.push_str(":id=");
        body.push_str(id);
    }
    if let Some(app) = &report.app {
        body.push_str(":app=");
        body.push_str(app);
    }
    if report.state == State::Blocked
        && let Some(kind) = report.kind
    {
        body.push_str(":kind=");
        body.push_str(kind.as_str());
    }
    if matches!(report.state, State::Working | State::Blocked)
        && let Some(progress) = report.progress
    {
        body.push_str(":progress=");
        body.push_str(&progress.min(100).to_string());
    }
    if let Some(title) = encoded_title {
        body.push_str(":title=");
        body.push_str(&title);
    }
    if let Some(msg) = encoded_msg {
        body.push_str(":msg=");
        body.push_str(&msg);
    }

    let sequence = format!("{OSC_PREFIX}{body}{ST_ESCAPE}");
    if sequence.len() > MAX_SEQUENCE_LEN {
        return Err(ProgramStatusError::SequenceTooLong(sequence.len()));
    }
    Ok(sequence)
}

pub fn format_query() -> String {
    format!("{OSC_PREFIX}?{ST_ESCAPE}")
}

pub fn format_reply(states: &[State], kinds: &[Kind]) -> String {
    let mut reply = format!("{OSC_PREFIX}?");
    if !states.is_empty() {
        reply.push_str(":states=");
        let s_list = states
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(",");
        reply.push_str(&s_list);
    }
    if !kinds.is_empty() {
        reply.push_str(":kinds=");
        let k_list = kinds
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>()
            .join(",");
        reply.push_str(&k_list);
    }
    reply.push_str(ST_ESCAPE);
    reply
}

fn decode_base64_flexible(input: &str) -> Option<Vec<u8>> {
    STANDARD
        .decode(input.as_bytes())
        .ok()
        .or_else(|| STANDARD_NO_PAD.decode(input.as_bytes()).ok())
}

pub fn parse_sequence(raw: &str) -> Result<ProgramStatusMessage, ParseError> {
    if raw.len() > MAX_SEQUENCE_LEN {
        return Err(ParseError::TooLong(raw.len()));
    }
    let body = raw
        .strip_prefix(OSC_PREFIX)
        .ok_or(ParseError::MissingPrefix)?;
    let body = if let Some(stripped) = body.strip_suffix(ST_ESCAPE) {
        stripped
    } else if let Some(stripped) = body.strip_suffix(ST_BEL) {
        stripped
    } else {
        return Err(ParseError::MissingTerminator);
    };

    let trimmed = body.trim();
    if trimmed == "?" {
        return Ok(ProgramStatusMessage::Query);
    }
    if let Some(stripped) = trimmed.strip_prefix('?') {
        let remainder = stripped.trim_start();
        if let Some(remainder) = remainder.strip_prefix(':') {
            let mut states = Vec::new();
            let mut kinds = Vec::new();
            for part in remainder.split(':') {
                let part = part.trim();
                if let Some((k, v)) = part.split_once('=') {
                    let k = k.trim();
                    let v = v.trim();
                    if k == "states" {
                        for s in v.split(',') {
                            if let Some(state) = State::from_str_opt(s.trim())
                                && state != State::Clear
                            {
                                states.push(state);
                            }
                        }
                    } else if k == "kinds" {
                        for k_str in v.split(',') {
                            if let Some(kind) = Kind::from_str_opt(k_str.trim()) {
                                kinds.push(kind);
                            }
                        }
                    }
                }
            }
            return Ok(ProgramStatusMessage::Reply { states, kinds });
        }
    }

    let mut state = None;
    let mut id = None;
    let mut kind = None;
    let mut progress = None;
    let mut app = None;
    let mut title = None;
    let mut msg = None;

    for pair in body.split(':') {
        let pair = pair.trim();
        let Some((raw_key, raw_val)) = pair.split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        let val = raw_val.trim();

        if key.is_empty() || key.len() > MAX_KEY_LEN || !key.bytes().all(|b| b.is_ascii_lowercase())
        {
            continue;
        }
        if !val.bytes().all(is_legal_value_byte) {
            continue;
        }

        match key {
            "state" => {
                state = State::from_str_opt(val);
            }
            "id" => {
                id = Some(val.to_string());
            }
            "kind" => {
                kind = Kind::from_str_opt(val);
            }
            "progress" => {
                progress = val.parse::<u8>().ok().filter(|&p| p <= 100);
            }
            "app" => {
                app = Some(val.to_string());
            }
            "title" => {
                title = Some(val.to_string());
            }
            "msg" => {
                msg = Some(val.to_string());
            }
            _ => {}
        }
    }

    let Some(state) = state else {
        return Err(ParseError::MissingState);
    };

    if let Some(ref id_val) = id
        && validate_id(id_val).is_err()
    {
        return Err(ParseError::InvalidId);
    }

    let app = app.filter(|a| validate_app(a).is_ok());

    let kind = if state == State::Blocked { kind } else { None };
    let progress = if matches!(state, State::Working | State::Blocked) {
        progress
    } else {
        None
    };

    let title = if let Some(t_enc) = title {
        if t_enc.len() > MAX_TITLE_ENCODED_LEN {
            return Err(ParseError::TextExceedsLimit("title"));
        }
        let decoded_bytes =
            decode_base64_flexible(&t_enc).ok_or(ParseError::Base64DecodeFailed("title"))?;
        let decoded_text = String::from_utf8(decoded_bytes)
            .map_err(|_| ParseError::Base64DecodeFailed("title"))?;
        if decoded_text.len() > MAX_TITLE_DECODED_LEN {
            return Err(ParseError::TextExceedsLimit("title"));
        }
        if contains_control_char(&decoded_text) {
            return Err(ParseError::ControlCharInText("title"));
        }
        Some(decoded_text)
    } else {
        None
    };

    let msg = if let Some(m_enc) = msg {
        if m_enc.len() > MAX_MSG_ENCODED_LEN {
            return Err(ParseError::TextExceedsLimit("msg"));
        }
        let decoded_bytes =
            decode_base64_flexible(&m_enc).ok_or(ParseError::Base64DecodeFailed("msg"))?;
        let decoded_text =
            String::from_utf8(decoded_bytes).map_err(|_| ParseError::Base64DecodeFailed("msg"))?;
        if decoded_text.len() > MAX_MSG_DECODED_LEN {
            return Err(ParseError::TextExceedsLimit("msg"));
        }
        if contains_control_char(&decoded_text) {
            return Err(ParseError::ControlCharInText("msg"));
        }
        Some(decoded_text)
    } else {
        None
    };

    Ok(ProgramStatusMessage::Report(ProgramStatusReport {
        state,
        id,
        kind,
        progress,
        app,
        title,
        msg,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub id: Option<String>,
    pub state: State,
    pub kind: Option<Kind>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct RecordStore {
    records: Vec<(Option<String>, Record)>,
}

impl RecordStore {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
        }
    }

    pub fn get(&self, id: Option<&str>) -> Option<&Record> {
        let key = id.map(str::to_string);
        self.records.iter().find(|(k, _)| *k == key).map(|(_, r)| r)
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn apply(&mut self, report: ProgramStatusReport) {
        if report.state == State::Clear {
            match report.id {
                None => {
                    self.records.clear();
                }
                Some(prefix) => {
                    let child_prefix = format!("{prefix}/");
                    self.records.retain(|(k, _)| {
                        if let Some(id) = k {
                            id != &prefix && !id.starts_with(&child_prefix)
                        } else {
                            true
                        }
                    });
                }
            }
            return;
        }

        let key = report.id.clone();
        let record = Record {
            id: report.id,
            state: report.state,
            kind: report.kind,
            progress: report.progress,
            app: report.app,
            title: report.title,
            msg: report.msg,
        };

        if let Some(pos) = self.records.iter().position(|(k, _)| *k == key) {
            self.records.remove(pos);
        } else if self.records.len() >= MAX_RECORDS_PER_TERMINAL {
            self.records.remove(0);
        }

        self.records.push((key, record));
    }

    pub fn resolve_app(&self, id: Option<&str>) -> Option<&str> {
        let Some(target) = id else {
            return self.get(None).and_then(|r| r.app.as_deref());
        };

        if let Some(record) = self.get(Some(target))
            && let Some(ref app) = record.app
        {
            return Some(app.as_str());
        }

        let mut current = target;
        while let Some((parent, _)) = current.rsplit_once('/') {
            if let Some(record) = self.get(Some(parent))
                && let Some(ref app) = record.app
            {
                return Some(app.as_str());
            }
            current = parent;
        }

        self.get(None).and_then(|r| r.app.as_deref())
    }

    pub fn on_process_exit_or_prompt(&mut self) {
        self.records
            .retain(|(_, r)| !matches!(r.state, State::Working | State::Blocked));
    }

    pub fn hard_reset(&mut self) {
        self.records.clear();
    }

    pub fn soft_reset(&mut self) {}
}

pub fn emit_program_status(report: &ProgramStatusReport) -> std::io::Result<()> {
    let sequence = match format_report(report) {
        Ok(seq) => seq,
        Err(err) => {
            tracing::warn!(%err, "failed to format program status report");
            return Ok(());
        }
    };
    let wrapped = crate::terminal::wrap_for_mux(sequence);
    let mut out = stdout().lock();
    out.write_all(wrapped.as_bytes())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_parse_root_working() {
        let report = ProgramStatusReport::working(Some("Indexing files".into()));
        let formatted = format_report(&report).unwrap();
        assert!(formatted.starts_with(OSC_PREFIX));
        assert!(formatted.ends_with(ST_ESCAPE));

        let parsed = parse_sequence(&formatted).unwrap();
        match parsed {
            ProgramStatusMessage::Report(r) => {
                assert_eq!(r.state, State::Working);
                assert_eq!(r.app.as_deref(), Some("maki"));
                assert_eq!(r.msg.as_deref(), Some("Indexing files"));
                assert!(r.id.is_none());
            }
            _ => panic!("expected report"),
        }
    }

    #[test]
    fn format_and_parse_blocked_with_kind() {
        let mut report =
            ProgramStatusReport::blocked(Kind::Permission, Some("Approve bash tool".into()));
        report.progress = Some(50);
        report.title = Some("Confirmation".into());
        let formatted = format_report(&report).unwrap();

        let parsed = parse_sequence(&formatted).unwrap();
        match parsed {
            ProgramStatusMessage::Report(r) => {
                assert_eq!(r.state, State::Blocked);
                assert_eq!(r.kind, Some(Kind::Permission));
                assert_eq!(r.progress, Some(50));
                assert_eq!(r.title.as_deref(), Some("Confirmation"));
                assert_eq!(r.msg.as_deref(), Some("Approve bash tool"));
            }
            _ => panic!("expected report"),
        }
    }

    #[test]
    fn parse_skips_malformed_pairs_and_whitespace() {
        let raw = "\u{1b}]7501; state = working : malformed_no_equal : unknown_key=value : app = cargo \u{1b}\\";
        let parsed = parse_sequence(raw).unwrap();
        match parsed {
            ProgramStatusMessage::Report(r) => {
                assert_eq!(r.state, State::Working);
                assert_eq!(r.app.as_deref(), Some("cargo"));
            }
            _ => panic!("expected report"),
        }
    }

    #[test]
    fn parse_bel_terminator() {
        let raw = "\u{1b}]7501;state=idle\u{7}";
        let parsed = parse_sequence(raw).unwrap();
        match parsed {
            ProgramStatusMessage::Report(r) => {
                assert_eq!(r.state, State::Idle);
            }
            _ => panic!("expected report"),
        }
    }

    #[test]
    fn parse_repeated_keys_last_wins() {
        let raw = "\u{1b}]7501;state=working:app=old:app=new\u{1b}\\";
        let parsed = parse_sequence(raw).unwrap();
        match parsed {
            ProgramStatusMessage::Report(r) => {
                assert_eq!(r.state, State::Working);
                assert_eq!(r.app.as_deref(), Some("new"));
            }
            _ => panic!("expected report"),
        }
    }

    #[test]
    fn reject_control_chars_in_msg_or_title() {
        let report = ProgramStatusReport {
            state: State::Working,
            id: None,
            kind: None,
            progress: None,
            app: Some("maki".into()),
            title: None,
            msg: Some("line1\nline2".into()),
        };
        assert!(format_report(&report).is_err());

        let tab_report = ProgramStatusReport {
            state: State::Working,
            id: None,
            kind: None,
            progress: None,
            app: Some("maki".into()),
            title: Some("tab\there".into()),
            msg: None,
        };
        assert!(format_report(&tab_report).is_err());
    }

    #[test]
    fn parse_rejects_decoded_control_chars() {
        let encoded_with_newline = STANDARD.encode("hello\nworld".as_bytes());
        let raw = format!("\u{1b}]7501;state=working:msg={encoded_with_newline}\u{1b}\\");
        assert!(matches!(
            parse_sequence(&raw),
            Err(ParseError::ControlCharInText("msg"))
        ));
    }

    #[test]
    fn id_depth_and_segment_validation() {
        assert!(validate_id("build").is_ok());
        assert!(validate_id("build/test").is_ok());
        assert!(validate_id("a/b/c/d/e/f/g/h").is_ok());
        assert!(validate_id("a/b/c/d/e/f/g/h/extra").is_err());
        assert!(validate_id("has space").is_err());
        assert!(validate_id("/leading_slash").is_err());
        assert!(validate_id("trailing/").is_err());
        assert!(validate_id("double//slash").is_err());
    }

    #[test]
    fn format_and_parse_query_and_reply() {
        let query = format_query();
        assert_eq!(parse_sequence(&query).unwrap(), ProgramStatusMessage::Query);

        let states = vec![
            State::Idle,
            State::Working,
            State::Done,
            State::Blocked,
            State::Error,
        ];
        let kinds = vec![Kind::Permission, Kind::Question, Kind::Auth];
        let reply = format_reply(&states, &kinds);
        let parsed = parse_sequence(&reply).unwrap();
        match parsed {
            ProgramStatusMessage::Reply {
                states: s,
                kinds: k,
            } => {
                assert_eq!(s, states);
                assert_eq!(k, kinds);
            }
            _ => panic!("expected reply"),
        }
    }

    #[test]
    fn record_store_app_inheritance() {
        let mut store = RecordStore::new();
        store.apply(ProgramStatusReport {
            state: State::Working,
            id: None,
            kind: None,
            progress: None,
            app: Some("deploy".into()),
            title: None,
            msg: None,
        });

        store.apply(ProgramStatusReport {
            state: State::Working,
            id: Some("us-east/sub".into()),
            kind: None,
            progress: None,
            app: None,
            title: None,
            msg: None,
        });

        assert_eq!(store.resolve_app(Some("us-east/sub")), Some("deploy"));
    }

    #[test]
    fn record_store_clear_cascades() {
        let mut store = RecordStore::new();
        store.apply(ProgramStatusReport::working(Some("root".into())));
        let mut child1 = ProgramStatusReport::working(Some("c1".into()));
        child1.id = Some("build".into());
        store.apply(child1);

        let mut child2 = ProgramStatusReport::working(Some("c2".into()));
        child2.id = Some("build/test".into());
        store.apply(child2);

        let mut other = ProgramStatusReport::working(Some("other".into()));
        other.id = Some("other".into());
        store.apply(other);

        assert_eq!(store.len(), 4);
        store.apply(ProgramStatusReport::clear(Some("build".into())));
        assert_eq!(store.len(), 2);
        assert!(store.get(Some("build")).is_none());
        assert!(store.get(Some("build/test")).is_none());
        assert!(store.get(None).is_some());
        assert!(store.get(Some("other")).is_some());

        store.apply(ProgramStatusReport::clear(None));
        assert!(store.is_empty());
    }

    #[test]
    fn record_store_process_exit_drops_working_and_blocked() {
        let mut store = RecordStore::new();
        store.apply(ProgramStatusReport::working(None));

        let mut r_blocked = ProgramStatusReport::blocked(Kind::Permission, None);
        r_blocked.id = Some("b1".into());
        store.apply(r_blocked);

        let mut r_done = ProgramStatusReport::done(None);
        r_done.id = Some("d1".into());
        store.apply(r_done);

        let mut r_err = ProgramStatusReport::error(None);
        r_err.id = Some("e1".into());
        store.apply(r_err);

        assert_eq!(store.len(), 4);
        store.on_process_exit_or_prompt();
        assert_eq!(store.len(), 2);
        assert!(store.get(Some("d1")).is_some());
        assert!(store.get(Some("e1")).is_some());
    }

    #[test]
    fn record_store_lru_eviction() {
        let mut store = RecordStore::new();
        for i in 0..MAX_RECORDS_PER_TERMINAL {
            let mut r = ProgramStatusReport::working(None);
            r.id = Some(format!("task-{i}"));
            store.apply(r);
        }
        assert_eq!(store.len(), MAX_RECORDS_PER_TERMINAL);
        assert!(store.get(Some("task-0")).is_some());

        let mut overflow = ProgramStatusReport::working(None);
        overflow.id = Some("task-overflow".into());
        store.apply(overflow);

        assert_eq!(store.len(), MAX_RECORDS_PER_TERMINAL);
        assert!(store.get(Some("task-0")).is_none());
        assert!(store.get(Some("task-overflow")).is_some());
    }
}
