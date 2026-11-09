use std::sync::{Arc, Mutex};

use maki_config::providers::{
    ModelDef, ProviderDef, ProvidersConfig, resolve_api_key_env, resolve_base_url,
};

use super::ResolvedAuth;
use super::responses::ResponsesProvider;
use crate::AgentError;
use crate::model::{FastPricing, Model, ModelEntry, ModelInfo, ModelPricing, ModelTier};
use crate::model_registry::{self, DeclaredTiers};
use crate::provider::Provider;
use crate::providers::Timeouts;
use crate::spec::{ProviderSpec, RESPONSES_SPEC};

pub const THINKING_FIELD: &str = "thinking";

pub fn base_spec(slug: &str) -> Option<&'static ProviderSpec> {
    if slug == RESPONSES_SPEC.slug || ProvidersConfig::load().get(slug).is_some() {
        Some(&RESPONSES_SPEC)
    } else {
        None
    }
}

pub(crate) fn is_declared(slug: &str) -> bool {
    slug == RESPONSES_SPEC.slug || ProvidersConfig::load_or_default().get(slug).is_some()
}

fn resolve_custom_auth(slug: &str) -> Result<(ResolvedAuth, String), AgentError> {
    let config = ProvidersConfig::load();
    let def = config.get(slug);
    if def.is_none() && slug != RESPONSES_SPEC.slug {
        return Err(AgentError::Config {
            message: format!("unknown custom provider '{slug}'"),
        });
    }

    let env_var = resolve_api_key_env(slug, def);
    let pool = super::KeyPool::resolve(slug, &env_var).ok();
    let key = pool.map(|p| p.current().to_string()).unwrap_or_default();

    Ok((
        ResolvedAuth::bearer(slug, &key)?.with_base_url(resolve_base_url(slug, def)),
        env_var,
    ))
}

pub fn create(slug: &str, timeouts: Timeouts) -> Result<Box<dyn Provider>, AgentError> {
    let (resolved, _) = resolve_custom_auth(slug)?;
    let base_url = resolved.base_url.clone();
    let auth = Arc::new(Mutex::new(resolved));
    Ok(Box::new(ResponsesProvider::new(
        slug.to_string(),
        base_url,
        auth,
        timeouts,
    )))
}

pub fn lookup_model(slug: &str, model_id: &str) -> Option<Model> {
    let config = ProvidersConfig::load();
    if let Some(def) = config.get(slug) {
        Some(model_from_def(def, &RESPONSES_SPEC, slug, model_id))
    } else if slug == RESPONSES_SPEC.slug {
        let empty_def = ProviderDef::default();
        Some(model_from_def(&empty_def, &RESPONSES_SPEC, slug, model_id))
    } else {
        None
    }
}

impl From<&ModelDef> for ModelEntry {
    fn from(def: &ModelDef) -> Self {
        Self {
            prefixes: vec![def.id.clone()],
            tier: def.tier,
            supports_tool_examples: def.supports_tool_examples,
            supports_thinking: def.supports_thinking,
            requires_thinking: def.requires_thinking.unwrap_or(false),
            supports_vision: def.supports_vision,
            supports_deferred_tools: None,
            max_output_tokens: def.max_output_tokens,
            context_window: def.context_window,
            pricing: def.has_pricing().then(|| ModelPricing {
                input: def.pricing_input.unwrap_or(0.0),
                output: def.pricing_output.unwrap_or(0.0),
                cache_write: def.pricing_cache_write.unwrap_or(0.0),
                cache_read: def.pricing_cache_read.unwrap_or(0.0),
                fast: def.has_fast_pricing().then(|| FastPricing {
                    input: def.pricing_fast_input.unwrap_or(0.0),
                    output: def.pricing_fast_output.unwrap_or(0.0),
                }),
            }),
            thinking_fields: def.thinking_fields.clone(),
            family: None,
            default: false,
        }
    }
}

fn model_from_def(
    def: &ProviderDef,
    base: &'static ProviderSpec,
    slug: &str,
    model_id: &str,
) -> Model {
    let subsidy_source = def.subsidised_by.as_deref();
    let def_model = def.models.iter().find(|m| m.id == model_id);
    let mut row = def_model.map(ModelEntry::from).unwrap_or_default();
    let discovered = model_registry::discovered(slug, model_id);
    let discovered = discovered.as_ref();
    row.max_output_tokens = row
        .max_output_tokens
        .or_else(|| discovered.and_then(|d| d.max_output_tokens))
        .or(base.fallback_max_output);
    row.context_window = row
        .context_window
        .or_else(|| discovered.and_then(|d| d.context_window))
        .or(Some(base.fallback_context_window));
    row.supports_thinking = row
        .supports_thinking
        .or_else(|| discovered.and_then(|d| d.supports_thinking))
        .or(Some(base.supports_thinking));
    row.requires_thinking = row.requires_thinking
        || discovered
            .and_then(|d| d.effort.as_ref())
            .is_some_and(|e| e.send_off == Some(false) && !e.supported.is_empty());
    row.supports_vision = row
        .supports_vision
        .or_else(|| discovered.and_then(|d| d.supports_vision))
        .or(Some(true));

    let pricing = row
        .pricing
        .take()
        .or_else(|| discovered.and_then(|d| d.pricing.clone()))
        .unwrap_or_default();
    row.pricing = Some(pricing);
    let mut model = row.to_model(slug, base, model_id.to_string());
    model.subsidised_by = subsidy_source.map(Arc::from);
    if let Some(d) = discovered {
        if d.pricing.as_ref().is_some_and(ModelPricing::is_zero) {
            model.discovered_free = true;
        }
        if let Some(extra) = &d.extra
            && extra
                .get("service_tiers")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|tiers| {
                    tiers
                        .iter()
                        .any(|t| t["id"] == "priority" || t["id"] == "fast")
                })
        {
            model.supports_fast_override = Some(crate::model::FastSupport::Supported);
        }
    }
    model.name = def_model
        .and_then(|m| m.name.clone())
        .or_else(|| discovered.and_then(|d| d.name.clone()));
    model
}

pub fn declared_model_specs() -> Vec<String> {
    declared_specs_from(&ProvidersConfig::load())
}

fn declared_specs_from(config: &ProvidersConfig) -> Vec<String> {
    let mut specs = Vec::new();
    let mut slugs: Vec<_> = config.providers.keys().collect();
    slugs.sort();
    for slug in slugs {
        if let Some(def) = config.providers.get(slug) {
            for model in &def.models {
                specs.push(format!("{slug}/{}", model.id));
            }
        }
    }
    specs
}

fn is_builtin_slug(slug: &str) -> bool {
    slug == RESPONSES_SPEC.slug
}

/// The declared specs, plus the providers whose list they fully describe.
/// A `discover_models` provider is left out of the second list: its declared
/// specs show before discovery finishes, but only a successful probe proves
/// which models it still serves.
pub fn declared_batch() -> (Vec<String>, Vec<String>) {
    declared_batch_from(&ProvidersConfig::load())
}

fn declared_batch_from(config: &ProvidersConfig) -> (Vec<String>, Vec<String>) {
    let specs = declared_specs_from(config);
    let mut providers: Vec<String> = config
        .providers
        .iter()
        .filter(|(slug, def)| !def.discover_models && !is_builtin_slug(slug))
        .map(|(slug, _)| (*slug).clone())
        .collect();
    providers.sort();
    (specs, providers)
}

/// Models a custom provider can start on: its own `default_model`, then one
/// declared model per tier. Never the protocol's default model, because a local
/// server or proxy rarely serves it. Sorted by slug, since `providers.toml` is a
/// map and startup should pick the same provider every run.
pub fn startup_specs(tiers: &[ModelTier]) -> Vec<String> {
    startup_specs_from(&ProvidersConfig::load(), tiers)
}

fn startup_specs_from(config: &ProvidersConfig, tiers: &[ModelTier]) -> Vec<String> {
    let mut specs = Vec::new();
    let mut slugs: Vec<_> = config.providers.keys().collect();
    slugs.sort();
    for tier in tiers {
        for slug in &slugs {
            if let Some(def) = config.providers.get(*slug)
                && let Some(m) = def.models.iter().find(|m| m.tier == *tier)
            {
                specs.push(format!("{slug}/{}", m.id));
            }
        }
    }
    if specs.is_empty() {
        for slug in &slugs {
            if let Some(def) = config.providers.get(*slug)
                && let Some(m) = def.models.first()
            {
                specs.push(format!("{slug}/{}", m.id));
            }
        }
    }
    specs
}

pub(crate) fn declared_tiers() -> DeclaredTiers {
    declared_tiers_from(&ProvidersConfig::load_or_default())
}

fn declared_tiers_from(config: &ProvidersConfig) -> DeclaredTiers {
    let mut declared = DeclaredTiers::new();
    let mut slugs: Vec<_> = config.providers.keys().collect();
    slugs.sort();
    for slug in slugs {
        if let Some(def) = config.providers.get(slug) {
            for model in &def.models {
                declared
                    .entry(slug.clone())
                    .or_default()
                    .entry(model.tier)
                    .or_insert_with(|| model.id.clone());
            }
        }
    }
    declared
}

pub enum TierLookup {
    Model(Model),
    NoModelForTier,
    Unknown,
}

pub fn resolve_tier(slug: &str, tier: ModelTier) -> TierLookup {
    let config = ProvidersConfig::load();
    let Some(def) = config.get(slug) else {
        return TierLookup::Unknown;
    };
    if let Some(m) = def.models.iter().find(|m| m.tier == tier) {
        return TierLookup::Model(model_from_def(def, &RESPONSES_SPEC, slug, &m.id));
    }
    TierLookup::NoModelForTier
}

pub async fn discover_models(timeouts: Timeouts) -> Vec<String> {
    let config = ProvidersConfig::load();
    model_registry::set_declared(declared_tiers_from(&config));
    let mut slugs: Vec<_> = config.providers.keys().collect();
    slugs.sort();
    let mut all_specs = Vec::new();
    for slug in slugs {
        if let Ok(provider) = create(slug, timeouts)
            && let Ok(mut models) = provider.list_models().await
        {
            if let Some(def) = config.providers.get(slug) {
                overlay_declared_tiers(def, &mut models);
            }
            model_registry::set_known_models(slug, models.clone());
            for m in models {
                all_specs.push(format!("{slug}/{}", m.id));
            }
        }
    }
    all_specs
}

fn overlay_declared_tiers(def: &ProviderDef, models: &mut [ModelInfo]) {
    for model in models {
        if let Some(declared) = def.models.iter().find(|m| m.id == model.id) {
            model.tier = Some(declared.tier);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn custom_lookup_returns_model_when_configured() {
        let mut config = ProvidersConfig::default();
        config.providers.insert(
            "custom-gw".to_string(),
            ProviderDef {
                base_url: Some("http://localhost:8000/v1".to_string()),
                models: vec![ModelDef {
                    id: "test-model".to_string(),
                    name: None,
                    tier: ModelTier::Strong,
                    context_window: Some(64000),
                    max_output_tokens: Some(4096),
                    supports_tool_examples: None,
                    supports_thinking: None,
                    requires_thinking: None,
                    thinking_fields: None,
                    supports_vision: None,
                    pricing_input: None,
                    pricing_output: None,
                    pricing_cache_write: None,
                    pricing_cache_read: None,
                    pricing_fast_input: None,
                    pricing_fast_output: None,
                }],
                ..Default::default()
            },
        );
        let def = config.get("custom-gw").unwrap();
        let model = model_from_def(def, &RESPONSES_SPEC, "custom-gw", "test-model");
        assert_eq!(model.id, "test-model");
        assert_eq!(model.provider.as_ref(), "custom-gw");
        assert_eq!(model.context_window, 64000);
    }

    fn test_def(model_id: &str) -> ProviderDef {
        ProviderDef {
            models: vec![ModelDef {
                id: model_id.to_string(),
                name: None,
                tier: ModelTier::Strong,
                context_window: None,
                max_output_tokens: None,
                supports_tool_examples: None,
                supports_thinking: None,
                requires_thinking: None,
                thinking_fields: None,
                supports_vision: None,
                pricing_input: None,
                pricing_output: None,
                pricing_cache_write: None,
                pricing_cache_read: None,
                pricing_fast_input: None,
                pricing_fast_output: None,
            }],
            ..Default::default()
        }
    }

    /// A `discover_models` provider's declared specs still show before its
    /// probe lands, but the batch must not claim it answered: a failed probe
    /// would otherwise mark its undiscovered recents retired.
    #[test]
    fn declared_batch_leaves_discovering_providers_out() {
        let mut config = ProvidersConfig::default();
        config.upsert("declared".to_string(), test_def("small"));
        let mut discovering = test_def("small");
        discovering.discover_models = true;
        config.upsert("discovering".to_string(), discovering);

        let (specs, providers) = declared_batch_from(&config);
        assert!(specs.contains(&"declared/small".to_string()));
        assert!(specs.contains(&"discovering/small".to_string()));
        assert_eq!(providers, ["declared"]);
    }
    #[test]
    fn declared_tiers_keep_the_first_model_per_tier() {
        let config: ProvidersConfig = toml::from_str(
            "[local]\n\
             [[local.models]]\nid = \"first\"\ntier = \"compaction\"\n\
             [[local.models]]\nid = \"second\"\ntier = \"compaction\"\n\
             [[local.models]]\nid = \"big\"\ntier = \"strong\"",
        )
        .unwrap();
        let local = BTreeMap::from([
            (ModelTier::Compaction, "first".to_owned()),
            (ModelTier::Strong, "big".to_owned()),
        ]);
        assert_eq!(
            declared_tiers_from(&config),
            DeclaredTiers::from([("local".to_owned(), local)])
        );
    }

    #[test]
    fn overlay_declared_tiers_sets_tier_for_declared_models_only() {
        let def: ProviderDef =
            serde_json::from_str(r#"{"models":[{"id":"declared","tier":"strong"}]}"#).unwrap();
        let mut models = vec![
            ModelInfo::id_only("declared".to_string()),
            ModelInfo::id_only("undeclared".to_string()),
        ];

        overlay_declared_tiers(&def, &mut models);
        assert_eq!(models[0].tier, Some(ModelTier::Strong));
        assert_eq!(models[1].tier, None);
    }
    #[test]
    fn custom_model_inherits_discovered_requires_thinking_and_pricing() {
        let slug = "magpie-gw";
        let model_id = "group/grok-4.7";
        crate::model_registry::set_known_models(
            slug,
            vec![crate::model::ModelInfo {
                id: model_id.to_string(),
                name: None,
                context_window: Some(500000),
                max_output_tokens: Some(500000),
                pricing: Some(ModelPricing {
                    input: 2.0,
                    output: 6.0,
                    cache_read: 0.5,
                    cache_write: 0.0,
                    fast: None,
                }),
                supports_thinking: Some(true),
                supports_vision: Some(true),
                tier: None,
                provider_info: None,
                extra: None,
                effort: Some(crate::model::ModelEffort {
                    supported: vec![
                        crate::Effort::Low,
                        crate::Effort::Medium,
                        crate::Effort::High,
                        crate::Effort::XHigh,
                    ],
                    send_off: Some(false),
                }),
            }],
        );

        let def = ProviderDef {
            base_url: Some("http://maclinux.ts:3425/v1".to_string()),
            ..Default::default()
        };
        let model = model_from_def(&def, &RESPONSES_SPEC, slug, model_id);
        assert_eq!(model.id, model_id);
        assert_eq!(model.context_window, 500000);
        assert_eq!(model.max_output_tokens, Some(500000));
        assert!(model.supports_thinking());
        assert!(model.requires_thinking());
        assert_eq!(model.pricing.input, 2.0);
        assert_eq!(model.pricing.output, 6.0);

        // When user asks for Off, it is clamped to minimal (snapped to low on dialect)
        let opts = crate::RequestOptions {
            thinking: crate::ThinkingConfig::Off,
            fast: false,
        }
        .clamped(&model);
        assert_eq!(
            opts.thinking,
            crate::ThinkingConfig::Effort(crate::Effort::Minimal)
        );
        let discovered = crate::model_registry::discovered(slug, model_id);
        let dialect = crate::providers::responses::responses_dialect(model_id, discovered.as_ref());
        let mut body = serde_json::json!({});
        crate::providers::responses::apply_responses_reasoning(
            &mut body,
            opts.thinking,
            &model,
            &dialect,
        );
        assert_eq!(body["reasoning"]["effort"], "low");
    }
}
