//! The user's model picks that outlive a session: the compaction model slot
//! and model combos (a main model plus the subagent model it delegates to).
//!
//! Discovered metadata (context windows, pricing) from `/models` endpoints is
//! stored in `known_models` and consulted by [`crate::model::Model::from_base`].
//!
//! The global lock never escapes this module: accessors lock internally and
//! return owned data, so a caller can never hold a read guard across model
//! construction (recursive read + queued writer = deadlock). The module owns
//! persistence: [`load`] at startup, [`persist_compaction`] and
//! [`persist_combos`] on user edits. Callers never touch the on-disk format
//! directly.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

use maki_config::combo;
use maki_storage::sessions::Effort;
use maki_storage::{StateDir, atomic_write};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::model::ModelInfo;

const TIERS_FILE: &str = "model-tiers";
const COMBOS_FILE: &str = "model-combos";
const COMPACTION_KEY: &str = "compaction";
const LEGACY_TIERS_MESSAGE: &str = "the model tier system was removed; delete this file (or keep only its \"compaction\" entry) and assign the compaction model with $ in the model picker";

static REGISTRY: LazyLock<RwLock<ModelRegistry>> = LazyLock::new(RwLock::default);

fn read() -> RwLockReadGuard<'static, ModelRegistry> {
    REGISTRY.read().unwrap()
}

fn write() -> RwLockWriteGuard<'static, ModelRegistry> {
    REGISTRY.write().unwrap()
}

/// A saved combo: the main model, the subagent model, and the effort the
/// subagent thinks at (`None` follows the session).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComboDef {
    pub main: String,
    pub sub: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
}

impl ComboDef {
    pub fn spec(&self) -> String {
        combo::join(&self.main, &self.sub, self.effort)
    }
}

pub fn discovered(provider: &str, model_id: &str) -> Option<ModelInfo> {
    read().discovered(provider, model_id).cloned()
}

pub fn discovery_complete(provider: &str) -> bool {
    read().known_models.contains_key(provider)
}

/// Typed `provider_info` for a discovered model. Key by the builtin slug, not
/// `model.provider`: a dynamic wrap's model carries its own slug.
pub fn provider_info<T: Send + Sync + 'static>(provider: &str, model_id: &str) -> Option<Arc<T>> {
    let info = read()
        .discovered(provider, model_id)?
        .provider_info
        .clone()?;
    Arc::downcast(info).ok()
}

pub fn set_known_models(provider: &str, models: Vec<ModelInfo>) {
    write().set_known_models(provider, models);
}

pub fn all_known_models() -> HashMap<String, Vec<ModelInfo>> {
    read().known_models.clone()
}

pub fn restore_cached_models(models: HashMap<String, Vec<ModelInfo>>) {
    let mut reg = write();
    for (provider, list) in models {
        reg.known_models.entry(provider).or_insert(list);
    }
}

/// Resolve model name: user-configured ModelDef name in ProvidersConfig takes priority,
/// falling back to API-discovered ModelInfo.name.
pub fn resolve_model_name(provider: &str, model_id: &str) -> Option<String> {
    let config = maki_config::providers::ProvidersConfig::load();
    config
        .get(provider)
        .and_then(|def| def.models.iter().find(|m| m.id == model_id))
        .and_then(|m| m.name.clone())
        .or_else(|| discovered(provider, model_id).and_then(|info| info.name))
}

pub fn resolve_model_name_for_spec(spec: &str) -> Option<String> {
    if let Some(parts) = combo::split(spec) {
        let main = resolve_model_name_for_spec(parts.main)?;
        let sub = resolve_model_name_for_spec(parts.sub)?;
        return Some(format!("{main} + {sub}"));
    }
    let (provider, model_id) = spec.split_once('/')?;
    resolve_model_name(provider, model_id)
}

pub fn resolve_model_name_or(spec: &str, fallback: &str) -> String {
    resolve_model_name_for_spec(spec).unwrap_or_else(|| fallback.to_string())
}

/// Subagents and new sessions build their model from here, so the window a
/// server reported belongs here too, or each of them would learn it again.
pub fn record_context_window(provider: &str, model_id: &str, window: u32) {
    if let Some(info) = write()
        .known_models
        .get_mut(provider)
        .and_then(|models| models.iter_mut().find(|m| m.id == model_id))
    {
        info.context_window = Some(window);
    }
}

/// The user's pick for the cheap model that rewrites history, if any.
pub fn compaction() -> Option<String> {
    read().compaction.clone()
}

/// Returns the pick as of this edit, for [`persist_compaction`]: file IO must
/// never run while holding the registry lock.
pub fn set_compaction(spec: String) -> Option<String> {
    let mut reg = write();
    reg.compaction = Some(spec);
    reg.compaction.clone()
}

pub fn unset_compaction(spec: &str) -> Option<String> {
    let mut reg = write();
    if reg.compaction.as_deref() == Some(spec) {
        reg.compaction = None;
    }
    reg.compaction.clone()
}

pub fn persist_compaction(dir: &StateDir, spec: Option<&str>) {
    write_compaction(dir.path().join(TIERS_FILE).as_path(), spec);
}

pub fn combos() -> Vec<ComboDef> {
    read().combos.clone()
}

/// Adds the combo, or replaces the one with the same main and sub. Returns
/// the list as of this edit, for [`persist_combos`].
pub fn add_combo(def: ComboDef) -> Vec<ComboDef> {
    let mut reg = write();
    reg.combos
        .retain(|c| c.main != def.main || c.sub != def.sub);
    reg.combos.push(def);
    reg.combos.clone()
}

/// Removes the combo whose main and sub match, whatever its effort.
pub fn remove_combo(main: &str, sub: &str) -> Vec<ComboDef> {
    let mut reg = write();
    reg.combos.retain(|c| c.main != main || c.sub != sub);
    reg.combos.clone()
}

/// Sets the effort of the combo with this main and sub. `false` when no such
/// combo exists.
pub fn set_combo_effort(main: &str, sub: &str, effort: Option<Effort>) -> Option<Vec<ComboDef>> {
    let mut reg = write();
    let combo = reg
        .combos
        .iter_mut()
        .find(|c| c.main == main && c.sub == sub)?;
    combo.effort = effort;
    Some(reg.combos.clone())
}

pub fn persist_combos(dir: &StateDir, combos: &[ComboDef]) {
    write_combos(dir.path().join(COMBOS_FILE).as_path(), combos);
}

/// Seeds the registry at startup: the user's compaction pick and combos from
/// `dir`. A leftover tier assignment is an error, not a silent drop: the user
/// asked for a model the tier system no longer routes to.
pub fn load(dir: &StateDir) -> Result<(), String> {
    let compaction = read_compaction(dir.path().join(TIERS_FILE).as_path())?;
    let combos = read_combos(dir.path().join(COMBOS_FILE).as_path());
    let mut reg = write();
    reg.compaction = compaction;
    reg.combos = combos;
    Ok(())
}

#[derive(Debug, Default)]
struct ModelRegistry {
    /// The user's compaction pick. Persisted to disk.
    compaction: Option<String>,
    /// The user's combos, in creation order. Persisted to disk.
    combos: Vec<ComboDef>,
    /// Ordered model info per provider, populated from `list_models()`.
    /// Not persisted - rebuilt every session.
    known_models: HashMap<String, Vec<ModelInfo>>,
}

impl ModelRegistry {
    fn set_known_models(&mut self, provider: &str, models: Vec<ModelInfo>) {
        self.known_models.insert(provider.to_string(), models);
    }

    /// Lookup discovered metadata for a model by ID.
    fn discovered(&self, provider: &str, model_id: &str) -> Option<&ModelInfo> {
        self.known_models
            .get(provider)?
            .iter()
            .find(|m| m.id == model_id)
    }
}

// On-disk format: { "compaction": "spec" }. The file predates the tier
// removal, when it also held strong/medium/weak picks; those now fail the
// load so the user cleans them up instead of losing them silently.

fn read_compaction(path: &Path) -> Result<Option<String>, String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let map: HashMap<String, String> = serde_json::from_str(&raw).map_err(|e| {
        format!(
            "failed to parse {}: {e}; {LEGACY_TIERS_MESSAGE}",
            path.display()
        )
    })?;
    if let Some(legacy) = map.keys().find(|key| key.as_str() != COMPACTION_KEY) {
        return Err(format!(
            "{}: model tier '{legacy}' is no longer supported; {LEGACY_TIERS_MESSAGE}",
            path.display()
        ));
    }
    Ok(map.get(COMPACTION_KEY).cloned())
}

fn write_compaction(path: &Path, spec: Option<&str>) {
    let map: HashMap<&str, &str> = spec
        .map(|spec| HashMap::from([(COMPACTION_KEY, spec)]))
        .unwrap_or_default();
    let json = match serde_json::to_vec_pretty(&map) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "failed to serialize compaction pick");
            return;
        }
    };
    if let Err(e) = atomic_write(path, &json) {
        warn!(path = %path.display(), error = %e, "failed to persist compaction pick");
    }
}

fn read_combos(path: &Path) -> Vec<ComboDef> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    if raw.trim().is_empty() {
        return Vec::new();
    }
    match serde_json::from_str(&raw) {
        Ok(combos) => combos,
        Err(e) => {
            warn!(path = %path.display(), error = %e, "failed to parse combos, ignoring");
            Vec::new()
        }
    }
}

fn write_combos(path: &Path, combos: &[ComboDef]) {
    let json = match serde_json::to_vec_pretty(combos) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "failed to serialize combos");
            return;
        }
    };
    if let Err(e) = atomic_write(path, &json) {
        warn!(path = %path.display(), error = %e, "failed to persist combos");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use test_case::test_case;

    const MAIN: &str = "anthropic/claude-opus-4-6";
    const SUB: &str = "anthropic/claude-haiku-4-5";
    const COMPACT_SPEC: &str = "zai/glm-5";

    #[test]
    fn discovered_looks_up_by_id() {
        let mut reg = ModelRegistry::default();
        reg.set_known_models(
            "llama-cpp",
            vec![
                ModelInfo::id_only("model-a".into()),
                ModelInfo {
                    context_window: Some(128_000),
                    ..ModelInfo::id_only("model-b".into())
                },
            ],
        );
        let info = reg.discovered("llama-cpp", "model-b").unwrap();
        assert_eq!(info.context_window, Some(128_000));
        assert!(reg.discovered("llama-cpp", "model-x").is_none());
        assert!(reg.discovered("ollama", "model-a").is_none());
    }

    #[test]
    fn compaction_persistence_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(TIERS_FILE);

        assert_eq!(read_compaction(&path).unwrap(), None);
        write_compaction(&path, Some(COMPACT_SPEC));
        assert_eq!(
            read_compaction(&path).unwrap().as_deref(),
            Some(COMPACT_SPEC)
        );
        write_compaction(&path, None);
        assert_eq!(read_compaction(&path).unwrap(), None);
    }

    #[test]
    fn compaction_handles_missing_or_blank_input() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            read_compaction(&tmp.path().join("does-not-exist")).unwrap(),
            None
        );
        for blank in [b"".as_slice(), b"   \n".as_slice()] {
            let path = tmp.path().join(TIERS_FILE);
            std::fs::write(&path, blank).unwrap();
            assert_eq!(read_compaction(&path).unwrap(), None);
        }
    }

    #[test_case(r#"{"strong": "ollama/a"}"# ; "agent_tier")]
    #[test_case(r#"{"compaction": "zai/glm-5", "weak": "ollama/b"}"# ; "mixed_with_compaction")]
    #[test_case(r#"{"ollama/a": "strong"}"# ; "legacy_spec_keyed")]
    #[test_case("not json at all" ; "garbage")]
    fn leftover_tier_assignments_fail_the_load(raw: &str) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(TIERS_FILE);
        std::fs::write(&path, raw).unwrap();
        let err = read_compaction(&path).unwrap_err();
        assert!(err.contains("tier"), "got: {err}");
    }

    #[test]
    fn compaction_edits_return_what_persist_writes() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());

        let snapshot = set_compaction(COMPACT_SPEC.into());
        persist_compaction(&dir, snapshot.as_deref());
        assert_eq!(snapshot.as_deref(), Some(COMPACT_SPEC));

        let snapshot = unset_compaction("other/spec");
        assert_eq!(snapshot.as_deref(), Some(COMPACT_SPEC));

        let snapshot = unset_compaction(COMPACT_SPEC);
        persist_compaction(&dir, snapshot.as_deref());
        assert_eq!(snapshot, None);
        assert_eq!(read_compaction(&dir.path().join(TIERS_FILE)).unwrap(), None);
    }

    #[test]
    fn combo_persistence_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(COMBOS_FILE);
        let combos = vec![
            ComboDef {
                main: MAIN.into(),
                sub: SUB.into(),
                effort: None,
            },
            ComboDef {
                main: SUB.into(),
                sub: MAIN.into(),
                effort: Some(Effort::High),
            },
        ];
        write_combos(&path, &combos);
        assert_eq!(read_combos(&path), combos);
    }

    #[test]
    fn combo_add_replaces_the_same_pair_and_keeps_order() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let def = ComboDef {
            main: MAIN.into(),
            sub: SUB.into(),
            effort: None,
        };
        persist_combos(&dir, &add_combo(def.clone()));
        let updated = ComboDef {
            effort: Some(Effort::Max),
            ..def.clone()
        };
        let snapshot = add_combo(updated.clone());
        persist_combos(&dir, &snapshot);
        assert_eq!(snapshot, vec![updated]);
        assert_eq!(read_combos(&dir.path().join(COMBOS_FILE)), snapshot);
    }

    #[test]
    fn combo_remove_and_effort_match_the_pair_not_the_effort() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let def = ComboDef {
            main: MAIN.into(),
            sub: SUB.into(),
            effort: Some(Effort::Low),
        };
        persist_combos(&dir, &add_combo(def));

        let snapshot = set_combo_effort(MAIN, SUB, Some(Effort::XHigh)).unwrap();
        assert_eq!(snapshot[0].effort, Some(Effort::XHigh));
        assert!(set_combo_effort(MAIN, "other/sub", None).is_none());

        let snapshot = remove_combo(MAIN, SUB);
        persist_combos(&dir, &snapshot);
        assert!(snapshot.is_empty());
        assert!(read_combos(&dir.path().join(COMBOS_FILE)).is_empty());
    }

    #[test]
    fn combo_spec_spells_the_canonical_form() {
        let def = ComboDef {
            main: MAIN.into(),
            sub: SUB.into(),
            effort: Some(Effort::High),
        };
        assert_eq!(def.spec(), format!("{MAIN}+{SUB}@high"));
    }

    #[test]
    fn resolve_model_name_prefers_config_over_discovery() {
        let mut def = maki_config::providers::ProviderDef::default();
        def.models.push(maki_config::providers::ModelDef {
            id: "m1".to_string(),
            name: Some("Configured Name".to_string()),
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
            like: None,
        });
        let mut config = maki_config::providers::ProvidersConfig::default();
        config.providers.insert("my-provider".to_string(), def);
        let _guard = maki_config::providers::set_override(config);

        set_known_models(
            "my-provider",
            vec![ModelInfo {
                id: "m1".to_string(),
                name: Some("Discovered Name".to_string()),
                ..ModelInfo::id_only("m1".to_string())
            }],
        );

        assert_eq!(
            resolve_model_name("my-provider", "m1"),
            Some("Configured Name".to_string())
        );

        set_known_models(
            "my-provider",
            vec![ModelInfo {
                id: "m2".to_string(),
                name: Some("Discovered Only".to_string()),
                ..ModelInfo::id_only("m2".to_string())
            }],
        );

        assert_eq!(
            resolve_model_name("my-provider", "m2"),
            Some("Discovered Only".to_string())
        );
        assert_eq!(
            resolve_model_name_or("my-provider/m2", "fallback"),
            "Discovered Only"
        );
        assert_eq!(
            resolve_model_name_or("my-provider/m3", "fallback"),
            "fallback"
        );
        assert_eq!(
            resolve_model_name_or("my-provider/m1+my-provider/m2", "fallback"),
            "Configured Name + Discovered Only"
        );
    }
}
