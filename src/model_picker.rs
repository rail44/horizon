//! The model picker: the modal provider→model two-stage chooser opened from
//! the composer's model chip or the "Switch Model…" palette entry. Same
//! searchable-`List` delegate pattern as the view chooser
//! (`src/view_chooser.rs`); the open/subscribe/close lifecycle lives in
//! `WorkspaceShell::open_model_picker`/`close_model_picker`
//! (`src/workspace/modals.rs`).
//!
//! Two stages live in ONE modal list: `Providers` lists every configured
//! provider in the daemon's `list_providers` order (the `[[providers]]` file
//! order), and confirming an available provider drills into `Models`. The
//! model stage lists the ids the provider's own `/models` listing answers
//! (`list_provider_models`) — the `[[providers]]` file declares no model
//! list, only an optional `default_model` for what runs when nothing has
//! been selected. Discovery is lazy per provider, so a key-unavailable
//! provider never has its endpoint asked at all. Esc walks back a stage
//! before it closes the modal. Key-unavailable providers stay visible but
//! grayed with the reason (they are listed, never hidden), and confirming
//! one is a no-op.

use std::collections::HashMap;

use gpui::*;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::{h_flex, IndexPath};
use horizon_agent::wire::ProviderSummary;

use crate::theme;

/// Which stage the two-stage picker is showing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PickerStage {
    Providers,
    Models { provider: usize },
}

/// One confirmable row at the current stage, as an index into the provider
/// table so a re-filter can never desync a row from the underlying data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PickerItem {
    Provider(usize),
    Model { provider: usize, model: usize },
}

/// What a model-stage confirm hands back to the shell: the provider name and
/// the model id to send to `SessionHub::set_session_model`. The `SessionModel`
/// echo, not this pair, is authoritative for what will actually run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConfirmedModel {
    pub(crate) provider: String,
    pub(crate) model: String,
}

/// A provider's live `/models` discovery state (see
/// `SessionHub::list_provider_models`).
#[derive(Clone, Debug, Default)]
struct LiveListings {
    /// Ids the provider itself listed, in that listing's order. Empty until
    /// the fetch answers.
    ids: Vec<String>,
    /// `true` from the drill-in until the fetch reply lands (or fails).
    loading: bool,
}

/// The delegate's whole state, free of GPUI types so the stage machine,
/// filtering, and confirm/back semantics are unit-testable without a window
/// (the `first_row_to_select` precedent in `src/workspace/modals.rs`).
pub(crate) struct PickerState {
    all: Vec<ProviderSummary>,
    stage: PickerStage,
    filtered: Vec<PickerItem>,
    /// The active search query, kept so a live-models reply can re-filter
    /// without dropping what the user typed.
    query: String,
    /// `true` from open until the async `list_providers` reply lands, so the
    /// empty surface can render "Loading…" rather than "no providers
    /// configured".
    loading: bool,
    /// Per provider index: the live `/models` discovery state (v22).
    live: HashMap<usize, LiveListings>,
}

impl PickerState {
    pub(crate) fn new() -> Self {
        Self {
            all: Vec::new(),
            stage: PickerStage::Providers,
            filtered: Vec::new(),
            query: String::new(),
            loading: true,
            live: HashMap::new(),
        }
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.loading
    }

    /// The async `list_providers` reply. A fetch failure delivers an empty
    /// list: the surface reads as "no providers configured", and a retry is
    /// one close+reopen away (the picker re-fetches on every open). Fresh
    /// providers discard any previous run's live listings.
    pub(crate) fn set_providers(&mut self, providers: Vec<ProviderSummary>) {
        self.all = providers;
        self.stage = PickerStage::Providers;
        self.loading = false;
        self.live.clear();
        self.refilter("");
    }

    pub(crate) fn providers(&self) -> &[ProviderSummary] {
        &self.all
    }

    /// How many model rows the stage has for `provider`.
    fn model_count(&self, provider: usize) -> usize {
        self.live.get(&provider).map_or(0, |live| live.ids.len())
    }

    /// The id at a row index — used by the render/filter hot path instead of
    /// [`Self::model_ids`], which would clone the whole list per call.
    fn model_id_at(&self, provider: usize, model: usize) -> Option<String> {
        self.live.get(&provider)?.ids.get(model).cloned()
    }

    /// Marks `provider`'s live listing as in flight. `true` when this call
    /// started the fetch (the caller then sends `list_provider_models`);
    /// `false` when it was already loaded or already in flight, so a drill-in
    /// that happens twice does not re-ask.
    pub(crate) fn begin_live_load(&mut self, provider: usize) -> bool {
        let entry = self.live.entry(provider).or_default();
        if entry.loading || !entry.ids.is_empty() {
            return false;
        }
        entry.loading = true;
        true
    }

    /// The async `list_provider_models` reply (a failed fetch arrives as an
    /// empty list). Re-filters so the new rows appear at once. An empty
    /// answer is not cached as "known empty" beyond this modal instance --
    /// the next open starts a fresh fetch.
    pub(crate) fn set_live_models(&mut self, provider: usize, ids: Vec<String>) {
        let entry = self.live.entry(provider).or_default();
        entry.ids = ids;
        entry.loading = false;
        let query = self.query.clone();
        self.refilter(&query);
    }

    fn is_live_loading(&self, provider: usize) -> bool {
        self.live.get(&provider).is_some_and(|live| live.loading)
    }

    /// The empty-surface label for the current stage and state.
    pub(crate) fn empty_label(&self) -> &'static str {
        match &self.stage {
            PickerStage::Providers => {
                if self.is_loading() {
                    "Loading providers…"
                } else {
                    "No providers configured"
                }
            }
            PickerStage::Models { provider } => {
                if self.is_live_loading(*provider) {
                    "Loading models…"
                } else if self.model_count(*provider) == 0 {
                    "No models listed"
                } else {
                    "No matching models"
                }
            }
        }
    }

    /// Rows visible at a stage, unfiltered -- the pure stage→rows mapping the
    /// tests drive directly.
    fn rows(&self) -> Vec<PickerItem> {
        match self.stage {
            PickerStage::Providers => (0..self.all.len()).map(PickerItem::Provider).collect(),
            PickerStage::Models { provider } => (0..self.model_count(provider))
                .map(|model| PickerItem::Model { provider, model })
                .collect(),
        }
    }

    fn item_text(&self, item: &PickerItem) -> String {
        match item {
            PickerItem::Provider(index) => self.all[*index].name.clone(),
            PickerItem::Model { provider, model } => {
                self.model_id_at(*provider, *model).unwrap_or_default()
            }
        }
    }

    fn refilter(&mut self, query: &str) {
        self.query = query.to_string();
        let needle = query.trim().to_ascii_lowercase();
        self.filtered = self
            .rows()
            .into_iter()
            .filter(|item| {
                needle.is_empty() || self.item_text(item).to_ascii_lowercase().contains(&needle)
            })
            .collect();
    }

    pub(crate) fn items(&self) -> &[PickerItem] {
        &self.filtered
    }

    pub(crate) fn stage(&self) -> &PickerStage {
        &self.stage
    }

    pub(crate) fn item_at(&self, index: IndexPath) -> Option<PickerItem> {
        self.filtered.get(index.row).cloned()
    }

    /// Confirm semantics: `Some` closes the modal with a model switch,
    /// `None` keeps it open (drill into a provider, or a no-op row). A
    /// provider row drills in only when it is available; a model row always
    /// confirms.
    pub(crate) fn confirm_at(&mut self, index: IndexPath) -> Option<ConfirmedModel> {
        let item = self.item_at(index)?;
        match item {
            PickerItem::Model { provider, model } => {
                let name = self.all.get(provider)?.name.clone();
                let id = self.model_id_at(provider, model)?;
                Some(ConfirmedModel {
                    provider: name,
                    model: id,
                })
            }
            PickerItem::Provider(index) => {
                let entry = self.all.get(index)?;
                if !entry.available {
                    return None;
                }
                self.stage = PickerStage::Models { provider: index };
                // The providers-stage query carries no meaning into the model
                // stage; the modal clears the search box with
                // `ListState::set_query("")` right after this returns.
                self.refilter("");
                None
            }
        }
    }

    /// Esc semantics: walk back a stage when one exists (`true` = stay open,
    /// now at `Providers`), `false` when already at the top stage so the
    /// modal closes.
    pub(crate) fn back(&mut self) -> bool {
        if matches!(self.stage, PickerStage::Providers) {
            return false;
        }
        self.stage = PickerStage::Providers;
        self.refilter("");
        true
    }
}

/// The human-readable reason a provider row is disabled, composed from the
/// summary's own fields: the daemon owns the availability *verdict*
/// (`available: false`), the shell only owns the wording. `api_key_env` is
/// the variable's NAME (never a value), and may be empty for a provider that
/// needs no key at all.
pub(crate) fn unavailable_reason(summary: &ProviderSummary) -> String {
    if summary.api_key_env.is_empty() {
        "not configured".to_string()
    } else {
        format!("environment variable `{}` is not set", summary.api_key_env)
    }
}

pub(crate) struct ModelPickerDelegate {
    state: PickerState,
    // The currently-selected row, mirrored from `set_selected_index` --
    // see `PaletteDelegate`'s own field doc (`src/palette.rs`) for why
    // this is the delegate's own responsibility to track.
    selected: Option<IndexPath>,
}

impl ModelPickerDelegate {
    pub(crate) fn new() -> Self {
        Self {
            state: PickerState::new(),
            selected: None,
        }
    }

    pub(crate) fn state(&self) -> &PickerState {
        &self.state
    }

    pub(crate) fn state_mut(&mut self) -> &mut PickerState {
        &mut self.state
    }
}

impl ListDelegate for ModelPickerDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.state.items().len()
    }

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.state.refilter(query);
        Task::ready(())
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let item = self.state.items().get(index.row)?.clone();
        let is_selected = self.selected == Some(index);
        let (title, detail, title_color) = match &item {
            PickerItem::Provider(index) => {
                let entry = &self.state.providers()[*index];
                let detail = if !entry.available {
                    unavailable_reason(entry)
                } else if entry.default {
                    "default".to_string()
                } else {
                    String::new()
                };
                // Disabled rows stay `text_subtle` even when selected --
                // decorative by definition, exempt from the selected-row
                // contrast floor (the same rule `PaletteDelegate::render_item`
                // applies to disabled commands, per `docs/theme-design.md`).
                let color = if entry.available {
                    if is_selected {
                        theme::readable_on(theme::text_primary(), theme::surface_selected())
                    } else {
                        theme::text_primary()
                    }
                } else {
                    theme::text_subtle()
                };
                (entry.name.clone(), detail, color)
            }
            PickerItem::Model { provider, model } => {
                let id = self
                    .state
                    .model_id_at(*provider, *model)
                    .unwrap_or_default();
                let color = if is_selected {
                    theme::readable_on(theme::text_primary(), theme::surface_selected())
                } else {
                    theme::text_primary()
                };
                (id, String::new(), color)
            }
        };
        let mut row = div().flex().flex_col().py_0p5();
        row = row.child(
            div()
                .text_size(px(13.0))
                .text_color(title_color)
                .child(title),
        );
        if !detail.is_empty() {
            row = row.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::text_muted())
                    .child(detail),
            );
        }
        Some(ListItem::new(index).child(row))
    }

    fn set_selected_index(
        &mut self,
        index: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = index;
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let label = self.state.empty_label();
        h_flex()
            .size_full()
            .justify_center()
            .text_size(px(13.0))
            .text_color(theme::text_muted())
            .child(label)
    }
}

#[cfg(test)]
mod tests {
    use gpui_component::IndexPath;
    use horizon_agent::wire::ProviderSummary;

    use super::{unavailable_reason, ConfirmedModel, PickerItem, PickerStage, PickerState};

    fn summary(
        name: &str,
        available: bool,
        default: bool,
        default_model: Option<&str>,
    ) -> ProviderSummary {
        ProviderSummary {
            name: name.to_string(),
            base_url: None,
            api_key_env: format!("{name}_API_KEY"),
            default_model: default_model.map(str::to_string),
            available,
            default,
        }
    }

    fn providers() -> Vec<ProviderSummary> {
        vec![
            summary("openai", true, true, Some("gpt-4o-mini")),
            summary("anthropic", false, false, None),
        ]
    }

    #[test]
    fn the_providers_stage_lists_every_entry_in_file_order() {
        let mut state = PickerState::new();
        assert!(state.is_loading());
        state.set_providers(providers());
        assert!(!state.is_loading());
        assert_eq!(
            state.items(),
            &[PickerItem::Provider(0), PickerItem::Provider(1),]
        );
    }

    #[test]
    fn confirming_an_available_provider_drills_in_and_lists_nothing_until_discovery() {
        let mut state = PickerState::new();
        state.set_providers(providers());
        assert_eq!(
            state.confirm_at(IndexPath::new(0)),
            None,
            "a provider confirm stays open"
        );
        assert_eq!(
            state.stage(),
            &PickerStage::Models { provider: 0 },
            "the drill lands on the confirmed provider"
        );
        assert!(
            state.items().is_empty(),
            "no file-declared list: rows wait for the /models reply"
        );
        assert!(state.begin_live_load(0), "the drill starts the live fetch");
        assert_eq!(state.empty_label(), "Loading models…");
        state.set_live_models(0, vec!["gpt-4o-mini".to_string(), "gpt-5.2".to_string()]);
        assert_eq!(
            state.items(),
            &[
                PickerItem::Model {
                    provider: 0,
                    model: 0
                },
                PickerItem::Model {
                    provider: 0,
                    model: 1
                },
            ],
            "the stage lists the discovered ids in listing order"
        );
    }

    #[test]
    fn confirming_an_unavailable_provider_is_a_noop() {
        let mut state = PickerState::new();
        state.set_providers(providers());
        assert_eq!(state.confirm_at(IndexPath::new(1)), None);
        assert_eq!(
            state.stage(),
            &PickerStage::Providers,
            "a key-unavailable provider must not drill"
        );
    }

    #[test]
    fn confirming_a_discovered_model_hands_back_the_provider_and_model_id() {
        let mut state = PickerState::new();
        state.set_providers(providers());
        state.confirm_at(IndexPath::new(0));
        state.set_live_models(0, vec!["gpt-4o-mini".to_string(), "gpt-5.2".to_string()]);
        assert_eq!(
            state.confirm_at(IndexPath::new(1)),
            Some(ConfirmedModel {
                provider: "openai".to_string(),
                model: "gpt-5.2".to_string(),
            })
        );
    }

    #[test]
    fn a_failed_or_empty_discovery_leaves_the_stage_empty() {
        let mut state = PickerState::new();
        state.set_providers(providers());
        state.confirm_at(IndexPath::new(0));
        assert!(state.begin_live_load(0));
        state.set_live_models(0, Vec::new());
        assert!(state.items().is_empty());
        assert_eq!(state.empty_label(), "No models listed");
        // An empty answer is not remembered as "known empty": a re-drill
        // (Esc back in) retries.
        assert!(state.begin_live_load(0));
    }

    #[test]
    fn a_live_fetch_is_not_started_twice_while_it_is_in_flight() {
        let mut state = PickerState::new();
        state.set_providers(providers());
        state.confirm_at(IndexPath::new(0));
        assert!(state.begin_live_load(0), "the first drill starts the fetch");
        assert!(
            !state.begin_live_load(0),
            "still in flight: no second fetch"
        );
        state.set_live_models(0, vec!["gpt-5.2".to_string()]);
        assert!(!state.begin_live_load(0), "already loaded: no refetch");
    }

    #[test]
    fn back_walks_to_providers_then_signals_close() {
        let mut state = PickerState::new();
        state.set_providers(providers());
        assert!(
            !state.back(),
            "Esc at the top stage closes the modal, it does not walk back"
        );
        state.confirm_at(IndexPath::new(0));
        assert!(state.back());
        assert_eq!(state.stage(), &PickerStage::Providers);
        assert_eq!(
            state.items().len(),
            2,
            "walking back restores the full stage"
        );
    }

    #[test]
    fn search_filters_within_the_current_stage_and_drill_resets_it() {
        let mut state = PickerState::new();
        state.set_providers(providers());

        state.refilter("open");
        assert_eq!(state.items(), &[PickerItem::Provider(0)]);
        // Confirm resolves by *visible row*, not by the row's unfiltered
        // position: while filtered, row 0 here is `openai`.
        assert_eq!(state.confirm_at(IndexPath::new(0)), None);
        assert_eq!(state.stage(), &PickerStage::Models { provider: 0 });
        // The drill drops the providers-stage query, and the model stage is
        // empty until discovery answers.
        assert!(state.items().is_empty());
        state.set_live_models(0, vec!["gpt-4o-mini".to_string(), "gpt-5.2".to_string()]);
        state.refilter("mini");
        assert_eq!(
            state.items(),
            &[PickerItem::Model {
                provider: 0,
                model: 0
            }]
        );
    }

    #[test]
    fn confirming_a_filtered_disabled_row_is_still_a_noop() {
        let mut state = PickerState::new();
        state.set_providers(providers());
        state.refilter("anth");
        assert_eq!(state.items(), &[PickerItem::Provider(1)]);
        // Row 0 under this filter is `anthropic` -- unavailable, so the
        // confirm is a no-op even though it is the only visible row.
        assert_eq!(state.confirm_at(IndexPath::new(0)), None);
        assert_eq!(state.stage(), &PickerStage::Providers);
    }

    #[test]
    fn the_unavailable_reason_names_the_missing_variable() {
        let mut entry = summary("anthropic", false, false, None);
        assert_eq!(
            unavailable_reason(&entry),
            "environment variable `anthropic_API_KEY` is not set"
        );
        entry.api_key_env = String::new();
        assert_eq!(unavailable_reason(&entry), "not configured");
    }
}
