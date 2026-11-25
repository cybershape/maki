use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use arc_swap::{ArcSwap, ArcSwapOption};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

use maki_config::ModelPolicy;
use maki_config::combo;
use maki_providers::Model;
use maki_providers::model_registry::{self, ComboDef};
use maki_providers::models_cache::ModelList;
use maki_providers::provider::ProviderStatus;
use maki_providers::spec::ProviderRegistry;
use maki_storage::model::with_recent;

use crate::components::Overlay;
use crate::components::list_picker::{ListPicker, PickerAction, PickerItem};
use crate::repaint::{Cadence, Dirty, Watch};

const TITLE: &str = " Models ";
const RECENT_SECTION: &str = "Recent";
const COMBO_SECTION: &str = "Combos";
const FOLLOW_SESSION_LABEL: &str = "follow session";
const COMBO_PENDING_NOTICE: &str = "combo: pick the subagent model, Enter to create (+ cancels)";
const COMBO_SEPARATOR_LABEL: &str = " + ";
const FREE_LABEL: &str = "Free";
const FREE_PREFIX: &str = "Free · ";
const PRICE_SEPARATOR: &str = " · ";
const PRICE_WIDTH: usize = 7;
const LOADING_NOTICE: &str = "loading models...";
const NO_KEY_LABEL: &str = "no key";
const OFFLINE_LABEL: &str = "offline";
const FALLBACK_LABEL: &str = "fallback";
const CURRENT_MARK: &str = "● ";
const FAST_MARK: &str = "⚡";
const FOOTER: &[(&str, &str)] = &[
    ("Enter", "select"),
    ("+", "combo"),
    ("-", "delete combo"),
    ("^", "combo effort"),
    ("$", "compaction"),
    ("%", "fold"),
    ("*", "fold all"),
];
const FOLD_KEY: KeyCode = KeyCode::Char('%');
const FOLD_ALL_KEY: KeyCode = KeyCode::Char('*');
const COMBO_KEY: KeyCode = KeyCode::Char('+');
const COMBO_DELETE_KEY: KeyCode = KeyCode::Char('-');
const COMBO_EFFORT_KEY: KeyCode = KeyCode::Char('^');

fn is_compaction_shortcut(key: KeyEvent) -> bool {
    // Shift+digit arrives as the character it types, with the SHIFT bit
    // already folded into it by key normalization.
    matches!(key.code, KeyCode::Char('$' | '€' | '¤')) // US, EU, Nordic
}

/// A provider's one row, which its models hide behind while it is folded. It
/// says how many it has, and what discovery said when that needs fixing.
fn provider_row(
    slug: String,
    display: String,
    count: usize,
    status: Option<&ProviderStatus>,
) -> ModelEntry {
    let (status, needs_setup) = match status {
        Some(ProviderStatus::MissingKey) => (Some(NO_KEY_LABEL), true),
        Some(ProviderStatus::Failed(_)) if count == 0 => (Some(OFFLINE_LABEL), true),
        Some(ProviderStatus::Failed(_)) => (Some(FALLBACK_LABEL), false),
        Some(ProviderStatus::Live) | None => (None, false),
    };
    let plural = if count == 1 { "model" } else { "models" };
    ModelEntry {
        spec: slug.clone(),
        id: display.clone(),
        name: None,
        provider: slug,
        provider_display: display,
        suffix: status.map(str::to_string),
        detail: format!("{count} {plural}"),
        highlighted: false,
        free: false,
        is_provider: true,
        combo: None,
        prefer_model_name: false,
        hidden: false,
        needs_setup,
    }
}

pub enum ModelPickerAction {
    Consumed,
    Select(String),
    AssignCompaction(String),
    UnassignCompaction(String),
    /// A combo was created, removed, or its effort changed. The picker has no
    /// state dir, so the caller saves [`model_registry::combos`].
    CombosChanged,
    /// The unfolded set changed. The picker has no state dir, so the caller
    /// saves [`ModelPicker::expanded_providers`].
    Folded,
    /// Enter on a provider that cannot serve models until it is set up.
    Login(String),
    Close,
}

struct ModelEntry {
    /// The qualified spec, or the bare slug on a provider row.
    spec: String,
    id: String,
    name: Option<String>,
    provider: String,
    provider_display: String,
    suffix: Option<String>,
    detail: String,
    /// The compaction pick, which the row highlights.
    highlighted: bool,
    free: bool,
    is_provider: bool,
    /// Set on a combo row: its two halves, for delete and effort cycling.
    combo: Option<(String, String)>,
    prefer_model_name: bool,
    /// A model of a folded provider, listed only while searching.
    hidden: bool,
    needs_setup: bool,
}

impl ModelEntry {
    fn is_recent(&self) -> bool {
        self.provider_display == RECENT_SECTION
    }
}

impl PickerItem for ModelEntry {
    fn label(&self) -> &str {
        if self.prefer_model_name {
            self.name.as_deref().unwrap_or(&self.id)
        } else {
            &self.id
        }
    }

    fn search_keys(&self) -> Vec<&str> {
        let mut keys = Vec::with_capacity(2);
        keys.push(self.label());
        if self.prefer_model_name && self.name.is_some() && self.name.as_deref() != Some(&self.id) {
            keys.push(&self.id);
        }
        keys
    }

    fn suffix(&self) -> Option<&str> {
        self.suffix.as_deref()
    }

    fn detail(&self) -> Option<&str> {
        Some(&self.detail)
    }

    fn section(&self) -> Option<&str> {
        Some(self.provider_display.as_str())
    }

    fn is_highlighted(&self) -> bool {
        self.highlighted
    }

    fn is_section_row(&self) -> bool {
        self.is_provider
    }

    fn is_hidden(&self) -> bool {
        self.hidden
    }
}

/// The recent models and the unfolded providers, one copy for every tab's
/// picker. A new tab opens without reading them from disk, and lists what
/// another tab changed.
#[derive(Default)]
pub struct PickerMemory {
    recents: ArcSwap<Vec<String>>,
    /// Providers listed with their models. Every other one is folded.
    expanded: ArcSwap<BTreeSet<String>>,
}

impl PickerMemory {
    pub fn new(recents: Vec<String>, expanded: BTreeSet<String>) -> Self {
        Self {
            recents: ArcSwap::from_pointee(recents),
            expanded: ArcSwap::from_pointee(expanded),
        }
    }

    pub fn record_recent(&self, spec: &str, policy: &ModelPolicy) {
        if policy.allows(spec) {
            self.recents
                .store(Arc::new(with_recent(&self.recents.load(), spec)));
        }
    }
}

pub struct ModelPicker {
    picker: ListPicker<ModelEntry>,
    models: Arc<ArcSwapOption<ModelList>>,
    available: Watch<ModelList>,
    memory: Arc<PickerMemory>,
    current_spec: String,
    needs_rebuild: bool,
    /// Entry to restore on refresh: `(was_recent, spec)`. Seeded from the
    /// preselection at open so it survives the background list arriving, and
    /// moved by user navigation.
    anchor: Option<(bool, String)>,
    prefer_model_name: bool,
    show_cost: bool,
    /// The main half of a combo being built: `+` marked it, Enter on another
    /// model completes the pair.
    pending_main: Option<String>,
}

impl ModelPicker {
    pub fn new(models: Arc<ArcSwapOption<ModelList>>) -> Self {
        Self {
            picker: ListPicker::new().with_footer(FOOTER),
            models,
            available: Watch::default(),
            memory: Arc::default(),
            current_spec: String::new(),
            needs_rebuild: false,
            anchor: None,
            prefer_model_name: true,
            show_cost: true,
            pending_main: None,
        }
    }

    pub fn sharing(mut self, memory: Arc<PickerMemory>) -> Self {
        self.memory = memory;
        self
    }

    pub fn with_prefer_model_name(mut self, prefer: bool) -> Self {
        self.prefer_model_name = prefer;
        self
    }

    pub fn with_show_cost(mut self, show: bool) -> Self {
        self.show_cost = show;
        self
    }

    pub fn expanded_providers(&self) -> Arc<BTreeSet<String>> {
        self.memory.expanded.load_full()
    }

    #[cfg(test)]
    pub fn set_recents(&mut self, recents: Vec<String>) {
        self.memory.recents.store(Arc::new(recents));
        self.needs_rebuild = true;
    }

    pub fn open(&mut self, current_spec: &str) {
        self.current_spec = current_spec.to_owned();
        self.anchor = None;
        self.pending_main = None;
        self.needs_rebuild = false;
        let _ = self.available.poll(self.models.load_full());
        let entries = self.load_entries();
        self.picker.open(entries, TITLE);
        self.show_loading();
        if self.preselect_current_model() {
            self.anchor = self.selected_anchor();
        }
    }

    /// Providers fetch their model lists in the background and drop them into
    /// a shared slot, which wakes nothing. An open picker has to notice on its
    /// own, so `App::tick` polls this instead of [`Self::view`] reading the
    /// slot mid render.
    pub fn refresh(&mut self) -> Dirty {
        if !self.picker.is_open() {
            return Dirty::NO;
        }
        let arrived = self.available.poll(self.models.load_full());
        if arrived == Dirty::NO && !self.needs_rebuild {
            return Dirty::NO;
        }
        self.needs_rebuild = false;
        let entries = self.load_entries();
        self.picker.replace_items(entries);
        self.show_loading();
        if let Some((was_recent, spec)) = &self.anchor {
            self.picker
                .select_item_by(|e| e.spec == *spec && e.is_recent() == *was_recent);
        } else {
            self.preselect_current_model();
        }
        Dirty::YES
    }

    /// A slow provider leaves a short list that looks final without this.
    /// A combo being built outranks it: the user is mid-gesture.
    fn show_loading(&mut self) {
        if let Some(main) = &self.pending_main {
            let name = model_registry::resolve_model_name_or(main, main);
            self.picker
                .set_notice(Some(format!("{COMBO_PENDING_NOTICE} [{name}]")));
            return;
        }
        let loading = self.available.get().is_some_and(|list| list.loading);
        self.picker.set_notice(loading.then_some(LOADING_NOTICE));
    }

    fn load_entries(&self) -> Vec<ModelEntry> {
        let list = self.available.get();
        // A model the provider has retired must not linger in Recent. Only a
        // provider that answered live can say it is gone: a failed probe falls
        // back to a short static list, and snapshots taken while discovery is
        // still running are missing the providers that have yet to answer.
        let retired = |spec: &String| {
            list.is_some_and(|l| {
                !l.loading
                    && !l.specs.contains(spec)
                    && spec.split_once('/').is_some_and(|(provider, _)| {
                        l.providers.get(provider) == Some(&ProviderStatus::Live)
                    })
            })
        };
        let mut entries = Vec::new();
        for def in model_registry::combos() {
            entries.push(combo_entry(&def, self.prefer_model_name, self.show_cost));
        }
        for spec in self.memory.recents.load().iter() {
            if retired(spec) {
                continue;
            }
            if let Some(mut e) = parse_model_entry(spec, self.prefer_model_name, self.show_cost) {
                // The provider name takes the suffix slot here, so a fast
                // mark moves into the detail column instead of vanishing.
                if e.suffix.as_deref() == Some(FAST_MARK) {
                    e.detail = if e.detail.is_empty() {
                        FAST_MARK.to_string()
                    } else {
                        format!("{FAST_MARK}{PRICE_SEPARATOR}{}", e.detail)
                    };
                }
                e.suffix = Some(std::mem::take(&mut e.provider_display));
                e.provider_display = RECENT_SECTION.to_string();
                entries.push(e);
            }
        }
        // Keyed by display name, then slug: the order the providers list in.
        let mut groups: BTreeMap<(String, String), Vec<ModelEntry>> = BTreeMap::new();
        for entry in list
            .iter()
            .flat_map(|l| &l.specs)
            .filter_map(|spec| parse_model_entry(spec, self.prefer_model_name, self.show_cost))
        {
            let key = (entry.provider_display.clone(), entry.provider.clone());
            groups.entry(key).or_default().push(entry);
        }
        // A provider with nothing to pick still lists when it needs fixing.
        for (slug, status) in list.iter().flat_map(|l| &l.providers) {
            if *status != ProviderStatus::Live
                && let Some(display) = provider_display(slug)
            {
                groups.entry((display, slug.clone())).or_default();
            }
        }
        let expanded = self.memory.expanded.load();
        for ((display, slug), mut models) in groups {
            let status = list.and_then(|l| l.providers.get(&slug));
            let hidden = !expanded.contains(&slug);
            entries.push(provider_row(slug, display, models.len(), status));
            models.sort_by(|a, b| b.free.cmp(&a.free).then_with(|| a.id.cmp(&b.id)));
            entries.extend(
                models
                    .into_iter()
                    .map(|model| ModelEntry { hidden, ..model }),
            );
        }
        for entry in &mut entries {
            if entry.spec == self.current_spec {
                entry.id.insert_str(0, CURRENT_MARK);
            }
        }
        entries
    }

    fn preselect_current_model(&mut self) -> bool {
        if self
            .picker
            .select_item_by(|e| e.spec == self.current_spec && e.is_recent())
            || self.picker.select_item_by(|e| e.spec == self.current_spec)
        {
            return true;
        }
        // The top row might be a folded provider, and Enter there unfolds it
        // instead of picking a model.
        self.picker.select_item_by(|e| !e.is_provider);
        false
    }

    pub fn is_open(&self) -> bool {
        self.picker.is_open()
    }

    pub fn close(&mut self) {
        self.picker.close();
    }

    pub fn contains(&self, pos: Position) -> bool {
        self.picker.contains(pos)
    }

    pub fn scroll(&mut self, delta: i32) {
        self.picker.scroll(delta);
    }

    fn selected_anchor(&self) -> Option<(bool, String)> {
        self.picker
            .selected_item()
            .map(|e| (e.is_recent(), e.spec.clone()))
    }

    fn track_anchor<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let before = self.picker.selected_index();
        let result = f(self);
        if let (Some(before), Some(after)) = (before, self.picker.selected_index())
            && before != after
        {
            self.anchor = self.selected_anchor();
        }
        result
    }

    pub fn handle_paste(&mut self, text: &str) {
        self.track_anchor(|p| p.picker.handle_paste(text));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ModelPickerAction {
        self.track_anchor(|p| p.handle_key_inner(key))
    }

    fn toggle_fold(&mut self, slug: String) -> ModelPickerAction {
        let mut expanded = BTreeSet::clone(&self.memory.expanded.load());
        if !expanded.remove(&slug) {
            expanded.insert(slug.clone());
        }
        self.memory.expanded.store(Arc::new(expanded));
        // Following the provider row keeps the cursor on it, so the next press
        // undoes this one.
        self.anchor = Some((false, slug));
        self.needs_rebuild = true;
        ModelPickerAction::Folded
    }

    fn toggle_all_folds(&mut self) -> ModelPickerAction {
        let expanded = if self.memory.expanded.load().is_empty() {
            self.available
                .get()
                .iter()
                .flat_map(|l| &l.specs)
                .filter_map(|spec| spec.split_once('/'))
                .map(|(slug, _)| slug.to_owned())
                .collect()
        } else {
            BTreeSet::new()
        };
        self.memory.expanded.store(Arc::new(expanded));
        if let Some(entry) = self.picker.selected_item().filter(|e| !e.is_recent()) {
            self.anchor = Some((false, entry.provider.clone()));
        }
        self.needs_rebuild = true;
        ModelPickerAction::Folded
    }

    fn handle_key_inner(&mut self, key: KeyEvent) -> ModelPickerAction {
        if key.code == FOLD_ALL_KEY {
            return self.toggle_all_folds();
        }
        let selected = self.picker.selected_item();
        // Enter on a provider row would hand back a bare slug as the model.
        if key.code == FOLD_KEY
            || (key.code == KeyCode::Enter && selected.is_some_and(|e| e.is_provider))
        {
            // Swallowed even on an empty list, so `%` never ends up typed
            // into the search.
            let Some(entry) = selected else {
                return ModelPickerAction::Consumed;
            };
            let slug = entry.provider.clone();
            if key.code == KeyCode::Enter && entry.needs_setup {
                return ModelPickerAction::Login(slug);
            }
            return self.toggle_fold(slug);
        }
        if key.code == COMBO_KEY {
            return self.combo_mark();
        }
        if key.code == COMBO_DELETE_KEY || key.code == COMBO_EFFORT_KEY {
            return self.combo_edit(key.code == COMBO_EFFORT_KEY);
        }
        if is_compaction_shortcut(key)
            && let Some(entry) = self.picker.selected_item()
        {
            if entry.is_provider || entry.combo.is_some() {
                return ModelPickerAction::Consumed;
            }
            let spec = entry.spec.clone();
            self.needs_rebuild = true;
            if entry.highlighted {
                ModelPickerAction::UnassignCompaction(spec)
            } else {
                ModelPickerAction::AssignCompaction(spec)
            }
        } else if key.code == KeyCode::Enter
            && self.pending_main.is_some()
            && let Some(entry) = self.picker.selected_item()
            && !entry.is_provider
            && entry.combo.is_none()
        {
            let main = self.pending_main.take().expect("pending main checked");
            let def = ComboDef {
                main,
                sub: entry.spec.clone(),
                effort: None,
            };
            model_registry::add_combo(def);
            self.needs_rebuild = true;
            ModelPickerAction::CombosChanged
        } else {
            match self.picker.handle_key(key) {
                PickerAction::Consumed => ModelPickerAction::Consumed,
                PickerAction::Select(entry) => ModelPickerAction::Select(entry.spec),
                PickerAction::Close => {
                    self.pending_main = None;
                    ModelPickerAction::Close
                }
                PickerAction::Toggle(..) => ModelPickerAction::Consumed,
            }
        }
    }

    /// `+` marks the selected model as a combo's main half, or cancels a
    /// pending mark. Provider and combo rows cannot be halves.
    fn combo_mark(&mut self) -> ModelPickerAction {
        if self.pending_main.take().is_some() {
            self.needs_rebuild = true;
            return ModelPickerAction::Consumed;
        }
        if let Some(entry) = self.picker.selected_item()
            && !entry.is_provider
            && entry.combo.is_none()
        {
            self.pending_main = Some(entry.spec.clone());
            self.needs_rebuild = true;
        }
        ModelPickerAction::Consumed
    }

    /// `-` deletes the combo under the cursor; `^` cycles its effort.
    fn combo_edit(&mut self, cycle_effort: bool) -> ModelPickerAction {
        let Some(entry) = self.picker.selected_item() else {
            return ModelPickerAction::Consumed;
        };
        let Some((main, sub)) = entry.combo.clone() else {
            return ModelPickerAction::Consumed;
        };
        self.needs_rebuild = true;
        if cycle_effort {
            let current = model_registry::combos()
                .into_iter()
                .find(|c| c.main == main && c.sub == sub)
                .and_then(|c| c.effort);
            model_registry::set_combo_effort(&main, &sub, combo::next_effort(current));
        } else {
            model_registry::remove_combo(&main, &sub);
        }
        ModelPickerAction::CombosChanged
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) -> (Rect, Option<Position>) {
        self.picker.view(frame, area)
    }
}

impl Overlay for ModelPicker {
    fn is_prompting(&self) -> bool {
        false
    }

    fn is_open(&self) -> bool {
        self.is_open()
    }

    fn close(&mut self) {
        self.close()
    }

    fn cadence(&self) -> Cadence {
        self.picker.cadence()
    }
}

fn format_pricing(model: &Model) -> Option<String> {
    let pricing = &model.pricing;
    (model.subsidised_by.is_none() && !pricing.is_zero()).then(|| {
        format!(
            "{:>PRICE_WIDTH$}/{:<PRICE_WIDTH$}",
            format!("${:.2}", pricing.input),
            format!("${:.2}", pricing.output),
        )
    })
}

fn provider_display(slug: &str) -> Option<String> {
    if let Some(spec) = ProviderRegistry::get(slug) {
        Some(spec.display_name.to_string())
    } else if let Some(builtin) = maki_config::providers::builtin_provider(slug) {
        Some(builtin.display_name.to_string())
    } else {
        let config = maki_config::providers::ProvidersConfig::load();
        let def = config.get(slug);
        Some(maki_config::providers::resolve_display_name(slug, def))
    }
}

/// The free label and price of a resolved model, the detail column's halves.
fn model_detail(model: Option<&Model>, show_cost: bool) -> (String, bool) {
    let Some(model) = model else {
        return (String::new(), false);
    };
    let free = model.is_free();
    let detail = match (show_cost.then(|| format_pricing(model)).flatten(), free) {
        (Some(price), true) => format!("{FREE_PREFIX}{price}"),
        (Some(price), false) => price,
        (None, true) => FREE_LABEL.to_string(),
        (None, false) => String::new(),
    };
    (detail, free)
}

fn parse_model_entry(spec: &str, prefer_model_name: bool, show_cost: bool) -> Option<ModelEntry> {
    if let Some(parts) = combo::split(spec) {
        return Some(combo_entry(
            &ComboDef {
                main: parts.main.to_string(),
                sub: parts.sub.to_string(),
                effort: parts.effort,
            },
            prefer_model_name,
            show_cost,
        ));
    }
    let (provider_str, model_id) = spec.split_once('/')?;
    let provider_display = provider_display(provider_str)?;

    let model = Model::from_spec(spec).ok();
    let (detail, free) = model_detail(model.as_ref(), show_cost);
    let fast = model.as_ref().is_some_and(Model::supports_fast);
    let id = model_id.to_string();
    let name = model_registry::resolve_model_name(provider_str, model_id);
    Some(ModelEntry {
        spec: spec.to_string(),
        id,
        name,
        provider: provider_str.to_string(),
        provider_display,
        suffix: fast.then(|| FAST_MARK.to_string()),
        detail,
        highlighted: model_registry::compaction().as_deref() == Some(spec),
        free,
        is_provider: false,
        combo: None,
        prefer_model_name,
        hidden: false,
        needs_setup: false,
    })
}

/// A combo's row: the two names joined, the subagent half and its effort in
/// the detail column. A half that no longer resolves still lists, named by
/// its spec, so the user can delete the combo instead of losing it silently.
fn combo_entry(def: &ComboDef, prefer_model_name: bool, show_cost: bool) -> ModelEntry {
    let spec = def.spec();
    let main_name = model_registry::resolve_model_name_or(&def.main, &def.main);
    let sub_name = model_registry::resolve_model_name_or(&def.sub, &def.sub);
    let label = format!("{main_name}{COMBO_SEPARATOR_LABEL}{sub_name}");
    let effort = def
        .effort
        .map(|e| e.to_string())
        .unwrap_or_else(|| FOLLOW_SESSION_LABEL.to_string());
    let model = Model::from_spec(&spec).ok();
    let (price, free) = model_detail(model.as_ref(), show_cost);
    let mut detail = format!("sub: {} @ {effort}", def.sub);
    if !price.is_empty() {
        detail = format!("{price}{PRICE_SEPARATOR}{detail}");
    }
    let provider = def
        .main
        .split_once('/')
        .map(|(slug, _)| slug.to_string())
        .unwrap_or_default();
    ModelEntry {
        spec,
        id: label.clone(),
        name: prefer_model_name.then_some(label),
        provider,
        provider_display: COMBO_SECTION.to_string(),
        suffix: None,
        detail,
        highlighted: false,
        free,
        is_provider: false,
        combo: Some((def.main.clone(), def.sub.clone())),
        prefer_model_name,
        hidden: false,
        needs_setup: false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::components::keybindings::key as kb;
    use crate::components::{buffer_text, key};
    use crossterm::event::{KeyCode, KeyEvent};
    use maki_providers::ModelInfo;
    use maki_providers::ModelPricing;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use test_case::test_case;

    const SAME_SIZED_LIST: &str = "a republished list of the same length is still a new list";
    const SWAPPED_SPEC: &str = "zai/glm-5";
    const RETIRED_SPEC: &str = "anthropic/claude-retired";
    const GLM_FILTER: &str = "glm";

    /// A provider that republishes the same number of specs has still changed
    /// the list. Comparing lengths calls that no change, and the picker goes on
    /// offering models that are gone.
    #[test]
    fn a_same_sized_model_list_owes_a_frame() {
        let models = Arc::new(ArcSwapOption::empty());
        models.store(Some(loaded(vec![
            "anthropic/claude-sonnet-4-20250514".into(),
        ])));
        let mut p = unfolded(Arc::clone(&models));
        p.open("");
        assert_eq!(p.refresh(), Dirty::NO);

        models.store(Some(loaded(vec![SWAPPED_SPEC.into()])));
        assert_eq!(p.refresh(), Dirty::YES, "{SAME_SIZED_LIST}");
        assert_eq!(
            p.picker.selected_item().map(|e| e.spec.as_str()),
            Some(SWAPPED_SPEC),
            "{SAME_SIZED_LIST}"
        );
    }

    fn loaded(specs: Vec<String>) -> Arc<ModelList> {
        let providers = specs
            .iter()
            .filter_map(|spec| spec.split_once('/'))
            .map(|(provider, _)| (provider.to_string(), ProviderStatus::Live))
            .collect();
        Arc::new(ModelList {
            specs,
            providers,
            loading: false,
        })
    }

    /// A picker with every test provider unfolded, for tests about which model
    /// the cursor lands on.
    fn unfolded(models: Arc<ArcSwapOption<ModelList>>) -> ModelPicker {
        let expanded = BTreeSet::from([PROVIDER.to_owned(), ZAI.to_owned()]);
        ModelPicker::new(models).sharing(Arc::new(PickerMemory::new(Vec::new(), expanded)))
    }

    fn rendered(p: &mut ModelPicker) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|f| _ = p.view(f, f.area())).unwrap();
        buffer_text(terminal.backend().buffer())
    }

    /// A slow provider must not look like a finished, short list.
    #[test]
    fn loading_row_shows_until_discovery_finishes() {
        let models = Arc::new(ArcSwapOption::empty());
        models.store(Some(Arc::new(ModelList {
            specs: vec![SWAPPED_SPEC.into()],
            providers: HashMap::new(),
            loading: true,
        })));
        let mut p = ModelPicker::new(Arc::clone(&models));
        p.open("");
        assert!(rendered(&mut p).contains(LOADING_NOTICE));

        models.store(Some(loaded(vec![SWAPPED_SPEC.into()])));
        assert_eq!(p.refresh(), Dirty::YES);
        assert!(!rendered(&mut p).contains(LOADING_NOTICE));
    }

    fn test_models() -> Arc<ArcSwapOption<ModelList>> {
        let models = Arc::new(ArcSwapOption::empty());
        models.store(Some(loaded(vec![
            "anthropic/claude-sonnet-4-20250514".into(),
            "anthropic/claude-opus-4-6-20260101".into(),
            "zai/glm-5".into(),
        ])));
        models
    }

    const FOLD_SPEC: &str = "anthropic/claude-sonnet-4-20250514";
    const KEPT_OPEN: &str = "the fold is not a choice, so the picker stays up";
    const SHARED_BY_TABS: &str = "every tab's picker lists the one set of recents and folds";
    const PROVIDER: &str = "anthropic";
    const ZAI: &str = "zai";

    fn lists_models_of(p: &ModelPicker, slug: &str) -> bool {
        p.load_entries()
            .iter()
            .any(|e| !e.is_provider && !e.hidden && e.provider == slug)
    }

    fn select_provider(p: &mut ModelPicker, slug: &str) {
        assert!(
            p.picker
                .select_item_by(|e| e.is_provider && e.provider == slug)
        );
    }

    #[test]
    fn providers_open_folded_but_search_still_finds_their_models() {
        let mut p = ModelPicker::new(test_models());
        p.open("");
        assert!(!lists_models_of(&p, PROVIDER));
        assert!(!lists_models_of(&p, ZAI));

        p.handle_paste("glm");
        let action = p.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, ModelPickerAction::Select(ref s) if s == SWAPPED_SPEC));
    }

    #[test_case(key(KeyCode::Char('%')) ; "the_fold_key")]
    #[test_case(key(KeyCode::Enter)     ; "enter")]
    fn a_provider_row_unfolds_and_folds_again(toggle: KeyEvent) {
        let mut p = ModelPicker::new(test_models());
        p.open("");
        select_provider(&mut p, PROVIDER);

        let action = p.handle_key(toggle);
        assert!(matches!(action, ModelPickerAction::Folded));
        assert!(p.is_open(), "{KEPT_OPEN}");
        assert_eq!(
            *p.expanded_providers(),
            BTreeSet::from([PROVIDER.to_owned()])
        );
        let _ = p.refresh();
        assert!(lists_models_of(&p, PROVIDER), "its models come out");
        assert!(!lists_models_of(&p, ZAI), "other providers are untouched");

        p.handle_key(toggle);
        assert!(
            p.expanded_providers().is_empty(),
            "the cursor stayed on the row"
        );
    }

    #[test]
    fn the_fold_key_on_a_model_folds_its_provider() {
        let mut p = unfolded(test_models());
        p.open(FOLD_SPEC);

        p.handle_key(key(KeyCode::Char('%')));
        assert_eq!(*p.expanded_providers(), BTreeSet::from([ZAI.to_owned()]));
        let _ = p.refresh();
        assert_eq!(
            p.picker.selected_item().map(|e| e.spec.as_str()),
            Some(PROVIDER),
            "the cursor follows the fold onto the provider row"
        );
    }

    #[test]
    fn the_fold_all_key_unfolds_everything_then_folds_it_back() {
        let mut p = ModelPicker::new(test_models());
        p.open("");

        assert!(matches!(
            p.handle_key(key(FOLD_ALL_KEY)),
            ModelPickerAction::Folded
        ));
        assert_eq!(
            *p.expanded_providers(),
            BTreeSet::from([PROVIDER.to_owned(), ZAI.to_owned()])
        );

        p.handle_key(key(FOLD_ALL_KEY));
        assert!(p.expanded_providers().is_empty());
    }

    fn pick_recent(p: &mut ModelPicker) {
        p.set_recents(vec![FOLD_SPEC.into()]);
    }

    fn unfold_provider(p: &mut ModelPicker) {
        p.open("");
        select_provider(p, PROVIDER);
        p.handle_key(key(FOLD_KEY));
    }

    fn lists_recent(p: &ModelPicker) -> bool {
        p.load_entries()
            .iter()
            .any(|e| e.is_recent() && e.spec == FOLD_SPEC)
    }

    fn lists_provider_models(p: &ModelPicker) -> bool {
        lists_models_of(p, PROVIDER)
    }

    #[test_case(pick_recent,     lists_recent          ; "a_pick")]
    #[test_case(unfold_provider, lists_provider_models ; "a_fold")]
    fn another_tab_lists_what_one_tab_changed(
        change: fn(&mut ModelPicker),
        lists_change: fn(&ModelPicker) -> bool,
    ) {
        let memory = Arc::<PickerMemory>::default();
        let mut first = ModelPicker::new(test_models()).sharing(Arc::clone(&memory));
        let mut second = ModelPicker::new(test_models()).sharing(memory);

        change(&mut first);
        second.open("");
        assert!(lists_change(&second), "{SHARED_BY_TABS}");
    }

    fn reported(specs: &[&str], status: ProviderStatus) -> Arc<ArcSwapOption<ModelList>> {
        Arc::new(ArcSwapOption::from_pointee(ModelList {
            specs: specs.iter().map(ToString::to_string).collect(),
            providers: HashMap::from([(PROVIDER.to_owned(), status)]),
            loading: false,
        }))
    }

    #[test_case(ProviderStatus::MissingKey,          &[],        Some(NO_KEY_LABEL),   true  ; "no_key")]
    #[test_case(ProviderStatus::Failed(String::new()), &[],        Some(OFFLINE_LABEL),  true  ; "offline")]
    #[test_case(ProviderStatus::Failed(String::new()), &[FOLD_SPEC], Some(FALLBACK_LABEL), false ; "fallback")]
    #[test_case(ProviderStatus::Live,                &[FOLD_SPEC], None,                 false ; "live")]
    fn a_provider_row_shows_what_discovery_said(
        status: ProviderStatus,
        specs: &[&str],
        label: Option<&str>,
        opens_setup: bool,
    ) {
        let mut p = ModelPicker::new(reported(specs, status));
        p.open("");
        select_provider(&mut p, PROVIDER);
        assert_eq!(p.picker.selected_item().unwrap().suffix(), label);

        let action = p.handle_key(key(KeyCode::Enter));
        if opens_setup {
            assert!(matches!(action, ModelPickerAction::Login(ref slug) if slug == PROVIDER));
        } else {
            assert!(matches!(action, ModelPickerAction::Folded));
        }
    }

    /// An allow-list can leave a provider that answered with nothing to pick,
    /// and a row for it would only be noise.
    #[test]
    fn a_live_provider_with_no_models_has_no_row() {
        let mut p = ModelPicker::new(reported(&[], ProviderStatus::Live));
        p.open("");
        assert!(p.load_entries().is_empty());
    }

    #[test]
    fn the_current_model_is_marked() {
        let mut p = ModelPicker::new(test_models());
        p.set_recents(vec![FOLD_SPEC.into()]);
        p.open(FOLD_SPEC);
        let marked: Vec<String> = p
            .load_entries()
            .into_iter()
            .filter(|e| e.id.starts_with(CURRENT_MARK))
            .map(|e| e.spec)
            .collect();
        assert_eq!(
            marked,
            [FOLD_SPEC, FOLD_SPEC],
            "its Recent copy and its own row"
        );
    }

    #[test_case(key(KeyCode::Esc)          ; "esc_closes")]
    #[test_case(kb::QUIT.to_key_event()    ; "ctrl_c_closes")]
    fn close_keys(cancel_key: KeyEvent) {
        let mut p = ModelPicker::new(test_models());
        p.open("");
        let action = p.handle_key(cancel_key);
        assert!(matches!(action, ModelPickerAction::Close));
        assert!(!p.is_open());
    }

    #[test]
    fn refresh_updates_items_and_preserves_search() {
        let models = Arc::new(ArcSwapOption::empty());
        models.store(Some(loaded(vec![
            "anthropic/claude-sonnet-4-20250514".into(),
        ])));
        let mut p = ModelPicker::new(models.clone());
        p.open("");

        p.handle_paste("opus");

        models.store(Some(loaded(vec![
            "anthropic/claude-sonnet-4-20250514".into(),
            "anthropic/claude-opus-4-6-20260101".into(),
        ])));
        let _ = p.refresh();

        let action = p.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(action, ModelPickerAction::Select(ref s) if s.contains("opus")),
            "after refresh, 'op' filter should match opus"
        );
    }

    #[test]
    fn open_preselects_current_model() {
        let mut p = unfolded(test_models());
        p.open("anthropic/claude-opus-4-6-20260101");
        let action = p.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(action, ModelPickerAction::Select(ref s) if s == "anthropic/claude-opus-4-6-20260101")
        );
    }

    #[test]
    fn parse_model_entry_valid() {
        let entry = parse_model_entry("responses/my-model", true, true).unwrap();
        assert_eq!(entry.id, "my-model");
        assert_eq!(entry.provider_display, "Responses API");
    }

    #[test]
    fn parse_model_entry_paid_model_not_marked_free() {
        let entry = parse_model_entry("anthropic/claude-sonnet-4-20250514", true, true).unwrap();
        assert!(
            !entry.detail.starts_with(FREE_PREFIX),
            "paid anthropic model must not be marked free"
        );
    }

    #[test]
    fn parse_model_entry_no_slash() {
        assert!(parse_model_entry("no-slash", true, true).is_none());
    }

    #[test_case(key(KeyCode::Char('$')) ; "dollar")]
    #[test_case(key(KeyCode::Char('€')) ; "euro")]
    fn compaction_shortcut_assigns_and_keeps_picker_open(k: KeyEvent) {
        let mut p = unfolded(test_models());
        p.open(FOLD_SPEC);
        let action = p.handle_key(k);
        assert!(
            matches!(&action, ModelPickerAction::AssignCompaction(s) if s == FOLD_SPEC),
            "expected AssignCompaction(claude-sonnet)",
        );
        assert!(p.is_open());
    }

    #[test]
    fn refresh_preserves_selection_for_current_model() {
        let models = Arc::new(ArcSwapOption::empty());
        let mut p = unfolded(models.clone());
        p.open("anthropic/claude-opus-4-6-20260101");

        models.store(Some(loaded(vec![
            "anthropic/claude-sonnet-4-20250514".into(),
            "anthropic/claude-opus-4-6-20260101".into(),
            "zai/glm-5".into(),
        ])));
        let _ = p.refresh();

        let action = p.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(action, ModelPickerAction::Select(ref s) if s == "anthropic/claude-opus-4-6-20260101"),
            "after async model arrival, current model should still be selected"
        );
    }

    #[test]
    fn recents_include_current_model_preselected() {
        let models = test_models();
        let mut p = unfolded(models);
        p.set_recents(vec![
            "zai/glm-5".into(),
            "anthropic/claude-sonnet-4-20250514".into(),
        ]);
        p.open("anthropic/claude-opus-4-6-20260101");

        p.picker.select(0);
        let action = p.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(action, ModelPickerAction::Select(ref s) if s == "zai/glm-5"),
            "first entry should be the most recent model",
        );

        p.set_recents(vec![
            "zai/glm-5".into(),
            "anthropic/claude-sonnet-4-20250514".into(),
        ]);
        p.open("zai/glm-5");
        let action = p.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(action, ModelPickerAction::Select(ref s) if s == "zai/glm-5"),
            "current model should be preselected",
        );
    }

    #[test]
    fn open_preselects_current_model_in_recent_section_when_in_recents() {
        let models = test_models();
        let mut p = unfolded(models);
        p.set_recents(vec![
            "zai/glm-5".into(),
            "anthropic/claude-sonnet-4-20250514".into(),
        ]);
        p.open("zai/glm-5");

        let entry = p.picker.selected_item().expect("selection on open");
        assert_eq!(entry.spec, "zai/glm-5");
        assert_eq!(
            entry.section(),
            Some("Recent"),
            "selection should land on the Recent copy when present in recents",
        );
    }

    #[test]
    fn open_preselects_current_model_in_provider_section_when_not_in_recents() {
        let models = test_models();
        let mut p = unfolded(models);
        p.set_recents(vec![
            "zai/glm-5".into(),
            "anthropic/claude-sonnet-4-20250514".into(),
        ]);
        p.open("anthropic/claude-opus-4-6-20260101");

        let entry = p.picker.selected_item().expect("selection on open");
        assert_eq!(entry.spec, "anthropic/claude-opus-4-6-20260101");
        assert_eq!(
            entry.section(),
            Some("anthropic"),
            "selection should land on provider entry when model is not in recents",
        );
    }

    #[test]
    fn refresh_keeps_selection_on_provider_entry() {
        let models = test_models();
        let mut p = unfolded(models);
        p.set_recents(vec![
            "zai/glm-5".into(),
            "anthropic/claude-sonnet-4-20250514".into(),
        ]);
        p.open("anthropic/claude-sonnet-4-20250514");
        p.handle_key(key(KeyCode::Down));
        p.handle_key(key(KeyCode::Down));
        p.handle_key(key(KeyCode::Down));
        p.handle_key(key(KeyCode::Char('$')));

        let _ = p.refresh();

        let entry = p.picker.selected_item().expect("selection after refresh");
        assert_eq!(entry.spec, "anthropic/claude-sonnet-4-20250514");
        assert_eq!(
            entry.section(),
            Some("anthropic"),
            "selection should stay on the provider entry, not jump to Recent",
        );
    }

    #[test]
    fn refresh_after_collapse_anchors_to_provider_entry() {
        let models = test_models();
        let mut p = unfolded(models.clone());
        p.set_recents(vec![
            "zai/glm-5".into(),
            "anthropic/claude-sonnet-4-20250514".into(),
        ]);
        p.open("anthropic/claude-sonnet-4-20250514");
        p.handle_key(key(KeyCode::Down));
        p.handle_key(key(KeyCode::Down));
        p.handle_key(key(KeyCode::Down));

        models.store(None);
        let _ = p.refresh();
        let entry = p.picker.selected_item().expect("selection during collapse");
        assert_eq!(entry.spec, "anthropic/claude-sonnet-4-20250514");
        assert_eq!(entry.section(), Some("Recent"));

        models.store(Some(loaded(vec![
            "anthropic/claude-sonnet-4-20250514".into(),
            "anthropic/claude-opus-4-6-20260101".into(),
            "zai/glm-5".into(),
        ])));
        let _ = p.refresh();

        let entry = p.picker.selected_item().expect("selection after arrival");
        assert_eq!(entry.spec, "anthropic/claude-sonnet-4-20250514");
        assert_eq!(
            entry.section(),
            Some("anthropic"),
            "cursor should migrate to the provider entry once it arrives",
        );
    }

    #[test]
    fn arrival_keeps_the_entry_preselected_at_open() {
        let models = Arc::new(ArcSwapOption::empty());
        let mut p = unfolded(models.clone());
        p.set_recents(vec![
            "zai/glm-5".into(),
            "anthropic/claude-sonnet-4-20250514".into(),
        ]);
        p.open("anthropic/claude-sonnet-4-20250514");

        let entry = p.picker.selected_item().expect("selection on open");
        assert_eq!(
            entry.section(),
            Some("Recent"),
            "only recents are listed before the background fetch finishes",
        );

        models.store(Some(loaded(vec![
            "anthropic/claude-sonnet-4-20250514".into(),
            "anthropic/claude-opus-4-6-20260101".into(),
            "zai/glm-5".into(),
        ])));
        let _ = p.refresh();

        let entry = p.picker.selected_item().expect("selection after arrival");
        assert_eq!(entry.spec, "anthropic/claude-sonnet-4-20250514");
        assert_eq!(
            entry.section(),
            Some("Recent"),
            "the entry preselected at open should stay selected when the provider list arrives",
        );
    }

    #[test]
    fn arrival_selects_current_model_missing_from_recents() {
        let models = Arc::new(ArcSwapOption::empty());
        let mut p = unfolded(models.clone());
        p.set_recents(vec!["zai/glm-5".into()]);
        p.open("anthropic/claude-opus-4-6-20260101");

        models.store(Some(loaded(vec![
            "anthropic/claude-opus-4-6-20260101".into(),
            "zai/glm-5".into(),
        ])));
        let _ = p.refresh();

        let entry = p.picker.selected_item().expect("selection after arrival");
        assert_eq!(entry.spec, "anthropic/claude-opus-4-6-20260101");
    }

    /// Discovery publishes a snapshot per batch, so the preselected entry
    /// has to survive every intermediate list, not just the finished one.
    #[test]
    fn progressive_arrival_keeps_the_entry_preselected_at_open() {
        let models = Arc::new(ArcSwapOption::empty());
        let mut p = unfolded(models.clone());
        p.set_recents(vec!["anthropic/claude-sonnet-4-20250514".into()]);
        p.open("anthropic/claude-sonnet-4-20250514");

        for specs in [
            vec!["anthropic/claude-sonnet-4-20250514".to_string()],
            vec![
                "anthropic/claude-sonnet-4-20250514".to_string(),
                "zai/glm-5".to_string(),
            ],
        ] {
            models.store(Some(Arc::new(ModelList {
                specs,
                providers: HashMap::new(),
                loading: true,
            })));
            let _ = p.refresh();
            let entry = p.picker.selected_item().expect("selection after a batch");
            assert_eq!(
                entry.section(),
                Some("Recent"),
                "an intermediate snapshot must not move the cursor to the provider entry",
            );
        }
    }

    #[test]
    fn recents_drop_models_the_provider_no_longer_lists() {
        let models = Arc::new(ArcSwapOption::empty());
        let mut p = unfolded(models.clone());
        p.set_recents(vec![RETIRED_SPEC.into(), SWAPPED_SPEC.into()]);
        p.open(SWAPPED_SPEC);

        models.store(Some(Arc::new(ModelList {
            specs: vec![SWAPPED_SPEC.into()],
            providers: HashMap::from([
                ("anthropic".into(), ProviderStatus::Live),
                ("zai".into(), ProviderStatus::Live),
            ]),
            loading: false,
        })));
        let _ = p.refresh();

        let entries = p.load_entries();
        assert!(!entries.iter().any(|e| e.spec == RETIRED_SPEC));
        assert!(entries.iter().any(|e| e.spec == SWAPPED_SPEC));
    }

    /// A failed probe falls back to the provider's static specs, so a recent
    /// outside that short list is missing without being retired.
    #[test]
    fn recents_survive_a_failed_probe() {
        let models = Arc::new(ArcSwapOption::empty());
        let mut p = unfolded(models.clone());
        p.set_recents(vec![RETIRED_SPEC.into()]);
        p.open(RETIRED_SPEC);

        models.store(Some(loaded(vec![SWAPPED_SPEC.into()])));
        let _ = p.refresh();

        assert!(p.load_entries().iter().any(|e| e.spec == RETIRED_SPEC));
    }

    /// A snapshot from a discovery still in flight is missing providers, so a
    /// recent absent from it may simply not have answered yet.
    #[test]
    fn recents_survive_an_incomplete_list() {
        let models = Arc::new(ArcSwapOption::empty());
        let mut p = unfolded(models.clone());
        p.set_recents(vec![RETIRED_SPEC.into()]);
        p.open(RETIRED_SPEC);

        models.store(Some(Arc::new(ModelList {
            specs: vec![SWAPPED_SPEC.into()],
            providers: HashMap::new(),
            loading: true,
        })));
        let _ = p.refresh();

        assert!(p.load_entries().iter().any(|e| e.spec == RETIRED_SPEC));
    }

    #[test]
    fn refresh_preserves_navigation_to_recent_entry() {
        let models = test_models();
        let mut p = unfolded(models.clone());
        p.set_recents(vec![
            "zai/glm-5".into(),
            "anthropic/claude-sonnet-4-20250514".into(),
        ]);
        p.open("anthropic/claude-sonnet-4-20250514");
        models.store(None);
        let _ = p.refresh();
        p.handle_key(key(KeyCode::Down));

        models.store(Some(loaded(vec![
            "anthropic/claude-sonnet-4-20250514".into(),
            "anthropic/claude-opus-4-6-20260101".into(),
            "zai/glm-5".into(),
        ])));
        let _ = p.refresh();

        let entry = p.picker.selected_item().expect("selection after arrival");
        assert_eq!(entry.spec, "zai/glm-5");
        assert_eq!(
            entry.section(),
            Some("Recent"),
            "user navigation to a Recent entry should survive refresh",
        );
    }

    #[test_case(
        |p: &mut ModelPicker| {
            for c in GLM_FILTER.chars() {
                p.handle_key(key(KeyCode::Char(c)));
            }
        }
        ; "typed"
    )]
    #[test_case(|p: &mut ModelPicker| p.handle_paste(GLM_FILTER) ; "pasted")]
    fn refresh_preserves_selection_with_active_search(filter: fn(&mut ModelPicker)) {
        let models = test_models();
        let mut p = unfolded(models.clone());
        p.set_recents(vec![
            "zai/glm-5".into(),
            "anthropic/claude-sonnet-4-20250514".into(),
        ]);
        p.open("anthropic/claude-sonnet-4-20250514");
        filter(&mut p);

        models.store(None);
        let _ = p.refresh();
        models.store(Some(loaded(vec![
            "anthropic/claude-sonnet-4-20250514".into(),
            "anthropic/claude-opus-4-6-20260101".into(),
            "zai/glm-5".into(),
        ])));
        let _ = p.refresh();

        let entry = p.picker.selected_item().expect("selection after refresh");
        assert_eq!(entry.spec, "zai/glm-5");
        assert_eq!(entry.section(), Some("zai"));
    }

    fn discovered(id: &str, pricing: ModelPricing) -> ModelInfo {
        ModelInfo {
            pricing: Some(pricing),
            ..ModelInfo::id_only(id.into())
        }
    }

    const OX_SPEC: &str = "responses/stealth/ox-alpha";
    const PAID_ID: &str = "vendor/paid-model";
    const PAID_PRICING: ModelPricing = ModelPricing::per_million(3.0, 15.0, 0.0, 0.0);
    const PAID_PRICE_LABEL: &str = "  $3.00/$15.00 ";

    fn register_discovered_models() {
        model_registry::set_known_models(
            "responses",
            vec![
                discovered("stealth/ox-alpha", ModelPricing::ZERO),
                discovered(PAID_ID, PAID_PRICING),
            ],
        );
    }

    #[test]
    fn zero_priced_discovery_marks_entry_free() {
        register_discovered_models();
        let entry = parse_model_entry(OX_SPEC, true, true).unwrap();
        assert!(entry.free, "zero-priced discovery must mark the entry free");
        assert_eq!(entry.detail, FREE_LABEL);
    }

    #[test]
    fn paid_discovery_not_marked_free() {
        register_discovered_models();
        let entry = parse_model_entry(&format!("responses/{PAID_ID}"), true, true).unwrap();
        assert!(
            !entry.detail.starts_with(FREE_PREFIX),
            "paid discovery must not mark the entry free"
        );
        assert_eq!(
            entry.detail, PAID_PRICE_LABEL,
            "paid discovery must show its price"
        );
    }

    #[test]
    fn paid_discovery_hides_price_when_show_cost_is_false() {
        register_discovered_models();
        let entry = parse_model_entry(&format!("responses/{PAID_ID}"), true, false).unwrap();
        assert!(
            !entry.detail.contains('$'),
            "paid discovery must not show price when show_cost is false"
        );
    }

    const FAST_ID: &str = "vendor/fast-model";

    fn register_fast_model(supports_fast: Option<bool>) {
        model_registry::set_known_models(
            "responses",
            vec![ModelInfo {
                supports_fast,
                ..discovered(FAST_ID, PAID_PRICING)
            }],
        );
    }

    #[test_case(Some(true), Some(FAST_MARK) ; "fast_model_carries_the_mark")]
    #[test_case(None, None ; "plain_model_has_no_mark")]
    #[test_case(Some(false), None ; "explicit_no_has_no_mark")]
    fn fast_discovery_marks_the_entry(supports_fast: Option<bool>, expected: Option<&str>) {
        register_fast_model(supports_fast);
        let entry = parse_model_entry(&format!("responses/{FAST_ID}"), true, true).unwrap();
        assert_eq!(entry.suffix.as_deref(), expected);
    }

    #[test]
    fn recent_fast_model_keeps_the_mark_in_detail() {
        register_fast_model(Some(true));
        let spec = format!("responses/{FAST_ID}");
        let models = Arc::new(ArcSwapOption::empty());
        models.store(Some(loaded(vec![spec.clone()])));
        let mut p = unfolded(models);
        p.set_recents(vec![spec]);
        p.open("");

        p.picker.select(0);
        let entry = p.picker.selected_item().expect("recent entry");
        assert_eq!(entry.section(), Some(RECENT_SECTION));
        assert!(
            entry.detail.starts_with(FAST_MARK),
            "the fast mark must survive the provider name taking the suffix"
        );
        assert_ne!(entry.suffix.as_deref(), Some(FAST_MARK));
    }

    #[test]
    fn free_models_sort_before_paid_within_a_provider() {
        register_discovered_models();
        let models = Arc::new(ArcSwapOption::empty());
        models.store(Some(loaded(vec![
            format!("responses/{PAID_ID}"),
            OX_SPEC.into(),
        ])));
        let mut p = unfolded(models);
        p.open("");
        let entries = p.load_entries();
        let ids: Vec<&str> = entries
            .iter()
            .filter(|e| !e.is_provider)
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(ids, ["stealth/ox-alpha", PAID_ID]);
    }

    const COMBO_MAIN_SPEC: &str = "anthropic/claude-opus-4-6-20260101";
    const COMBO_SUB_SPEC: &str = "zai/glm-5";

    fn select_model(p: &mut ModelPicker, spec: &str) {
        assert!(
            p.picker
                .select_item_by(|e| e.spec == spec && !e.is_provider)
        );
    }

    fn create_combo(p: &mut ModelPicker) -> model_registry::ComboDef {
        select_model(p, COMBO_MAIN_SPEC);
        assert!(matches!(
            p.handle_key(key(COMBO_KEY)),
            ModelPickerAction::Consumed
        ));
        select_model(p, COMBO_SUB_SPEC);
        assert!(matches!(
            p.handle_key(key(KeyCode::Enter)),
            ModelPickerAction::CombosChanged
        ));
        let _ = p.refresh();
        model_registry::combos().pop().expect("combo created")
    }

    #[test]
    fn two_step_gesture_creates_a_combo_listed_in_its_own_section() {
        let mut p = unfolded(test_models());
        p.open("");
        let def = create_combo(&mut p);
        assert_eq!(def.main, COMBO_MAIN_SPEC);
        assert_eq!(def.sub, COMBO_SUB_SPEC);
        assert_eq!(def.effort, None);

        let entries = p.load_entries();
        let row = entries
            .iter()
            .find(|e| e.combo.is_some())
            .expect("combo row");
        assert_eq!(row.section(), Some(COMBO_SECTION));
        assert_eq!(row.spec, format!("{COMBO_MAIN_SPEC}+{COMBO_SUB_SPEC}"));
        assert!(row.detail.contains(FOLLOW_SESSION_LABEL));
        model_registry::remove_combo(&def.main, &def.sub);
    }

    #[test]
    fn enter_on_a_combo_selects_its_full_spec() {
        let mut p = unfolded(test_models());
        p.open("");
        let def = create_combo(&mut p);
        select_model(&mut p, &def.spec());
        let action = p.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, ModelPickerAction::Select(ref s) if s == &def.spec()));
        model_registry::remove_combo(&def.main, &def.sub);
    }

    #[test]
    fn the_delete_key_removes_the_combo_under_the_cursor() {
        let mut p = unfolded(test_models());
        p.open("");
        let def = create_combo(&mut p);
        select_model(&mut p, &def.spec());
        assert!(matches!(
            p.handle_key(key(COMBO_DELETE_KEY)),
            ModelPickerAction::CombosChanged
        ));
        assert!(model_registry::combos().is_empty());
    }

    #[test]
    fn the_effort_key_cycles_through_every_level_and_back() {
        let mut p = unfolded(test_models());
        p.open("");
        let def = create_combo(&mut p);
        select_model(&mut p, &def.spec());
        let mut expected = combo::next_effort(None);
        loop {
            assert!(matches!(
                p.handle_key(key(COMBO_EFFORT_KEY)),
                ModelPickerAction::CombosChanged
            ));
            let current = model_registry::combos()[0].effort;
            assert_eq!(current, expected);
            let _ = p.refresh();
            select_model(&mut p, &model_registry::combos()[0].spec());
            match expected {
                None => break,
                Some(e) => expected = combo::next_effort(Some(e)),
            }
        }
        model_registry::remove_combo(&def.main, &def.sub);
    }

    #[test]
    fn the_combo_key_again_cancels_the_pending_main() {
        let mut p = unfolded(test_models());
        p.open("");
        select_model(&mut p, COMBO_MAIN_SPEC);
        p.handle_key(key(COMBO_KEY));
        assert!(p.pending_main.is_some());
        p.handle_key(key(COMBO_KEY));
        assert!(p.pending_main.is_none());
        select_model(&mut p, COMBO_SUB_SPEC);
        let action = p.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, ModelPickerAction::Select(ref s) if s == COMBO_SUB_SPEC));
        assert!(model_registry::combos().is_empty());
    }

    #[test]
    fn model_entry_label_and_search_keys_respect_prefer_model_name() {
        let entry_with_name = ModelEntry {
            spec: "responses/gpt-6.1-sol".into(),
            id: "gpt-6.1-sol".into(),
            name: Some("GPT-6.1 Sol".into()),
            provider: "responses".into(),
            provider_display: "Responses".into(),
            suffix: None,
            detail: "".into(),
            highlighted: false,
            free: false,
            is_provider: false,
            combo: None,
            prefer_model_name: true,
            hidden: false,
            needs_setup: false,
        };

        assert_eq!(entry_with_name.label(), "GPT-6.1 Sol");
        assert_eq!(
            entry_with_name.search_keys(),
            vec!["GPT-6.1 Sol", "gpt-6.1-sol"]
        );

        let mut entry_disabled = entry_with_name;
        entry_disabled.prefer_model_name = false;
        assert_eq!(entry_disabled.label(), "gpt-6.1-sol");
        assert_eq!(entry_disabled.search_keys(), vec!["gpt-6.1-sol"]);

        let entry_no_name = ModelEntry {
            spec: "responses/raw-model".into(),
            id: "raw-model".into(),
            name: None,
            provider: "responses".into(),
            provider_display: "Responses".into(),
            suffix: None,
            detail: "".into(),
            highlighted: false,
            free: false,
            is_provider: false,
            combo: None,
            prefer_model_name: true,
            hidden: false,
            needs_setup: false,
        };
        assert_eq!(entry_no_name.label(), "raw-model");
        assert_eq!(entry_no_name.search_keys(), vec!["raw-model"]);
    }
}
