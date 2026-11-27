//! Runs JavaScript in a sandboxed QuickJS-NG (`rquickjs`) interpreter.
//! Supports top-level `await` with last-expression completion values,
//! automatic IIFE fallback for top-level `return`, concurrent async tool calls,
//! native memory/stack limits, and script-only execution time budgeting.

use std::env::{current_dir, var};
use std::fs::{self, Metadata};
use std::future::Future;
use std::path::{Component, Path, PathBuf, absolute};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use futures::channel::oneshot as futures_oneshot;
use futures::executor::block_on;
use rquickjs::context::EvalOptions;
use rquickjs::function::{Async, Opt};
use rquickjs::{
    Array, AsyncContext, AsyncRuntime, CaughtError, Ctx, Error as JsError, Exception, Function,
    Object, Promise, String as JsString, Type as JsType, Value as JsValue,
};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value};

use crate::error::InterpreterError;
use crate::runner::{
    DirCreate, ENV_ALLOWLIST, FileOp, FileWrite, HOST_TIME_EXCEEDED, InterpreterResult,
    OS_CALLS_DENIED, PendingCall, REMOVAL_UNSUPPORTED,
};

const JS_SCRIPT_NAME: &str = "agent.js";
const PREAMBLE_NAME: &str = "preamble.js";
const BOOTSTRAP_NAME: &str = "bootstrap.js";
const MAX_STACK_SIZE: usize = 2 * 1024 * 1024;
const MAX_CONVERT_DEPTH: usize = 64;
const MAX_SCAN_ENTRIES: usize = 100_000;
const MAX_SCAN_BYTES: usize = 64 * 1024 * 1024;
const SLEEP_POLL_SLICE: Duration = Duration::from_millis(50);

const TIME_LIMIT_EXCEEDED: &str = "time limit exceeded";
const MEMORY_LIMIT_EXCEEDED: &str = "memory limit exceeded (out of memory)";
const CANCELLED_ERR: &str = "cancelled";
const RETURN_OUTSIDE_FN: &str = "return not in a function";
const OBJECT_ARGS_HINT: &str =
    "pass arguments as an object (e.g. read({ path: '/file', offset: 1, limit: 100 }))";
const TOO_DEEP_ERR: &str = "value nesting exceeds maximum depth";
const SCAN_LIMIT_ERR: &str = "directory listing exceeded sandbox limit";
const NULL_BYTE_ERR_PREFIX: &str = "source contains raw NUL byte (0x00)";
const NULL_BYTE_HINT: &str = "likely from an unescaped \\u0000 in JSON; escape backticks as \\` inside template literals, use normal quotes/concatenation, or write \\\\u0000 / \\\\x00 inside a JS string literal";

const JS_BOOTSTRAP: &str = r#"
(function () {
  const ERROR_PREFIX = "[ERROR] ";
  const SEP = __maki_path_sep;

  function safeFormat(val) {
    if (typeof val === "string") return val;
    if (val === null) return "null";
    if (val === undefined) return "undefined";
    if (val instanceof Error) return val.stack || (val.name + ": " + val.message);
    if (typeof val === "object") {
      const seen = new WeakSet();
      try {
        return JSON.stringify(val, function (_k, v) {
          if (typeof v === "bigint") return v.toString();
          if (v !== null && typeof v === "object") {
            if (seen.has(v)) return "[Circular]";
            seen.add(v);
          }
          return v;
        });
      } catch (_e) {
        return String(val);
      }
    }
    return String(val);
  }

  function writeLine() {
    const parts = [];
    for (let i = 0; i < arguments.length; i++) {
      parts.push(safeFormat(arguments[i]));
    }
    __maki_stdout(parts.join(" "));
  }

  globalThis.console = {
    log: writeLine,
    info: writeLine,
    warn: writeLine,
    error: writeLine,
  };
  globalThis.print = writeLine;

  globalThis.gather = async function gather(...calls) {
    if (calls.length === 1 && Array.isArray(calls[0])) {
      calls = calls[0];
    }
    const settled = await Promise.allSettled(calls);
    return settled.map((entry) => {
      if (entry.status === "fulfilled") return entry.value;
      const reason = entry.reason;
      const msg =
        reason && typeof reason === "object" && "message" in reason
          ? String(reason.message)
          : String(reason);
      return ERROR_PREFIX + msg;
    });
  };

  function isAbsPath(p) {
    if (!p) return false;
    if (p.startsWith("/")) return true;
    return /^[A-Za-z]:[\\/]/.test(p);
  }

  function splitPath(p) {
    return String(p).replace(/\\/g, "/").split("/");
  }

  function normalizePath(p) {
    const raw = String(p).replace(/\\/g, "/");
    if (raw === "") return ".";
    const driveMatch = raw.match(/^([A-Za-z]:)(\/.*)?$/);
    const prefix = driveMatch ? driveMatch[1] + (driveMatch[2] ? "/" : "") : raw.startsWith("/") ? "/" : "";
    const body = driveMatch ? (driveMatch[2] || "") : raw;
    const isAbs = prefix.endsWith("/");
    const parts = body.split("/");
    const stack = [];
    for (const part of parts) {
      if (!part || part === ".") continue;
      if (part === "..") {
        if (stack.length > 0 && stack[stack.length - 1] !== "..") {
          stack.pop();
        } else if (!isAbs) {
          stack.push("..");
        }
      } else {
        stack.push(part);
      }
    }
    const joined = stack.join("/");
    if (prefix) return prefix + joined;
    return joined || ".";
  }

  function joinPaths(...parts) {
    const filtered = parts.filter((p) => p !== undefined && p !== null && String(p).length > 0).map(String);
    if (filtered.length === 0) return ".";
    return normalizePath(filtered.join("/"));
  }

  function resolvePaths(...parts) {
    let acc = "";
    for (let i = parts.length - 1; i >= 0; i--) {
      const seg = String(parts[i] ?? "");
      if (!seg) continue;
      acc = acc ? seg + "/" + acc : seg;
      if (isAbsPath(seg)) break;
    }
    if (!isAbsPath(acc)) {
      acc = acc ? __maki_cwd + "/" + acc : __maki_cwd;
    }
    return normalizePath(acc);
  }

  function dirnamePath(p) {
    const norm = normalizePath(p);
    if (norm === "/" || /^[A-Za-z]:\/$/.test(norm)) return norm;
    const trimmed = norm.endsWith("/") ? norm.slice(0, -1) : norm;
    const idx = trimmed.lastIndexOf("/");
    if (idx === -1) return ".";
    if (idx === 0) return "/";
    return trimmed.slice(0, idx);
  }

  function basenamePath(p, ext) {
    const norm = normalizePath(p);
    const trimmed = norm.length > 1 && norm.endsWith("/") ? norm.slice(0, -1) : norm;
    const idx = trimmed.lastIndexOf("/");
    let base = idx === -1 ? trimmed : trimmed.slice(idx + 1);
    if (ext && base.endsWith(ext)) {
      base = base.slice(0, base.length - ext.length);
    }
    return base;
  }

  function extnamePath(p) {
    const base = basenamePath(p);
    const idx = base.lastIndexOf(".");
    if (idx <= 0) return "";
    return base.slice(idx);
  }

  function relativePath(from, to) {
    const fromParts = resolvePaths(from).split("/").filter(Boolean);
    const toParts = resolvePaths(to).split("/").filter(Boolean);
    let common = 0;
    while (common < fromParts.length && common < toParts.length && fromParts[common] === toParts[common]) {
      common++;
    }
    const up = new Array(fromParts.length - common).fill("..");
    return [...up, ...toParts.slice(common)].join("/") || ".";
  }

  globalThis.path = {
    sep: SEP,
    join: joinPaths,
    normalize: normalizePath,
    resolve: resolvePaths,
    dirname: dirnamePath,
    basename: basenamePath,
    extname: extnamePath,
    isAbsolute: isAbsPath,
    relative: relativePath,
  };

  function checkUtf8Encoding(opts) {
    if (opts === undefined || opts === null) return;
    const enc = typeof opts === "string" ? opts : opts.encoding;
    if (enc === undefined || enc === null) return;
    const norm = String(enc).toLowerCase().replace(/[-_]/g, "");
    if (norm !== "utf8" && norm !== "cp65001") {
      throw new Error(
        "JavaScript sandbox only supports UTF-8 encoding. For other encodings (" +
          String(enc) +
          "), execute a script via the shell tool on the host system."
      );
    }
  }

  function wrapStat(raw) {
    return {
      size: raw.size,
      mtimeMs: raw.mtimeMs,
      mtime: new Date(raw.mtimeMs),
      isFile: () => raw.isFile,
      isDirectory: () => raw.isDirectory,
      isSymbolicLink: () => raw.isSymbolicLink,
    };
  }

  globalThis.fs = {
    readFileSync(p, opts) {
      checkUtf8Encoding(opts);
      return __maki_fs_read(String(p));
    },
    writeFileSync(p, data, opts) {
      checkUtf8Encoding(opts);
      __maki_fs_write(String(p), String(data), false);
    },
    appendFileSync(p, data, opts) {
      checkUtf8Encoding(opts);
      __maki_fs_write(String(p), String(data), true);
    },
    existsSync(p) {
      return __maki_fs_exists(String(p));
    },
    statSync(p) {
      return wrapStat(__maki_fs_stat(String(p)));
    },
    readdirSync(p, opts) {
      const entries = __maki_fs_readdir(String(p));
      if (opts && opts.withFileTypes) {
        return entries.map((e) => ({
          name: e.name,
          isFile: () => e.isFile,
          isDirectory: () => e.isDirectory,
          isSymbolicLink: () => e.isSymbolicLink,
        }));
      }
      return entries.map((e) => e.name);
    },
    mkdirSync(p, opts) {
      const recursive = Boolean(opts && opts.recursive);
      __maki_fs_mkdir(String(p), recursive);
    },
    unlinkSync() {
      throw new Error(__maki_removal_err);
    },
    rmSync() {
      throw new Error(__maki_removal_err);
    },
    rmdirSync() {
      throw new Error(__maki_removal_err);
    },
    renameSync() {
      throw new Error(__maki_removal_err);
    },
  };

  globalThis.process = {
    env: globalThis.env,
    cwd: () => __maki_cwd,
  };

  globalThis.require = function require(name) {
    const mod = String(name);
    if (mod === "fs" || mod === "node:fs") return globalThis.fs;
    if (mod === "path" || mod === "node:path") return globalThis.path;
    if (mod === "process" || mod === "node:process") return globalThis.process;
    if (mod in globalThis && typeof globalThis[mod] === "object" && globalThis[mod] !== null) {
      throw new Error(
        "'" + mod + "' is already pre-bound as a global object; use it directly without require()."
      );
    }
    throw new Error(
      "Module '" +
        mod +
        "' is not available in this JavaScript sandbox (built-ins: fs, path, process, env, gather, sleep). For external packages or Node.js modules, run a script via the shell tool on the host system."
    );
  };
})();
"#;

pub struct JsLimits {
    pub timeout: Duration,
    pub max_memory: usize,
}

pub type JsToolFuture = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;
pub type JsToolFn = Arc<dyn Fn(PendingCall) -> JsToolFuture + Send + Sync>;
pub type JsFileFn = Arc<dyn Fn(FileOp) -> Result<String, String> + Send + Sync>;
pub type JsOutputFn = Arc<dyn Fn(&str) + Send + Sync>;

pub struct JsRunConfig {
    pub preamble: String,
    pub tool_names: Vec<String>,
    pub call_tool: JsToolFn,
    pub files: Option<JsFileFn>,
    pub limits: JsLimits,
    pub cancelled: Arc<AtomicBool>,
    pub on_output: JsOutputFn,
}

struct ScriptClock {
    origin: Instant,
    timeout_ns: u64,
    accumulated_ns: AtomicU64,
    poll_start_ns: AtomicU64,
    timed_out: AtomicBool,
    cancelled: Arc<AtomicBool>,
}

impl ScriptClock {
    fn new(timeout: Duration, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            origin: Instant::now(),
            timeout_ns: timeout.as_nanos().min(u64::MAX as u128) as u64,
            accumulated_ns: AtomicU64::new(0),
            poll_start_ns: AtomicU64::new(0),
            timed_out: AtomicBool::new(false),
            cancelled,
        }
    }

    fn now_ns(&self) -> u64 {
        self.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64
    }

    fn begin_poll(&self) -> u64 {
        let now = self.now_ns();
        self.poll_start_ns
            .store(now.saturating_add(1), Ordering::Relaxed);
        now
    }

    fn end_poll(&self, started_ns: u64) {
        self.poll_start_ns.store(0, Ordering::Relaxed);
        let delta = self.now_ns().saturating_sub(started_ns);
        self.accumulated_ns.fetch_add(delta, Ordering::Relaxed);
    }

    fn charge_host_time(&self, duration: Duration) -> bool {
        let add = duration.as_nanos().min(u64::MAX as u128) as u64;
        let prev = self.accumulated_ns.fetch_add(add, Ordering::Relaxed);
        if prev.saturating_add(add) >= self.timeout_ns {
            self.timed_out.store(true, Ordering::Relaxed);
            return false;
        }
        true
    }

    fn is_interrupted(&self) -> bool {
        if self.cancelled.load(Ordering::Relaxed) {
            return true;
        }
        let acc = self.accumulated_ns.load(Ordering::Relaxed);
        let active = match self.poll_start_ns.load(Ordering::Relaxed) {
            0 => 0,
            encoded => self.now_ns().saturating_sub(encoded - 1),
        };
        if acc.saturating_add(active) >= self.timeout_ns {
            self.timed_out.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }

    fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::Relaxed)
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

struct PollGuard<'a> {
    clock: &'a ScriptClock,
    started_ns: u64,
}

impl Drop for PollGuard<'_> {
    fn drop(&mut self) {
        self.clock.end_poll(self.started_ns);
    }
}

struct TimedFuture<F> {
    inner: Pin<Box<F>>,
    clock: Arc<ScriptClock>,
}

impl<F: Future> Future for TimedFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let started_ns = this.clock.begin_poll();
        let _guard = PollGuard {
            clock: &this.clock,
            started_ns,
        };
        this.inner.as_mut().poll(cx)
    }
}

fn check_no_nul_bytes(src: &str, filename: &str) -> Result<(), InterpreterError> {
    let Some(pos) = src.as_bytes().iter().position(|&b| b == 0) else {
        return Ok(());
    };
    let prefix = &src[..pos];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |i| i + 1);
    let col = prefix[line_start..].chars().count() + 1;
    Err(InterpreterError::Parse(format!(
        "{NULL_BYTE_ERR_PREFIX} at {filename}:{line}:{col} ({NULL_BYTE_HINT})"
    )))
}

pub fn run(code: &str, config: JsRunConfig) -> Result<InterpreterResult, InterpreterError> {
    check_no_nul_bytes(code, JS_SCRIPT_NAME)?;
    if !config.preamble.trim().is_empty() {
        check_no_nul_bytes(&config.preamble, PREAMBLE_NAME)?;
    }

    let cwd = current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .map_err(|e| InterpreterError::Runtime(format!("cannot resolve cwd: {e}")))?;

    let clock = Arc::new(ScriptClock::new(
        config.limits.timeout,
        Arc::clone(&config.cancelled),
    ));
    let stdout_buf = Arc::new(Mutex::new(String::new()));

    let fut = run_async(
        code.to_owned(),
        config,
        cwd,
        Arc::clone(&clock),
        Arc::clone(&stdout_buf),
    );

    let timed = TimedFuture {
        inner: Box::pin(fut),
        clock: Arc::clone(&clock),
    };

    let output = block_on(timed)?;
    let stdout = stdout_buf.lock().map(|s| s.clone()).unwrap_or_default();
    Ok(InterpreterResult { output, stdout })
}

async fn run_async(
    code: String,
    config: JsRunConfig,
    cwd: String,
    clock: Arc<ScriptClock>,
    stdout_buf: Arc<Mutex<String>>,
) -> Result<Option<Value>, InterpreterError> {
    let runtime = AsyncRuntime::new()
        .map_err(|e| InterpreterError::Runtime(format!("failed to initialize JS runtime: {e}")))?;
    runtime.set_memory_limit(config.limits.max_memory).await;
    runtime.set_max_stack_size(MAX_STACK_SIZE).await;
    {
        let clock = Arc::clone(&clock);
        runtime
            .set_interrupt_handler(Some(Box::new(move || clock.is_interrupted())))
            .await;
    }

    let context = AsyncContext::full(&runtime)
        .await
        .map_err(|e| InterpreterError::Runtime(format!("failed to initialize JS context: {e}")))?;

    let JsRunConfig {
        preamble,
        tool_names,
        call_tool,
        files,
        on_output,
        ..
    } = config;

    let result = context
        .async_with(async |ctx| {
            install_builtins(
                &ctx,
                &tool_names,
                call_tool,
                files,
                &cwd,
                Arc::clone(&clock),
                stdout_buf,
                on_output,
            )?;

            eval_script_unit(&ctx, JS_BOOTSTRAP, BOOTSTRAP_NAME, &clock)?;

            if !preamble.trim().is_empty() {
                eval_script_unit(&ctx, &preamble, PREAMBLE_NAME, &clock)?;
            }

            eval_user_code(&ctx, &code, &clock).await
        })
        .await;

    runtime.idle().await;
    result
}

#[allow(clippy::too_many_arguments)]
fn install_builtins<'js>(
    ctx: &Ctx<'js>,
    tool_names: &[String],
    call_tool: JsToolFn,
    files: Option<JsFileFn>,
    cwd: &str,
    clock: Arc<ScriptClock>,
    stdout_buf: Arc<Mutex<String>>,
    on_output: JsOutputFn,
) -> Result<(), InterpreterError> {
    let globals = ctx.globals();

    let sep = if cfg!(windows) { "\\" } else { "/" };
    globals
        .set("__maki_path_sep", sep)
        .and_then(|()| globals.set("__maki_cwd", cwd))
        .and_then(|()| globals.set("__maki_removal_err", REMOVAL_UNSUPPORTED))
        .map_err(|e| map_js_error(ctx, e, &clock))?;

    let env_obj = Object::new(ctx.clone()).map_err(|e| map_js_error(ctx, e, &clock))?;
    for &key in ENV_ALLOWLIST {
        if let Ok(val) = var(key) {
            env_obj
                .set(key, val)
                .map_err(|e| map_js_error(ctx, e, &clock))?;
        }
    }
    globals
        .set("env", env_obj)
        .map_err(|e| map_js_error(ctx, e, &clock))?;

    let stdout_fn = Function::new(ctx.clone(), move |line: String| {
        let mut with_nl = String::with_capacity(line.len() + 1);
        with_nl.push_str(&line);
        with_nl.push('\n');
        if let Ok(mut buf) = stdout_buf.lock() {
            buf.push_str(&with_nl);
        }
        on_output(&with_nl);
    })
    .map_err(|e| map_js_error(ctx, e, &clock))?;
    globals
        .set("__maki_stdout", stdout_fn)
        .map_err(|e| map_js_error(ctx, e, &clock))?;

    let sleep_clock = Arc::clone(&clock);
    let sleep_fn = Function::new(
        ctx.clone(),
        Async(move |ctx: Ctx<'js>, ms: f64| {
            let sleep_clock = Arc::clone(&sleep_clock);
            async move {
                if !ms.is_finite() || ms < 0.0 {
                    return Err(Exception::throw_message(
                        &ctx,
                        "sleep(ms) requires a non-negative finite number",
                    ));
                }
                let duration = Duration::from_secs_f64(ms / 1000.0);
                if !sleep_clock.charge_host_time(duration) {
                    return Err(Exception::throw_message(&ctx, HOST_TIME_EXCEEDED));
                }
                let (tx, rx) = futures_oneshot::channel::<()>();
                let cancel = Arc::clone(&sleep_clock.cancelled);
                thread::spawn(move || {
                    let deadline = Instant::now() + duration;
                    while Instant::now() < deadline {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let rem = deadline.saturating_duration_since(Instant::now());
                        thread::sleep(rem.min(SLEEP_POLL_SLICE));
                    }
                    let _ = tx.send(());
                });
                let _ = rx.await;
                if sleep_clock.is_cancelled() {
                    return Err(Exception::throw_message(&ctx, CANCELLED_ERR));
                }
                Ok(())
            }
        }),
    )
    .map_err(|e| map_js_error(ctx, e, &clock))?;
    globals
        .set("sleep", sleep_fn)
        .map_err(|e| map_js_error(ctx, e, &clock))?;

    install_fs_helpers(ctx, &globals, files, cwd, &clock)?;

    for name in tool_names {
        let tool_name = name.clone();
        let caller = Arc::clone(&call_tool);
        let func = Function::new(
            ctx.clone(),
            Async(move |ctx: Ctx<'js>, arg: Opt<JsValue<'js>>| {
                let tool_name = tool_name.clone();
                let caller = Arc::clone(&caller);
                let parsed = parse_tool_arg(arg.0);
                async move {
                    let args = match parsed {
                        Ok(args) => args,
                        Err(msg) => return Err(Exception::throw_message(&ctx, &msg)),
                    };
                    let pc = PendingCall {
                        call_id: 0,
                        name: tool_name,
                        args,
                        kwargs: Vec::new(),
                    };
                    match caller(pc).await {
                        Ok(val) => json_to_js(&ctx, &val),
                        Err(msg) => Err(Exception::throw_message(&ctx, &msg)),
                    }
                }
            }),
        )
        .map_err(|e| map_js_error(ctx, e, &clock))?;
        globals
            .set(name.as_str(), func)
            .map_err(|e| map_js_error(ctx, e, &clock))?;
    }

    Ok(())
}

fn install_fs_helpers<'js>(
    ctx: &Ctx<'js>,
    globals: &Object<'js>,
    files: Option<JsFileFn>,
    cwd: &str,
    clock: &Arc<ScriptClock>,
) -> Result<(), InterpreterError> {
    let cwd_arc: Arc<str> = Arc::from(cwd);

    let files_read = files.clone();
    let fs_read = Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String| {
        let Some(ref f) = files_read else {
            return Err(Exception::throw_message(&ctx, OS_CALLS_DENIED));
        };
        f(FileOp::Read(path)).map_err(|e| Exception::throw_message(&ctx, &e))
    })
    .map_err(|e| map_js_error(ctx, e, clock))?;

    let files_write = files.clone();
    let fs_write = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, path: String, content: String, append: bool| {
            let Some(ref f) = files_write else {
                return Err(Exception::throw_message(&ctx, OS_CALLS_DENIED));
            };
            f(FileOp::Write(FileWrite {
                path,
                content,
                append,
            }))
            .map(|_| ())
            .map_err(|e| Exception::throw_message(&ctx, &e))
        },
    )
    .map_err(|e| map_js_error(ctx, e, clock))?;

    let files_exists = files.clone();
    let cwd_exists = Arc::clone(&cwd_arc);
    let fs_exists = Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String| {
        let Some(ref f) = files_exists else {
            return Err(Exception::throw_message(&ctx, OS_CALLS_DENIED));
        };
        let abs = resolve_abs_path(&cwd_exists, &path);
        f(FileOp::Inspect(abs.clone())).map_err(|e| Exception::throw_message(&ctx, &e))?;
        Ok(Path::new(&abs).exists())
    })
    .map_err(|e| map_js_error(ctx, e, clock))?;

    let files_stat = files.clone();
    let cwd_stat = Arc::clone(&cwd_arc);
    let fs_stat = Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String| {
        let Some(ref f) = files_stat else {
            return Err(Exception::throw_message(&ctx, OS_CALLS_DENIED));
        };
        let abs = resolve_abs_path(&cwd_stat, &path);
        f(FileOp::Inspect(abs.clone())).map_err(|e| Exception::throw_message(&ctx, &e))?;
        let meta = fs::metadata(&abs)
            .map_err(|e| Exception::throw_message(&ctx, &format!("{path}: {e}")))?;
        stat_to_js(&ctx, &meta)
    })
    .map_err(|e| map_js_error(ctx, e, clock))?;

    let files_readdir = files.clone();
    let cwd_readdir = Arc::clone(&cwd_arc);
    let fs_readdir = Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String| {
        let Some(ref f) = files_readdir else {
            return Err(Exception::throw_message(&ctx, OS_CALLS_DENIED));
        };
        let abs = resolve_abs_path(&cwd_readdir, &path);
        f(FileOp::Inspect(abs.clone())).map_err(|e| Exception::throw_message(&ctx, &e))?;
        let mut entries: Vec<(String, bool, bool, bool)> = Vec::new();
        let mut total_bytes = 0usize;
        let rd = fs::read_dir(&abs)
            .map_err(|e| Exception::throw_message(&ctx, &format!("{path}: {e}")))?;
        for entry in rd {
            let entry =
                entry.map_err(|e| Exception::throw_message(&ctx, &format!("{path}: {e}")))?;
            if entries.len() >= MAX_SCAN_ENTRIES {
                return Err(Exception::throw_message(&ctx, SCAN_LIMIT_ERR));
            }
            if let Ok(name) = entry.file_name().into_string() {
                total_bytes = total_bytes.saturating_add(name.len());
                if total_bytes > MAX_SCAN_BYTES {
                    return Err(Exception::throw_message(&ctx, SCAN_LIMIT_ERR));
                }
                let ft = entry.file_type().ok();
                let is_file = ft.as_ref().is_some_and(|t| t.is_file());
                let is_dir = ft.as_ref().is_some_and(|t| t.is_dir());
                let is_symlink = ft.as_ref().is_some_and(|t| t.is_symlink());
                entries.push((name, is_file, is_dir, is_symlink));
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let arr = Array::new(ctx.clone())?;
        for (idx, (name, is_file, is_dir, is_symlink)) in entries.into_iter().enumerate() {
            let obj = Object::new(ctx.clone())?;
            obj.set("name", name)?;
            obj.set("isFile", is_file)?;
            obj.set("isDirectory", is_dir)?;
            obj.set("isSymbolicLink", is_symlink)?;
            arr.set(idx, obj)?;
        }
        Ok(arr)
    })
    .map_err(|e| map_js_error(ctx, e, clock))?;

    let files_mkdir = files;
    let cwd_mkdir = Arc::clone(&cwd_arc);
    let fs_mkdir = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, path: String, recursive: bool| {
            let Some(ref f) = files_mkdir else {
                return Err(Exception::throw_message(&ctx, OS_CALLS_DENIED));
            };
            let abs = resolve_abs_path(&cwd_mkdir, &path);
            f(FileOp::Inspect(abs.clone())).map_err(|e| Exception::throw_message(&ctx, &e))?;
            if recursive && Path::new(&abs).is_dir() {
                return Ok(());
            }
            f(FileOp::Mkdir(DirCreate {
                path: abs,
                parents: recursive,
            }))
            .map(|_| ())
            .map_err(|e| Exception::throw_message(&ctx, &e))
        },
    )
    .map_err(|e| map_js_error(ctx, e, clock))?;

    globals
        .set("__maki_fs_read", fs_read)
        .and_then(|()| globals.set("__maki_fs_write", fs_write))
        .and_then(|()| globals.set("__maki_fs_exists", fs_exists))
        .and_then(|()| globals.set("__maki_fs_stat", fs_stat))
        .and_then(|()| globals.set("__maki_fs_readdir", fs_readdir))
        .and_then(|()| globals.set("__maki_fs_mkdir", fs_mkdir))
        .map_err(|e| map_js_error(ctx, e, clock))?;

    Ok(())
}

fn eval_script_unit<'js>(
    ctx: &Ctx<'js>,
    src: &str,
    filename: &str,
    clock: &ScriptClock,
) -> Result<(), InterpreterError> {
    let mut opts = EvalOptions::default();
    opts.global = true;
    opts.strict = false;
    opts.backtrace_barrier = true;
    opts.filename = Some(filename.to_owned());
    ctx.eval_with_options::<(), _>(src, opts)
        .map_err(|e| map_js_error(ctx, e, clock))
}

async fn eval_user_code<'js>(
    ctx: &Ctx<'js>,
    code: &str,
    clock: &ScriptClock,
) -> Result<Option<Value>, InterpreterError> {
    let mut opts = EvalOptions::default();
    opts.global = true;
    opts.strict = false;
    opts.backtrace_barrier = true;
    opts.promise = true;
    opts.filename = Some(JS_SCRIPT_NAME.to_owned());

    let (promise, unwrap_envelope) = match ctx.eval_with_options::<Promise<'js>, _>(code, opts) {
        Ok(promise) => (promise, true),
        Err(err) => {
            let formatted = format_caught_error(ctx, err);
            if let Some(interrupted) = check_interrupt_error(clock) {
                return Err(interrupted);
            }
            if !formatted.contains(RETURN_OUTSIDE_FN) {
                return Err(classify_error(formatted));
            }
            // Compile failed before executing anything, so wrapping in an async IIFE has zero side effects.
            let wrapped = format!("(async () => {{\n{code}\n}})()");
            let mut fallback_opts = EvalOptions::default();
            fallback_opts.global = true;
            fallback_opts.strict = false;
            fallback_opts.backtrace_barrier = true;
            fallback_opts.filename = Some(JS_SCRIPT_NAME.to_owned());
            let promise = ctx
                .eval_with_options::<Promise<'js>, _>(wrapped, fallback_opts)
                .map_err(|e| map_js_error(ctx, e, clock))?;
            (promise, false)
        }
    };

    let resolved: JsValue<'js> = promise
        .into_future()
        .await
        .map_err(|e| map_js_error(ctx, e, clock))?;

    let mut value = if unwrap_envelope {
        if let Some(obj) = resolved.as_object() {
            obj.get::<_, JsValue<'js>>("value")
                .map_err(|e| map_js_error(ctx, e, clock))?
        } else {
            resolved
        }
    } else {
        resolved
    };

    // If the script's last expression was itself a Promise (e.g. an async IIFE), await it.
    if let Some(inner_promise) = value.as_promise().cloned() {
        value = inner_promise
            .into_future()
            .await
            .map_err(|e| map_js_error(ctx, e, clock))?;
    }

    if let Some(interrupted) = check_interrupt_error(clock) {
        return Err(interrupted);
    }

    if value.is_undefined() {
        return Ok(None);
    }

    let json = js_to_json(&value, 0).map_err(InterpreterError::Runtime)?;
    Ok(Some(json))
}

fn check_interrupt_error(clock: &ScriptClock) -> Option<InterpreterError> {
    if clock.is_cancelled() {
        return Some(InterpreterError::Runtime(CANCELLED_ERR.to_owned()));
    }
    if clock.timed_out() {
        return Some(InterpreterError::Runtime(TIME_LIMIT_EXCEEDED.to_owned()));
    }
    None
}

fn map_js_error<'js>(ctx: &Ctx<'js>, err: JsError, clock: &ScriptClock) -> InterpreterError {
    let msg = format_caught_error(ctx, err);
    if let Some(interrupted) = check_interrupt_error(clock) {
        return interrupted;
    }
    classify_error(msg)
}

fn classify_error(msg: String) -> InterpreterError {
    if msg.starts_with("SyntaxError:") {
        InterpreterError::Parse(msg)
    } else {
        InterpreterError::Runtime(msg)
    }
}

fn format_caught_error<'js>(ctx: &Ctx<'js>, err: JsError) -> String {
    match CaughtError::from_error(ctx, err) {
        CaughtError::Exception(ex) => {
            let name = ex
                .as_object()
                .get::<_, String>("name")
                .unwrap_or_else(|_| "Error".to_owned());
            let message = ex.message().unwrap_or_default();
            let header = if message.is_empty() {
                name
            } else {
                format!("{name}: {message}")
            };
            match ex.stack() {
                Some(stack) if !stack.trim().is_empty() => {
                    format!("{header}\n{}", stack.trim_end())
                }
                _ => header,
            }
        }
        CaughtError::Value(val) => {
            if val.is_null() {
                return MEMORY_LIMIT_EXCEEDED.to_owned();
            }
            if let Some(s) = val.as_string().and_then(|s| s.to_string().ok()) {
                return s;
            }
            js_to_json(&val, 0)
                .map(|j| j.to_string())
                .unwrap_or_else(|_| format!("{val:?}"))
        }
        CaughtError::Error(e) => e.to_string(),
    }
}

fn parse_tool_arg(arg: Option<JsValue<'_>>) -> Result<Vec<Value>, String> {
    let Some(val) = arg else {
        return Ok(Vec::new());
    };
    if val.is_undefined() || val.is_null() {
        return Ok(Vec::new());
    }
    if val.type_of() != JsType::Object || val.is_array() {
        return Err(OBJECT_ARGS_HINT.to_owned());
    }
    let json = js_to_json(&val, 0)?;
    Ok(vec![json])
}

fn stat_to_js<'js>(ctx: &Ctx<'js>, meta: &Metadata) -> rquickjs::Result<Object<'js>> {
    let obj = Object::new(ctx.clone())?;
    obj.set("size", meta.len() as f64)?;
    obj.set("isFile", meta.is_file())?;
    obj.set("isDirectory", meta.is_dir())?;
    obj.set("isSymbolicLink", meta.file_type().is_symlink())?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    obj.set("mtimeMs", mtime_ms)?;
    Ok(obj)
}

fn resolve_abs_path(cwd: &str, raw: &str) -> String {
    let path = Path::new(raw);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(cwd).join(path)
    };
    let abs = absolute(&joined).unwrap_or(joined);
    let mut clean = PathBuf::new();
    for component in abs.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other),
        }
    }
    clean.to_string_lossy().into_owned()
}

fn json_to_js<'js>(ctx: &Ctx<'js>, val: &Value) -> rquickjs::Result<JsValue<'js>> {
    match val {
        Value::Null => Ok(JsValue::new_null(ctx.clone())),
        Value::Bool(b) => Ok(JsValue::new_bool(ctx.clone(), *b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64()
                && let Ok(i32_val) = i32::try_from(i)
            {
                return Ok(JsValue::new_int(ctx.clone(), i32_val));
            }
            Ok(JsValue::new_float(ctx.clone(), n.as_f64().unwrap_or(0.0)))
        }
        Value::String(s) => Ok(JsString::from_str(ctx.clone(), s)?.into_value()),
        Value::Array(items) => {
            let arr = Array::new(ctx.clone())?;
            for (idx, item) in items.iter().enumerate() {
                arr.set(idx, json_to_js(ctx, item)?)?;
            }
            Ok(arr.into_value())
        }
        Value::Object(map) => {
            let obj = Object::new(ctx.clone())?;
            for (k, v) in map {
                obj.set(k.as_str(), json_to_js(ctx, v)?)?;
            }
            Ok(obj.into_value())
        }
    }
}

fn js_to_json(val: &JsValue<'_>, depth: usize) -> Result<Value, String> {
    if depth > MAX_CONVERT_DEPTH {
        return Err(TOO_DEEP_ERR.to_owned());
    }
    match val.type_of() {
        JsType::Uninitialized | JsType::Undefined | JsType::Null => Ok(Value::Null),
        JsType::Bool => Ok(Value::Bool(val.as_bool().unwrap_or(false))),
        JsType::Int => Ok(Value::Number(val.as_int().unwrap_or(0).into())),
        JsType::Float => {
            let f = val.as_float().unwrap_or(0.0);
            Ok(JsonNumber::from_f64(f).map_or(Value::Null, Value::Number))
        }
        JsType::BigInt => {
            let s = val
                .as_big_int()
                .and_then(|b| b.clone().to_i64().ok())
                .map(|i| Value::Number(i.into()))
                .unwrap_or(Value::Null);
            Ok(s)
        }
        JsType::String => {
            let s = val
                .as_string()
                .ok_or_else(|| "expected string".to_owned())?
                .to_string()
                .map_err(|e| e.to_string())?;
            Ok(Value::String(s))
        }
        JsType::Array => {
            let arr = val.as_array().ok_or_else(|| "expected array".to_owned())?;
            let mut out = Vec::with_capacity(arr.len());
            for item in arr.iter::<JsValue<'_>>() {
                let item = item.map_err(|e| e.to_string())?;
                out.push(js_to_json(&item, depth + 1)?);
            }
            Ok(Value::Array(out))
        }
        JsType::Object => {
            let obj = val
                .as_object()
                .ok_or_else(|| "expected object".to_owned())?;
            let mut map = JsonMap::new();
            for prop in obj.props::<String, JsValue<'_>>() {
                let (k, v) = prop.map_err(|e| e.to_string())?;
                if v.is_undefined() || v.is_function() || v.is_symbol() {
                    continue;
                }
                map.insert(k, js_to_json(&v, depth + 1)?);
            }
            Ok(Value::Object(map))
        }
        _ => Ok(Value::String(format!("{val:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use serde_json::json;
    use tempfile::TempDir;
    use test_case::test_case;

    use super::*;

    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
    const DEFAULT_MEM: usize = 32 * 1024 * 1024;

    fn noop_tools() -> JsToolFn {
        Arc::new(|pc| Box::pin(async move { Err(format!("unexpected tool call: {}", pc.name)) }))
    }

    fn noop_output() -> JsOutputFn {
        Arc::new(|_| {})
    }

    fn default_config() -> JsRunConfig {
        JsRunConfig {
            preamble: String::new(),
            tool_names: Vec::new(),
            call_tool: noop_tools(),
            files: None,
            limits: JsLimits {
                timeout: DEFAULT_TIMEOUT,
                max_memory: DEFAULT_MEM,
            },
            cancelled: Arc::new(AtomicBool::new(false)),
            on_output: noop_output(),
        }
    }

    fn run_simple(code: &str) -> Result<InterpreterResult, InterpreterError> {
        run(code, default_config())
    }

    #[test_case("1 + 2",                                          Some(json!(3))    ; "last_expr_number")]
    #[test_case("const x = 40; x + 2",                            Some(json!(42))   ; "multi_stmt_last_expr")]
    #[test_case("const x = 10; return x * 2;",                    Some(json!(20))   ; "top_level_return_fallback")]
    #[test_case("(async () => { return 'hi'; })()",               Some(json!("hi")) ; "async_iife_unwrapped")]
    #[test_case("x = 7; x + 1",                                   Some(json!(8))    ; "non_strict_bare_assignment")]
    #[test_case("for (let i = 0; i < 3; i++) { console.log(i); }", None             ; "console_log_no_return")]
    #[test_case("const a = 1; for (let i = 0; i < 2; i++) a;",    Some(json!(1))    ; "semicolon_compound_stmt")]
    fn eval_styles_work_without_rules(code: &str, expected: Option<Value>) {
        let res = run_simple(code).expect("script should succeed");
        assert_eq!(res.output, expected);
    }

    #[test]
    fn console_log_and_print_stream_and_format_objects() {
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let lines_cb = Arc::clone(&lines);
        let res = run(
            r#"console.log("hello", { a: 1 }); print([1, 2]);"#,
            JsRunConfig {
                on_output: Arc::new(move |chunk| {
                    lines_cb.lock().unwrap().push(chunk.to_owned());
                }),
                ..default_config()
            },
        )
        .expect("should succeed");

        assert_eq!(res.stdout, "hello {\"a\":1}\n[1,2]\n");
        assert_eq!(*lines.lock().unwrap(), vec!["hello {\"a\":1}\n", "[1,2]\n"]);
    }

    #[test]
    fn gather_runs_concurrently_and_preserves_other_results_on_error() {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let in_flight_cb = Arc::clone(&in_flight);
        let max_cb = Arc::clone(&max_concurrent);

        let caller: JsToolFn = Arc::new(move |pc| {
            let in_flight = Arc::clone(&in_flight_cb);
            let max_concurrent = Arc::clone(&max_cb);
            Box::pin(async move {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_concurrent.fetch_max(now, Ordering::SeqCst);
                let (tx, rx) = futures_oneshot::channel();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(60));
                    let _ = tx.send(());
                });
                let _ = rx.await;
                in_flight.fetch_sub(1, Ordering::SeqCst);

                let mode = pc
                    .args
                    .first()
                    .and_then(|v| v.get("mode"))
                    .and_then(Value::as_str)
                    .unwrap_or("ok");
                if mode == "fail" {
                    Err("boom".to_owned())
                } else {
                    Ok(Value::String(format!("ok:{mode}")))
                }
            })
        });

        let res = run(
            r#"const [a, b, c] = await gather(
                probe({ mode: "first" }),
                probe({ mode: "fail" }),
                probe({ mode: "third" })
            );
            [a, b, c]"#,
            JsRunConfig {
                tool_names: vec!["probe".to_owned()],
                call_tool: caller,
                ..default_config()
            },
        )
        .expect("gather should catch rejected tool slot");

        assert_eq!(
            res.output,
            Some(json!(["ok:first", "[ERROR] boom", "ok:third"]))
        );
        assert!(
            max_concurrent.load(Ordering::SeqCst) >= 2,
            "tool calls inside gather should execute concurrently"
        );
    }

    #[test]
    fn non_object_tool_argument_gives_actionable_hint() {
        let caller: JsToolFn = Arc::new(|_| Box::pin(async { Ok(Value::String("ok".into())) }));
        let err = run(
            r#"await read("foo.txt")"#,
            JsRunConfig {
                tool_names: vec!["read".to_owned()],
                call_tool: caller,
                ..default_config()
            },
        )
        .expect_err("positional string arg should fail");
        assert!(err.to_string().contains(OBJECT_ARGS_HINT), "got: {err}");
    }

    #[test]
    fn infinite_loop_hits_script_timeout() {
        let err = run(
            "while (true) {}",
            JsRunConfig {
                limits: JsLimits {
                    timeout: Duration::from_millis(120),
                    max_memory: DEFAULT_MEM,
                },
                ..default_config()
            },
        )
        .expect_err("infinite loop should time out");
        assert!(err.to_string().contains(TIME_LIMIT_EXCEEDED), "got: {err}");
    }

    #[test]
    fn tool_wait_time_does_not_burn_script_timeout() {
        let caller: JsToolFn = Arc::new(|_| {
            Box::pin(async {
                let (tx, rx) = futures_oneshot::channel();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(250));
                    let _ = tx.send(());
                });
                let _ = rx.await;
                Ok(Value::String("done".into()))
            })
        });

        let res = run(
            "await slow()",
            JsRunConfig {
                tool_names: vec!["slow".to_owned()],
                call_tool: caller,
                limits: JsLimits {
                    timeout: Duration::from_millis(120),
                    max_memory: DEFAULT_MEM,
                },
                ..default_config()
            },
        )
        .expect("waiting on tool should not count against script timeout");
        assert_eq!(res.output, Some(json!("done")));
    }

    #[test]
    fn cancel_flag_interrupts_infinite_loop() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancelled);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(80));
            trigger.store(true, Ordering::Relaxed);
        });

        let err = run(
            "while (true) {}",
            JsRunConfig {
                cancelled,
                ..default_config()
            },
        )
        .expect_err("cancellation should stop infinite loop");
        assert!(err.to_string().contains(CANCELLED_ERR), "got: {err}");
    }

    #[test]
    fn memory_limit_is_enforced() {
        let err = run(
            "const a = []; for (let i = 0; i < 1000000; i++) a.push('x'.repeat(1024));",
            JsRunConfig {
                limits: JsLimits {
                    timeout: DEFAULT_TIMEOUT,
                    max_memory: 4 * 1024 * 1024,
                },
                ..default_config()
            },
        )
        .expect_err("exceeding memory limit should fail");
        assert!(
            err.to_string().contains(MEMORY_LIMIT_EXCEEDED),
            "got: {err}"
        );
    }

    #[test]
    fn fs_path_and_require_respect_sandbox_callbacks() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().into_owned();

        let files: JsFileFn = Arc::new(|op| match op {
            FileOp::Read(p) => fs::read_to_string(&p).map_err(|e| e.to_string()),
            FileOp::Write(w) => {
                if w.append {
                    use std::io::Write;
                    let mut f = fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&w.path)
                        .map_err(|e| e.to_string())?;
                    f.write_all(w.content.as_bytes())
                        .map_err(|e| e.to_string())?;
                } else {
                    fs::write(&w.path, &w.content).map_err(|e| e.to_string())?;
                }
                Ok(String::new())
            }
            FileOp::Inspect(_) => Ok(String::new()),
            FileOp::Mkdir(d) => {
                if d.parents {
                    fs::create_dir_all(&d.path).map_err(|e| e.to_string())?;
                } else {
                    fs::create_dir(&d.path).map_err(|e| e.to_string())?;
                }
                Ok(String::new())
            }
        });

        let code = format!(
            r#"
            const f = require("node:fs");
            const p = require("path");
            const sub = p.join({root:?}, "nested", "dir");
            f.mkdirSync(sub, {{ recursive: true }});
            const file = p.join(sub, "hello.txt");
            f.writeFileSync(file, "hi");
            f.appendFileSync(file, "!");
            const text = f.readFileSync(file, "utf-8");
            const entries = f.readdirSync(sub);
            const st = f.statSync(file);
            ({{ text, entries, isFile: st.isFile(), size: st.size, exists: f.existsSync(file) }})
            "#
        );

        let res = run(
            &code,
            JsRunConfig {
                files: Some(files),
                ..default_config()
            },
        )
        .expect("fs/path operations should succeed");

        assert_eq!(
            res.output,
            Some(json!({
                "text": "hi!",
                "entries": ["hello.txt"],
                "isFile": true,
                "size": 3,
                "exists": true,
            }))
        );
    }

    #[test]
    fn fs_removal_is_denied_with_shared_message() {
        let err = run_simple("fs.unlinkSync('/tmp/anything')").expect_err("unlink should fail");
        assert!(err.to_string().contains(REMOVAL_UNSUPPORTED), "got: {err}");
    }

    #[test_case("const s = `\0`;", "agent.js:1:12" ; "single_line")]
    #[test_case("const a = '中文';\nconst b = `前缀\0后缀`;", "agent.js:2:14" ; "multiline_utf8")]
    fn raw_nul_byte_reports_location_and_hint(code: &str, expected_loc: &str) {
        let err = run_simple(code).expect_err("raw NUL byte should fail before eval");
        assert!(matches!(err, InterpreterError::Parse(_)), "got: {err}");
        let msg = err.to_string();
        assert!(msg.contains(NULL_BYTE_ERR_PREFIX), "got: {msg}");
        assert!(msg.contains(expected_loc), "got: {msg}");
        assert!(msg.contains(NULL_BYTE_HINT), "got: {msg}");
    }

    #[test]
    fn escaped_nul_in_js_string_literal_still_succeeds() {
        let res = run_simple(r#"`a\u0000b`.replaceAll('\x00', '`')"#)
            .expect("escaped NUL sequences should succeed");
        assert_eq!(res.output, Some(json!("a`b")));
    }
}
