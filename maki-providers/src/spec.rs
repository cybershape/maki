use std::sync::{Arc, Mutex};

use crate::AgentError;
use crate::model::{Model, ModelEntry, ModelFamily, ModelTier};
use crate::pricing::PricingSchedule;
use crate::provider::Provider;
use crate::providers::{ResolvedAuth, Timeouts, custom, plugin};
use crate::types::{ThinkingConfig, ThinkingFallback};
use maki_config::PROVIDER_BUILTINS;
use maki_config::providers::{BuiltInProvider, Protocol, ProviderPlan};

pub const BASES: &[&str] = &[];

pub static RESPONSES_SPEC: ProviderSpec = ProviderSpec {
    slug: "responses",
    display_name: "Responses API",
    api_key_env: "OPENAI_API_KEY",
    family: ModelFamily::Gpt,
    supports_thinking: true,
    supports_deferred_tools: false,
    sends_thinking: sends_as_chosen,
    thinking_fields_fallback: |_| Some(ThinkingFallback::BudgetField),
    accepts_arbitrary_models: true,
    fallback_max_output: Some(32_000),
    fallback_context_window: 128_000,
    models_toml: NO_CURATED_MODELS,
    pricing_schedule: None,
    build: Build::Declared(&[]),
    aperture: None,
    login: None,
    docs: GeneratedDocs {
        api_urls: &[],
        features: None,
        auth: AuthDoc::EnvVar,
        catalog: CatalogDoc::Discovered("Configured via providers.toml"),
        trailing_notes: &[],
    },
};

/// Stands in for the model table when a provider curates none.
pub const GENERIC_DISCOVERY_NOTE: &str =
    "No hardcoded model catalog. Use any model ID supported by this provider.";

/// Everything maki knows about one provider without asking the network.
///
/// This struct is the checklist for adding a native provider. Every field is
/// mandatory in the literal, and that is the point: there is deliberately no
/// `Default`, no `#[non_exhaustive]`, and no row uses `..`, so adding a field
/// here breaks every row until someone answers for each one.
///
/// The checklist is two files: this row, and `models/<slug>.toml` next to it
/// carrying the provider's curated table. A Lua plugin provider gets one of
/// these built from its declaration instead, see [`plugin::spec`].
#[derive(Debug)]
pub struct ProviderSpec {
    pub slug: &'static str,
    pub display_name: &'static str,
    pub api_key_env: &'static str,

    pub family: ModelFamily,
    pub supports_thinking: bool,
    /// The endpoint honours `defer_loading` and expands `tool_reference`
    /// blocks. A property of the backend, not the protocol, so a slug that
    /// borrows this row's protocol does not inherit it; see
    /// [`crate::Model::supports_deferred_tools`].
    pub supports_deferred_tools: bool,
    /// What a model here is sent for a thinking choice, so the badge and the
    /// picker can show it. A fn, because one provider can serve models with
    /// different levels. Read through [`crate::ThinkingConfig::shown`].
    pub sends_thinking: fn(&Model, ThinkingConfig) -> ThinkingConfig,
    /// How the selected endpoint handles modes outside `thinking_fields`.
    /// `None` ignores the fields, `Some` names their fallback. This also says
    /// whether a toggle fragment can carry an explicit token budget.
    pub thinking_fields_fallback: fn(&Model) -> Option<ThinkingFallback>,
    pub accepts_arbitrary_models: bool,
    pub fallback_max_output: Option<u32>,
    pub fallback_context_window: u32,
    /// This provider's curated table, embedded from `models/<slug>.toml`. Read
    /// it through [`ProviderSpec::models`], which serves the parsed rows.
    /// [`NO_CURATED_MODELS`] for a provider whose ids all come from a live
    /// catalog, and for a declaration, which carries its rows on
    /// [`Build::Declared`].
    pub models_toml: &'static str,
    /// Set by the providers whose rates move with the wall clock, so the hours
    /// sit next to the prices they scale. Everyone else bills flat.
    pub pricing_schedule: Option<&'static PricingSchedule>,

    pub build: Build,
    /// `Some` iff Aperture can proxy onto this provider. Lives on the spec, not
    /// on [`Native`], so the route survives a provider losing its bespoke impl.
    pub aperture: Option<ApertureRoute>,
    /// `None` for slugs that are never a `maki auth login` target and so have
    /// no `maki-config` row. `Some` is the *only* authoring site for that row;
    /// see [`ProviderSpec::config_row`].
    pub login: Option<LoginConfig>,
    pub docs: GeneratedDocs,
}

/// For a provider whose API takes every choice as it is, `adaptive` included.
pub(crate) fn sends_as_chosen(_: &Model, choice: ThinkingConfig) -> ThinkingConfig {
    choice
}

/// For a provider whose ids all come from a live catalog.
pub const NO_CURATED_MODELS: &str = "";

pub type NewFn = fn(Timeouts) -> Result<Box<dyn Provider>, AgentError>;

/// The third argument is the system prefix, which a fn-pointer type has no
/// room to name.
pub type WithAuthFn = fn(Arc<Mutex<ResolvedAuth>>, Timeouts, Option<String>) -> Box<dyn Provider>;

/// How maki builds the provider this row describes.
#[derive(Debug, Clone, Copy)]
pub enum Build {
    /// A bespoke `impl Provider`.
    Native(Native),
    /// A Lua plugin's declaration, with the model rows it states.
    Declared(&'static [ModelEntry]),
    /// `catalog::try_create`, out of models.dev. The row carries metadata only.
    Catalog,
}

/// How maki builds a provider that has its own `impl Provider`.
#[derive(Debug, Clone, Copy)]
pub struct Native {
    /// Built from config and env by maki itself.
    pub new: NewFn,
    /// Build against auth someone else resolved. Used by plugin providers
    /// that extend a base slug and by Aperture's gateway routing.
    pub with_auth: WithAuthFn,
}

#[derive(Debug, Clone, Copy)]
pub struct ApertureRoute {
    /// Path prefix maki sends to the gateway, which appends the whole incoming
    /// request path to the upstream's base url. Empty where the upstream base
    /// url already carries its own path, and each provider says why at its own
    /// spec. Mandatory rather than defaulted, so a new provider cannot silently
    /// inherit `/v1`.
    pub path_prefix: &'static str,
}

/// The half of a spec only `maki-config` reads. `maki-config` cannot see
/// `maki-providers`, so it gets this through `inventory` as a projection.
#[derive(Debug)]
pub struct LoginConfig {
    pub protocol: Protocol,
    pub default_base_url: &'static str,
    pub default_model: &'static str,
    pub plans: Option<&'static [(&'static str, ProviderPlan)]>,
    pub login_url: Option<&'static str>,
    /// Prompt for a base url during login, as local inference servers need.
    pub needs_url: bool,
}

#[derive(Debug)]
pub struct GeneratedDocs {
    pub api_urls: &'static [&'static str],
    pub features: Option<&'static str>,
    pub auth: AuthDoc,
    pub catalog: CatalogDoc,
    /// Emitted after the model table.
    pub trailing_notes: &'static [&'static str],
}

#[derive(Debug)]
pub enum AuthDoc {
    /// `` `ENV_VAR` ``
    EnvVar,
    /// `` `ENV_VAR` `` followed by a note (OAuth availability, shared keys).
    EnvVarWith(&'static str),
    /// The env var is not the story: Ollama's `OLLAMA_HOST`, Aperture's
    /// `APERTURE_HOST`.
    Custom(&'static str),
}

#[derive(Debug)]
pub enum CatalogDoc {
    /// Render [`ProviderSpec::models`] as the tier table.
    Table,
    /// The table is empty; say where models come from instead.
    Discovered(&'static str),
}

impl ProviderSpec {
    /// The one way to reach this provider's curated table.
    pub fn models(&self) -> &'static [ModelEntry] {
        match self.build {
            Build::Declared(models) => models,
            Build::Native(_) | Build::Catalog => &[],
        }
    }

    pub const fn native(&self) -> Option<Native> {
        match self.build {
            Build::Native(native) => Some(native),
            Build::Declared(_) | Build::Catalog => None,
        }
    }

    /// The `maki-config` view of this provider, derived. Const-panics for a
    /// spec with no `login`, so submitting a login-less provider to the
    /// inventory is a compile error rather than a wrong row.
    pub const fn config_row(&self) -> BuiltInProvider {
        match &self.login {
            Some(login) => BuiltInProvider {
                slug: self.slug,
                display_name: self.display_name,
                default_api_key_env: self.api_key_env,
                protocol: login.protocol,
                default_base_url: login.default_base_url,
                default_model: Some(login.default_model),
                plans: login.plans,
                login_url: login.login_url,
                needs_url: login.needs_url,
            },
            None => panic!("provider has no login config; do not submit it to the inventory"),
        }
    }
}

/// The order users see: doc sections, offline model listing, picker batches.
/// Written out rather than collected with `inventory`, whose iteration order is
/// unspecified and would silently reshuffle the generated docs.
///
/// Each entry copies a module's `SPEC` const, so the two live at different
/// addresses: identify a spec by `slug`, never by pointer.
const BUILTINS: &[ProviderSpec] = &[];

pub struct ProviderRegistry;

impl ProviderRegistry {
    /// The row that describes `slug` itself: compiled in, or built from a
    /// plugin's declaration. The one to reach for unless plugin providers
    /// must be left out on purpose, see [`Self::compiled`].
    pub fn get(slug: &str) -> Option<&'static ProviderSpec> {
        if slug == RESPONSES_SPEC.slug {
            return Some(&RESPONSES_SPEC);
        }
        Self::compiled(slug).or_else(|| plugin::spec(slug))
    }

    /// Like [`Self::get`], but a `providers.toml` slug resolves to its base
    /// provider's spec, so thinking support, display name and tier defaults
    /// still answer for a stub that declares no models.
    ///
    /// The fallback resolves through [`Self::compiled`], never back through
    /// here, so the lookup cannot recurse.
    pub fn for_slug(slug: &str) -> Option<&'static ProviderSpec> {
        Self::get(slug).or_else(|| custom::base_spec(slug))
    }

    /// Every row [`Self::get`] answers for: the compiled-in ones in their
    /// order, then the plugin providers by slug.
    pub fn all() -> Vec<&'static ProviderSpec> {
        std::iter::once(&RESPONSES_SPEC)
            .chain(plugin::specs())
            .collect()
    }

    /// A row compiled into this binary, never a plugin's.
    pub fn compiled(slug: &str) -> Option<&'static ProviderSpec> {
        if slug == RESPONSES_SPEC.slug {
            Some(&RESPONSES_SPEC)
        } else {
            BUILTINS.iter().find(|s| s.slug == slug)
        }
    }

    pub fn all_compiled() -> &'static [ProviderSpec] {
        BUILTINS
    }

    /// Whether maki ships `slug`, compiled in or as a bundled plugin, loaded
    /// or not. Such a slug is never a third party's to take.
    pub fn is_shipped(slug: &str) -> bool {
        Self::compiled(slug).is_some() || PROVIDER_BUILTINS.contains(&slug)
    }

    pub fn find_default_for_tier(slug: &str, tier: ModelTier) -> Option<&'static ModelEntry> {
        Self::for_slug(slug)?
            .models()
            .iter()
            .find(|e| e.default && e.tier == tier)
    }
}

/// Which mechanism constructs a provider for this slug. The variant order is
/// the resolution order, and this is the only place it is written down.
///
/// A different question from [`ProviderRegistry::get`], which answers what maki
/// knows about a slug: a catalog-backed builtin has a spec row and is still
/// `Catalog` here.
pub enum Owner {
    Plugin,
    Custom,
    Unknown,
}

impl Owner {
    pub fn of(slug: &str) -> Self {
        if plugin::owns_slug(slug) {
            return Self::Plugin;
        }
        if custom::is_declared(slug) {
            return Self::Custom;
        }
        Self::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_spec_is_valid() {
        assert_eq!(RESPONSES_SPEC.slug, "responses");
        assert!(RESPONSES_SPEC.accepts_arbitrary_models);
    }
}
