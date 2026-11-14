use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use crate::headers::HeaderError;
use crate::provider::BoxFuture;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

use maki_config::providers::Protocol;
use maki_storage::id::SessionRef;

use super::ResolvedAuth;
use super::responses::ResponsesProvider;
use crate::AgentError;
use crate::model::{Model, ModelInfo};
use crate::provider::Provider;
use crate::providers::Timeouts;
use crate::spec::{ProviderSpec, RESPONSES_SPEC};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OpenAiWire {
    pub max_tokens_field: Option<String>,
    pub include_stream_usage: Option<bool>,
    pub usage_cost: bool,
    pub thinking: Option<ThinkingWire>,
}

/// Where the session id rides, for a provider that routes a session to the
/// same backend or keys its cache by it: headers, a body field, or both.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionCarrier {
    #[serde(
        skip_serializing_if = "<[_]>::is_empty",
        deserialize_with = "deserialize_session_headers"
    )]
    pub headers: Cow<'static, [Cow<'static, str>]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_field: Option<Cow<'static, str>>,
}

impl SessionCarrier {
    pub(crate) fn apply(
        &self,
        session: &SessionRef,
        body: &mut Value,
        auth: &mut ResolvedAuth,
    ) -> Result<(), HeaderError> {
        if let (Some(field), Some(object)) = (&self.body_field, body.as_object_mut()) {
            object.insert(field.to_string(), json!(session.as_str()));
        }
        self.apply_headers(session, auth)
    }

    pub(crate) fn apply_headers(
        &self,
        session: &SessionRef,
        auth: &mut ResolvedAuth,
    ) -> Result<(), HeaderError> {
        self.headers
            .iter()
            .try_for_each(|name| auth.set_header(name, session.as_str()))
    }
}

fn deserialize_session_headers<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Cow<'static, [Cow<'static, str>]>, D::Error> {
    Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|name| Ok(Cow::Owned(name.to_ascii_lowercase())))
        .collect()
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
    pub session_id: SessionCarrier,
    pub replay_reasoning: bool,
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
            session_id: SessionCarrier::default(),
            replay_reasoning: false,
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
        .with_system_prefix(options.system_prefix)
        .with_session_id(options.session_id)
        .with_replay_reasoning(options.replay_reasoning);
    Box::new(provider)
}
