use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use futures::TryStreamExt;
use reqwest::Client;
use reqwest::header::HeaderMap;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, warn};

use std::sync::Arc;

use crate::model::{Model, ModelInfo};
use crate::provider::{BoxFuture, Provider};
use crate::providers::{ResolvedAuth, SseBody, Timeouts, ok_sse, sse_error_status};
use crate::types::{EffortDialect, ThinkingFields};
use crate::{
    AgentError, ContentBlock, Message, ProviderEvent, RequestOptions, Role, StopReason,
    StreamResponse, ThinkingConfig, TokenUsage, dialect,
};

use maki_config::providers::ProvidersConfig;
use maki_storage::frame::ReasoningFrame;
use maki_storage::id::{MakiId, SessionRef};
use maki_storage::sessions::Effort;

const RESPONSES_PATH: &str = "/responses";
const FAILED_RESPONSE_STATUS: u16 = 500;
const NANO_AIU_PER_USD: f64 = 100_000_000_000.0;
const SERVICE_TIER_FAST: &str = "fast";
const GPT_6_2_FRAGMENT: &[u8] = b"gpt-6.2";
const CONFIGURATION_UPDATE: &str = "configuration_update";

fn is_gpt_6_2(model_id: &str) -> bool {
    model_id
        .as_bytes()
        .windows(GPT_6_2_FRAGMENT.len())
        .any(|fragment| fragment.eq_ignore_ascii_case(GPT_6_2_FRAGMENT))
}

fn copilot_usd(copilot_usage: &Value) -> Option<f64> {
    copilot_usage["total_nano_aiu"]
        .as_f64()
        .map(|nano_aiu| nano_aiu / NANO_AIU_PER_USD)
}

fn tool_parameters(tool: &Value) -> Value {
    match tool.get("input_schema") {
        Some(schema) if schema.is_object() => schema.clone(),
        _ => json!({"type": "object", "properties": {}, "additionalProperties": true}),
    }
}

pub fn build_body(
    model: &Model,
    messages: &[Message],
    system: &str,
    tools: &Value,
    replay_reasoning: bool,
) -> Value {
    build_body_with_frame(model, messages, system, tools, replay_reasoning, None)
}

fn build_body_with_frame(
    model: &Model,
    messages: &[Message],
    system: &str,
    tools: &Value,
    replay_reasoning: bool,
    frame: Option<MakiId>,
) -> Value {
    let developer_prompt = is_gpt_6_2(&model.id);
    let input_prompt = developer_prompt
        .then_some(system)
        .filter(|prompt| !prompt.trim().is_empty());
    let input = convert_input(messages, replay_reasoning, input_prompt, frame);
    let wire_tools = convert_tools(tools);

    let mut body = json!({
        "model": model.id,
        "input": input,
        "stream": true,
        "store": false,
    });
    if !developer_prompt {
        body["instructions"] = json!(system);
    }
    if wire_tools.as_array().is_some_and(|a| !a.is_empty()) {
        body["tools"] = wire_tools;
    }
    body
}

pub fn plan_dialect(model_id: &str) -> &'static EffortDialect<'static> {
    if is_gpt_6_2(model_id) {
        return &dialect::GPT_6;
    }
    if !model_id.contains("-codex") {
        return if model_id.starts_with("gpt-6-") || model_id.starts_with("gpt-6.") {
            &dialect::GPT_6
        } else if model_id.starts_with("gpt-5.6-") {
            &dialect::GPT_5_6
        } else if model_id.starts_with("gpt-5.5") {
            &dialect::CODING_PLAN
        } else {
            &dialect::GPT_5_6
        };
    }
    if model_id.starts_with("gpt-5.1-codex") && !model_id.starts_with("gpt-5.1-codex-max") {
        &dialect::CODEX_5_1
    } else {
        &dialect::CODEX
    }
}

pub fn responses_dialect<'a>(
    model_id: &str,
    discovered: Option<&'a ModelInfo>,
) -> EffortDialect<'a> {
    let base = plan_dialect(model_id);
    if let Some(effort) = discovered.and_then(|d| d.effort.as_ref()) {
        effort.refine(base)
    } else {
        base.clone()
    }
}

pub fn apply_responses_reasoning(
    body: &mut Value,
    thinking: ThinkingConfig,
    model: &Model,
    dialect: &EffortDialect,
) {
    if let Some(effort) = thinking.effort_str(dialect, model) {
        body["reasoning"] = reasoning_config(effort);
    }
}

/// Opts a request into Fast mode (formerly priority processing). `fast` is
/// already gated by [`RequestOptions::clamped`], so a model without a fast tier
/// never reaches here with it set. Leaving the field off lets the project's own
/// default service tier apply.
pub fn apply_responses_fast(body: &mut Value, fast: bool) {
    if fast {
        body["service_tier"] = json!(SERVICE_TIER_FAST);
    }
}

fn reasoning_config(effort: &str) -> Value {
    let mut reasoning = json!({ "effort": effort });
    if effort != dialect::OFF {
        reasoning["summary"] = json!("auto");
    }
    reasoning
}

fn native_reasoning(body: &Map<String, Value>) -> Option<(Effort, &Value)> {
    let reasoning = body.get("reasoning")?;
    let fields = reasoning.as_object()?;
    if body.len() != 1 || fields.keys().any(|key| key != "effort" && key != "summary") {
        return None;
    }
    Some((fields.get("effort")?.as_str()?.parse().ok()?, reasoning))
}

fn declared_frame_reasoning(fields: &ThinkingFields, effort: Effort) -> Option<&Value> {
    let mut choices = fields
        .levels
        .values()
        .chain(&fields.adaptive)
        .chain(&fields.off)
        .filter_map(native_reasoning);
    let (_, first) = choices.next()?;
    let mut selected = (first["effort"] == effort.as_str()).then_some(first);
    for (level, reasoning) in choices {
        if reasoning.get("summary") != first.get("summary") {
            return None;
        }
        if level == effort {
            selected = Some(reasoning);
        }
    }
    selected
}

fn can_replay(body: &Value, model: &Model) -> bool {
    body["model"].as_str() == Some(model.id.as_str())
        && body["store"] == false
        && body["previous_response_id"].is_null()
        && body["conversation"].is_null()
        && ["/mode", "/reasoning/mode"].into_iter().all(|path| {
            body.pointer(path)
                .is_none_or(|mode| mode.is_null() || mode == "standard")
        })
        && ["/multi_agent", "/reasoning/multi_agent"]
            .into_iter()
            .all(|path| {
                body.pointer(path)
                    .is_none_or(|mode| mode.is_null() || mode == false)
            })
}

fn convert_input(
    messages: &[Message],
    replay_reasoning: bool,
    developer_prompt: Option<&str>,
    frame: Option<MakiId>,
) -> Value {
    let mut input = Vec::new();
    if let Some(text) = developer_prompt {
        input.push(json!({
            "type": "message",
            "role": "developer",
            "content": [{"type": "input_text", "text": text}]
        }));
    }

    for msg in messages {
        match msg.role {
            Role::User => {
                if let Some(effort) = frame.and_then(|frame| msg.reasoning_effort(frame)) {
                    input.push(json!({
                        "type": CONFIGURATION_UPDATE,
                        "reasoning": {"effort": effort.as_str()},
                    }));
                }
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            input.push(json!({
                                "type": "message",
                                "role": "user",
                                "content": [{"type": "input_text", "text": text}]
                            }));
                        }
                        ContentBlock::Image { source } => {
                            input.push(json!({
                                "type": "message",
                                "role": "user",
                                "content": [{"type": "input_image", "image_url": source.to_data_url()}]
                            }));
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": crate::types::non_empty_tool_result(content),
                            }));
                        }
                        ContentBlock::ToolUse { .. }
                        | ContentBlock::Thinking { .. }
                        | ContentBlock::RedactedThinking { .. }
                        | ContentBlock::ReasoningEffort { .. }
                        | ContentBlock::Unknown(_) => {}
                    }
                }
            }
            Role::Assistant => {
                let mut reasoning = String::new();
                let mut reasoning_id = None;
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for block in &msg.content {
                    match block {
                        ContentBlock::Thinking {
                            thinking,
                            reasoning_id: id,
                            ..
                        } => {
                            reasoning.push_str(thinking);
                            reasoning_id = reasoning_id.or_else(|| id.clone());
                        }
                        ContentBlock::Text { text } => text_parts.push(text.as_str()),
                        ContentBlock::ToolUse {
                            id, name, input, ..
                        } => {
                            tool_calls.push((id, name, input));
                        }
                        ContentBlock::ToolResult { .. }
                        | ContentBlock::Image { .. }
                        | ContentBlock::RedactedThinking { .. }
                        | ContentBlock::ReasoningEffort { .. }
                        | ContentBlock::Unknown(_) => {}
                    }
                }

                if replay_reasoning && !reasoning.is_empty() {
                    let mut item = json!({
                        "type": "reasoning",
                        "summary": [],
                        "content": [{"type": "reasoning_text", "text": reasoning}]
                    });
                    if let Some(id) = reasoning_id {
                        item["id"] = json!(id);
                    }
                    input.push(item);
                }

                if !text_parts.is_empty() {
                    let joined = text_parts.join("");
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": joined}]
                    }));
                }

                for (id, name, args) in tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": args.to_string(),
                    }));
                }
            }
        }
    }

    Value::Array(input)
}

pub(crate) fn convert_tools(anthropic_tools: &Value) -> Value {
    let Some(tools) = anthropic_tools.as_array() else {
        return json!([]);
    };

    Value::Array(
        tools
            .iter()
            .filter_map(|t| {
                Some(json!({
                    "type": "function",
                    "name": t.get("name")?,
                    "description": t.get("description")?,
                    "parameters": tool_parameters(t),
                    "strict": false,
                }))
            })
            .collect(),
    )
}

static SUMMARY_REJECTED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn summary_rejected(base: &str) -> bool {
    SUMMARY_REJECTED.lock().unwrap().contains(base)
}

fn reject_summary(base: &str) {
    SUMMARY_REJECTED.lock().unwrap().insert(base.to_owned());
}

fn has_summary(body: &Value) -> bool {
    body.get("reasoning")
        .and_then(|reasoning| reasoning.get("summary"))
        .is_some()
}

fn strip_summary(body: &mut Value) {
    if let Some(reasoning) = body.get_mut("reasoning").and_then(Value::as_object_mut) {
        reasoning.remove("summary");
    }
}

/// `headers` are the provider's own for this request: over maki's, under
/// the credentials.
pub(crate) async fn do_stream(
    client: &Client,
    model: &crate::model::Model,
    headers: &HeaderMap,
    body: &Value,
    event_tx: &UnboundedSender<ProviderEvent>,
    auth: &ResolvedAuth,
    stream_timeout: Duration,
) -> Result<StreamResponse, AgentError> {
    let base = auth.base_url.as_deref().ok_or_else(|| AgentError::Config {
        message: "Responses API requires a base_url in auth".into(),
    })?;
    let mut body = Cow::Borrowed(body);
    if summary_rejected(base) && has_summary(&body) {
        strip_summary(body.to_mut());
    }
    let result = post_responses(
        client,
        model,
        headers,
        &body,
        event_tx,
        auth,
        base,
        stream_timeout,
    )
    .await;
    match result {
        Err(err) if has_summary(&body) && err.is_unsupported_reasoning_summary() => {
            reject_summary(base);
            strip_summary(body.to_mut());
            post_responses(
                client,
                model,
                headers,
                &body,
                event_tx,
                auth,
                base,
                stream_timeout,
            )
            .await
        }
        result => result,
    }
}

#[allow(clippy::too_many_arguments)]
async fn post_responses(
    client: &Client,
    model: &crate::model::Model,
    headers: &HeaderMap,
    body: &Value,
    event_tx: &UnboundedSender<ProviderEvent>,
    auth: &ResolvedAuth,
    base: &str,
    stream_timeout: Duration,
) -> Result<StreamResponse, AgentError> {
    let json_body = serde_json::to_vec(body)?;

    let request = auth.finish(
        client
            .post(format!("{base}{RESPONSES_PATH}"))
            .header("content-type", "application/json")
            .headers(headers.clone())
            .body(json_body),
    )?;

    debug!(
        model = %model.id,
        provider = "OpenAI Coding Plan",
        "sending Responses API request"
    );

    ok_sse(client, request, stream_timeout, |body| {
        parse_sse(body, event_tx)
    })
    .await
}

pub struct ResponsesProvider {
    pub slug: String,
    pub base_url: Option<String>,
    pub system_prefix: Option<String>,
    auth: Arc<Mutex<ResolvedAuth>>,
    timeouts: Timeouts,
    client: Client,
    body_hook: Option<Arc<dyn super::codec::BodyHook>>,
    session_id: super::codec::SessionCarrier,
    pub replay_reasoning: bool,
}

fn parse_price_value(val: &Value) -> Option<f64> {
    if let Some(n) = val.as_f64() {
        Some(n)
    } else if let Some(s) = val.as_str() {
        s.parse::<f64>().ok()
    } else {
        None
    }
}

fn normalize_price(val: Option<f64>) -> f64 {
    let p = val.unwrap_or(0.0);
    if p > 0.0 && p < 0.01 {
        let scaled = p * 1_000_000.0;
        (scaled * 1_000_000.0).round() / 1_000_000.0
    } else {
        p
    }
}

fn parse_pricing(pricing_val: &Value) -> Option<crate::model::ModelPricing> {
    if !pricing_val.is_object() {
        return None;
    }
    let input = normalize_price(
        pricing_val
            .get("prompt")
            .or_else(|| pricing_val.get("input"))
            .and_then(parse_price_value),
    );
    let output = normalize_price(
        pricing_val
            .get("completion")
            .or_else(|| pricing_val.get("output"))
            .and_then(parse_price_value),
    );
    let cache_read = normalize_price(
        pricing_val
            .get("input_cache_read")
            .or_else(|| pricing_val.get("cache_read"))
            .and_then(parse_price_value),
    );
    let cache_write = normalize_price(
        pricing_val
            .get("input_cache_write")
            .or_else(|| pricing_val.get("cache_write"))
            .and_then(parse_price_value),
    );
    Some(crate::model::ModelPricing {
        input,
        output,
        cache_write,
        cache_read,
        fast: None,
    })
}

fn parse_model_info(m: &Value) -> Option<ModelInfo> {
    let id = m["id"].as_str().or_else(|| m["slug"].as_str())?;
    let context_window = m["context_window"]
        .as_u64()
        .or_else(|| m["context_length"].as_u64())
        .or_else(|| m["max_model_len"].as_u64())
        .or_else(|| m["max_input_tokens"].as_u64())
        .and_then(|v| u32::try_from(v).ok());
    let max_output_tokens = m["max_output_tokens"]
        .as_u64()
        .or_else(|| m["max_tokens"].as_u64())
        .and_then(|v| u32::try_from(v).ok());

    let supports_vision = m
        .get("modalities")
        .and_then(|mod_val| mod_val.get("input"))
        .or_else(|| m.get("input_modalities"))
        .and_then(Value::as_array)
        .map(|modalities| modalities.iter().any(|modality| modality == "image"));

    let pricing = m.get("pricing").and_then(parse_pricing);
    let reasoning_flag = m.get("reasoning").and_then(Value::as_bool);

    let mut efforts = Vec::new();
    let mut has_none = false;
    let levels_array = m
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .or_else(|| m.get("reasoning_levels").and_then(Value::as_array));

    if let Some(levels) = levels_array {
        for lvl in levels {
            let effort_str = lvl.as_str().or_else(|| lvl["effort"].as_str());
            if let Some(s) = effort_str {
                if s == dialect::OFF {
                    has_none = true;
                } else if let Ok(e) = s.parse::<crate::Effort>() {
                    efforts.push(e);
                }
            }
        }
        efforts.sort_unstable();
        efforts.dedup();
    }

    let (effort, supports_thinking) = if reasoning_flag == Some(false) {
        (None, Some(false))
    } else if !efforts.is_empty() || has_none {
        (
            Some(crate::model::ModelEffort {
                supported: efforts.clone(),
                send_off: Some(has_none),
            }),
            Some(!efforts.is_empty()),
        )
    } else if reasoning_flag == Some(true) {
        (None, Some(true))
    } else {
        (None, None)
    };

    let name = m
        .get("name")
        .or_else(|| m.get("display_name"))
        .and_then(Value::as_str)
        .map(str::to_string);

    Some(ModelInfo {
        id: id.to_string(),
        name,
        context_window,
        max_output_tokens,
        pricing,
        supports_thinking,
        supports_vision,
        provider_info: None,
        extra: Some(m.clone()),
        effort,
        supports_fast: ModelInfo::fast_tier_declared(m).then_some(true),
    })
}

impl ResponsesProvider {
    pub fn new(
        slug: String,
        base_url: Option<String>,
        auth: Arc<Mutex<ResolvedAuth>>,
        timeouts: Timeouts,
    ) -> Self {
        Self {
            slug,
            base_url,
            system_prefix: None,
            auth,
            timeouts,
            client: super::http_client(timeouts),
            body_hook: None,
            session_id: super::codec::SessionCarrier::default(),
            replay_reasoning: false,
        }
    }

    pub fn with_replay_reasoning(mut self, replay_reasoning: bool) -> Self {
        self.replay_reasoning = replay_reasoning;
        self
    }

    pub fn with_session_id(mut self, session_id: super::codec::SessionCarrier) -> Self {
        self.session_id = session_id;
        self
    }

    pub fn with_body_hook(mut self, hook: Option<Arc<dyn super::codec::BodyHook>>) -> Self {
        self.body_hook = hook;
        self
    }

    pub fn with_system_prefix(mut self, prefix: Option<String>) -> Self {
        self.system_prefix = prefix;
        self
    }

    fn apply_request_reasoning(&self, body: &mut Value, model: &Model, thinking: ThinkingConfig) {
        if let Some((fields, fallback)) = model.effective_thinking_fields() {
            thinking.apply_fields(body, model, fields, fallback);
        } else {
            let discovered = crate::model_registry::discovered(&self.slug, &model.id);
            let dialect = responses_dialect(&model.id, discovered.as_ref());
            apply_responses_reasoning(body, thinking, model, &dialect);
        }
    }

    fn frame_reasoning(model: &Model, effort: Effort) -> Option<Value> {
        if let Some((fields, _)) = model.effective_thinking_fields() {
            return declared_frame_reasoning(fields, effort).cloned();
        }
        Some(reasoning_config(effort.as_str()))
    }

    fn resolve_url(&self, auth: &ResolvedAuth) -> Option<String> {
        if let Some(b) = &auth.base_url {
            return Some(b.clone());
        }
        if let Some(env) = maki_config::providers::base_url_override(&self.slug) {
            return Some(env);
        }
        let config = ProvidersConfig::load();
        if let Some(def) = config.get(&self.slug)
            && let Some(b) = &def.base_url
        {
            return Some(b.clone());
        }
        self.base_url.clone()
    }

    async fn do_list_models(&self, auth: &ResolvedAuth) -> Result<Vec<ModelInfo>, AgentError> {
        if let Some(base) = self.resolve_url(auth) {
            let url = format!("{base}/models");
            if let Ok(req) = auth.finish(
                self.client
                    .get(&url)
                    .header("user-agent", super::user_agent()),
            ) && let Ok(text) = super::ok_text(&self.client, req).await
                && let Ok(val) = serde_json::from_str::<Value>(&text)
            {
                let raw_items = val["data"].as_array().or_else(|| val["models"].as_array());
                if let Some(data) = raw_items {
                    let mut models: Vec<ModelInfo> =
                        data.iter().filter_map(parse_model_info).collect();
                    models.sort_by(|a, b| a.id.cmp(&b.id));
                    if !models.is_empty() {
                        crate::model_registry::set_known_models(&self.slug, models.clone());
                        return Ok(models);
                    }
                }
            }
        }
        let config = ProvidersConfig::load();
        if let Some(def) = config.get(&self.slug) {
            let models: Vec<ModelInfo> = def
                .models
                .iter()
                .map(|m| {
                    let effort = m.thinking_fields.as_ref().map(|f| {
                        let supported: Vec<_> = f.levels.keys().copied().collect();
                        crate::model::ModelEffort {
                            supported,
                            send_off: Some(f.off.is_some()),
                        }
                    });
                    ModelInfo {
                        id: m.id.clone(),
                        name: m.name.clone(),
                        context_window: m.context_window,
                        max_output_tokens: m.max_output_tokens,
                        pricing: None,
                        supports_thinking: m
                            .supports_thinking
                            .or_else(|| effort.as_ref().map(|e| !e.supported.is_empty())),
                        supports_vision: m.supports_vision,
                        provider_info: None,
                        extra: None,
                        effort,
                        supports_fast: m.has_fast_pricing().then_some(true),
                    }
                })
                .collect();
            return Ok(models);
        }
        Ok(Vec::new())
    }
}

impl Provider for ResponsesProvider {
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a UnboundedSender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        self.stream_message_with_reasoning_frame(
            model, messages, system, tools, event_tx, opts, session_id, None,
        )
    }

    fn configuration_effort(&self, model: &Model, opts: RequestOptions) -> Option<Effort> {
        if !is_gpt_6_2(&model.id) {
            return None;
        }
        let mut body = json!({});
        self.apply_request_reasoning(&mut body, model, opts.clamped(model).thinking);
        let (effort, _) = native_reasoning(body.as_object()?)?;
        if let Some((fields, _)) = model.effective_thinking_fields() {
            declared_frame_reasoning(fields, effort)?;
        }
        Some(effort)
    }

    fn stream_message_with_reasoning_frame<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a UnboundedSender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&'a SessionRef>,
        reasoning_frame: Option<&'a ReasoningFrame>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        let frame_reasoning = reasoning_frame
            .filter(|frame| is_gpt_6_2(&model.id) && frame.model == model.spec())
            .filter(|_| {
                session_id.is_none()
                    || !matches!(
                        self.session_id.body_field.as_deref(),
                        Some(
                            "model"
                                | "input"
                                | "reasoning"
                                | "store"
                                | "previous_response_id"
                                | "conversation"
                                | "mode"
                                | "multi_agent"
                        )
                    )
            })
            .and_then(|frame| Some((frame, Self::frame_reasoning(model, frame.effort)?)));
        let mut auth = self.auth.lock().unwrap().clone();
        if auth.base_url.is_none() {
            auth.base_url = self.resolve_url(&auth);
        }
        Box::pin(async move {
            let effective_system = match &self.system_prefix {
                Some(prefix) => Cow::Owned(format!("{prefix}\n\n{system}")),
                None => Cow::Borrowed(system),
            };
            let mut body = build_body_with_frame(
                model,
                messages,
                &effective_system,
                tools,
                self.replay_reasoning,
                frame_reasoning
                    .as_ref()
                    .filter(|_| self.body_hook.is_none())
                    .map(|(frame, _)| frame.id),
            );
            if self.body_hook.is_some() || frame_reasoning.is_none() {
                self.apply_request_reasoning(&mut body, model, opts.thinking);
            }
            apply_responses_fast(&mut body, opts.fast);
            if let Some(session) = session_id {
                let _ = self.session_id.apply(session, &mut body, &mut auth);
            }
            if let Some(hook) = &self.body_hook {
                let original = frame_reasoning
                    .as_ref()
                    .map(|_| (body["input"].clone(), body.get("reasoning").cloned()));
                let ctx = super::codec::RequestCtx {
                    model,
                    opts: &opts,
                    session: session_id,
                    discovered: None,
                };
                body = hook.call(body, &ctx).await?;
                if let (Some((frame, baseline)), Some((input, reasoning))) =
                    (frame_reasoning, original)
                {
                    if body["input"] == input
                        && body.get("reasoning") == reasoning.as_ref()
                        && can_replay(&body, model)
                    {
                        body["reasoning"] = baseline;
                        body["input"] = convert_input(
                            messages,
                            self.replay_reasoning,
                            (!effective_system.trim().is_empty())
                                .then_some(effective_system.as_ref()),
                            Some(frame.id),
                        );
                    } else {
                        warn!(provider = %self.slug, model = %model.id, "body hook changed the Responses replay fields; sending its request without frame updates");
                    }
                }
            } else if let Some((_, baseline)) = frame_reasoning {
                body["reasoning"] = baseline;
            }
            do_stream(
                &self.client,
                model,
                &reqwest::header::HeaderMap::new(),
                &body,
                event_tx,
                &auth,
                self.timeouts.stream,
            )
            .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AgentError>> {
        let auth = self.auth.lock().unwrap().clone();
        Box::pin(async move { self.do_list_models(&auth).await })
    }
}

struct ToolAccumulator {
    output_index: u64,
    call_id: String,
    name: String,
    arguments: String,
}

pub(crate) async fn parse_sse(
    mut events: SseBody,
    event_tx: &UnboundedSender<ProviderEvent>,
) -> Result<StreamResponse, AgentError> {
    let mut text = String::new();
    let mut reasoning_text = String::new();
    let mut reasoning_id: Option<String> = None;
    let mut tool_accumulators: Vec<ToolAccumulator> = Vec::new();
    let mut usage = TokenUsage::default();
    let mut stop_reason: Option<StopReason> = None;
    let mut is_first_content = true;

    while let Some(event) = events.try_next().await? {
        let Some(data) = event.data.as_deref().map(str::trim) else {
            continue;
        };
        let current_event = event.event.as_deref().map_or("", str::trim);

        if current_event == "error" {
            if let Ok(ev) = serde_json::from_str::<crate::providers::SseErrorPayload>(data) {
                warn!(error_type = %ev.error.r#type, message = %ev.error.message, "SSE error in stream");
                return Err(ev.into_agent_error());
            }
            let parsed: Value = serde_json::from_str(data).unwrap_or_default();
            let message = parsed["message"]
                .as_str()
                .unwrap_or("unknown error")
                .to_string();
            return Err(AgentError::api(500, message));
        }

        let Ok(parsed) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let event = if current_event.is_empty() {
            parsed["type"].as_str().unwrap_or_default()
        } else {
            current_event
        };

        match event {
            "response.output_text.delta" => {
                if let Some(delta) = parsed["delta"].as_str()
                    && !delta.is_empty()
                {
                    let delta = if is_first_content {
                        is_first_content = false;
                        delta.trim_start().to_string()
                    } else {
                        delta.to_string()
                    };
                    if !delta.is_empty() {
                        text.push_str(&delta);
                        event_tx.send(ProviderEvent::TextDelta { text: delta })?;
                    }
                }
            }

            "response.output_item.added" => {
                let item = &parsed["item"];
                let output_index = parsed["output_index"]
                    .as_u64()
                    .unwrap_or(tool_accumulators.len() as u64);
                match item["type"].as_str() {
                    Some("function_call") => {
                        let call_id = item["call_id"].as_str().unwrap_or_default().to_string();
                        let name = item["name"]
                            .as_str()
                            .or_else(|| item["function"]["name"].as_str())
                            .or_else(|| item["function_call"]["name"].as_str())
                            .unwrap_or_default()
                            .to_string();
                        if !name.is_empty() {
                            event_tx.send(ProviderEvent::ToolUseStart {
                                id: call_id.clone(),
                                name: name.clone(),
                            })?;
                        }
                        tool_accumulators.push(ToolAccumulator {
                            output_index,
                            call_id,
                            name,
                            arguments: String::new(),
                        });
                    }
                    Some("reasoning") => {
                        reasoning_id = item["id"].as_str().map(str::to_owned);
                    }
                    _ => {}
                }
            }

            "response.function_call_arguments.delta" => {
                let delta: Cow<'_, str> = if let Some(s) = parsed["delta"].as_str() {
                    Cow::Borrowed(s)
                } else if let Some(obj) = parsed["delta"].as_object() {
                    Cow::Owned(serde_json::to_string(obj).unwrap_or_default())
                } else {
                    Cow::Borrowed("")
                };
                let acc = if let Some(idx) = parsed["output_index"].as_u64() {
                    tool_accumulators.iter_mut().find(|a| a.output_index == idx)
                } else {
                    tool_accumulators.last_mut()
                };
                if let Some(acc) = acc {
                    if acc.name.is_empty() {
                        let name = parsed["name"]
                            .as_str()
                            .or_else(|| parsed["item"]["name"].as_str())
                            .or_else(|| parsed["item"]["function"]["name"].as_str())
                            .unwrap_or_default();
                        if !name.is_empty() {
                            acc.name = name.to_string();
                            event_tx.send(ProviderEvent::ToolUseStart {
                                id: acc.call_id.clone(),
                                name: acc.name.clone(),
                            })?;
                        }
                    }
                    if !delta.is_empty() {
                        acc.arguments.push_str(&delta);
                    }
                }
            }

            "response.in_progress" => {
                if let Some(pp) = parsed.get("prompt_progress") {
                    let processed = pp["processed"].as_u64().unwrap_or(0) as u32;
                    let total = pp["total"].as_u64().unwrap_or(0) as u32;
                    let cache = pp["cache"].as_u64().unwrap_or(0) as u32;
                    event_tx.send(ProviderEvent::PromptProgress {
                        processed,
                        total,
                        cache,
                    })?;
                }
            }

            "response.output_item.done" => {
                let item = &parsed["item"];
                if item["type"].as_str() == Some("function_call") {
                    let call_id = item["call_id"].as_str().unwrap_or_default().to_string();
                    let name = item["name"]
                        .as_str()
                        .or_else(|| item["function"]["name"].as_str())
                        .or_else(|| item["function_call"]["name"].as_str())
                        .unwrap_or_default()
                        .to_string();
                    let arguments = if let Some(s) = item["arguments"].as_str() {
                        s.to_string()
                    } else if let Some(obj) = item["arguments"].as_object() {
                        serde_json::to_string(obj).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let acc = if let Some(idx) = parsed["output_index"].as_u64() {
                        tool_accumulators
                            .iter_mut()
                            .find(|acc| acc.output_index == idx)
                    } else {
                        tool_accumulators.last_mut()
                    };
                    if let Some(acc) = acc {
                        let should_emit_start = acc.name.is_empty() && !name.is_empty();
                        if acc.call_id.is_empty() {
                            acc.call_id = call_id.clone();
                        }
                        if acc.name.is_empty() {
                            acc.name = name.clone();
                        }
                        if !arguments.is_empty() {
                            acc.arguments = arguments;
                        }
                        if should_emit_start {
                            event_tx.send(ProviderEvent::ToolUseStart {
                                id: acc.call_id.clone(),
                                name: acc.name.clone(),
                            })?;
                        }
                    } else {
                        if !name.is_empty() {
                            event_tx.send(ProviderEvent::ToolUseStart {
                                id: call_id.clone(),
                                name: name.clone(),
                            })?;
                        }
                        tool_accumulators.push(ToolAccumulator {
                            output_index: tool_accumulators.len() as u64,
                            call_id,
                            name,
                            arguments,
                        });
                    }
                }
            }

            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                if let Some(delta) = parsed["delta"].as_str()
                    && !delta.is_empty()
                {
                    reasoning_text.push_str(delta);
                    event_tx.send(ProviderEvent::ThinkingDelta {
                        text: delta.to_string(),
                    })?;
                }
            }

            "response.reasoning_summary_part.added" if !reasoning_text.is_empty() => {
                reasoning_text.push_str("\n\n");
                event_tx.send(ProviderEvent::ThinkingDelta {
                    text: "\n\n".to_string(),
                })?;
            }

            "response.completed" => {
                if let Some(parsed_usage) = parse_usage(&parsed) {
                    usage = parsed_usage;
                }
                let status = parsed["response"]["status"].as_str().unwrap_or("completed");
                stop_reason = Some(match status {
                    "completed" => {
                        if tool_accumulators.is_empty() {
                            StopReason::EndTurn
                        } else {
                            StopReason::ToolUse
                        }
                    }
                    "incomplete" => StopReason::MaxTokens,
                    _ => StopReason::EndTurn,
                });
            }

            "response.incomplete" => {
                if let Some(parsed_usage) = parse_usage(&parsed) {
                    usage = parsed_usage;
                }
                stop_reason = Some(StopReason::MaxTokens);
            }

            "response.failed" => {
                let error = &parsed["response"]["error"];
                let message = error["message"]
                    .as_str()
                    .unwrap_or("response generation failed")
                    .to_string();
                let status = error["code"]
                    .as_str()
                    .and_then(sse_error_status)
                    .unwrap_or(FAILED_RESPONSE_STATUS);
                return Err(AgentError::api(status, message));
            }

            _ => {}
        }
    }

    let mut content_blocks: Vec<ContentBlock> = Vec::new();

    if !reasoning_text.is_empty() {
        content_blocks.push(ContentBlock::Thinking {
            thinking: reasoning_text,
            signature: None,
            duration_ms: None,
            reasoning_id,
        });
    }

    if !text.is_empty() {
        content_blocks.push(ContentBlock::Text { text });
    }

    for acc in tool_accumulators {
        let input: Value = match serde_json::from_str(&acc.arguments) {
            Ok(v) => {
                debug!(tool = %acc.name, json = %acc.arguments, "tool input JSON");
                v
            }
            Err(e) => {
                warn!(error = %e, tool = %acc.name, json = %acc.arguments, "malformed tool JSON, falling back to {{}}");
                Value::Object(Default::default())
            }
        };
        content_blocks.push(ContentBlock::tool_use(acc.call_id, acc.name, input));
    }

    Ok(StreamResponse {
        message: Message {
            role: Role::Assistant,
            content: content_blocks,
            ..Default::default()
        },
        usage,
        stop_reason,
        ..Default::default()
    })
}

/// The usage a `response.completed` or `response.incomplete` event carries.
fn parse_usage(event: &Value) -> Option<TokenUsage> {
    let u = event["response"].get("usage")?;
    let input_tokens = u["input_tokens"].as_u64().unwrap_or(0);
    let output_tokens = u["output_tokens"].as_u64().unwrap_or(0);

    let cached = u["input_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0);

    Some(TokenUsage {
        input: input_tokens.saturating_sub(cached),
        output: output_tokens,
        cache_read: cached,
        cache_creation: 0,
        cost: copilot_usd(&event["copilot_usage"]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::iter;
    use test_case::test_case;
    use tokio::sync::mpsc::unbounded_channel;

    use crate::test_support::{Canned, serve, sse_body, sse_event};

    const OVERLOAD_MESSAGE: &str = "Our servers are currently overloaded. Please try again later.";
    const BAD_REQUEST_MESSAGE: &str = "Invalid value for 'model'";
    const RATE_LIMIT_MESSAGE: &str = "Rate limit hit";
    const TOOL_NAME: &str = "word_count";
    const TOOL_DESCRIPTION: &str = "Count words.";
    const TOOL_MUST_SURVIVE: &str = "a tool without a schema still belongs in the request";

    #[test]
    fn strip_summary_keeps_effort() {
        let mut body = json!({"reasoning": {"effort": "high", "summary": "auto"}});
        assert!(has_summary(&body));
        strip_summary(&mut body);
        assert!(!has_summary(&body));
        assert_eq!(body, json!({"reasoning": {"effort": "high"}}));
    }

    #[test]
    fn summary_memo_records_rejection_once() {
        let base = "https://summary-memo.test/v1";
        assert!(!summary_rejected(base));
        reject_summary(base);
        assert!(summary_rejected(base));
    }

    #[test]
    fn convert_tools_defaults_missing_parameters() {
        let tools = json!([{ "name": TOOL_NAME, "description": TOOL_DESCRIPTION }]);
        let converted = convert_tools(&tools);
        assert_eq!(
            converted[0]["name"],
            json!(TOOL_NAME),
            "{TOOL_MUST_SURVIVE}"
        );
        assert_eq!(
            converted[0]["parameters"],
            json!({"type": "object", "properties": {}, "additionalProperties": true})
        );
    }

    async fn run_sse(sse: &str) -> (Result<StreamResponse, AgentError>, Vec<ProviderEvent>) {
        let (tx, mut rx) = unbounded_channel();
        let result = parse_sse(sse_body(sse.as_bytes()), &tx).await;
        (result, iter::from_fn(|| rx.try_recv().ok()).collect())
    }

    const COPILOT_NANO_AIU: u64 = 375_000;
    const COPILOT_USD: f64 = 0.00000375;

    #[test_case("response.completed", "completed", StopReason::EndTurn, json!({"total_nano_aiu": COPILOT_NANO_AIU}), Some(COPILOT_USD) ; "completed")]
    #[test_case("response.incomplete", "incomplete", StopReason::MaxTokens, json!({"total_nano_aiu": COPILOT_NANO_AIU}), Some(COPILOT_USD) ; "incomplete")]
    #[test_case("response.completed", "completed", StopReason::EndTurn, json!({"token_details": []}), None ; "no_total")]
    #[test_case("response.completed", "completed", StopReason::EndTurn, json!({"total_nano_aiu": COPILOT_NANO_AIU as f64}), Some(COPILOT_USD) ; "float_total")]
    #[test_case("response.completed", "completed", StopReason::EndTurn, json!({"total_nano_aiu": "375000"}), None ; "string_total")]
    #[test_case("response.completed", "completed", StopReason::EndTurn, json!("billed"), None ; "not_an_object")]
    #[tokio::test]
    async fn parse_sse_copilot_usage_prices_the_turn(
        event: &str,
        status: &str,
        stop: StopReason,
        copilot_usage: Value,
        cost: Option<f64>,
    ) {
        let sse = sse_event(
            event,
            &json!({
                "copilot_usage": copilot_usage,
                "response": {"status": status, "usage": {"input_tokens": 100, "output_tokens": 10}},
            }),
        );

        let (resp, _events) = run_sse(&sse).await;
        let resp = resp.unwrap();

        assert_eq!(
            resp.usage,
            TokenUsage {
                input: 100,
                output: 10,
                cache_creation: 0,
                cache_read: 0,
                cost,
            }
        );
        assert_eq!(resp.stop_reason, Some(stop));
    }

    #[tokio::test]
    async fn parse_sse_text_and_usage() {
        let sse = "\
event: response.output_text.delta\n\
data: {\"delta\":\"Hello\"}\n\
\n\
event: response.output_text.delta\n\
data: {\"delta\":\" world\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":40}}}}\n\
\n";

        let (resp, events) = run_sse(sse).await;
        let resp = resp.unwrap();

        assert_eq!(resp.usage.input, 60);
        assert_eq!(resp.usage.output, 10);
        assert_eq!(resp.usage.cache_read, 40);
        assert_eq!(resp.stop_reason, Some(StopReason::EndTurn));
        assert!(
            matches!(&resp.message.content[0], ContentBlock::Text { text } if text == "Hello world")
        );

        let deltas: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["Hello", " world"]);
    }

    #[tokio::test]
    async fn parse_sse_tool_calls() {
        let sse = "\
event: response.output_item.added\n\
data: {\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"bash\"}}\n\
\n\
event: response.output_item.added\n\
data: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"c2\",\"name\":\"read\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"output_index\":0,\"delta\":\"{\\\"command\\\": \\\"ls\\\"}\"}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"output_index\":1,\"delta\":\"{\\\"path\\\": \\\"/tmp\\\"}\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n\
\n";

        let (resp, events) = run_sse(sse).await;
        let resp = resp.unwrap();

        let tools: Vec<_> = resp.message.tool_uses().collect();
        assert_eq!(tools.len(), 2);
        assert_eq!((tools[0].0, tools[0].1), ("c1", "bash"));
        assert_eq!(tools[0].2["command"], "ls");
        assert_eq!((tools[1].0, tools[1].1), ("c2", "read"));
        assert_eq!(tools[1].2["path"], "/tmp");
        assert_eq!(resp.stop_reason, Some(StopReason::ToolUse));

        let starts: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::ToolUseStart { id, name } => Some((id.as_str(), name.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec![("c1", "bash"), ("c2", "read")]);
    }

    // Codex hides an overload in `code` on an otherwise healthy 200 stream, so these tags are what
    // decide between backing off and giving up: https://github.com/tontinton/maki/issues/777
    #[test_case("service_unavailable_error", "server_is_overloaded", OVERLOAD_MESSAGE, 529, true ; "codex_overload")]
    #[test_case("overloaded_error", "", OVERLOAD_MESSAGE, 529, true                              ; "overload_without_code")]
    #[test_case("invalid_request_error", "invalid_value", BAD_REQUEST_MESSAGE, 400, false        ; "bad_request")]
    #[tokio::test]
    async fn parse_sse_error_event(
        error_type: &str,
        code: &str,
        message: &str,
        status: u16,
        retryable: bool,
    ) {
        let data = json!({
            "type": "error",
            "error": { "type": error_type, "code": code, "message": message },
        });

        let (result, _) = run_sse(&format!("event: error\ndata: {data}\n\n")).await;
        let err = result.unwrap_err();
        assert_eq!(err.to_string(), format!("API error ({status}): {message}"));
        assert_eq!(err.is_retryable(), retryable);
    }

    #[test_case("rate_limit_exceeded", RATE_LIMIT_MESSAGE, 429 ; "rate_limit")]
    #[test_case("server_is_overloaded", OVERLOAD_MESSAGE, 529  ; "overload")]
    #[tokio::test]
    async fn parse_sse_response_failed(code: &str, message: &str, status: u16) {
        let data = json!({
            "response": { "error": { "code": code, "message": message } },
        });

        let (result, _) = run_sse(&format!("event: response.failed\ndata: {data}\n\n")).await;
        let err = result.unwrap_err();
        assert_eq!(err.to_string(), format!("API error ({status}): {message}"));
        assert!(err.is_retryable());
    }

    #[tokio::test]
    async fn parse_sse_incomplete_response() {
        let sse = "\
event: response.output_text.delta\n\
data: {\"delta\":\"partial\"}\n\
\n\
event: response.incomplete\n\
data: {\"response\":{\"status\":\"incomplete\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\
\n";

        let (resp, _) = run_sse(sse).await;
        let resp = resp.unwrap();
        assert_eq!(resp.stop_reason, Some(StopReason::MaxTokens));
        assert!(
            matches!(&resp.message.content[0], ContentBlock::Text { text } if text == "partial")
        );
    }

    #[test_case("gpt-6.2", true ; "exact")]
    #[test_case("代理-GpT-6.20-preview", true ; "mixed_case_substring")]
    #[test_case("gpt-6.1-sol", false ; "older_model")]
    #[test_case("gpt-６.2", false ; "non_ascii_lookalike")]
    fn build_body_places_system_prompt_before_history(model_id: &str, developer_prompt: bool) {
        const SYSTEM: &str = "You are a coding agent.";

        let model = Model::from_spec(&format!("responses/{model_id}")).unwrap();
        let messages = [
            Message::user("hello".to_string()),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "thinking...".to_string(),
                    },
                    ContentBlock::tool_use("tc_1", "bash", json!({"command": "ls"})),
                ],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::tool_result("tc_1", "file.txt", false)],
                ..Default::default()
            },
        ];

        let body = build_body(&model, &messages, SYSTEM, &json!([]), false);
        let items = body["input"].as_array().unwrap();
        let items = if developer_prompt {
            assert!(body.get("instructions").is_none());
            assert_eq!(
                items[0],
                json!({
                    "type": "message",
                    "role": "developer",
                    "content": [{"type": "input_text", "text": SYSTEM}]
                })
            );
            &items[1..]
        } else {
            assert_eq!(body["instructions"], SYSTEM);
            items.as_slice()
        };
        assert_eq!(items.len(), 4);

        assert_eq!(items[0]["type"], "message");
        assert_eq!(items[0]["role"], "user");
        assert_eq!(items[0]["content"][0]["type"], "input_text");
        assert_eq!(items[0]["content"][0]["text"], "hello");

        assert_eq!(items[1]["type"], "message");
        assert_eq!(items[1]["role"], "assistant");
        assert_eq!(items[1]["content"][0]["type"], "output_text");
        assert_eq!(items[1]["content"][0]["text"], "thinking...");

        assert_eq!(items[2]["type"], "function_call");
        assert_eq!(items[2]["call_id"], "tc_1");
        assert_eq!(items[2]["name"], "bash");

        assert_eq!(items[3]["type"], "function_call_output");
        assert_eq!(items[3]["call_id"], "tc_1");
        assert_eq!(items[3]["output"], "file.txt");
    }

    #[test_case("gpt-6.2", "", None ; "empty_matching_prompt")]
    #[test_case("gpt-6.2", " \n\t", None ; "blank_matching_prompt")]
    #[test_case("gpt-6.1-sol", "", Some("") ; "empty_older_prompt")]
    fn build_body_handles_blank_system_prompt(
        model_id: &str,
        system: &str,
        instructions: Option<&str>,
    ) {
        let model = Model::from_spec(&format!("responses/{model_id}")).unwrap();
        let body = build_body(&model, &[], system, &json!([]), false);

        assert_eq!(
            body.get("instructions"),
            instructions.map(Value::from).as_ref()
        );
        assert_eq!(body["input"], json!([]));
    }

    #[test_case("System prompt.", "Prefix.\n\nSystem prompt." ; "prefix_and_system")]
    #[test_case("", "Prefix.\n\n" ; "prefix_only")]
    #[tokio::test]
    async fn responses_stream_carries_prefixed_developer_prompt(system: &str, expected: &str) {
        use crate::test_support::{Canned, serve};

        const PREFIX: &str = "Prefix.";
        const SCRIPT: &[Canned] = &[Canned::json(401, r#"{"error":{"message":"no"}}"#)];

        let (base_url, requests) = serve(SCRIPT);
        let auth = ResolvedAuth::bearer("responses", "sk-test")
            .unwrap()
            .with_base_url(Some(base_url));
        let provider = ResponsesProvider::new(
            "responses".to_string(),
            None,
            Arc::new(Mutex::new(auth)),
            Timeouts::default(),
        )
        .with_system_prefix(Some(PREFIX.to_string()));
        let model = Model::from_spec("responses/gpt-6.2").unwrap();
        let (tx, _rx) = unbounded_channel();
        let _ = provider
            .stream_message(
                &model,
                &[],
                system,
                &json!([]),
                &tx,
                RequestOptions::default(),
                None,
            )
            .await;

        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        let body = recorded[0].json();
        assert!(body.get("instructions").is_none());
        assert_eq!(body["input"][0]["role"], "developer");
        assert_eq!(body["input"][0]["content"][0]["text"], expected);
    }

    const FRAME_MODEL: &str = "responses/proxy/GpT-6.2-sol";
    const FRAME_PROMPT: &str = "Follow the system prompt.";
    const FRAME_USER_TEXT: &str = "Read the file.";
    const COMPLETE: Canned = Canned::sse(
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
    );

    fn frame_provider(base_url: String) -> ResponsesProvider {
        let auth = ResolvedAuth::bearer("responses", "sk-test")
            .unwrap()
            .with_base_url(Some(base_url));
        ResponsesProvider::new(
            "responses".to_owned(),
            None,
            Arc::new(Mutex::new(auth)),
            Timeouts::default(),
        )
    }

    fn frame_user(frame: MakiId, effort: Effort) -> Message {
        let mut message = Message::user(FRAME_USER_TEXT.to_owned());
        message
            .content
            .insert(0, ContentBlock::ReasoningEffort { frame, effort });
        message
    }

    #[tokio::test]
    async fn reasoning_frame_keeps_the_request_prefix_through_switches_and_tool_round() {
        const TURNS: [(Effort, bool); 4] = [
            (Effort::Low, false),
            (Effort::High, true),
            (Effort::High, false),
            (Effort::Low, true),
        ];
        const SCRIPT: &[Canned] = &[COMPLETE; TURNS.len() + 1];
        let (base_url, requests) = serve(SCRIPT);
        let provider = frame_provider(base_url);
        let model = Model::from_spec(FRAME_MODEL).unwrap();
        let frame = ReasoningFrame {
            id: MakiId::generate(),
            model: model.spec(),
            effort: Effort::Low,
        };
        let (tx, _rx) = unbounded_channel();
        let mut messages = Vec::new();
        for (effort, update) in TURNS {
            messages.push(if update {
                frame_user(frame.id, effort)
            } else {
                Message::user(FRAME_USER_TEXT.to_owned())
            });
            provider
                .stream_message_with_reasoning_frame(
                    &model,
                    &messages,
                    FRAME_PROMPT,
                    &json!([]),
                    &tx,
                    RequestOptions {
                        thinking: ThinkingConfig::Effort(effort),
                        fast: false,
                    },
                    None,
                    Some(&frame),
                )
                .await
                .unwrap();
            messages.push(Message::empty_marker());
        }
        messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::tool_use("call", TOOL_NAME, json!({}))],
            ..Default::default()
        });
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::tool_result("call", "42", false)],
            ..Default::default()
        });
        provider
            .stream_message_with_reasoning_frame(
                &model,
                &messages,
                FRAME_PROMPT,
                &json!([]),
                &tx,
                RequestOptions::default(),
                None,
                Some(&frame),
            )
            .await
            .unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), SCRIPT.len());
        let mut previous_input = Vec::new();
        for request in requests.iter() {
            let body = request.json();
            assert_eq!(body["reasoning"], json!({"effort":"low", "summary":"auto"}));
            assert_eq!(body["store"], false);
            let input = body["input"].as_array().unwrap();
            assert!(input.starts_with(&previous_input));
            previous_input.clone_from(input);
        }
        let updates: Vec<_> = previous_input
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                (item["type"] == CONFIGURATION_UPDATE)
                    .then_some((index, &item["reasoning"]["effort"]))
            })
            .collect();
        assert_eq!(updates, [(3, &json!("high")), (8, &json!("low"))]);
        for (index, _) in updates {
            assert_eq!(previous_input[index + 1]["role"], "user");
        }
        assert_eq!(
            previous_input.last().unwrap()["type"],
            "function_call_output"
        );
    }

    #[test_case(FRAME_MODEL, ThinkingConfig::Off, None ; "empty")]
    #[test_case(FRAME_MODEL, ThinkingConfig::Adaptive, Some(Effort::Medium) ; "adaptive")]
    #[test_case(FRAME_MODEL, ThinkingConfig::Effort(Effort::Minimal), Some(Effort::Low) ; "snapped")]
    #[test_case("responses/gpt-6.1-sol", ThinkingConfig::Effort(Effort::High), None ; "other_model")]
    #[test_case("responses/gpt-６.2", ThinkingConfig::Effort(Effort::High), None ; "non_ascii_lookalike")]
    fn configuration_effort_uses_explicit_native_levels(
        spec: &str,
        thinking: ThinkingConfig,
        expected: Option<Effort>,
    ) {
        let model = Model::from_spec(spec).unwrap();
        let provider = frame_provider(String::new());
        assert_eq!(
            provider.configuration_effort(
                &model,
                RequestOptions {
                    thinking,
                    fast: false
                }
            ),
            expected
        );
    }

    #[test_case(json!({"reasoning":{"effort":"high","summary":"detailed"}}), Some(Effort::High) ; "native_summary")]
    #[test_case(json!({"reasoning":{"effort":"high","summary":"auto"}}), None ; "different_summary")]
    #[test_case(json!({"reasoning":{"effort":"high","summary":"detailed"},"thinking":true}), None ; "additional_setting")]
    #[test_case(json!({"reasoning_effort":"high"}), None ; "non_native")]
    #[tokio::test]
    async fn reasoning_frame_preserves_declared_native_summary(
        fragment: Value,
        expected: Option<Effort>,
    ) {
        const SCRIPT: &[Canned] = &[COMPLETE];
        let (base_url, requests) = serve(SCRIPT);
        let provider = frame_provider(base_url);
        let mut model = Model::from_spec(FRAME_MODEL).unwrap();
        model.thinking_fields = Some(Box::new(ThinkingFields {
            levels: [
                (
                    Effort::Low,
                    json!({"reasoning":{"effort":"low","summary":"detailed"}})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                (Effort::High, fragment.as_object().unwrap().clone()),
            ]
            .into(),
            ..Default::default()
        }));
        let frame = ReasoningFrame {
            id: MakiId::generate(),
            model: model.spec(),
            effort: Effort::Low,
        };
        let (tx, _rx) = unbounded_channel();
        let opts = RequestOptions {
            thinking: ThinkingConfig::Effort(Effort::High),
            fast: false,
        };
        assert_eq!(provider.configuration_effort(&model, opts), expected);
        provider
            .stream_message_with_reasoning_frame(
                &model,
                &[frame_user(frame.id, Effort::High)],
                FRAME_PROMPT,
                &json!([]),
                &tx,
                opts,
                None,
                expected.map(|_| &frame),
            )
            .await
            .unwrap();
        let body = requests.lock().unwrap()[0].json();
        if expected.is_some() {
            assert_eq!(
                body["reasoning"],
                json!({"effort":"low","summary":"detailed"})
            );
            assert_eq!(body["input"][1]["type"], CONFIGURATION_UPDATE);
        } else {
            for (key, value) in fragment.as_object().unwrap() {
                assert_eq!(&body[key], value);
            }
            assert_eq!(body["input"][1]["role"], "user");
        }
    }

    #[test_case(FRAME_MODEL, true ; "matching_model")]
    #[test_case("responses/gpt-6.1-sol", false ; "other_model")]
    #[tokio::test]
    async fn reasoning_frame_filters_model_and_message_boundaries(spec: &str, supported: bool) {
        const SCRIPT: &[Canned] = &[COMPLETE; 2];
        let (base_url, requests) = serve(SCRIPT);
        let provider = frame_provider(base_url);
        let model = Model::from_spec(spec).unwrap();
        let frame = ReasoningFrame {
            id: MakiId::generate(),
            model: model.spec(),
            effort: Effort::Low,
        };
        let mut late = Message::user(FRAME_USER_TEXT.to_owned());
        late.content.push(ContentBlock::ReasoningEffort {
            frame: frame.id,
            effort: Effort::High,
        });
        let mut assistant = frame_user(frame.id, Effort::High);
        assistant.role = Role::Assistant;
        let messages = [
            frame_user(MakiId::generate(), Effort::High),
            late,
            assistant,
            frame_user(frame.id, Effort::High),
        ];
        let (tx, _rx) = unbounded_channel();
        for current in [None, Some(&frame)] {
            provider
                .stream_message_with_reasoning_frame(
                    &model,
                    &messages,
                    FRAME_PROMPT,
                    &json!([]),
                    &tx,
                    RequestOptions::default(),
                    None,
                    current,
                )
                .await
                .unwrap();
        }
        let requests = requests.lock().unwrap();
        let no_frame = requests[0].json();
        if supported {
            assert!(no_frame.get("reasoning").is_none());
        }
        assert!(
            !no_frame["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == CONFIGURATION_UPDATE)
        );
        let with_frame = requests[1].json();
        assert_eq!(
            with_frame["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["type"] == CONFIGURATION_UPDATE)
                .count(),
            usize::from(supported)
        );
        if !supported {
            assert_eq!(with_frame, no_frame);
        }
    }

    struct ReasoningHook(Option<&'static str>);

    impl super::super::codec::BodyHook for ReasoningHook {
        fn call<'a>(
            &'a self,
            mut body: Value,
            ctx: &super::super::codec::RequestCtx<'_>,
        ) -> BoxFuture<'a, Result<Value, AgentError>> {
            assert_eq!(ctx.opts.thinking, ThinkingConfig::Effort(Effort::High));
            body["metadata"] = json!({"source": "hook"});
            if let Some(effort) = self.0 {
                body["reasoning"]["effort"] = json!(effort);
            }
            Box::pin(async move { Ok(body) })
        }
    }

    #[test_case(None, "low", true ; "unchanged_reasoning")]
    #[test_case(Some("medium"), "medium", false ; "rewritten_reasoning")]
    #[tokio::test]
    async fn reasoning_frame_preserves_hook_options(
        change: Option<&'static str>,
        effort: &str,
        replay: bool,
    ) {
        const SCRIPT: &[Canned] = &[COMPLETE];
        let (base_url, requests) = serve(SCRIPT);
        let provider =
            frame_provider(base_url).with_body_hook(Some(Arc::new(ReasoningHook(change))));
        let model = Model::from_spec(FRAME_MODEL).unwrap();
        let frame = ReasoningFrame {
            id: MakiId::generate(),
            model: model.spec(),
            effort: Effort::Low,
        };
        let (tx, _rx) = unbounded_channel();
        provider
            .stream_message_with_reasoning_frame(
                &model,
                &[frame_user(frame.id, Effort::High)],
                FRAME_PROMPT,
                &json!([]),
                &tx,
                RequestOptions {
                    thinking: ThinkingConfig::Effort(Effort::High),
                    fast: false,
                },
                None,
                Some(&frame),
            )
            .await
            .unwrap();
        let body = requests.lock().unwrap()[0].json();
        assert_eq!(body["metadata"]["source"], "hook");
        assert_eq!(body["reasoning"]["effort"], effort);
        assert_eq!(body["input"][1]["type"] == CONFIGURATION_UPDATE, replay);
    }

    #[test]
    fn convert_input_replays_past_reasoning() {
        let assistant = |reasoning_id: Option<String>| Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "weighing it".to_string(),
                    signature: None,
                    duration_ms: None,
                    reasoning_id,
                },
                ContentBlock::Text {
                    text: "answer".to_string(),
                },
            ],
            ..Default::default()
        };
        let messages = [Message::user("hello".to_string()), assistant(None)];

        let input = convert_input(&messages, true, None, None);
        let items = input.as_array().unwrap();

        assert_eq!(items[1]["type"], "reasoning");
        assert_eq!(items[1]["summary"], json!([]));
        assert_eq!(items[1]["content"][0]["type"], "reasoning_text");
        assert_eq!(items[1]["content"][0]["text"], "weighing it");
        assert!(items[1].get("id").is_none());
        assert_eq!(items[2]["type"], "message");
        assert_eq!(items[2]["role"], "assistant");
        assert_eq!(items[2]["content"][0]["text"], "answer");

        let dropped = convert_input(&messages, false, None, None);
        assert_eq!(dropped[1]["type"], "message");
        assert_eq!(dropped[1]["role"], "assistant");

        let with_id = [
            Message::user("hello".to_string()),
            assistant(Some("rs_1".to_string())),
        ];
        let input = convert_input(&with_id, true, None, None);
        assert_eq!(input[1]["id"], "rs_1");
    }

    #[test]
    fn convert_input_blank_tool_result_gets_marker() {
        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::tool_result("tc_1", "", false)],
            ..Default::default()
        }];

        let input = convert_input(&messages, false, None, None);
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["output"], crate::types::EMPTY_RESPONSE_MARKER);
    }

    #[tokio::test]
    async fn parse_sse_reasoning_text_delta() {
        let sse = "\
event: response.output_item.added\n\
data: {\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[],\"content\":[],\"encrypted_content\":\"\",\"status\":\"in_progress\"}}\n\
\n\
event: response.reasoning_text.delta\n\
data: {\"delta\":\"Let me think\"}\n\
\n\
event: response.reasoning_text.delta\n\
data: {\"delta\":\" about this\"}\n\
\n\
event: response.output_item.added\n\
data: {\"output_index\":1,\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"status\":\"in_progress\",\"content\":[],\"role\":\"assistant\"}}\n\
\n\
event: response.output_text.delta\n\
data: {\"delta\":\"Hello world\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":20,\"input_tokens_details\":{\"cached_tokens\":10},\"output_tokens_details\":{\"reasoning_tokens\":5}}}}\n\
\n";

        let (resp, events) = run_sse(sse).await;
        let resp = resp.unwrap();

        assert_eq!(resp.usage.input, 90);
        assert_eq!(resp.usage.output, 20);
        assert_eq!(resp.usage.cache_read, 10);

        assert_eq!(resp.message.content.len(), 2);
        assert!(
            matches!(&resp.message.content[0], ContentBlock::Thinking { thinking, reasoning_id, .. } if thinking == "Let me think about this" && reasoning_id.as_deref() == Some("rs_1"))
        );
        assert!(
            matches!(&resp.message.content[1], ContentBlock::Text { text } if text == "Hello world")
        );

        let thinking_deltas: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::ThinkingDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(thinking_deltas, vec!["Let me think", " about this"]);

        let text_deltas: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text_deltas, vec!["Hello world"]);
    }

    #[tokio::test]
    async fn parse_sse_reasoning_summary_text_delta() {
        let sse = "\
event: response.reasoning_summary_text.delta\n\
data: {\"delta\":\"Summary part\"}\n\
\n\
event: response.output_text.delta\n\
data: {\"delta\":\"Answer\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\
\n";

        let (resp, events) = run_sse(sse).await;
        let resp = resp.unwrap();

        assert!(
            matches!(&resp.message.content[0], ContentBlock::Thinking { thinking, .. } if thinking == "Summary part")
        );

        let thinking_deltas: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::ThinkingDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(thinking_deltas, vec!["Summary part"]);
    }

    #[tokio::test]
    async fn parse_sse_reasoning_only_no_text() {
        let sse = "\
event: response.reasoning_text.delta\n\
data: {\"delta\":\"Thinking only\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"output_tokens_details\":{\"reasoning_tokens\":5}}}}\n\
\n";

        let (resp, _) = run_sse(sse).await;
        let resp = resp.unwrap();

        assert_eq!(resp.message.content.len(), 1);
        assert!(
            matches!(&resp.message.content[0], ContentBlock::Thinking { thinking, .. } if thinking == "Thinking only")
        );
        assert_eq!(resp.usage.output, 5);
    }

    #[tokio::test]
    async fn parse_sse_malformed_tool_json_yields_empty_object() {
        let sse = "\
event: response.output_item.added\n\
data: {\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"bash\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"delta\":\"{broken\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\
\n";

        let (resp, _) = run_sse(sse).await;
        let resp = resp.unwrap();
        let tools: Vec<_> = resp.message.tool_uses().collect();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].1, "bash");
        assert_eq!(*tools[0].2, Value::Object(Default::default()));
    }

    // llama.cpp's /v1/responses endpoint omits output_index in SSE events
    // (see https://github.com/ggml-org/llama.cpp/issues/20607)

    #[tokio::test]
    async fn parse_sse_tool_call_without_output_index() {
        let sse = "\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"bash\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"delta\":\"{\\\"command\\\": \\\"ls\\\"}\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n\
\n";

        let (resp, _) = run_sse(sse).await;
        let resp = resp.unwrap();

        let tools: Vec<_> = resp.message.tool_uses().collect();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].0, "c1");
        assert_eq!(tools[0].1, "bash");
        assert_eq!(tools[0].2["command"], "ls");
        assert_eq!(resp.stop_reason, Some(StopReason::ToolUse));
    }

    #[tokio::test]
    async fn parse_sse_sequential_tool_calls_without_output_index() {
        // Simulates llama.cpp streaming two sequential tool calls without output_index
        let sse = "\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"bash\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"delta\":\"{\\\"command\\\": \\\"ls\\\"}\"}\n\
\n\
event: response.output_item.done\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"bash\",\"arguments\":\"{\\\"command\\\": \\\"ls\\\"}\"}}\n\
\n\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c2\",\"name\":\"read\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"delta\":\"{\\\"path\\\": \\\"/tmp\\\"}\"}\n\
\n\
event: response.output_item.done\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c2\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\": \\\"/tmp\\\"}\"}}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n\
\n";

        let (resp, _) = run_sse(sse).await;
        let resp = resp.unwrap();

        let tools: Vec<_> = resp.message.tool_uses().collect();
        assert_eq!(tools.len(), 2);
        assert_eq!((tools[0].0, tools[0].1), ("c1", "bash"));
        assert_eq!(tools[0].2["command"], "ls");
        assert_eq!((tools[1].0, tools[1].1), ("c2", "read"));
        assert_eq!(tools[1].2["path"], "/tmp");
    }

    #[tokio::test]
    async fn parse_sse_tool_done_without_output_index_updates_last_acc() {
        // done event without output_index should update the last accumulator
        let sse = "\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"glob\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"delta\":\"{\\\"pattern\\\": \\\"*.rs\\\"}\"}\n\
\n\
event: response.output_item.done\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"glob\",\"arguments\":\"{\\\"pattern\\\": \\\"*.rs\\\", \\\"path\\\": \\\"src\\\"}\"}}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n\
\n";

        let (resp, _) = run_sse(sse).await;
        let resp = resp.unwrap();

        let tools: Vec<_> = resp.message.tool_uses().collect();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].1, "glob");
        assert_eq!(tools[0].2["pattern"], "*.rs");
        assert_eq!(tools[0].2["path"], "src");
    }

    #[tokio::test]
    async fn parse_sse_prompt_progress_events() {
        let sse = "\
event: response.in_progress\n\
data: {\"prompt_progress\":{\"processed\":100,\"total\":1000,\"cache\":50}}\n\
\n\
event: response.in_progress\n\
data: {\"prompt_progress\":{\"processed\":500,\"total\":1000,\"cache\":50}}\n\
\n\
event: response.output_text.delta\n\
data: {\"delta\":\"Hello\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}\n\
\n";

        let (_resp, events) = run_sse(sse).await;

        let progress: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::PromptProgress {
                    processed,
                    total,
                    cache,
                } => Some((*processed, *total, *cache)),
                _ => None,
            })
            .collect();
        assert_eq!(progress, vec![(100, 1000, 50), (500, 1000, 50)]);
    }

    #[tokio::test]
    async fn parse_sse_done_arguments_as_json_object() {
        let sse = "\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"read\"}}\n\
\n\
event: response.output_item.done\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"read\",\"arguments\":{\"path\":\"/tmp/file.txt\"}}}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}
\
\n";

        let (resp, _) = run_sse(sse).await;
        let resp = resp.unwrap();

        let tools: Vec<_> = resp.message.tool_uses().collect();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].1, "read");
        assert_eq!(tools[0].2["path"], "/tmp/file.txt");
    }

    #[tokio::test]
    async fn parse_sse_reasoning_summary_part_added() {
        let sse = "\
event: response.reasoning_summary_part.added\n\
data: {\"id\":\"sp_1\"}\n\
\n\
event: response.reasoning_summary_text.delta\n\
data: {\"delta\":\"First part\"}\n\
\n\
event: response.reasoning_summary_part.added\n\
data: {\"id\":\"sp_2\"}\n\
\n\
event: response.reasoning_summary_text.delta\n\
data: {\"delta\":\"Second part\"}\n\
\n\
event: response.output_text.delta\n\
data: {\"delta\":\"Answer\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\
\n";

        let (resp, events) = run_sse(sse).await;
        let resp = resp.unwrap();
        let thinking = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::ThinkingDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();

        const EXPECTED: &str = "First part\n\nSecond part";
        assert_eq!(thinking, EXPECTED);
        assert!(
            matches!(&resp.message.content[0], ContentBlock::Thinking { thinking, .. } if thinking == EXPECTED)
        );
    }

    #[tokio::test]
    async fn parse_sse_delta_arguments_as_json_object() {
        let sse = "\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"grep\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"delta\":{\"pattern\":\"TODO\",\"path\":\"src\"}}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}
\
\n";

        let (resp, _) = run_sse(sse).await;
        let resp = resp.unwrap();

        let tools: Vec<_> = resp.message.tool_uses().collect();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].1, "grep");
        assert_eq!(tools[0].2["pattern"], "TODO");
        assert_eq!(tools[0].2["path"], "src");
    }

    #[tokio::test]
    async fn parse_sse_done_object_args_overrides_empty_delta() {
        let sse = "\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"edit\"}}\n\
\n\
event: response.output_item.done\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"edit\",\"arguments\":{\"path\":\"foo.rs\",\"old_string\":\"a\",\"new_string\":\"b\"}}}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}
\
\n";

        let (resp, _) = run_sse(sse).await;
        let resp = resp.unwrap();

        let tools: Vec<_> = resp.message.tool_uses().collect();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].1, "edit");
        assert_eq!(tools[0].2["path"], "foo.rs");
        assert_eq!(tools[0].2["old_string"], "a");
        assert_eq!(tools[0].2["new_string"], "b");
    }

    #[tokio::test]
    async fn parse_sse_no_reasoning_tokens_in_usage() {
        let sse = "\
event: response.output_text.delta\n\
data: {\"delta\":\"Hello\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":40}}}}\n\
\n";

        let (resp, _) = run_sse(sse).await;
        let resp = resp.unwrap();

        assert_eq!(resp.usage.input, 60);
        assert_eq!(resp.usage.output, 10);
        assert_eq!(resp.usage.cache_read, 40);
    }

    #[test_case(ThinkingConfig::Adaptive, "gpt-5.3-codex", "medium" ; "adaptive_codex")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Minimal), "gpt-5.3-codex", "low" ; "minimal_snaps_to_low_on_codex")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Low), "gpt-5.3-codex", "low" ; "low_codex")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Medium), "gpt-5.3-codex", "medium" ; "medium_codex")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::High), "gpt-5.3-codex", "high" ; "high_codex")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::XHigh), "gpt-5.3-codex", "xhigh" ; "xhigh_codex")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Max), "gpt-5.3-codex", "xhigh" ; "max_snaps_to_xhigh_on_codex")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::XHigh), "gpt-5.1-codex", "high" ; "xhigh_snaps_to_high_on_5_1")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::XHigh), "gpt-5.1-codex-max", "xhigh" ; "xhigh_passes_through_on_5_1_max")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Minimal), "gpt-5.5", "minimal" ; "minimal_passes_through_on_5_5")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Max), "gpt-5.5", "xhigh" ; "max_snaps_to_xhigh_on_5_5")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Minimal), "gpt-5.6-sol", "minimal" ; "minimal_passes_through_on_5_6_sol")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Max), "gpt-5.6-sol", "max" ; "max_passes_through_on_5_6_sol")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Max), "gpt-5.6-terra", "max" ; "max_passes_through_on_5_6_terra")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Max), "gpt-5.6-luna", "max" ; "max_passes_through_on_5_6_luna")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Max), "gpt-6-sol", "max" ; "max_passes_through_on_6_sol")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Minimal), "gpt-6-sol", "low" ; "minimal_snaps_to_low_on_6_sol")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Max), "gpt-6-luna", "max" ; "max_passes_through_on_6_luna")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Minimal), "gpt-6-luna", "low" ; "minimal_snaps_to_low_on_6_luna")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Max), "gpt-6-astra", "max" ; "max_passes_through_on_6_astra")]
    #[test_case(ThinkingConfig::Effort(crate::Effort::Minimal), "gpt-6-astra", "low" ; "minimal_snaps_to_low_on_6_astra")]
    #[test_case(ThinkingConfig::Adaptive, "gpt-6-astra", "medium" ; "adaptive_on_6_astra")]
    fn responses_reasoning_uses_responses_effort_object(
        thinking: ThinkingConfig,
        model_id: &str,
        expected: &str,
    ) {
        let model = Model::from_spec(&format!("responses/{model_id}")).unwrap();
        let mut body = json!({});
        let dialect = responses_dialect(&model.id, None);
        apply_responses_reasoning(&mut body, thinking, &model, &dialect);
        assert_eq!(body["reasoning"]["effort"], expected);
    }

    #[test_case("gpt-5.3-codex")]
    #[test_case("gpt-6-sol")]
    #[test_case("gpt-6-luna")]
    #[test_case("gpt-6-astra")]
    fn responses_reasoning_omits_effort_when_disabled(model_id: &str) {
        let model = Model::from_spec(&format!("responses/{model_id}")).unwrap();
        let mut body = json!({});
        let dialect = responses_dialect(&model.id, None);
        apply_responses_reasoning(&mut body, ThinkingConfig::Off, &model, &dialect);
        assert!(body.get("reasoning").is_none());
    }

    #[test_case("gpt-5.5")]
    #[test_case("gpt-5.6-luna")]
    #[test_case("gpt-5.6-terra")]
    #[test_case("gpt-5.6-sol")]
    fn responses_reasoning_sends_none_when_disabled(model_id: &str) {
        let model = Model::from_spec(&format!("responses/{model_id}")).unwrap();
        let mut body = json!({});
        let dialect = responses_dialect(&model.id, None);
        apply_responses_reasoning(&mut body, ThinkingConfig::Off, &model, &dialect);
        assert_eq!(body["reasoning"]["effort"], "none");
        assert!(body["reasoning"].get("summary").is_none());
    }

    #[test]
    fn responses_fast_sets_service_tier() {
        let mut body = json!({});
        apply_responses_fast(&mut body, true);
        assert_eq!(body["service_tier"], SERVICE_TIER_FAST);
    }

    #[test]
    fn responses_fast_off_leaves_tier_to_the_project_default() {
        let mut body = json!({});
        apply_responses_fast(&mut body, false);
        assert!(body.get("service_tier").is_none());
    }

    #[test_case(true, Some(SERVICE_TIER_FAST) ; "fast_on_sends_the_tier")]
    #[test_case(false, None ; "fast_off_omits_the_tier")]
    #[tokio::test]
    async fn responses_stream_carries_service_tier_only_when_fast(
        fast: bool,
        expected: Option<&str>,
    ) {
        use crate::test_support::{Canned, serve};
        use std::sync::{Arc, Mutex};

        const PROMPT: &str = "hi";
        const MISSING_TIER: &str = "service_tier did not match the fast flag on the wire";
        const SCRIPT: &[Canned] = &[Canned::json(401, r#"{"error":{"message":"no"}}"#)];

        let (base_url, requests) = serve(SCRIPT);
        let auth = crate::providers::ResolvedAuth::bearer("responses", "sk-fast")
            .unwrap()
            .with_base_url(Some(base_url));
        let provider = ResponsesProvider::new(
            "responses".to_string(),
            None,
            Arc::new(Mutex::new(auth)),
            Timeouts::default(),
        );
        let model = Model::from_spec("responses/gpt-6.1-sol").unwrap();
        let (tx, _rx) = unbounded_channel();
        let opts = RequestOptions {
            fast,
            ..Default::default()
        };
        let _ = provider
            .stream_message(
                &model,
                &[Message::user(PROMPT.to_owned())],
                "",
                &json!([]),
                &tx,
                opts,
                None,
            )
            .await;

        let recorded = requests.lock().unwrap();
        let [request] = recorded.as_slice() else {
            panic!("{MISSING_TIER}");
        };
        assert_eq!(
            request.json()["service_tier"].as_str(),
            expected,
            "{MISSING_TIER}"
        );
    }

    #[test]
    fn parse_model_info_extracts_reasoning_levels() {
        let raw = json!({
            "id": "custom-thinking-model",
            "context_length": 128000,
            "max_tokens": 16000,
            "supported_reasoning_levels": [
                {"effort": "low"},
                {"effort": "high"},
                {"effort": "none"}
            ]
        });
        let info = parse_model_info(&raw).expect("parses model info");
        assert_eq!(info.id, "custom-thinking-model");
        assert_eq!(info.supports_thinking, Some(true));
        let effort = info.effort.expect("has effort");
        assert_eq!(
            effort.supported,
            vec![crate::Effort::Low, crate::Effort::High]
        );
        assert_eq!(effort.send_off, Some(true));
    }

    #[test]
    fn parse_model_info_curl_magpie_sample() {
        let grok = json!({
            "context_length": 500000,
            "context_window": 500000,
            "id": "group/grok-4.7",
            "max_input_tokens": 500000,
            "max_output_tokens": 500000,
            "modalities": {"input": ["text", "image"]},
            "pricing": {
                "completion": "0.000006",
                "input_cache_read": "0.0000005",
                "prompt": "0.000002"
            },
            "reasoning": true,
            "reasoning_levels": ["low", "medium", "high", "xhigh"],
            "supported_reasoning_levels": [
                {"effort": "low"},
                {"effort": "medium"},
                {"effort": "high"},
                {"effort": "xhigh"}
            ]
        });
        let info = parse_model_info(&grok).expect("parses grok-4.7");
        assert_eq!(info.id, "group/grok-4.7");
        assert_eq!(info.context_window, Some(500000));
        assert_eq!(info.max_output_tokens, Some(500000));
        assert_eq!(info.supports_vision, Some(true));
        assert_eq!(info.supports_thinking, Some(true));
        let pricing = info.pricing.expect("has pricing");
        assert_eq!(pricing.input, 2.0);
        assert_eq!(pricing.output, 6.0);
        assert_eq!(pricing.cache_read, 0.5);
        let effort = info.effort.expect("has effort");
        assert_eq!(
            effort.supported,
            vec![
                crate::Effort::Low,
                crate::Effort::Medium,
                crate::Effort::High,
                crate::Effort::XHigh
            ]
        );

        let composer = json!({
            "id": "cursor/composer-2.5-fast",
            "reasoning": false,
            "reasoning_levels": [],
            "supported_reasoning_levels": []
        });
        let info = parse_model_info(&composer).expect("parses composer");
        assert_eq!(info.supports_thinking, Some(false));
        assert!(info.effort.is_none());
        assert_eq!(info.supports_fast, None);

        let codex = json!({
            "id": "codex/gpt-6.1-sol",
            "context_length": 400000,
            "max_output_tokens": 128000,
            "pricing": {
                "completion": "0.00001",
                "input_cache_read": "0.0000001",
                "input_cache_write": "0.0000025",
                "prompt": "0.000002"
            },
            "service_tiers": [
                {"id": "priority", "name": "Fast"}
            ]
        });
        let info = parse_model_info(&codex).expect("parses codex");
        let pricing = info.pricing.expect("has pricing");
        assert_eq!(pricing.input, 2.0);
        assert_eq!(pricing.output, 10.0);
        assert_eq!(pricing.cache_read, 0.1);
        assert_eq!(pricing.cache_write, 2.5);
        assert!(info.extra.is_some());
        assert_eq!(info.supports_fast, Some(true));
    }

    #[test_case(r#"{"id":"m","service_tiers":[{"id":"fast"}]}"#, Some(true) ; "fast_tier")]
    #[test_case(r#"{"id":"m","service_tiers":[{"id":"priority"}]}"#, Some(true) ; "priority_tier")]
    #[test_case(r#"{"id":"m","service_tiers":[{"id":"default"}]}"#, None ; "other_tiers_only")]
    #[test_case(r#"{"id":"m"}"#, None ; "no_tiers")]
    fn parse_model_info_derives_fast_support(row: &str, expected: Option<bool>) {
        let info = parse_model_info(&serde_json::from_str(row).unwrap()).expect("parses row");
        assert_eq!(info.supports_fast, expected);
    }

    #[test]
    fn parse_model_info_extracts_name_and_display_name() {
        let with_name = json!({
            "id": "gpt-6.1-sol",
            "name": "GPT-6.1 Sol"
        });
        let info = parse_model_info(&with_name).expect("parses model info");
        assert_eq!(info.id, "gpt-6.1-sol");
        assert_eq!(info.name.as_deref(), Some("GPT-6.1 Sol"));

        let with_display_name = json!({
            "id": "claude-opus-4-7",
            "display_name": "Claude Opus 4.7"
        });
        let info = parse_model_info(&with_display_name).expect("parses model info");
        assert_eq!(info.id, "claude-opus-4-7");
        assert_eq!(info.name.as_deref(), Some("Claude Opus 4.7"));

        let without_name = json!({
            "id": "raw-model"
        });
        let info = parse_model_info(&without_name).expect("parses model info");
        assert_eq!(info.id, "raw-model");
        assert_eq!(info.name, None);
    }
}
