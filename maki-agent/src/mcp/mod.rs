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
pub mod http;
pub mod oauth;
pub mod protocol;
pub mod stdio;
pub mod transport;

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use arc_swap::{ArcSwap, Guard};
use maki_config::ProjectConfig;
use maki_providers::Message;
use serde::Serialize;
use serde_json::Value;
use tracing::{info, warn};

use self::config::{
    McpConfig, McpConfigErrors, McpServerInfo, McpServerStatus, RawServerConfig, RawTransport,
    ServerConfig, Transport, load_config, parse_server, transport_kind,
};
use self::error::McpError;
use self::http::HttpTransport;
use self::stdio::StdioTransport;
use self::transport::McpTransport;
use crate::tools::schema::{inject_toolcall_reason, sanitize_tool_input_schema};

const SEPARATOR: &str = ".";
const WIRE_SEPARATOR: &str = "__";
pub const UNKNOWN_MCP: &str = "unknown_mcp";

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
    arguments: Vec<protocol::PromptArgument>,
}

impl McpPromptDef {
    fn from_info(server_name: &str, info: protocol::PromptInfo) -> Self {
        Self {
            qualified_name: format!("{server_name}{SEPARATOR}{}", info.name),
            raw_name: info.name,
            description: info.description.unwrap_or_default(),
            arguments: info.arguments,
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
                    required: a.required,
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
                raw_name: info.name,
                description: info.description,
                input_schema: info.input_schema,
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
    wire_name: String,
}

impl ToolDescriptor {
    fn wire_name(&self) -> &str {
        &self.wire_name
    }
}

struct ToolRef {
    raw_name: String,
    transport: Arc<dyn McpTransport>,
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
        ack: flume::Sender<()>,
    },
}

#[derive(Clone)]
pub struct McpHandle {
    cmd_tx: flume::Sender<McpCommand>,
    index: Arc<ArcSwap<ToolIndex>>,
    snapshot: Arc<ArcSwap<McpSnapshot>>,
    /// Nothing is ever sent on it. `run` drops the sender once the first
    /// connect pass has published, and that disconnect is the signal.
    ready_rx: flume::Receiver<Infallible>,
}

/// One session's view of MCP: wraps the shared handle.
/// Loads are no longer used since tools are dispatched through code_execution.
#[derive(Clone)]
pub struct McpSession {
    handle: McpHandle,
}

impl std::ops::Deref for McpSession {
    type Target = McpHandle;
    fn deref(&self) -> &McpHandle {
        &self.handle
    }
}

impl McpSession {
    pub fn new(handle: McpHandle, _history: &[Message]) -> Self {
        Self { handle }
    }

    /// A view over the same handle with no loads, for a new (sub)session.
    pub fn fresh(&self) -> Self {
        Self::new(self.handle.clone(), &[])
    }

    pub fn extend_tools(&self, _tools: &mut Value) {}

    pub fn append_late_tools(&self, _tools: &mut Value) {}

    pub fn wire_names(&self) -> Vec<String> {
        self.handle
            .index
            .load()
            .descriptors
            .iter()
            .map(|d| d.wire_name().to_owned())
            .collect()
    }
}

impl McpHandle {
    pub fn modules(&self) -> Arc<[McpModuleInfo]> {
        Arc::clone(&self.index.load().modules)
    }

    pub fn has_modules(&self) -> bool {
        !self.index.load().modules.is_empty()
    }

    pub fn send(&self, cmd: McpCommand) {
        if let Err(e) = self.cmd_tx.try_send(cmd) {
            warn!(error = %e, "MCP command loop is gone");
        }
    }

    /// Resolves once every enabled server has connected or failed. Await it
    /// before building a request's tool list, or an early prompt ships without
    /// the MCP tools.
    pub async fn ready(&self) {
        let _ = self.ready_rx.recv_async().await;
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
        transport::call_tool(transport.as_ref(), &raw_name, args).await
    }

    pub async fn get_prompt(
        &self,
        qualified_name: &str,
        arguments: &HashMap<String, String>,
    ) -> Result<Vec<protocol::PromptMessage>, McpError> {
        let (raw_name, transport) = {
            let idx = self.index.load();
            let Some(p) = idx.prompts.get(qualified_name) else {
                return Err(McpError::UnknownPrompt {
                    name: qualified_name.into(),
                });
            };
            (p.raw_name.clone(), Arc::clone(&p.transport))
        };
        transport::get_prompt(transport.as_ref(), &raw_name, arguments).await
    }

    pub async fn shutdown(&self) {
        let (ack_tx, ack_rx) = flume::bounded(1);
        self.send(McpCommand::Shutdown { ack: ack_tx });
        let finished = futures_lite::future::or(
            async {
                let _ = ack_rx.recv_async().await;
                true
            },
            async {
                smol::Timer::after(MCP_SHUTDOWN_TIMEOUT).await;
                false
            },
        )
        .await;
        if !finished {
            warn!("MCP shutdown timed out after {MCP_SHUTDOWN_TIMEOUT:?}");
        }
    }

    /// Handle whose command loop is whatever the caller does with `cmd_rx`,
    /// so a test can hold a `Shutdown` unacked and observe a wedged teardown.
    #[cfg(test)]
    pub(crate) fn for_test(cmd_tx: flume::Sender<McpCommand>) -> Self {
        Self {
            cmd_tx,
            index: Arc::new(ArcSwap::from_pointee(ToolIndex::default())),
            snapshot: Arc::new(ArcSwap::from_pointee(McpSnapshot::default())),
            ready_rx: flume::bounded(0).1,
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
    let (config, config_errors) = smol::unblock(move || load_config(&cwd, project_config)).await;
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
        smol::unblock(move || load_config(&owned_cwd, project_config)).await;
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

    let inner = parse_entries(config);

    let snapshot = Arc::new(ArcSwap::from_pointee(McpSnapshot::default()));
    let index: Arc<ArcSwap<ToolIndex>> = Arc::new(ArcSwap::from_pointee(ToolIndex::default()));
    publish(&inner, &index, &snapshot);

    let (cmd_tx, cmd_rx) = flume::unbounded();
    let (ready_tx, ready_rx) = flume::bounded(0);
    let handle = McpHandle {
        cmd_tx,
        index: Arc::clone(&index),
        snapshot: Arc::clone(&snapshot),
        ready_rx,
    };

    info!(total = inner.entries.len(), "MCP servers connecting");

    smol::spawn(run(inner, index, snapshot, cmd_rx, ready_tx)).detach();
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
    cmd_rx: flume::Receiver<McpCommand>,
    ready_tx: flume::Sender<Infallible>,
) {
    let (connected_tx, connected_rx) = flume::unbounded();
    // Held, not detached: dropping a pending connect drops the transport it
    // owns, which kills the child process group it just spawned.
    let connects = spawn_connects(&inner, connected_tx);
    let mut ready = Some(ready_tx);

    let mut ack: Option<flume::Sender<()>> = None;
    loop {
        release_ready(&inner, &mut ready);
        let step = futures_lite::future::or(
            async {
                match connected_rx.recv_async().await {
                    Ok((i, result)) => Step::Connected(i, result),
                    // Every connect landed, so only commands wake us now.
                    Err(_) => futures_lite::future::pending().await,
                }
            },
            async {
                match cmd_rx.recv_async().await {
                    Ok(cmd) => Step::Command(cmd),
                    Err(_) => Step::Closed,
                }
            },
        )
        .await;

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
        let _ = tx.try_send(());
    }
}

/// Nothing left in `Connecting` means every server landed and the last publish
/// already carried its tools, so waiters can go.
fn release_ready(inner: &McpManagerInner, ready: &mut Option<flume::Sender<Infallible>>) {
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
    if let McpError::HttpError {
        status: 401,
        reason,
        ..
    } = e
    {
        McpServerStatus::NeedsAuth {
            url: Some(reason.clone()),
        }
    } else {
        McpServerStatus::Failed(e.to_string())
    }
}

struct StartResult {
    transport: Arc<dyn McpTransport>,
    tool_infos: Vec<protocol::ToolInfo>,
    prompt_infos: Vec<protocol::PromptInfo>,
}

async fn start_server(config: &ServerConfig) -> Result<StartResult, McpError> {
    let transport: Arc<dyn McpTransport> = match &config.transport {
        Transport::Stdio {
            program,
            args,
            environment,
        } => Arc::new(StdioTransport::spawn(
            &config.name,
            program,
            args,
            environment,
            config.timeout,
        )?),
        Transport::Http {
            url,
            headers,
            ca_file,
            ..
        } => Arc::new(HttpTransport::new(
            &config.name,
            url,
            headers,
            config.timeout,
            maki_storage::StateDir::resolve().ok(),
            ca_file.as_deref(),
        )?),
    };
    let capabilities = transport::initialize(transport.as_ref()).await?;
    // Asymmetric on purpose: sloppy servers omit `capabilities` yet serve
    // tools/list fine, so always ask (fatal only when tools were declared).
    // Prompts only when declared: undeclared endpoints may answer junk,
    // and junk must not take down the server's tools.
    let tool_infos = match transport::list_tools(transport.as_ref()).await {
        Ok(tools) => tools,
        Err(e) if !capabilities.tools => {
            warn!(server = config.name, error = %e, "tools/list failed; server declared no tools");
            Vec::new()
        }
        Err(e) => return Err(e),
    };
    let prompt_infos = if capabilities.prompts {
        transport::list_prompts(transport.as_ref()).await?
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
        transport,
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
    tx: flume::Sender<(usize, Result<StartResult, McpError>)>,
) -> Vec<smol::Task<()>> {
    inner
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.status == McpServerStatus::Connecting)
        .filter_map(|(i, e)| e.config.clone().map(|c| (i, c)))
        .map(|(i, config)| {
            let tx = tx.clone();
            smol::spawn(async move {
                let _ = tx.send_async((i, start_server(&config).await)).await;
            })
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
            let description = entry
                .config
                .as_ref()
                .and_then(|c| c.description.clone())
                .unwrap_or_else(|| "(no description)".to_string());
            let mut functions = Vec::with_capacity(entry.tools.len());
            for t in &entry.tools {
                tools.insert(
                    Arc::clone(&t.qualified_name),
                    ToolRef {
                        raw_name: t.raw_name.clone(),
                        transport: Arc::clone(transport),
                    },
                );
                let mut sanitized_schema = sanitize_tool_input_schema(t.input_schema.clone());
                inject_toolcall_reason(&mut sanitized_schema);
                functions.push(McpFunctionInfo {
                    name: t.raw_name.clone(),
                    description: t.description.clone(),
                    schema: sanitized_schema,
                });
                descriptors.push(ToolDescriptor {
                    wire_name: wire_tool_name(&t.qualified_name),
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
    use std::collections::HashMap;
    use std::convert::Infallible;
    use std::path::PathBuf;
    use std::sync::Arc;

    use arc_swap::ArcSwap;
    use serde_json::Value;
    use serde_json::json;

    use super::config::McpServerStatus;
    use super::error::McpError;
    use super::transport::{self, McpTransport};
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
                transport: Some(Arc::new(StubTransport(Arc::from("stub")))),
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
                    transport: Some(Arc::new(StubTransport(Arc::from(server)))),
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
                cmd_tx: flume::unbounded().0,
                index,
                snapshot,
                ready_rx: flume::bounded(0).1,
            },
            &[],
        )
    }

    pub fn pending_stub_session(tools: &[(&str, &str)]) -> (McpSession, flume::Sender<Infallible>) {
        let (ready_tx, ready_rx) = flume::bounded(0);
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
        let entries = server_map
            .into_iter()
            .map(|(server, tools)| ServerEntry {
                name: server.into(),
                config: None,
                transport_kind: "stub",
                origin: PathBuf::new(),
                status: McpServerStatus::Running,
                transport: Some(Arc::new(StubTransport(Arc::from(server)))),
                tools,
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
        (
            McpSession::new(
                McpHandle {
                    cmd_tx: flume::unbounded().0,
                    index,
                    snapshot,
                    ready_rx,
                },
                &[],
            ),
            ready_tx,
        )
    }

    /// Fails every call with `UnknownTool`, which is how a test proves the call
    /// reached MCP at all.
    struct StubTransport(Arc<str>);

    impl McpTransport for StubTransport {
        fn send_request<'a>(
            &'a self,
            method: &'a str,
            _params: Option<Value>,
        ) -> transport::BoxFuture<'a, Result<Value, McpError>> {
            Box::pin(async move {
                Err(McpError::UnknownTool {
                    name: method.into(),
                })
            })
        }
        fn send_notification<'a>(
            &'a self,
            _method: &'a str,
            _params: Option<Value>,
        ) -> transport::BoxFuture<'a, Result<(), McpError>> {
            Box::pin(async { Ok(()) })
        }
        fn shutdown<'a>(&'a self) -> transport::BoxFuture<'a, ()> {
            Box::pin(async {})
        }
        fn server_name(&self) -> &Arc<str> {
            &self.0
        }
        fn transport_kind(&self) -> &'static str {
            "stub"
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

fn spawn_persist_enabled(path: PathBuf, name: String, enabled: bool) {
    let log_name = name.clone();
    smol::spawn(async move {
        if let Err(e) = smol::unblock(move || config::persist_enabled(&path, &name, enabled)).await
        {
            warn!(error = %e, server = %log_name, "failed to persist MCP toggle");
        }
    })
    .detach();
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
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<str>>>> = OnceLock::new();
    let mut map = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
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
    use config::{RawHttpFields, RawServerConfig, RawStdioFields, RawTransport};
    use serde_json::json;
    use smol::lock::Mutex as AsyncMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[cfg(unix)]
    use std::time::Instant;
    use test_case::test_case;

    const DEFAULT_TIMEOUT_MS: u64 = 30_000;
    const MISSING_PROGRAM: &str = "/nonexistent/definitely-not-here";
    const MISSING_CA_FILE: &str = "/nonexistent/ca.pem";

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
        name: Arc<str>,
        shutdowns: AtomicUsize,
        call_entered: flume::Sender<()>,
        call_entered_rx: flume::Receiver<()>,
        call_gate: AsyncMutex<()>,
    }

    impl FakeTransport {
        fn new() -> Arc<Self> {
            let (call_entered, call_entered_rx) = flume::bounded(1);
            Arc::new(Self {
                name: Arc::from("fake"),
                shutdowns: AtomicUsize::new(0),
                call_entered,
                call_entered_rx,
                call_gate: AsyncMutex::new(()),
            })
        }

        fn shutdowns(&self) -> usize {
            self.shutdowns.load(Ordering::SeqCst)
        }
    }

    impl McpTransport for FakeTransport {
        fn send_request<'a>(
            &'a self,
            method: &'a str,
            _params: Option<Value>,
        ) -> transport::BoxFuture<'a, Result<Value, McpError>> {
            Box::pin(async move {
                if method == "tools/call" {
                    let _ = self.call_entered.try_send(());
                    let _g = self.call_gate.lock().await;
                    Ok(json!({ "content": [{ "type": "text", "text": "ok" }] }))
                } else {
                    Ok(Value::Null)
                }
            })
        }
        fn send_notification<'a>(
            &'a self,
            _method: &'a str,
            _params: Option<Value>,
        ) -> transport::BoxFuture<'a, Result<(), McpError>> {
            Box::pin(async move { Ok(()) })
        }
        fn shutdown<'a>(&'a self) -> transport::BoxFuture<'a, ()> {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }
        fn server_name(&self) -> &Arc<str> {
            &self.name
        }
        fn transport_kind(&self) -> &'static str {
            "fake"
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
        let inner = McpManagerInner {
            entries,
            generation: 0,
        };
        let index = Arc::new(ArcSwap::from_pointee(ToolIndex::default()));
        let snapshot = Arc::new(ArcSwap::from_pointee(McpSnapshot::default()));
        publish(&inner, &index, &snapshot);
        let handle = McpHandle {
            cmd_tx: flume::unbounded().0,
            index,
            snapshot,
            ready_rx: flume::bounded(0).1,
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

    #[test_case(TOOL_NAME, Some(TOOL_NAME) ; "internal_name")]
    #[test_case(WIRE_TOOL_NAME, Some(TOOL_NAME) ; "wire_name")]
    #[test_case("read", None ; "native_name")]
    #[test_case("gone__tool", None ; "unpublished_server")]
    fn resolve_maps_published_names_only(name: &str, expected: Option<&str>) {
        let (_inner, handle) = setup(vec![fake_entry("srv", FakeTransport::new())]);
        assert_eq!(handle.resolve(name).as_deref(), expected);
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

    #[test]
    fn wire_names_returns_all_published_tools() {
        let (_inner, handle) = setup(vec![fake_entry("srv", FakeTransport::new())]);
        assert_eq!(handle.wire_names(), vec![WIRE_TOOL_NAME]);
    }

    /// `ready` carries the correctness of connecting in the background: a prompt
    /// typed during startup must not ship before the servers settle.
    #[test]
    fn ready_settles_every_server_status() {
        smol::block_on(async {
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
        });
    }

    /// `sleep` spawns fine and never answers `initialize`, so its connect only
    /// ends on the request timeout, far past the shutdown one. Shutdown has to
    /// preempt it, or quitting during startup hangs.
    #[cfg(unix)]
    #[test]
    fn shutdown_preempts_an_in_flight_connect() {
        const BLOCKED: &str = "shutdown must not wait for an in-flight connect";
        smol::block_on(async {
            let config = make_config(vec![("slow-srv", stdio_raw(&["sleep", "60"]))]);
            let handle = start_with_config(config).unwrap();
            let started = Instant::now();
            handle.shutdown().await;
            assert!(started.elapsed() < MCP_SHUTDOWN_TIMEOUT, "{BLOCKED}");
        });
    }

    /// If a refresh fails, the entry must end up empty. A zombie tool left behind would be
    /// handed to the model on the next turn and then try to call into a dead transport.
    #[test]
    fn failed_refresh_clears_entry() {
        smol::block_on(async {
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
        });
    }

    #[test]
    fn disable_purges_entry_and_published_view() {
        smol::block_on(async {
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
        });
    }

    /// Regression: the lock-free refactor fixed a case where `call_tool` held the inner read
    /// lock across the transport await, so any in-flight call blocked every publish behind it.
    /// The rendezvous here stays deterministic: the call signals on `call_entered`, the test
    /// waits for that signal, then calls `publish` while the call is still parked on `call_gate`.
    #[test]
    fn slow_tool_call_does_not_block_publish() {
        smol::block_on(async {
            let t = FakeTransport::new();
            let (mut inner, handle) = setup(vec![fake_entry("srv", Arc::clone(&t) as _)]);

            let held = t.call_gate.lock().await;
            let entered = t.call_entered_rx.clone();
            let call_handle = {
                let handle = handle.clone();
                smol::spawn(async move { handle.call_tool(TOOL_NAME, &json!({})).await.unwrap() })
            };

            entered.recv_async().await.unwrap();
            inner.generation += 1;
            publish(&inner, &handle.index, &handle.snapshot);
            assert_eq!(handle.snapshot.load().generation, 1);

            drop(held);
            call_handle.await;
        });
    }

    #[test]
    fn shutdown_command_drains_and_acks() {
        smol::block_on(async {
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
            let (cmd_tx, cmd_rx) = flume::unbounded();
            let loop_task = smol::spawn(run(
                inner,
                Arc::clone(&index),
                Arc::clone(&snapshot),
                cmd_rx,
                flume::bounded(0).0,
            ));

            let (ack_tx, ack_rx) = flume::bounded(1);
            cmd_tx.send(McpCommand::Shutdown { ack: ack_tx }).unwrap();
            ack_rx.recv_async().await.unwrap();
            loop_task.await;

            assert_eq!(t1.shutdowns(), 1);
            assert_eq!(t2.shutdowns(), 1);
            assert!(snapshot.load().infos.iter().all(|i| i.tool_count == 0));
        });
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
