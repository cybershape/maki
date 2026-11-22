use std::sync::{Arc, Mutex};

use maki_config::providers::{
    ModelDef, ProviderDef, ProvidersConfig, resolve_api_key_env, resolve_base_url,
};
use maki_storage::blocking;

use super::responses::ResponsesProvider;
use super::{KeyPool, ResolvedAuth};
use crate::AgentError;
use crate::model::{FastPricing, Model, ModelEntry, ModelInfo, ModelPricing, ModelTier};
use crate::model_registry::{self, DeclaredTiers};
use crate::provider::{ModelBatch, Provider, ProviderStatus};
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
    let key = if let Some(def) = def {
        if def.api_key_env.is_some() {
            KeyPool::resolve(slug, &env_var)?.current().to_string()
        } else {
            KeyPool::resolve(slug, &env_var)
                .map(|p| p.current().to_string())
                .unwrap_or_default()
        }
    } else {
        KeyPool::resolve(slug, &env_var)
            .map(|p| p.current().to_string())
            .unwrap_or_default()
    };

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
            pricing: (def.has_pricing() || def.has_fast_pricing()).then(|| ModelPricing {
                input: def.pricing_input.unwrap_or(0.0),
                output: def.pricing_output.unwrap_or(0.0),
                cache_write: def.pricing_cache_write.unwrap_or(0.0),
                cache_read: def.pricing_cache_read.unwrap_or(0.0),
                fast: fast_pricing(def),
            }),
            thinking_fields: def.thinking_fields.clone(),
            family: None,
            default: false,
        }
    }
}

fn fast_pricing(def: &ModelDef) -> Option<FastPricing> {
    def.has_fast_pricing().then(|| FastPricing {
        input: def.pricing_fast_input.unwrap_or(0.0),
        output: def.pricing_fast_output.unwrap_or(0.0),
    })
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

    let mut pricing = row
        .pricing
        .take()
        .or_else(|| discovered.and_then(|d| d.pricing.clone()))
        .unwrap_or_default();
    // Discovered pricing never carries a fast tier, so a replayed listing
    // must not drop the one the config states.
    if pricing.fast.is_none() {
        pricing.fast = def_model.and_then(fast_pricing);
    }
    row.pricing = Some(pricing);
    let mut model = row.to_model(slug, base, model_id.to_string());
    model.subsidised_by = subsidy_source.map(Arc::from);
    if let Some(d) = discovered {
        if d.pricing.as_ref().is_some_and(ModelPricing::is_zero) {
            model.discovered_free = true;
        }
        if d.supports_fast == Some(true)
            || d.extra.as_ref().is_some_and(ModelInfo::fast_tier_declared)
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

fn custom_entries(config: &ProvidersConfig) -> Vec<(&String, &ProviderDef)> {
    config
        .providers
        .iter()
        .filter(|(slug, _)| !is_builtin_slug(slug))
        .collect()
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

pub async fn discover_models(timeouts: Timeouts, mut emit: impl FnMut(ModelBatch)) {
    let config = blocking(ProvidersConfig::load).await;
    model_registry::set_declared(declared_tiers_from(&config));
    let mut entries = custom_entries(&config);
    entries.sort_by_key(|(_, def)| def.discover_models);
    for (slug, def) in entries {
        let mut models: Vec<String> = def
            .models
            .iter()
            .map(|m| format!("{slug}/{}", m.id))
            .collect();
        let has_key = KeyPool::resolve(slug, &resolve_api_key_env(slug, Some(def))).is_ok();
        let is_local = def
            .base_url
            .as_ref()
            .is_some_and(|u| u.contains("localhost") || u.contains("127.0.0.1"));
        let status = if !has_key && !is_local {
            models.clear();
            ProviderStatus::MissingKey
        } else if !def.discover_models {
            ProviderStatus::Live
        } else {
            match probe(slug, def, timeouts).await {
                Ok(listed) => {
                    for spec in listed {
                        if !models.contains(&spec) {
                            models.push(spec);
                        }
                    }
                    ProviderStatus::Live
                }
                Err(e) => {
                    tracing::warn!(slug, error = %e, "failed to list models for custom provider");
                    ProviderStatus::Failed(format!("{slug}: {e}"))
                }
            }
        };
        emit(ModelBatch {
            provider: slug.clone(),
            models,
            status,
        });
    }
}

async fn probe(
    slug: &str,
    def: &ProviderDef,
    timeouts: Timeouts,
) -> Result<Vec<String>, AgentError> {
    let mut models = create(slug, timeouts)?.list_models().await?;
    overlay_declared_tiers(def, &mut models);
    model_registry::set_known_models(slug, models.clone());
    Ok(models
        .into_iter()
        .map(|m| format!("{slug}/{}", m.id))
        .collect())
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
    use test_case::test_case;

    fn responses_def(model_id: &str) -> ProviderDef {
        serde_json::from_str(&format!(r#"{{"models":[{{"id":"{model_id}"}}]}}"#)).unwrap()
    }

    #[test]
    fn fast_pricing_alone_makes_the_model_fast_capable() {
        let slug = "fast-only-gw";
        let mut config = ProvidersConfig::default();
        config.providers.insert(
            slug.to_string(),
            serde_json::from_str(
                r#"{"models":[{"id":"m","tier":"strong","pricing_fast_input":6.0,"pricing_fast_output":30.0}]}"#,
            )
            .unwrap(),
        );
        let _guard = maki_config::providers::set_override(config);
        let model = lookup_model(slug, "m").unwrap();
        assert!(
            model.supports_fast(),
            "pricing_fast_* alone must enable fast mode"
        );
    }

    #[test]
    fn cached_discovery_does_not_mask_configured_fast_pricing() {
        let slug = "fast-cached-gw";
        let model_id = "m-cached";
        // A live answer from an earlier run, replayed from disk: pricing
        // survives serialization, `extra` does not.
        model_registry::set_known_models(
            slug,
            vec![ModelInfo {
                pricing: Some(ModelPricing::per_million(3.0, 15.0, 3.75, 0.3)),
                extra: Some(
                    serde_json::json!({"service_tiers": [{"id": "priority", "name": "Fast"}]}),
                ),
                ..ModelInfo::id_only(model_id.to_string())
            }],
        );
        let cached = model_registry::all_known_models();
        let json = serde_json::to_string(&cached).unwrap();
        let mut restored: std::collections::HashMap<String, Vec<ModelInfo>> =
            serde_json::from_str(&json).unwrap();
        let rows = restored.remove(slug).unwrap();
        let replay_slug = format!("{slug}-replay");
        restored.insert(replay_slug.clone(), rows);
        model_registry::restore_cached_models(restored);

        let mut config = ProvidersConfig::default();
        config.providers.insert(
            replay_slug.clone(),
            serde_json::from_str(&format!(
                r#"{{"models":[{{"id":"{model_id}","tier":"strong","pricing_fast_input":6.0,"pricing_fast_output":30.0}}]}}"#
            ))
            .unwrap(),
        );
        let _guard = maki_config::providers::set_override(config);
        let model = lookup_model(&replay_slug, model_id).unwrap();
        assert!(
            model.supports_fast(),
            "configured fast pricing must hold even when replayed discovery carries standard pricing"
        );
    }

    #[test]
    fn discovered_fast_tier_survives_a_cache_roundtrip() {
        let slug = "fast-cache-gw";
        let replay_slug = "fast-cache-gw-replay";
        let model_id = "m-fast";
        let extra = serde_json::json!({"service_tiers": [{"id": "priority", "name": "Fast"}]});
        model_registry::set_known_models(
            slug,
            vec![ModelInfo {
                supports_fast: Some(true),
                extra: Some(extra.clone()),
                ..ModelInfo::id_only(model_id.to_string())
            }],
        );
        let cached = model_registry::all_known_models();
        let json = serde_json::to_string(&cached).unwrap();
        let mut restored: std::collections::HashMap<String, Vec<ModelInfo>> =
            serde_json::from_str(&json).unwrap();
        // Replay lands on a registry that never saw the live answer, under a
        // slug of its own so `or_insert` cannot mask the loss.
        let rows = restored.remove(slug).unwrap();
        restored.insert(replay_slug.to_string(), rows);
        model_registry::restore_cached_models(restored);
        let def = ProviderDef::default();
        let model = model_from_def(&def, &RESPONSES_SPEC, replay_slug, model_id);
        assert!(
            model.supports_fast(),
            "a discovered fast tier must survive the on-disk cache roundtrip"
        );
    }

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

    // The exact regression this fixes: discovery parsed context_window but
    // never stored it, so custom models always got the protocol fallback.
    #[test]
    fn discovered_metadata_flows_into_custom_model_from_def() {
        let slug = "custom-discovery-metadata-test";
        let model_id = "vllm-model";
        let expected_window: u32 = 131_072;
        let expected_output: u32 = 8_192;

        crate::model_registry::set_known_models(
            slug,
            vec![ModelInfo {
                context_window: Some(expected_window),
                max_output_tokens: Some(expected_output),
                ..ModelInfo::id_only(model_id.to_string())
            }],
        );

        let def = responses_def(model_id);
        let model = model_from_def(&def, &RESPONSES_SPEC, slug, model_id);
        assert_eq!(model.context_window, expected_window);
        assert_eq!(model.max_output_tokens, Some(expected_output));
    }

    #[test_case(Some(true), Some(false), true ; "declared_outranks_discovery")]
    #[test_case(None, Some(false), false ; "text_only_discovery_turns_vision_off")]
    #[test_case(None, Some(true), true ; "image_discovery_keeps_vision_on")]
    fn discovered_vision_flows_into_custom_model(
        declared: Option<bool>,
        discovered: Option<bool>,
        expected: bool,
    ) {
        let slug = format!("custom-vision-test-{declared:?}-{discovered:?}");
        let model_id = "vllm-model";
        crate::model_registry::set_known_models(
            &slug,
            vec![ModelInfo {
                supports_vision: discovered,
                ..ModelInfo::id_only(model_id.to_string())
            }],
        );
        let mut def = responses_def(model_id);
        def.models[0].supports_vision = declared;
        let model = model_from_def(&def, &RESPONSES_SPEC, &slug, model_id);
        assert_eq!(model.supports_vision(), expected);
    }

    const ENDPOINT_INPUT_PRICE: f64 = 0.5;
    const ENDPOINT_OUTPUT_PRICE: f64 = 1.5;
    const DECLARED_INPUT_PRICE: f64 = 3.0;
    const DECLARED_OUTPUT_PRICE: f64 = 15.0;

    fn seed_discovered_pricing(slug: &str, model_id: &str, pricing: ModelPricing) {
        crate::model_registry::set_known_models(
            slug,
            vec![ModelInfo {
                pricing: Some(pricing),
                ..ModelInfo::id_only(model_id.to_string())
            }],
        );
    }

    #[test]
    fn discovered_pricing_fills_a_model_with_no_declared_rates() {
        let slug = "custom-discovery-pricing-test";
        let model_id = "endpoint-priced";
        seed_discovered_pricing(
            slug,
            model_id,
            ModelPricing::per_million(ENDPOINT_INPUT_PRICE, ENDPOINT_OUTPUT_PRICE, 0.0, 0.0),
        );

        let model = model_from_def(&responses_def(model_id), &RESPONSES_SPEC, slug, model_id);
        assert_eq!(model.pricing.input, ENDPOINT_INPUT_PRICE);
        assert_eq!(model.pricing.output, ENDPOINT_OUTPUT_PRICE);
    }

    #[test]
    fn declared_pricing_wins_over_discovered_pricing() {
        let slug = "custom-declared-pricing-test";
        let model_id = "declared-price-wins";
        seed_discovered_pricing(
            slug,
            model_id,
            ModelPricing::per_million(ENDPOINT_INPUT_PRICE, ENDPOINT_OUTPUT_PRICE, 0.0, 0.0),
        );
        let mut def = responses_def(model_id);
        def.models[0].pricing_input = Some(DECLARED_INPUT_PRICE);
        def.models[0].pricing_output = Some(DECLARED_OUTPUT_PRICE);

        let model = model_from_def(&def, &RESPONSES_SPEC, slug, model_id);
        assert_eq!(model.pricing.input, DECLARED_INPUT_PRICE);
        assert_eq!(model.pricing.output, DECLARED_OUTPUT_PRICE);
    }

    #[test]
    fn discovered_zero_price_marks_the_model_free() {
        let slug = "custom-discovery-free-test";
        let model_id = "endpoint-free";
        seed_discovered_pricing(slug, model_id, ModelPricing::ZERO);

        let model = model_from_def(&responses_def(model_id), &RESPONSES_SPEC, slug, model_id);
        assert!(model.is_free());
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
                supports_fast: None,
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
