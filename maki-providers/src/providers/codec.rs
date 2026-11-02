use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use crate::provider::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use maki_config::providers::Protocol;

use super::ResolvedAuth;
use super::responses::ResponsesProvider;
use crate::AgentError;
use crate::model::{Model, ModelInfo};
use crate::provider::Provider;
use crate::providers::Timeouts;
use crate::spec::{ProviderSpec, RESPONSES_SPEC};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenAiWire {
    pub max_tokens_field: Option<String>,
    pub include_stream_usage: Option<bool>,
    pub thinking: Option<ThinkingWire>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionCarrier {
    Header,
    Body,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffortField;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingWire {
    pub dialect: Option<String>,
}

#[allow(dead_code)]
pub struct RequestCtx<'a> {
    pub model: &'a Model,
    pub opts: &'a crate::RequestOptions,
    pub session: Option<&'a maki_storage::id::SessionRef>,
    pub discovered: Option<ModelInfo>,
}

#[allow(dead_code)]
pub trait BodyHook: Send + Sync {
    fn call<'a>(
        &'a self,
        body: Value,
        ctx: &RequestCtx<'_>,
    ) -> BoxFuture<'a, Result<Value, AgentError>>;
}

#[allow(dead_code)]
pub struct CodecOptions {
    pub protocol: Protocol,
    pub slug: String,
    pub api_key_env: Cow<'static, str>,
    pub base_url: Cow<'static, str>,
    pub provider_name: String,
    pub system_prefix: Option<String>,
    pub openai: OpenAiWire,
    pub build_body: Option<Arc<dyn BodyHook>>,
}

impl CodecOptions {
    pub fn new(protocol: Protocol, slug: String) -> Self {
        Self {
            protocol,
            api_key_env: Cow::Borrowed(""),
            base_url: Cow::Borrowed(""),
            provider_name: slug.clone(),
            slug,
            system_prefix: None,
            openai: OpenAiWire::default(),
            build_body: None,
        }
    }
}

pub(crate) fn protocol_spec(_protocol: Protocol) -> &'static ProviderSpec {
    &RESPONSES_SPEC
}

pub fn build(
    options: CodecOptions,
    auth: Arc<Mutex<ResolvedAuth>>,
    timeouts: Timeouts,
) -> Box<dyn Provider> {
    let base_url = (!options.base_url.is_empty()).then(|| options.base_url.into_owned());
    let provider = ResponsesProvider::new(options.slug, base_url, auth, timeouts)
        .with_body_hook(options.build_body)
        .with_system_prefix(options.system_prefix);
    Box::new(provider)
}
