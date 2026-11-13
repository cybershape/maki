//! MCP client: manages transports and routes tool calls to servers.
//!
//! Tool names are namespaced as `server.tool` so two servers can both expose `search`
//! without colliding. Names are deduped via `Arc<str>` in a small cache.
//!
//! All mutable state lives in the `run` task, which owns `McpManagerInner` exclusively.
//! Commands come in through a channel (one at a time, no interleaving). Reads go through
//! two lock-free `ArcSwap`s: a `ToolIndex` for tool calls and an `McpSnapshot` for the UI.
//! This way a slow tool call never blocks a toggle and vice versa.
//!
//! Servers connect inside `run`, not in `start`, so a slow `initialize` never delays the
//! caller's first frame. Whoever needs the tools waits on `McpHandle::ready`. Connects
//! land alongside commands, so one slow server holds up neither a fast one nor a shutdown.
//!
//! `McpSnapshotReader` is a read-only handle. Outside code physically cannot publish a
//! snapshot, so the "only `run` publishes" invariant is enforced by the type system.

pub mod config;
pub mod error;
pub mod oauth;
pub mod transport;

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::future::pending;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use arc_swap::{ArcSwap, Guard};
use maki_config::ProjectConfig;
use maki_providers::{ContentBlock, DEFER_LOADING_KEY, Message, Model};
use rmcp::model::{Prompt, PromptArgument, Tool};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::{oneshot, watch};
use tokio::task::{JoinSet, spawn_blocking};
use tokio::time::timeout;
use tracing::{info, warn};

use self::config::{
    McpConfig, McpConfigErrors, McpServerInfo, McpServerStatus, RawServerConfig, RawTransport,
    ServerConfig, Transport, load_config, parse_server, transport_kind,
};
use self::error::McpError;
use self::transport::{McpTransport, PromptMessage};
use crate::tools::CallOrigin;
use crate::tools::schema::{inject_toolcall_reason, sanitize_tool_input_schema};
use crate::types::TextOutput;

const SEPARATOR: &str = ".";
const WIRE_SEPARATOR: &str = "__";
pub const UNKNOWN_MCP: &str = "unknown_mcp";
pub const TOOL_SEARCH_TOOL_NAME: &str = "tool_search";
/// Below this many deferrable tools, a search round-trip plus its
/// prompt-cache miss cost more than a handful of upfront definitions.
/// Overridden by `defer_tools` in mcp.toml.
const DEFAULT_DEFER_TOOLS: usize = 10;
#[allow(dead_code)]
const EMPTY_CATALOG: &str = "(none yet)";
/// Loads per search are capped so one broad query can't flood the context.
const MAX_SEARCH_LOADS: usize = 5;
const NAME_HIT_SCORE: usize = 2;
const DESCRIPTION_HIT_SCORE: usize = 1;
/// Overflow names shown to the model so it can re-search by exact name.
const MAX_OVERFLOW_NAMES: usize = 20;
const SEARCH_NO_MATCH: &str = "No deferred MCP tools matched";
const SEARCH_OVERFLOW_PREFIX: &str = "Also matched but not loaded: ";
/// A nested search loads nothing, so it must not promise a next request that
/// carries the matches.
const SEARCH_HEADER_MODEL: (&str, &str) = ("Loaded", ", callable from your next message:");
const SEARCH_HEADER_NESTED: (&str, &str) = ("Matched", ", callable here by name:");
pub(crate) const SEARCH_EMPTY_QUERY: &str = "query must not be empty";

/// Convert internal qualified name (`server.tool`) to wire format (`server__tool`)
/// for LLM provider APIs that reject dots in tool names.
///
/// Lossless: server names can't contain `__` (only alphanumeric + `-`),
/// so the first `__` in the wire name is always the separator boundary.
pub fn wire_tool_name(qualified: &str) -> String {
    qualified.replacen(SEPARATOR, WIRE_SEPARATOR, 1)
}

/// Convert wire format (`server__tool`) back to internal qualified name (`server.tool`).
///
/// Only the first `__` is the separator — tool names may contain underscores.
pub fn internal_tool_name(wire: &str) -> String {
    wire.replacen(WIRE_SEPARATOR, SEPARATOR, 1)
}

/// Shaped like an MCP tool's wire name, whether or not a server publishes it.
pub fn is_wire_name(name: &str) -> bool {
    name.contains(WIRE_SEPARATOR)
}
const MCP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

struct McpToolDef {
    qualified_name: Arc<str>,
    raw_name: String,
    description: String,
    input_schema: Value,
}

struct McpPromptDef {
    qualified_name: String,
    raw_name: String,
    description: String,
    arguments: Vec<PromptArgument>,
}

impl McpPromptDef {
    fn from_info(server_name: &str, info: Prompt) -> Self {
        Self {
            qualified_name: format!("{server_name}{SEPARATOR}{}", info.name),
            raw_name: info.name,
            description: info.description.unwrap_or_default(),
            arguments: info.arguments.unwrap_or_default(),
        }
    }

    fn to_info(&self, server_name: &str) -> McpPromptInfo {
        McpPromptInfo {
            display_name: format!("{server_name}:{}", self.raw_name),
            qualified_name: self.qualified_name.clone(),
            description: self.description.clone(),
            arguments: self
                .arguments
                .iter()
                .map(|a| McpPromptArg {
                    name: a.name.clone(),
                    description: a.description.clone().unwrap_or_default(),
                    required: a.required.unwrap_or_default(),
                })
                .collect(),
        }
    }
}

struct ServerEntry {
    name: String,
    config: Option<ServerConfig>,
    transport_kind: &'static str,
    origin: PathBuf,
    status: McpServerStatus,
    transport: Option<Arc<dyn McpTransport>>,
    tools: Vec<McpToolDef>,
    prompts: Vec<McpPromptDef>,
}

impl ServerEntry {
    async fn clear_connection(&mut self) {
        if let Some(old) = self.transport.take() {
            // A live tool call can still be holding an `Arc` to this transport via the
            // `ToolIndex`, so we cannot rely on `Drop` to reap the child in time.
            kill_process_groups(&old.child_pids());
            old.shutdown().await;
        }
        self.tools.clear();
        self.prompts.clear();
    }

    fn populate(&mut self, result: StartResult) {
        let StartResult {
            transport,
            tool_infos,
            prompt_infos,
        } = result;
        self.tools = tool_infos
            .into_iter()
            .filter(|info| {
                if !config::is_valid_tool_name(&info.name) {
                    warn!(tool = %info.name, server = %self.name, "skipping tool with invalid name");
                    return false;
                }
                // Wire format is server__tool — check total length fits LLM API limits
                let wire_len = self.name.len() + 2 + info.name.len();
                if wire_len > 64 {
                    warn!(
                        tool = %info.name,
                        server = %self.name,
                        wire_len,
                        "skipping tool: wire name exceeds 64 char LLM API limit"
                    );
                    return false;
                }
                true
            })
            .map(|info| McpToolDef {
                qualified_name: intern(format!("{}{SEPARATOR}{}", self.name, info.name)),
                raw_name: info.name.into_owned(),
                description: info.description.unwrap_or_default().into_owned(),
                input_schema: Value::Object(Arc::unwrap_or_clone(info.input_schema)),
            })
            .collect();
        self.prompts = prompt_infos
            .into_iter()
            .map(|info| McpPromptDef::from_info(&self.name, info))
            .collect();
        self.transport = Some(transport);
        self.status = McpServerStatus::Running;
    }
}

struct McpManagerInner {
    entries: Vec<ServerEntry>,
    generation: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct McpFunctionInfo {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct McpModuleInfo {
    pub name: String,
    pub description: String,
    pub functions: Vec<McpFunctionInfo>,
}

#[derive(Default)]
struct ToolIndex {
    tools: HashMap<Arc<str>, ToolRef>,
    prompts: HashMap<String, PromptRef>,
    descriptors: Arc<[ToolDescriptor]>,
    modules: Arc<[McpModuleInfo]>,
}

/// One published MCP tool. Wire name and search text are derived from
/// `definition` on demand: searches are model-paced and rare, so nothing
/// to cache.
struct ToolDescriptor {
    qualified_name: Arc<str>,
    always_load: bool,
    definition: Value,
}

impl ToolDescriptor {
    fn wire_name(&self) -> &str {
        self.definition["name"].as_str().unwrap_or_default()
    }
}

struct ToolRef {
    raw_name: String,
    transport: Arc<dyn McpTransport>,
    /// Whether the published schema carries a `toolcall_reason` we injected.
    /// A field the server declared itself belongs to the tool, not to us.
    reason_injected: bool,
}

struct PromptRef {
    raw_name: String,
    transport: Arc<dyn McpTransport>,
}

#[derive(Clone)]
pub struct McpPromptInfo {
    pub display_name: String,
    pub qualified_name: String,
    pub description: String,
    pub arguments: Vec<McpPromptArg>,
}

#[derive(Clone)]
pub struct McpPromptArg {
    pub name: String,
    pub description: String,
    pub required: bool,
}

#[derive(Clone, Default)]
pub struct McpSnapshot {
    pub infos: Vec<McpServerInfo>,
    pub prompts: Vec<McpPromptInfo>,
    pub pids: Vec<u32>,
    pub generation: u64,
}

/// Read-only view of the latest published `McpSnapshot`. Handing this out instead of the
/// raw `ArcSwap` keeps outside code from publishing snapshots of its own.
#[derive(Clone)]
pub struct McpSnapshotReader(Arc<ArcSwap<McpSnapshot>>);

impl McpSnapshotReader {
    pub fn empty() -> Self {
        Self::from_snapshot(McpSnapshot::default())
    }

    pub fn from_snapshot(snapshot: McpSnapshot) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(snapshot)))
    }

    pub fn load(&self) -> Guard<Arc<McpSnapshot>> {
        self.0.load()
    }

    pub fn load_full(&self) -> Arc<McpSnapshot> {
        self.0.load_full()
    }
}

pub enum McpCommand {
    Toggle {
        server: String,
        enabled: bool,
    },
    Reconnect {
        server: String,
    },
    /// Drain every running transport and stop the loop. The loop sends `()` on `ack` once
    /// every shutdown has finished, so callers can wait with a timeout.
    Shutdown {
        ack: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
pub struct McpHandle {
    cmd_tx: UnboundedSender<McpCommand>,
    index: Arc<ArcSwap<ToolIndex>>,
    snapshot: Arc<ArcSwap<McpSnapshot>>,
    /// Never changes after startup, so it lives here instead of being
    /// copied into every republished `ToolIndex`.
    defer_tools: usize,
    /// Nothing is ever sent on it. `run` drops the sender once the first
    /// connect pass has published, and that disconnect is the signal.
    ready_rx: watch::Receiver<()>,
}

/// Who keeps a deferred definition out of the model's context until it is
/// loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolDeferral {
    /// Maki: a loaded tool joins the array, which rewrites the cached tools
    /// prefix once per load.
    Client,
    /// The API: deferred tools ship with `defer_loading` and a load is a
    /// `tool_reference` in the tool result, so the array stays the same
    /// bytes all session long.
    Native,
}

impl ToolDeferral {
    pub fn for_model(model: &Model) -> Self {
        if model.supports_deferred_tools() {
            Self::Native
        } else {
            Self::Client
        }
    }
}

/// Whether definitions go into a frame being built or one already sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum Arrival {
    WithFrame,
    Late,
}

/// One session's view of MCP: the shared handle plus the deferred tools
/// this session loaded. Loads are per session, so a subagent's searches
/// never bloat the parent's context.
///
/// A frame builds its array once with `extend_tools` and then only grows it
/// with `append_late_tools`, so the `tool_search` catalog keeps the exact
/// bytes it was first sent with.
#[derive(Clone)]
pub struct McpSession {
    handle: McpHandle,
    loaded: Arc<Mutex<HashSet<Arc<str>>>>,
}

impl std::ops::Deref for McpSession {
    type Target = McpHandle;
    fn deref(&self) -> &McpHandle {
        &self.handle
    }
}

impl McpSession {
    /// `history` seeds the loaded set when resuming: tools the model was
    /// already calling, or a search had loaded, stay declared across
    /// restarts. A pure string scan, so it is safe before servers connect,
    /// and unknown names are inert.
    pub fn new(handle: McpHandle, history: &[Message]) -> Self {
        let loaded = history
            .iter()
            .flat_map(|m| &m.content)
            .flat_map(|block| match block {
                ContentBlock::ToolUse { name, .. } => std::slice::from_ref(name),
                ContentBlock::ToolResult { loaded_tools, .. } => loaded_tools.as_slice(),
                _ => &[],
            })
            .filter(|name| is_wire_name(name))
            .map(|name| internal_tool_name(name).into())
            .collect();
        Self {
            handle,
            loaded: Arc::new(Mutex::new(loaded)),
        }
    }

    /// A view over the same handle with no loads, for a new (sub)session.
    pub fn fresh(&self) -> Self {
        Self::new(self.handle.clone(), &[])
    }

    /// Append a frame's MCP definitions: skipped in this fork to preserve prompt cache.
    pub fn extend_tools(&self, _tools: &mut Value, _deferral: ToolDeferral) {}

    /// Adds published tools: skipped in this fork to preserve prompt cache.
    pub fn append_late_tools(&self, _tools: &mut Value, _deferral: ToolDeferral) {}

    /// The one place MCP definitions enter a tool array, so a frame grows by
    /// the same rules it was built with.
    ///
    /// The `defer_tools` threshold is measured against the full index, not
    /// what's left deferred, so loading tools mid-session can never flip the
    /// remainder into the context. Servers that connect later push it over
    /// just the same, and then `tool_search` joins with them. Native deferral
    /// ignores it and always ships `tool_search`, so a late server can join
    /// as deferred entries, the only addition that keeps the cache and the
    /// thinking bound to the prefix. A late `always_load` tool joins that way
    /// too: in full it would cost both.
    #[allow(dead_code)]
    fn add_missing(&self, tools: &mut Value, deferral: ToolDeferral, arrival: Arrival) {
        let Some(arr) = tools.as_array_mut() else {
            debug_assert!(false, "tools must be a JSON array");
            return;
        };
        let existing = wire_names(arr);
        let searchable = existing.contains(TOOL_SEARCH_TOOL_NAME);
        let idx = self.handle.index.load();
        let defer = self.deferring(&idx, deferral);
        let loaded = self.lock_loaded();
        let mut cataloged: Vec<&ToolDescriptor> = Vec::new();
        for d in idx
            .descriptors
            .iter()
            .filter(|d| !existing.contains(d.wire_name()))
        {
            let eager =
                d.always_load && !(arrival == Arrival::Late && deferral == ToolDeferral::Native);
            if !defer || eager {
                arr.push(d.definition.clone());
                continue;
            }
            match deferral {
                ToolDeferral::Client if loaded.contains(&*d.qualified_name) => {
                    arr.push(d.definition.clone());
                }
                ToolDeferral::Client => cataloged.push(d),
                ToolDeferral::Native => {
                    arr.push(natively_deferred(d));
                    cataloged.push(d);
                }
            }
        }
        drop(loaded);
        let wants_search = defer && (!cataloged.is_empty() || deferral == ToolDeferral::Native);
        match (wants_search, searchable, arrival) {
            (true, false, _) => arr.push(tool_search_definition(&cataloged)),
            (true, true, Arrival::WithFrame) => warn!(
                deferred = cataloged.len(),
                "a tool named {TOOL_SEARCH_TOOL_NAME} already exists; deferred MCP tools stay hidden"
            ),
            _ => {}
        }
    }

    /// Native deferral ignores the threshold, see [`Self::add_missing`].
    fn deferring(&self, idx: &ToolIndex, deferral: ToolDeferral) -> bool {
        deferral == ToolDeferral::Native
            || idx.descriptors.iter().filter(|d| !d.always_load).count() > self.handle.defer_tools
    }

    /// The one writer of the loaded set. Returns the wire names for the
    /// caller's tool result, so the load is in history for a native replay
    /// and a resume. A nested call has no result in history to replay, so
    /// loading from one would leave a resumed session with a different
    /// tool array than the live one.
    fn load<'d>(
        &self,
        hits: impl IntoIterator<Item = &'d ToolDescriptor>,
        origin: CallOrigin,
    ) -> Vec<String> {
        if !origin.is_model() {
            return Vec::new();
        }
        let mut loaded = self.lock_loaded();
        hits.into_iter()
            .map(|d| {
                loaded.insert(Arc::clone(&d.qualified_name));
                d.wire_name().to_owned()
            })
            .collect()
    }

    /// Rank deferred tools against `query` keywords (exact name first, then
    /// name hits over description hits). A model-originated search loads the
    /// top `MAX_SEARCH_LOADS` and names them in `loaded_tools`; a nested one
    /// only reports the names, which the sandbox can already call.
    pub fn search_tools(
        &self,
        query: &str,
        origin: CallOrigin,
        deferral: ToolDeferral,
    ) -> Result<TextOutput, String> {
        let q = query.trim().to_lowercase();
        let tokens: Vec<&str> = q
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .collect();
        if tokens.is_empty() {
            return Err(SEARCH_EMPTY_QUERY.into());
        }
        let idx = self.handle.index.load();
        let mut matches: Vec<(bool, usize, &ToolDescriptor)> = idx
            .descriptors
            .iter()
            .filter(|d| loadable(d, deferral))
            .filter_map(|d| {
                let name = d.wire_name().to_lowercase();
                let haystack = build_haystack(&d.definition);
                // The catalog shows bare tool names, so exact match must
                // accept both `server__tool` and `tool`.
                let exact = name == q
                    || d.qualified_name
                        .split_once(SEPARATOR)
                        .is_some_and(|(_, raw)| raw.eq_ignore_ascii_case(&q));
                let score: usize = tokens
                    .iter()
                    .map(|t| {
                        if name.contains(t) {
                            NAME_HIT_SCORE
                        } else if haystack.contains(t) {
                            DESCRIPTION_HIT_SCORE
                        } else {
                            0
                        }
                    })
                    .sum();
                (exact || score > 0).then_some((exact, score, d))
            })
            .collect();
        matches.sort_by(|a, b| {
            (b.0, b.1)
                .cmp(&(a.0, a.1))
                .then_with(|| a.2.wire_name().cmp(b.2.wire_name()))
        });
        let (hits, overflow) = matches.split_at(matches.len().min(MAX_SEARCH_LOADS));
        let loaded_tools = self.load(hits.iter().map(|(_, _, d)| *d), origin);
        info!(
            query = %q,
            hits = hits.len(),
            overflow = overflow.len(),
            origin = ?origin,
            "MCP tool search"
        );
        if hits.is_empty() {
            return Ok(format!(
                "{SEARCH_NO_MATCH} '{query}'. Try other keywords or an exact name from the catalog."
            )
            .into());
        }
        let plural = if hits.len() == 1 { "tool" } else { "tools" };
        let (verb, tail) = if origin.is_model() {
            SEARCH_HEADER_MODEL
        } else {
            SEARCH_HEADER_NESTED
        };
        let mut out = format!("{verb} {} {plural}{tail}", hits.len());
        for (_, _, d) in hits {
            out.push_str(&format!("\n- `{}`", d.wire_name()));
        }
        if !overflow.is_empty() {
            let shown = overflow.len().min(MAX_OVERFLOW_NAMES);
            let names: Vec<String> = overflow[..shown]
                .iter()
                .map(|(_, _, d)| format!("`{}`", d.wire_name()))
                .collect();
            out.push_str(&format!("\n{SEARCH_OVERFLOW_PREFIX}{}", names.join(", ")));
            if overflow.len() > shown {
                out.push_str(&format!(" and {} more", overflow.len() - shown));
            }
            out.push_str(". Search an exact tool name to load it.");
        }
        Ok(TextOutput {
            loaded_tools,
            ..out.into()
        })
    }

    /// Every published tool's wire name, deferred ones included. Whoever
    /// enumerates callable names has to see past the `tool_search` catalog that
    /// `extend_tools` hides deferred tools behind.
    pub fn wire_names(&self) -> Vec<String> {
        self.handle
            .index
            .load()
            .descriptors
            .iter()
            .map(|d| d.wire_name().to_owned())
            .collect()
    }

    /// A deferred tool the model calls straight from the catalog is loaded
    /// like a search hit. Empty for a tool that is not deferred, so its
    /// result stays verbatim.
    pub fn load_called(
        &self,
        qualified_name: &str,
        origin: CallOrigin,
        deferral: ToolDeferral,
    ) -> Vec<String> {
        let idx = self.handle.index.load();
        if !self.deferring(&idx, deferral) {
            return Vec::new();
        }
        let hit = idx
            .descriptors
            .iter()
            .find(|d| loadable(d, deferral) && &*d.qualified_name == qualified_name);
        self.load(hit, origin)
    }

    fn lock_loaded(&self) -> std::sync::MutexGuard<'_, HashSet<Arc<str>>> {
        self.loaded.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Native deferral can hold an `always_load` tool back (see
/// [`McpSession::add_missing`]), so it must stay loadable there. Loading one
/// that went out in full is harmless: the API is only referenced names the
/// request still defers.
fn loadable(d: &ToolDescriptor, deferral: ToolDeferral) -> bool {
    !d.always_load || deferral == ToolDeferral::Native
}

impl McpHandle {
    pub fn modules(&self) -> Arc<[McpModuleInfo]> {
        Arc::clone(&self.index.load().modules)
    }

    pub fn has_modules(&self) -> bool {
        !self.index.load().modules.is_empty()
    }
    pub fn send(&self, cmd: McpCommand) {
        if let Err(e) = self.cmd_tx.send(cmd) {
            warn!(error = %e, "MCP command loop is gone");
        }
    }

    /// Resolves once every enabled server has connected or failed. Await it
    /// before building a request's tool list, or an early prompt ships without
    /// the MCP tools.
    pub async fn ready(&self) {
        let _ = self.ready_rx.clone().changed().await;
    }

    pub fn reader(&self) -> McpSnapshotReader {
        McpSnapshotReader(Arc::clone(&self.snapshot))
    }

    /// Where wire names (`server__tool`, what providers send) and internal names
    /// (`server.tool`, what the index holds) meet. Returns the interned qualified
    /// name every other MCP entry point takes, or `None` when no server publishes
    /// it. Deferred tools resolve too: hidden from the tool array is not the same
    /// as uncallable.
    pub fn resolve(&self, name: &str) -> Option<Arc<str>> {
        let index = self.index.load();
        let interned = |n: &str| index.tools.get_key_value(n).map(|(k, _)| Arc::clone(k));
        interned(name).or_else(|| {
            is_wire_name(name)
                .then(|| interned(&internal_tool_name(name)))
                .flatten()
        })
    }

    /// Whether the published schema of this tool carries a `toolcall_reason`
    /// we injected. A field the server declared itself belongs to the tool,
    /// so its value must reach the server untouched.
    pub fn reason_injected(&self, qualified_name: &str) -> bool {
        self.index
            .load()
            .tools
            .get(qualified_name)
            .is_some_and(|t| t.reason_injected)
    }

    pub async fn call_tool(&self, qualified_name: &str, args: &Value) -> Result<String, McpError> {
        let (raw_name, transport) = {
            let idx = self.index.load();
            let Some(t) = idx.tools.get(qualified_name) else {
                return Err(McpError::UnknownTool {
                    name: qualified_name.into(),
                });
            };
            (t.raw_name.clone(), Arc::clone(&t.transport))
        };
        transport.call_tool(&raw_name, args).await
    }

    pub async fn get_prompt(
        &self,
        qualified_name: &str,
        arguments: &HashMap<String, String>,
    ) -> Result<Vec<PromptMessage>, McpError> {
        let (raw_name, transport) = {
            let idx = self.index.load();
            let Some(p) = idx.prompts.get(qualified_name) else {
                return Err(McpError::UnknownPrompt {
                    name: qualified_name.into(),
                });
            };
            (p.raw_name.clone(), Arc::clone(&p.transport))
        };
        transport.get_prompt(&raw_name, arguments).await
    }

    pub async fn shutdown(&self) {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.send(McpCommand::Shutdown { ack: ack_tx });
        if timeout(MCP_SHUTDOWN_TIMEOUT, ack_rx).await.is_err() {
            warn!("MCP shutdown timed out after {MCP_SHUTDOWN_TIMEOUT:?}");
        }
    }

    /// Handle whose command loop is whatever the caller does with `cmd_rx`,
    /// so a test can hold a `Shutdown` unacked and observe a wedged teardown.
    #[cfg(test)]
    pub(crate) fn for_test(cmd_tx: UnboundedSender<McpCommand>) -> Self {
        Self {
            cmd_tx,
            index: Arc::new(ArcSwap::from_pointee(ToolIndex::default())),
            snapshot: Arc::new(ArcSwap::from_pointee(McpSnapshot::default())),
            defer_tools: 0,
            ready_rx: watch::channel(()).1,
        }
    }
}

/// Returns as soon as the config is read, so nothing with a screen waits on a
/// slow `initialize`. Await `McpHandle::ready` before touching the tool index.
pub async fn start(
    cwd: &Path,
    project_config: ProjectConfig,
) -> (Option<McpHandle>, McpConfigErrors) {
    tracing::info!(cwd = %cwd.display(), "starting MCP");
    let cwd = cwd.to_owned();
    let (config, config_errors) = spawn_blocking(move || load_config(&cwd, project_config))
        .await
        .expect("MCP config load panicked");
    (start_with_config(config), config_errors)
}

/// `start` for callers with no frame to protect, who want the tools up front.
pub async fn start_connected(
    cwd: &Path,
    project_config: ProjectConfig,
) -> (Option<McpHandle>, McpConfigErrors) {
    let (handle, config_errors) = start(cwd, project_config).await;
    if let Some(handle) = &handle {
        handle.ready().await;
    }
    (handle, config_errors)
}

/// `start` plus servers declared at runtime. `mcp.toml` wins on name, so a
/// runtime server can never swap out the credentials the user configured or
/// revive one they disabled.
pub async fn start_with_extra(
    cwd: &Path,
    project_config: ProjectConfig,
    extra: Vec<(String, RawTransport)>,
) -> (Option<McpHandle>, McpConfigErrors) {
    let owned_cwd = cwd.to_owned();
    let (mut config, config_errors) =
        spawn_blocking(move || load_config(&owned_cwd, project_config))
            .await
            .expect("MCP config load panicked");
    for (name, transport) in extra {
        match config.mcp.entry(name) {
            Entry::Vacant(slot) => {
                slot.insert(RawServerConfig::runtime(transport));
            }
            Entry::Occupied(slot) => {
                warn!(server = slot.key(), "runtime MCP server already configured");
            }
        }
    }
    (start_with_config(config), config_errors)
}

pub fn start_with_config(config: McpConfig) -> Option<McpHandle> {
    if config.is_empty() {
        tracing::info!("no MCP servers configured, skipping");
        return None;
    }

    let defer_tools = config.defer_tools.unwrap_or(DEFAULT_DEFER_TOOLS);
    let inner = parse_entries(config);

    let snapshot = Arc::new(ArcSwap::from_pointee(McpSnapshot::default()));
    let index: Arc<ArcSwap<ToolIndex>> = Arc::new(ArcSwap::from_pointee(ToolIndex::default()));
    publish(&inner, &index, &snapshot);

    let (cmd_tx, cmd_rx) = unbounded_channel();
    let (ready_tx, ready_rx) = watch::channel(());
    let handle = McpHandle {
        cmd_tx,
        index: Arc::clone(&index),
        snapshot: Arc::clone(&snapshot),
        defer_tools,
        ready_rx,
    };

    info!(total = inner.entries.len(), "MCP servers connecting");

    tokio::spawn(run(inner, index, snapshot, cmd_rx, ready_tx));
    Some(handle)
}

/// Connect results ride the same loop as commands, so a `Shutdown` arriving
/// mid-connect never waits for a slow `initialize`.
enum Step {
    Connected(usize, Result<StartResult, McpError>),
    Command(McpCommand),
    /// Every handle is gone.
    Closed,
}

async fn run(
    mut inner: McpManagerInner,
    index: Arc<ArcSwap<ToolIndex>>,
    snapshot: Arc<ArcSwap<McpSnapshot>>,
    mut cmd_rx: UnboundedReceiver<McpCommand>,
    ready_tx: watch::Sender<()>,
) {
    let (connected_tx, mut connected_rx) = unbounded_channel();
    // Held: dropping the set aborts a pending connect, which drops the
    // transport it owns and kills the child process group it just spawned.
    let connects = spawn_connects(&inner, connected_tx);
    let mut ready = Some(ready_tx);

    let mut ack: Option<oneshot::Sender<()>> = None;
    loop {
        release_ready(&inner, &mut ready);
        // A connect result that lands as the last command handle goes must
        // still be recorded, so the transport it carries is not dropped.
        let step = tokio::select! {
            biased;
            connected = async {
                match connected_rx.recv().await {
                    Some((i, result)) => Step::Connected(i, result),
                    // Every connect landed, so only commands wake us now.
                    None => pending().await,
                }
            } => connected,
            cmd = cmd_rx.recv() => cmd.map_or(Step::Closed, Step::Command),
        };

        match step {
            Step::Connected(i, result) => {
                // A toggle or reconnect that ran mid-connect owns the entry
                // now, so a late result must not resurrect it. Dropping it
                // kills the transport it carries.
                if inner.entries[i].status == McpServerStatus::Connecting {
                    let _ = apply_start_result(&mut inner.entries[i], result, "start");
                }
            }
            Step::Command(McpCommand::Toggle { server, enabled }) => {
                handle_toggle(&mut inner, &server, enabled).await;
            }
            Step::Command(McpCommand::Reconnect { server }) => {
                handle_reconnect(&mut inner, &server).await;
            }
            Step::Command(McpCommand::Shutdown { ack: tx }) => {
                ack = Some(tx);
                break;
            }
            Step::Closed => break,
        }
        inner.generation += 1;
        publish(&inner, &index, &snapshot);
    }
    drop(connects);
    shutdown_all(&mut inner).await;
    inner.generation += 1;
    publish(&inner, &index, &snapshot);
    if let Some(tx) = ack {
        let _ = tx.send(());
    }
}

/// Nothing left in `Connecting` means every server landed and the last publish
/// already carried its tools, so waiters can go.
fn release_ready(inner: &McpManagerInner, ready: &mut Option<watch::Sender<()>>) {
    if ready.is_none()
        || inner
            .entries
            .iter()
            .any(|e| e.status == McpServerStatus::Connecting)
    {
        return;
    }
    drop(ready.take());
    info!(
        running = inner
            .entries
            .iter()
            .filter(|e| e.transport.is_some())
            .count(),
        total = inner.entries.len(),
        "MCP servers initialized"
    );
}

async fn handle_toggle(inner: &mut McpManagerInner, server_name: &str, enabled: bool) {
    if let Some(path) = inner
        .entries
        .iter()
        .find(|e| e.name == server_name)
        .map(|e| e.origin.clone())
    {
        spawn_persist_enabled(path, server_name.to_owned(), enabled);
    }

    if enabled {
        if let Err(e) = refresh_server(inner, server_name).await {
            warn!(server = %server_name, error = %e, "MCP server refresh failed");
        }
    } else if let Some(entry) = inner.entries.iter_mut().find(|e| e.name == server_name) {
        entry.clear_connection().await;
        entry.status = McpServerStatus::Disabled;
    }

    info!(server = server_name, enabled, "MCP toggle complete");
}

/// Restart the server with its stored config. Fresh OAuth tokens are picked up
/// from storage by the transport, so no credentials travel through the command.
async fn handle_reconnect(inner: &mut McpManagerInner, server_name: &str) {
    let Some(entry) = inner.entries.iter().find(|e| e.name == server_name) else {
        warn!(server = server_name, "reconnect for unknown server");
        return;
    };
    if entry.status == McpServerStatus::Disabled {
        info!(
            server = server_name,
            "ignoring reconnect for disabled server"
        );
        return;
    }
    if let Err(e) = refresh_server(inner, server_name).await {
        warn!(server = %server_name, error = %e, "reconnect failed");
    }
    info!(server = server_name, "MCP reconnect complete");
}

async fn shutdown_all(inner: &mut McpManagerInner) {
    for entry in &mut inner.entries {
        entry.clear_connection().await;
        if entry.status != McpServerStatus::Disabled {
            entry.status = McpServerStatus::Failed("shutdown".into());
        }
    }
    info!("MCP command loop shutting down");
}

/// Tear the old transport down and wipe tools/prompts *before* starting the new one. That way
/// a failed start leaves the entry empty instead of holding zombie tool references into a dead
/// transport.
async fn refresh_server(inner: &mut McpManagerInner, server_name: &str) -> Result<(), McpError> {
    let Some(idx) = inner.entries.iter().position(|e| e.name == server_name) else {
        return Err(McpError::Config(format!("unknown server '{server_name}'")));
    };

    let config = inner.entries[idx]
        .config
        .clone()
        .ok_or_else(|| McpError::Config(format!("server '{server_name}' has no config")))?;

    {
        let entry = &mut inner.entries[idx];
        entry.status = McpServerStatus::Connecting;
        entry.clear_connection().await;
    }

    let result = start_server(&config).await;
    apply_start_result(&mut inner.entries[idx], result, "refresh")?;
    info!(
        server = server_name,
        tools = inner.entries[idx].tools.len(),
        "MCP server refreshed"
    );
    Ok(())
}

fn status_from_err(e: &McpError) -> McpServerStatus {
    match e {
        McpError::HttpError {
            status: 401,
            reason,
            ..
        } => McpServerStatus::NeedsAuth {
            url: Some(reason.clone()),
        },
        McpError::OAuthRequired { .. } => McpServerStatus::NeedsAuth { url: None },
        _ => McpServerStatus::Failed(e.to_string()),
    }
}

struct StartResult {
    transport: Arc<dyn McpTransport>,
    tool_infos: Vec<Tool>,
    prompt_infos: Vec<Prompt>,
}

async fn start_server(config: &ServerConfig) -> Result<StartResult, McpError> {
    let storage = maki_storage::StateDir::resolve().ok();
    let connection = transport::connect(config, storage.as_ref()).await?;
    // Asymmetric on purpose: some servers leave `tools` out of their
    // capabilities yet serve tools/list fine, so always ask (fatal only when
    // tools were declared).
    // Prompts only when declared: undeclared endpoints may answer junk,
    // and junk must not take down the server's tools.
    let tool_infos = match connection.list_tools().await {
        Ok(tools) => tools,
        Err(e) if !connection.declares_tools() => {
            warn!(server = config.name, error = %e, "tools/list failed; server declared no tools");
            Vec::new()
        }
        Err(e) => return Err(e),
    };
    let prompt_infos = if connection.declares_prompts() {
        connection.list_prompts().await?
    } else {
        Vec::new()
    };
    info!(
        server = config.name,
        tool_count = tool_infos.len(),
        prompt_count = prompt_infos.len(),
        "MCP server initialized"
    );
    Ok(StartResult {
        transport: Arc::new(connection),
        tool_infos,
        prompt_infos,
    })
}

fn parse_entries(config: McpConfig) -> McpManagerInner {
    let origins = config.origins;
    let mut entries = Vec::with_capacity(config.mcp.len());

    for (name, raw) in config.mcp {
        let transport_kind = transport_kind(&raw.transport);
        let origin = origins.get(&name).cloned().unwrap_or_default();
        let disabled = !raw.enabled;
        let (config, status) = match parse_server(name.clone(), raw, &origin) {
            Ok(sc) if disabled => (Some(sc), McpServerStatus::Disabled),
            Ok(sc) => (Some(sc), McpServerStatus::Connecting),
            Err(e) => {
                warn!(server = %name, error = %e, "invalid MCP server config");
                (None, McpServerStatus::Failed(e.to_string()))
            }
        };
        entries.push(ServerEntry {
            name,
            config,
            transport_kind,
            origin,
            status,
            transport: None,
            tools: Vec::new(),
            prompts: Vec::new(),
        });
    }

    // Config maps are unordered; a stable order keeps the tool_search
    // catalog and tools array byte-identical across runs (prompt cache).
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    McpManagerInner {
        entries,
        generation: 0,
    }
}

/// One task per enabled server, each reporting back as it lands, so `run`
/// publishes a fast server's tools without waiting for the slowest.
fn spawn_connects(
    inner: &McpManagerInner,
    tx: UnboundedSender<(usize, Result<StartResult, McpError>)>,
) -> JoinSet<()> {
    inner
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.status == McpServerStatus::Connecting)
        .filter_map(|(i, e)| e.config.clone().map(|c| (i, c)))
        .map(|(i, config)| {
            let tx = tx.clone();
            async move {
                let _ = tx.send((i, start_server(&config).await));
            }
        })
        .collect()
}

fn apply_start_result(
    entry: &mut ServerEntry,
    result: Result<StartResult, McpError>,
    action: &'static str,
) -> Result<(), McpError> {
    match result {
        Ok(start) => {
            entry.populate(start);
            Ok(())
        }
        Err(e) => {
            entry.status = status_from_err(&e);
            if !matches!(entry.status, McpServerStatus::NeedsAuth { .. }) {
                warn!(server = %entry.name, action, error = %e, "MCP server start failed");
            }
            Err(e)
        }
    }
}

/// The only place read-side state is updated. Every mutation in the command loop ends here.
fn publish(inner: &McpManagerInner, index: &ArcSwap<ToolIndex>, snapshot: &ArcSwap<McpSnapshot>) {
    let mut tools = HashMap::new();
    let mut prompts = HashMap::new();
    let mut descriptors: Vec<ToolDescriptor> = Vec::new();
    let mut modules: Vec<McpModuleInfo> = Vec::new();
    let mut server_infos = Vec::with_capacity(inner.entries.len());
    let mut prompt_infos = Vec::new();
    let mut pids = Vec::new();

    for entry in &inner.entries {
        let (url, oauth, ca_file) = match entry.config.as_ref().map(|c| &c.transport) {
            Some(Transport::Http {
                url,
                oauth,
                ca_file,
                ..
            }) => (Some(url.clone()), oauth.clone(), ca_file.clone()),
            _ => (None, None, None),
        };

        if let Some(ref transport) = entry.transport
            && entry.status != McpServerStatus::Disabled
        {
            let always_load = entry.config.as_ref().is_some_and(|c| c.always_load);
            let description = entry
                .config
                .as_ref()
                .and_then(|c| c.description.clone())
                .unwrap_or_else(|| "(no description)".to_string());
            let mut functions = Vec::with_capacity(entry.tools.len());
            for t in &entry.tools {
                let mut sanitized_schema = sanitize_tool_input_schema(t.input_schema.clone());
                let reason_injected = inject_toolcall_reason(&mut sanitized_schema);
                tools.insert(
                    Arc::clone(&t.qualified_name),
                    ToolRef {
                        raw_name: t.raw_name.clone(),
                        transport: Arc::clone(transport),
                        reason_injected,
                    },
                );
                functions.push(McpFunctionInfo {
                    name: t.raw_name.clone(),
                    description: t.description.clone(),
                    schema: sanitized_schema.clone(),
                });
                descriptors.push(ToolDescriptor {
                    qualified_name: Arc::clone(&t.qualified_name),
                    always_load,
                    definition: json!({
                        "name": wire_tool_name(&t.qualified_name),
                        "description": t.description,
                        "input_schema": sanitized_schema,
                    }),
                });
            }
            functions.sort_by(|a, b| a.name.cmp(&b.name));
            modules.push(McpModuleInfo {
                name: entry.name.clone(),
                description,
                functions,
            });
            for p in &entry.prompts {
                prompts.insert(
                    p.qualified_name.clone(),
                    PromptRef {
                        raw_name: p.raw_name.clone(),
                        transport: Arc::clone(transport),
                    },
                );
                prompt_infos.push(p.to_info(&entry.name));
            }
            pids.extend(transport.child_pids());
        }

        server_infos.push(McpServerInfo {
            name: entry.name.clone(),
            transport_kind: entry.transport_kind,
            tool_count: entry.tools.len(),
            prompt_count: entry.prompts.len(),
            status: entry.status.clone(),
            config_path: entry.origin.clone(),
            url,
            oauth,
            ca_file,
        });
    }

    modules.sort_by(|a, b| a.name.cmp(&b.name));
    index.store(Arc::new(ToolIndex {
        tools,
        prompts,
        descriptors: descriptors.into(),
        modules: modules.into(),
    }));
    snapshot.store(Arc::new(McpSnapshot {
        infos: server_infos,
        prompts: prompt_infos,
        pids,
        generation: inner.generation,
    }));
}

/// Sessions for dispatch-level tests. Always compiled, so tests outside this
/// crate can build a `ToolContext` that carries MCP.
pub mod test_support {
    use std::path::PathBuf;
    use std::sync::Arc;

    use arc_swap::ArcSwap;
    use serde_json::{Value, json};
    use tokio::sync::mpsc::unbounded_channel;
    use tokio::sync::watch;

    use std::collections::HashMap;

    use super::config::McpServerStatus;
    use super::error::McpError;
    use super::transport::{BoxFuture, McpTransport, PromptMessage};
    use super::{
        McpHandle, McpManagerInner, McpSession, McpSnapshot, McpToolDef, SEPARATOR, ServerEntry,
        ToolIndex, publish,
    };

    /// Goes through the real `publish` path, so it cannot drift from how the
    /// index is built in production. Tools are named `server.tool`.
    pub fn stub_session(tools: &[(&str, &str)]) -> McpSession {
        let mut server_map: HashMap<&str, Vec<McpToolDef>> = HashMap::new();
        for (qualified, description) in tools {
            let (server, raw) = qualified
                .split_once(SEPARATOR)
                .unwrap_or(("stub", *qualified));
            server_map.entry(server).or_default().push(McpToolDef {
                qualified_name: Arc::from(*qualified),
                raw_name: raw.into(),
                description: (*description).into(),
                input_schema: json!({}),
            });
        }
        let entries = if server_map.is_empty() {
            vec![ServerEntry {
                name: "stub".into(),
                config: None,
                transport_kind: "stub",
                origin: PathBuf::new(),
                status: McpServerStatus::Running,
                transport: Some(Arc::new(StubTransport)),
                tools: Vec::new(),
                prompts: Vec::new(),
            }]
        } else {
            let mut srv_names: Vec<_> = server_map.keys().copied().collect();
            srv_names.sort();
            srv_names
                .into_iter()
                .map(|server| ServerEntry {
                    name: server.into(),
                    config: None,
                    transport_kind: "stub",
                    origin: PathBuf::new(),
                    status: McpServerStatus::Running,
                    transport: Some(Arc::new(StubTransport)),
                    tools: server_map.remove(server).unwrap(),
                    prompts: Vec::new(),
                })
                .collect()
        };
        let inner = McpManagerInner {
            entries,
            generation: 0,
        };
        let index = Arc::new(ArcSwap::from_pointee(ToolIndex::default()));
        let snapshot = Arc::new(ArcSwap::from_pointee(McpSnapshot::default()));
        publish(&inner, &index, &snapshot);
        McpSession::new(
            McpHandle {
                cmd_tx: unbounded_channel().0,
                index,
                snapshot,
                defer_tools: 0,
                ready_rx: watch::channel(()).1,
            },
            &[],
        )
    }

    pub fn pending_stub_session(tools: &[(&str, &str)]) -> (McpSession, watch::Sender<()>) {
        let (ready_tx, ready_rx) = watch::channel(());
        let mut session = stub_session(tools);
        session.handle.ready_rx = ready_rx;
        (session, ready_tx)
    }

    /// A session whose tools answer with the JSON of the args they received,
    /// so a test can see exactly what reached the server. `schemas` maps each
    /// qualified name to its declared input schema.
    pub fn echo_session(tools: &[(&str, Value)]) -> McpSession {
        let mut server_map: HashMap<&str, Vec<McpToolDef>> = HashMap::new();
        for (qualified, schema) in tools {
            let (server, raw) = qualified
                .split_once(SEPARATOR)
                .unwrap_or(("stub", *qualified));
            server_map.entry(server).or_default().push(McpToolDef {
                qualified_name: Arc::from(*qualified),
                raw_name: raw.into(),
                description: String::new(),
                input_schema: schema.clone(),
            });
        }
        let mut srv_names: Vec<_> = server_map.keys().copied().collect();
        srv_names.sort();
        let entries = srv_names
            .into_iter()
            .map(|server| ServerEntry {
                name: server.into(),
                config: None,
                transport_kind: "stub",
                origin: PathBuf::new(),
                status: McpServerStatus::Running,
                transport: Some(Arc::new(EchoTransport)),
                tools: server_map.remove(server).unwrap(),
                prompts: Vec::new(),
            })
            .collect();
        let inner = McpManagerInner {
            entries,
            generation: 0,
        };
        let index = Arc::new(ArcSwap::from_pointee(ToolIndex::default()));
        let snapshot = Arc::new(ArcSwap::from_pointee(McpSnapshot::default()));
        publish(&inner, &index, &snapshot);
        McpSession::new(
            McpHandle {
                cmd_tx: unbounded_channel().0,
                index,
                snapshot,
                defer_tools: 0,
                ready_rx: watch::channel(()).1,
            },
            &[],
        )
    }

    /// Fails every call with `UnknownTool`, which is how a test proves the call
    /// reached MCP at all.
    struct StubTransport;

    impl McpTransport for StubTransport {
        fn call_tool<'a>(
            &'a self,
            name: &'a str,
            _args: &'a Value,
        ) -> BoxFuture<'a, Result<String, McpError>> {
            Box::pin(async move { Err(McpError::UnknownTool { name: name.into() }) })
        }
        fn get_prompt<'a>(
            &'a self,
            name: &'a str,
            _arguments: &'a HashMap<String, String>,
        ) -> BoxFuture<'a, Result<Vec<PromptMessage>, McpError>> {
            Box::pin(async move { Err(McpError::UnknownPrompt { name: name.into() }) })
        }
        fn shutdown<'a>(&'a self) -> BoxFuture<'a, ()> {
            Box::pin(async {})
        }
    }

    /// Answers every call with the JSON of the args it received.
    struct EchoTransport;

    impl McpTransport for EchoTransport {
        fn call_tool<'a>(
            &'a self,
            _name: &'a str,
            args: &'a Value,
        ) -> BoxFuture<'a, Result<String, McpError>> {
            Box::pin(async move { Ok(args.to_string()) })
        }
        fn get_prompt<'a>(
            &'a self,
            name: &'a str,
            _arguments: &'a HashMap<String, String>,
        ) -> BoxFuture<'a, Result<Vec<PromptMessage>, McpError>> {
            Box::pin(async move { Err(McpError::UnknownPrompt { name: name.into() }) })
        }
        fn shutdown<'a>(&'a self) -> BoxFuture<'a, ()> {
            Box::pin(async {})
        }
    }
}

#[cfg(test)]
pub(crate) fn tool_names(tools: &Value) -> Vec<&str> {
    tools
        .as_array()
        .expect("tools must be a JSON array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect()
}

#[allow(dead_code)]
fn wire_names(tools: &[Value]) -> HashSet<String> {
    tools
        .iter()
        .filter_map(|t| t["name"].as_str().map(String::from))
        .collect()
}

#[allow(dead_code)]
fn natively_deferred(d: &ToolDescriptor) -> Value {
    let mut definition = d.definition.clone();
    definition[DEFER_LOADING_KEY] = json!(true);
    definition
}

fn build_haystack(definition: &Value) -> String {
    let mut hay = definition["description"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    if let Some(props) = definition["input_schema"]["properties"].as_object() {
        for key in props.keys() {
            hay.push(' ');
            hay.push_str(&key.to_lowercase());
        }
    }
    hay
}

#[allow(dead_code)]
fn tool_search_definition(deferred: &[&ToolDescriptor]) -> Value {
    // Grouping by server drops the repeated `server__` prefix, a few
    // tokens per tool. Descriptors arrive grouped because entries are
    // sorted and published per server.
    let mut catalog = String::new();
    let mut current_server = "";
    for d in deferred {
        let (server, raw) = d
            .qualified_name
            .split_once(SEPARATOR)
            .unwrap_or((UNKNOWN_MCP, &d.qualified_name));
        if server == current_server {
            catalog.push_str(", ");
        } else {
            if !catalog.is_empty() {
                catalog.push('\n');
            }
            catalog.push_str(server);
            catalog.push_str(": ");
            current_server = server;
        }
        catalog.push_str(raw);
    }
    if catalog.is_empty() {
        catalog.push_str(EMPTY_CATALOG);
    }
    json!({
        "name": TOOL_SEARCH_TOOL_NAME,
        "description": format!(
            "Search and load deferred MCP tools; they are not callable until \
             loaded, from your next message on. Keywords match tool names, \
             descriptions, and parameter names; an exact tool name always wins. \
             Search also finds tools from servers that connect after this list \
             was written.\n\
             Deferred tools:\n{catalog}"
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords or an exact tool name from the catalog"
                }
            },
            "required": ["query"]
        }
    })
}

fn spawn_persist_enabled(path: PathBuf, name: String, enabled: bool) {
    spawn_blocking(move || {
        if let Err(e) = config::persist_enabled(&path, &name, enabled) {
            warn!(error = %e, server = %name, "failed to persist MCP toggle");
        }
    });
}

#[cfg(unix)]
pub fn kill_process_groups(pids: &[u32]) {
    use crate::child_guard::sigkill_process_group;

    pids.iter().copied().for_each(sigkill_process_group);
}

#[cfg(not(unix))]
pub fn kill_process_groups(_pids: &[u32]) {}

/// Dedup cache for qualified MCP tool names. The set is bounded (finite per session)
/// and `Arc<str>` means entries get freed when the cache drops, unlike the old `Box::leak`.
fn intern(name: String) -> Arc<str> {
    static CACHE: LazyLock<Mutex<HashMap<String, Arc<str>>>> = LazyLock::new(Mutex::default);
    let mut map = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = map.get(&name) {
        return Arc::clone(existing);
    }
    let arc: Arc<str> = Arc::from(name.as_str());
    map.insert(name, Arc::clone(&arc));
    arc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::schema::TOOLCALL_REASON_FIELD;
    use config::{RawHttpFields, RawServerConfig, RawStdioFields, RawTransport};
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[cfg(unix)]
    use std::time::Instant;
    use test_case::test_case;
    use tokio::sync::{Mutex as AsyncMutex, Notify};

    const DEFAULT_TIMEOUT_MS: u64 = 30_000;
    const MISSING_PROGRAM: &str = "/nonexistent/definitely-not-here";
    const MISSING_CA_FILE: &str = "/nonexistent/ca.pem";
    const SERVER: &str = "srv";
    const CHALLENGE: &str = r#"Bearer resource_metadata="https://srv.example/.well-known""#;

    fn stdio_raw(cmd: &[&str]) -> RawServerConfig {
        RawServerConfig {
            enabled: true,
            description: None,
            timeout: DEFAULT_TIMEOUT_MS,
            always_load: false,
            transport: RawTransport::Stdio(RawStdioFields {
                command: cmd.iter().map(|s| s.to_string()).collect(),
                environment: HashMap::new(),
            }),
        }
    }

    fn make_config(entries: Vec<(&str, RawServerConfig)>) -> McpConfig {
        let mut mcp = HashMap::new();
        let mut origins = HashMap::new();
        for (name, cfg) in entries {
            origins.insert(name.to_string(), PathBuf::from("/test/config.toml"));
            mcp.insert(name.to_string(), cfg);
        }
        McpConfig {
            mcp,
            origins,
            ..Default::default()
        }
    }

    const TOOL_NAME: &str = "srv.tool";
    const WIRE_TOOL_NAME: &str = "srv__tool";

    /// Counts shutdowns, signals on `call_entered` the moment a `tools/call` begins, and holds
    /// the call inside `call_gate` until tests release it. That way tests can meet an in-flight
    /// RPC at a known point without polling.
    struct FakeTransport {
        shutdowns: AtomicUsize,
        call_entered: Notify,
        call_gate: AsyncMutex<()>,
    }

    impl FakeTransport {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                shutdowns: AtomicUsize::new(0),
                call_entered: Notify::new(),
                call_gate: AsyncMutex::new(()),
            })
        }

        fn shutdowns(&self) -> usize {
            self.shutdowns.load(Ordering::SeqCst)
        }
    }

    impl McpTransport for FakeTransport {
        fn call_tool<'a>(
            &'a self,
            _name: &'a str,
            _args: &'a Value,
        ) -> transport::BoxFuture<'a, Result<String, McpError>> {
            Box::pin(async move {
                self.call_entered.notify_one();
                let _g = self.call_gate.lock().await;
                Ok("ok".into())
            })
        }
        fn get_prompt<'a>(
            &'a self,
            _name: &'a str,
            _arguments: &'a HashMap<String, String>,
        ) -> transport::BoxFuture<'a, Result<Vec<PromptMessage>, McpError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn shutdown<'a>(&'a self) -> transport::BoxFuture<'a, ()> {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    fn fake_entry(name: &str, transport: Arc<dyn McpTransport>) -> ServerEntry {
        let qualified = intern(format!("{name}{SEPARATOR}tool"));
        ServerEntry {
            name: name.into(),
            config: None,
            transport_kind: "fake",
            origin: PathBuf::new(),
            status: McpServerStatus::Running,
            transport: Some(transport),
            tools: vec![McpToolDef {
                qualified_name: qualified,
                raw_name: "tool".into(),
                description: String::new(),
                input_schema: json!({}),
            }],
            prompts: Vec::new(),
        }
    }

    fn bad_stdio_config(name: &str) -> ServerConfig {
        ServerConfig {
            name: name.into(),
            description: None,
            timeout: Duration::from_secs(1),
            always_load: false,
            transport: Transport::Stdio {
                program: MISSING_PROGRAM.into(),
                args: vec![],
                environment: HashMap::new(),
            },
        }
    }

    /// Build `inner`, publish it into fresh `ArcSwap`s, and return a live `McpSession` pointing
    /// at the same state so tests can hit both the mutation and the read path.
    fn setup(entries: Vec<ServerEntry>) -> (McpManagerInner, McpSession) {
        setup_with_defer(entries, 0)
    }

    fn setup_with_defer(
        entries: Vec<ServerEntry>,
        defer_tools: usize,
    ) -> (McpManagerInner, McpSession) {
        let inner = McpManagerInner {
            entries,
            generation: 0,
        };
        let index = Arc::new(ArcSwap::from_pointee(ToolIndex::default()));
        let snapshot = Arc::new(ArcSwap::from_pointee(McpSnapshot::default()));
        publish(&inner, &index, &snapshot);
        let handle = McpHandle {
            cmd_tx: unbounded_channel().0,
            index,
            snapshot,
            defer_tools,
            ready_rx: watch::channel(()).1,
        };
        (inner, McpSession::new(handle, &[]))
    }

    #[test]
    fn parse_entries_sorts_servers_by_name() {
        let config = make_config(vec![
            ("zeta", stdio_raw(&["z"])),
            ("alpha", stdio_raw(&["a"])),
            ("mid", stdio_raw(&["m"])),
        ]);
        let inner = parse_entries(config);
        let names: Vec<&str> = inner.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn disabled_server_with_missing_ca_file_stays_disabled() {
        let mut raw = RawServerConfig::runtime(RawTransport::Http(RawHttpFields {
            url: "https://mcp.example.com/mcp".into(),
            headers: HashMap::new(),
            oauth: None,
            ca_file: Some(MISSING_CA_FILE.into()),
        }));
        raw.enabled = false;
        let inner = parse_entries(make_config(vec![("srv", raw)]));
        assert_eq!(inner.entries[0].status, McpServerStatus::Disabled);
    }

    #[test]
    fn wire_names_returns_all_published_tools() {
        let (_inner, handle) = setup(vec![fake_entry("srv", FakeTransport::new())]);
        assert_eq!(handle.wire_names(), vec![WIRE_TOOL_NAME]);
    }

    #[test_case(
        McpError::HttpError { server: SERVER.into(), status: 401, reason: CHALLENGE.into() },
        McpServerStatus::NeedsAuth { url: Some(CHALLENGE.into()) }
        ; "challenge"
    )]
    #[test_case(
        McpError::OAuthRequired { server: SERVER.into() },
        McpServerStatus::NeedsAuth { url: None }
        ; "login_without_challenge"
    )]
    fn start_error_sets_the_status(error: McpError, status: McpServerStatus) {
        assert_eq!(status_from_err(&error), status);
    }

    #[test_case(TOOL_NAME, Some(TOOL_NAME) ; "internal_name")]
    #[test_case(WIRE_TOOL_NAME, Some(TOOL_NAME) ; "wire_name")]
    #[test_case("read", None ; "native_name")]
    #[test_case("gone__tool", None ; "unpublished_server")]
    fn resolve_maps_published_names_only(name: &str, expected: Option<&str>) {
        let (_inner, handle) = setup(vec![fake_entry("srv", FakeTransport::new())]);
        assert_eq!(handle.resolve(name).as_deref(), expected);
    }

    /// A `toolcall_reason` the server declares itself is the tool's argument,
    /// not our narration: publish must remember which side owns the field.
    #[test_case(json!({ "type": "object" }), true ; "injected")]
    #[test_case(
        json!({
            "type": "object",
            "properties": { TOOLCALL_REASON_FIELD: { "type": "string", "description": "the tool's own" } },
        }),
        false ; "server_declared"
    )]
    fn reason_injected_tracks_who_declared_the_field(schema: Value, expected: bool) {
        let mut entry = fake_entry("srv", FakeTransport::new());
        entry.tools[0].input_schema = schema;
        let (_inner, handle) = setup(vec![entry]);
        assert_eq!(handle.reason_injected(TOOL_NAME), expected);
        assert!(
            !handle.reason_injected("gone.tool"),
            "unknown tool owns nothing"
        );
    }

    /// `ready` carries the correctness of connecting in the background: a prompt
    /// typed during startup must not ship before the servers settle.
    #[tokio::test]
    async fn ready_settles_every_server_status() {
        assert!(start_with_config(McpConfig::default()).is_none());

        let mut disabled = stdio_raw(&["unused-disabled-cmd"]);
        disabled.enabled = false;
        let config = make_config(vec![
            ("disabled-srv", disabled),
            ("unparseable-srv", stdio_raw(&[])),
            ("unspawnable-srv", stdio_raw(&[MISSING_PROGRAM])),
        ]);
        let handle = start_with_config(config).unwrap();
        handle.ready().await;

        let infos = handle.reader().load().infos.clone();
        let status = |name: &str| {
            &infos
                .iter()
                .find(|i| i.name == name)
                .unwrap_or_else(|| panic!("{name} must be published"))
                .status
        };
        let failed = |name: &str| matches!(status(name), McpServerStatus::Failed(_));
        assert!(failed("unparseable-srv"));
        assert!(failed("unspawnable-srv"));
        assert_eq!(*status("disabled-srv"), McpServerStatus::Disabled);
    }

    /// `sleep` spawns fine and never answers `initialize`, so its connect only
    /// ends on the request timeout, far past the shutdown one. Shutdown has to
    /// preempt it, or quitting during startup hangs.
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_preempts_an_in_flight_connect() {
        const BLOCKED: &str = "shutdown must not wait for an in-flight connect";
        let config = make_config(vec![("slow-srv", stdio_raw(&["sleep", "60"]))]);
        let handle = start_with_config(config).unwrap();
        let started = Instant::now();
        handle.shutdown().await;
        assert!(started.elapsed() < MCP_SHUTDOWN_TIMEOUT, "{BLOCKED}");
    }

    /// If a refresh fails, the entry must end up empty. A zombie tool left behind would be
    /// handed to the model on the next turn and then try to call into a dead transport.
    #[tokio::test]
    async fn failed_refresh_clears_entry() {
        let t = FakeTransport::new();
        let (mut inner, _) = setup(vec![fake_entry("srv", Arc::clone(&t) as _)]);
        inner.entries[0].config = Some(bad_stdio_config("srv"));

        assert!(refresh_server(&mut inner, "srv").await.is_err());

        let entry = &inner.entries[0];
        assert_eq!(t.shutdowns(), 1);
        assert!(entry.tools.is_empty());
        assert!(entry.prompts.is_empty());
        assert!(entry.transport.is_none());
        assert!(matches!(entry.status, McpServerStatus::Failed(_)));
    }

    #[tokio::test]
    async fn disable_purges_entry_and_published_view() {
        let t = FakeTransport::new();
        let (mut inner, handle) = setup(vec![fake_entry("srv", Arc::clone(&t) as _)]);

        assert!(handle.resolve(TOOL_NAME).is_some());
        assert_eq!(handle.modules().len(), 1);

        handle_toggle(&mut inner, "srv", false).await;
        publish(&inner, &handle.index, &handle.snapshot);

        let entry = &inner.entries[0];
        assert_eq!(t.shutdowns(), 1);
        assert!(entry.tools.is_empty());
        assert!(entry.transport.is_none());
        assert_eq!(entry.status, McpServerStatus::Disabled);
        assert!(handle.resolve(TOOL_NAME).is_none());
        assert_eq!(handle.modules().len(), 0);
    }

    #[test]
    fn modules_extracts_descriptions_and_sorted_functions() {
        let srv = ServerEntry {
            name: "github".into(),
            config: Some(ServerConfig {
                name: "github".into(),
                description: Some("GitHub ops".into()),
                timeout: Duration::from_secs(10),
                always_load: false,
                transport: Transport::Stdio {
                    program: "echo".into(),
                    args: vec![],
                    environment: HashMap::new(),
                },
            }),
            transport_kind: "fake",
            origin: PathBuf::new(),
            status: McpServerStatus::Running,
            transport: Some(FakeTransport::new()),
            tools: vec![
                McpToolDef {
                    qualified_name: Arc::from("github.list_issues"),
                    raw_name: "list_issues".into(),
                    description: "List issues".into(),
                    input_schema: json!({ "type": "object" }),
                },
                McpToolDef {
                    qualified_name: Arc::from("github.create_issue"),
                    raw_name: "create_issue".into(),
                    description: "Create issue".into(),
                    input_schema: json!({ "type": "object" }),
                },
            ],
            prompts: Vec::new(),
        };
        let (_inner, handle) = setup(vec![srv]);
        let mods = handle.modules();
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].name, "github");
        assert_eq!(mods[0].description, "GitHub ops");
        assert_eq!(mods[0].functions.len(), 2);
        assert_eq!(mods[0].functions[0].name, "create_issue");
        assert_eq!(mods[0].functions[1].name, "list_issues");
    }

    /// Regression: the lock-free refactor fixed a case where `call_tool` held the inner read
    /// lock across the transport await, so any in-flight call blocked every publish behind it.
    /// The rendezvous here stays deterministic: the call signals on `call_entered`, the test
    /// waits for that signal, then calls `publish` while the call is still parked on `call_gate`.
    #[tokio::test]
    async fn slow_tool_call_does_not_block_publish() {
        let t = FakeTransport::new();
        let (mut inner, handle) = setup(vec![fake_entry("srv", Arc::clone(&t) as _)]);

        let held = t.call_gate.lock().await;
        let call_handle = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.call_tool(TOOL_NAME, &json!({})).await.unwrap() })
        };

        t.call_entered.notified().await;
        inner.generation += 1;
        publish(&inner, &handle.index, &handle.snapshot);
        assert_eq!(handle.snapshot.load().generation, 1);

        drop(held);
        call_handle.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_command_drains_and_acks() {
        let (t1, t2) = (FakeTransport::new(), FakeTransport::new());
        let inner = McpManagerInner {
            entries: vec![
                fake_entry("a", Arc::clone(&t1) as _),
                fake_entry("b", Arc::clone(&t2) as _),
            ],
            generation: 0,
        };
        let index = Arc::new(ArcSwap::from_pointee(ToolIndex::default()));
        let snapshot = Arc::new(ArcSwap::from_pointee(McpSnapshot::default()));
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let loop_task = tokio::spawn(run(
            inner,
            Arc::clone(&index),
            Arc::clone(&snapshot),
            cmd_rx,
            watch::channel(()).0,
        ));

        let (ack_tx, ack_rx) = oneshot::channel();
        cmd_tx.send(McpCommand::Shutdown { ack: ack_tx }).unwrap();
        ack_rx.await.unwrap();
        loop_task.await.unwrap();

        assert_eq!(t1.shutdowns(), 1);
        assert_eq!(t2.shutdowns(), 1);
        assert!(snapshot.load().infos.iter().all(|i| i.tool_count == 0));
    }

    #[test]
    fn is_valid_tool_name_enforces_wire_format() {
        use config::is_valid_tool_name;
        // Valid: alphanumeric, underscore, hyphen, 1-64 chars
        assert!(is_valid_tool_name("search"));
        assert!(is_valid_tool_name("web_search"));
        assert!(is_valid_tool_name("my-tool"));
        assert!(is_valid_tool_name(&"a".repeat(64)));
        // Invalid: empty, dots, special chars, too long
        assert!(!is_valid_tool_name(""));
        assert!(!is_valid_tool_name("web.search"));
        assert!(!is_valid_tool_name("admin.delete"));
        assert!(!is_valid_tool_name("tool!"));
        assert!(!is_valid_tool_name("*"));
        assert!(!is_valid_tool_name(&"a".repeat(65)));
    }
}
