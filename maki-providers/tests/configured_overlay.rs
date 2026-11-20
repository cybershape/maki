//! What a `providers.toml` entry means for a slug a declaration serves.

use std::fs;
use std::path::{Path, PathBuf};

use maki_config::providers::Protocol;
use maki_providers::Timeouts;
use maki_providers::plugin::{
    self, DeclAuthority, PlanDecl, ProviderDecl, RegisterError, Registration,
};
use maki_providers::spec::Owner;
use maki_providers::test_support::run_with_isolated_home;
use maki_storage::StateDir;
use maki_storage::auth::{ProviderCredentials, save_provider_credentials};
use serde_json::json;
use test_case::test_case;

const PROVIDERS_FILE: &str = "providers.toml";

const DECLARED_SLUG: &str = "overlaid-provider";
const DECLARED_HOST: &str = "api.overlaid.example";
const DECLARED_URL: &str = "https://api.overlaid.example/v1";
const DECLARED_DISPLAY_NAME: &str = "Overlaid";
const GATEWAY_URL: &str = "https://gateway.example/v1";
const LONE_KEY: &str = "discover_models = true";

const GATEWAY_HEADER: &str = "X-Gateway-Key";
const UNSET_VAR: &str = "MAKI_TEST_UNSET_GATEWAY_KEY";

const OWN_SLUG: &str = "configured-provider";
const CONFIGURED_URL: &str = "https://configured.example/v1";
const OWN_HOST: &str = "declared.example";
const OWN_DISPLAY_NAME: &str = "Configured";

const CONFIG_DIR_FAILED: &str = "the isolated config directory did not resolve";
const WRITE_FAILED: &str = "providers.toml could not be written";
const PROVIDER_LOST: &str = "a providers.toml overlay took the slug off its declaration";
const BUILT_DESPITE_BAD_HEADER: &str = "a bad [<slug>.headers] must fail the build";

const PLAN_KEY: &str = "coding";
const PLAN_HOST: &str = "plan.overlaid.example";
const PLAN_KEY_ENV: &str = "MAKI_TEST_PLANNED_PROVIDER_KEY";

fn config_test_home() -> Option<PathBuf> {
    run_with_isolated_home(&[(UNSET_VAR, None), (KEY_ENV, None)])
}

fn seed_providers(home: &Path, providers_toml: &str) {
    let config = maki_storage::paths::config_dir().expect(CONFIG_DIR_FAILED);
    assert!(
        config.starts_with(home),
        "config must stay under the test home"
    );
    fs::write(config.join(PROVIDERS_FILE), providers_toml).expect(WRITE_FAILED);
}

fn own_definition() -> String {
    format!("protocol = \"openai-responses\"\nbase_url = \"{CONFIGURED_URL}\"")
}

fn declaration(slug: &str, display_name: &str, host: &str) -> Registration {
    Registration {
        decl: ProviderDecl {
            slug: slug.to_owned(),
            display_name: display_name.to_owned(),
            codec: Some(Protocol::OpenaiResponses),
            base_url: Some(format!("https://{host}/v1")),
            net_hosts: vec![host.to_owned()],
            ..ProviderDecl::default()
        },
        hooks: plugin::ProviderHooks::default(),
    }
}

fn load_bundled() {
    plugin::begin_load();
    plugin::register(
        declaration(DECLARED_SLUG, DECLARED_DISPLAY_NAME, DECLARED_HOST),
        DeclAuthority::Bundled,
        &[],
    )
    .expect(PROVIDER_LOST);
    plugin::commit_load();
}

/// Users write `[deepseek] base_url = ...` to send a bundled provider through
/// their gateway. If that entry cost the plugin its slug, they would get a
/// bare models.dev stub instead of the provider they meant to tweak. So the
/// declaration stays, whatever the entry says. Even a `protocol` only earns a
/// warning, because refusing the bundled load would stop maki from starting.
#[test_case(&format!("base_url = \"{GATEWAY_URL}\""), GATEWAY_URL ; "a_gateway")]
#[test_case(LONE_KEY, DECLARED_URL ; "a_lone_key")]
#[test_case(&own_definition(), CONFIGURED_URL ; "a_whole_definition")]
fn an_entry_keeps_the_bundled_declaration(entry: &str, origin: &str) {
    let Some(home) = config_test_home() else {
        return;
    };
    seed_providers(&home, &format!("[{DECLARED_SLUG}]\n{entry}\n"));
    load_bundled();

    assert!(
        matches!(Owner::of(DECLARED_SLUG), Owner::Plugin),
        "{PROVIDER_LOST}"
    );
    assert_eq!(
        plugin::effective_base_url(DECLARED_SLUG).as_deref(),
        Some(origin)
    );
}

/// A bad `[<slug>.headers]` only fails the provider when it is built. Failing
/// the bundled load instead would stop maki from starting, over a provider the
/// user may never pick.
#[test]
fn a_bad_header_on_a_bundled_slug_fails_only_its_build() {
    let Some(home) = config_test_home() else {
        return;
    };
    seed_providers(
        &home,
        &format!("[{DECLARED_SLUG}.headers]\n\"{GATEWAY_HEADER}\" = \"${{{UNSET_VAR}}}\"\n"),
    );
    load_bundled();

    assert!(
        matches!(Owner::of(DECLARED_SLUG), Owner::Plugin),
        "{PROVIDER_LOST}"
    );
    let error = plugin::create(DECLARED_SLUG, Timeouts::default())
        .err()
        .expect(BUILT_DESPITE_BAD_HEADER)
        .to_string();
    assert!(
        error.contains(GATEWAY_HEADER) && error.contains(UNSET_VAR),
        "{error}"
    );
}

/// With `plan = "..."` the user picks, but the plugin wrote every origin on
/// the menu. So the origin is only trusted when maki itself wrote it.
#[test_case(DeclAuthority::ThirdParty, false ; "third_party")]
#[test_case(DeclAuthority::Config, false ; "config")]
#[test_case(DeclAuthority::Bundled, true ; "bundled")]
fn a_planned_origin_is_vouched_only_for_a_bundled_declaration(
    authority: DeclAuthority,
    vouched: bool,
) {
    let Some(home) = config_test_home() else {
        return;
    };
    seed_providers(
        &home,
        &format!("[{DECLARED_SLUG}]\nplan = \"{PLAN_KEY}\"\n"),
    );
    let plan_url = format!("https://{PLAN_HOST}/v1");
    let mut reg = declaration(DECLARED_SLUG, DECLARED_DISPLAY_NAME, DECLARED_HOST);
    reg.decl.api_key_env = Some(PLAN_KEY_ENV.to_owned());
    reg.decl.net_hosts.push(PLAN_HOST.to_owned());
    reg.decl.plans = vec![PlanDecl {
        key: PLAN_KEY.to_owned(),
        display_name: DECLARED_DISPLAY_NAME.to_owned(),
        base_url: Some(plan_url.clone()),
        default_model: None,
        login_url: None,
    }];
    plugin::begin_load();
    let grant = match authority {
        DeclAuthority::ThirdParty => vec![DECLARED_SLUG.to_owned()],
        DeclAuthority::Bundled | DeclAuthority::Config => Vec::new(),
    };
    plugin::register(reg, authority, &grant).expect(PROVIDER_LOST);
    plugin::commit_load();

    assert_eq!(
        plugin::effective_base_url(DECLARED_SLUG).as_deref(),
        Some(plan_url.as_str())
    );
    assert_eq!(plugin::vouched_origin(DECLARED_SLUG).is_some(), vouched);
}

/// An entry with a `protocol` is a whole provider already. A third-party
/// plugin taking the same slug would be a second provider with the same name.
#[test]
fn a_slug_providers_toml_defines_is_still_refused() {
    let Some(home) = config_test_home() else {
        return;
    };
    seed_providers(&home, &format!("[{OWN_SLUG}]\n{}\n", own_definition()));
    plugin::begin_load();
    let error = plugin::register(
        declaration(OWN_SLUG, OWN_DISPLAY_NAME, OWN_HOST),
        DeclAuthority::ThirdParty,
        &[OWN_SLUG.to_owned()],
    )
    .unwrap_err();
    plugin::commit_load();

    assert!(matches!(error, RegisterError::ConfiguredSlug(_)), "{error}");
}

const CATALOG_SLUG: &str = "takeover-catalog";
const CATALOG_FILE: &str = "models-dev-catalog.json";
const SAVED_KEY: &str = "sk-takeover";
const SAVED_BEARER: &str = "Bearer sk-takeover";
const KEY_ENV: &str = "MAKI_TEST_TAKEOVER_CATALOG_KEY";
const CREDENTIALS_LOST: &str = "the takeover must hand the plugin the saved key";

/// The takeover's whole point, and its risk: `maki auth login` saved the key
/// under the slug, and the plugin that replaced the catalog provider sends it.
fn save_catalog_key(home: &Path) {
    seed_providers(home, "");
    let cache = maki_storage::paths::cache_dir().unwrap();
    fs::write(
        cache.join(CATALOG_FILE),
        json!({
            CATALOG_SLUG: {
                "npm": "@ai-sdk/openai-compatible",
                "api": DECLARED_URL,
            }
        })
        .to_string(),
    )
    .unwrap();
    save_provider_credentials(
        &StateDir::resolve().unwrap(),
        CATALOG_SLUG,
        &ProviderCredentials {
            api_key: SAVED_KEY.into(),
            host: None,
        },
    )
    .unwrap();
}

#[test]
fn a_taken_over_slugs_saved_key_becomes_the_plugins() {
    let Some(home) = config_test_home() else {
        return;
    };
    save_catalog_key(&home);
    let mut reg = declaration(CATALOG_SLUG, DECLARED_DISPLAY_NAME, DECLARED_HOST);
    reg.decl.api_key_env = Some(KEY_ENV.to_owned());
    plugin::begin_load();
    plugin::register(reg, DeclAuthority::ThirdParty, &[CATALOG_SLUG.to_owned()])
        .expect(CREDENTIALS_LOST);
    plugin::commit_load();

    let auth = plugin::resolved_auth(CATALOG_SLUG).expect(CREDENTIALS_LOST);
    assert_eq!(auth.header("authorization"), Some(SAVED_BEARER));
}

/// A v0.6.4 `plugin =` selection in `providers.toml` keeps the slug owned by
/// the named package, so the catalog client cannot take it over and receive
/// the saved key. The load warns the user to migrate.
const LEGACY_SLUG: &str = "legacy-selected";
const LEGACY_PACKAGE: &str = "legacy-package";
const LEGACY_OWNED: &str = "a legacy selection keeps its slug owned";
const LEGACY_WARNED: &str = "a legacy selection warns the user to migrate";

#[test]
fn a_legacy_plugin_selection_keeps_the_slug_owned() {
    let Some(home) = config_test_home() else {
        return;
    };
    seed_providers(
        &home,
        &format!("[{LEGACY_SLUG}]\nplugin = \"{LEGACY_PACKAGE}\"\n"),
    );

    plugin::begin_load();
    let warnings = plugin::keep_legacy_selections();
    plugin::commit_load();

    assert!(plugin::owns_slug(LEGACY_SLUG), "{LEGACY_OWNED}");
    assert!(
        warnings
            .iter()
            .any(|w| w.contains(LEGACY_SLUG) && w.contains(LEGACY_PACKAGE)),
        "{LEGACY_WARNED}: {warnings:?}"
    );
}
