//! The model picker: the modal provider→model two-stage chooser opened from
//! the composer's model chip or the "Switch Model…" palette entry (parent
//! task #1's Phase 2). Same searchable-`List` delegate pattern as the view
//! chooser (`src/view_chooser.rs`); the open/subscribe/close lifecycle lives
//! in `WorkspaceShell::open_model_picker`/`close_model_picker`
//! (`src/workspace/modals.rs`).
//!
//! Two stages live in ONE modal list: `Providers` lists every configured
//! provider in the daemon's `list_providers` order (the `[[providers]]` file
//! order), and confirming an available provider drills into `Models` (that
//! provider's aliases, file order again; the first alias is the entry's
//! default model). Esc walks back a stage before it closes the modal.
//! Key-unavailable providers stay visible but grayed with the reason (they
//! are listed, never hidden -- task #2's registry contract), and confirming
//! one is a no-op.

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
    Model { provider: usize, alias: usize },
}

/// What a model-stage confirm hands back to the shell: the provider name and
/// the alias to send to `SessionHub::set_session_model`. The daemon resolves
/// the alias to the actual model id -- the `SessionModel` echo, not this
/// pair, is authoritative for what will actually run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConfirmedModel {
    pub(crate) provider: String,
    pub(crate) alias: String,
}

/// The delegate's whole state, free of GPUI types so the stage machine,
/// filtering, and confirm/back semantics are unit-testable without a window
/// (the `first_row_to_select` precedent in `src/workspace/modals.rs`).
pub(crate) struct PickerState {
    all: Vec<ProviderSummary>,
    stage: PickerStage,
    filtered: Vec<PickerItem>,
    /// `true` from open until the async `list_providers` reply lands, so the
    /// empty surface can render "Loading…" rather than "no providers
    /// configured".
    loading: bool,
}

impl PickerState {
    pub(crate) fn new() -> Self {
        Self {
            all: Vec::new(),
            stage: PickerStage::Providers,
            filtered: Vec::new(),
            loading: true,
        }
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.loading
    }

    /// The async `list_providers` reply. A fetch failure delivers an empty
    /// list: the surface reads as "no providers configured", and a retry is
    /// one close+reopen away (the picker re-fetches on every open).
    pub(crate) fn set_providers(&mut self, providers: Vec<ProviderSummary>) {
        self.all = providers;
        self.stage = PickerStage::Providers;
        self.loading = false;
        self.refilter("");
    }

    pub(crate) fn providers(&self) -> &[ProviderSummary] {
        &self.all
    }

    /// Rows visible at a stage, unfiltered -- the pure stage→rows mapping the
    /// tests drive directly.
    fn rows(stage: &PickerStage, all: &[ProviderSummary]) -> Vec<PickerItem> {
        match stage {
            PickerStage::Providers => (0..all.len()).map(PickerItem::Provider).collect(),
            PickerStage::Models { provider } => {
                let Some(entry) = all.get(*provider) else {
                    return Vec::new();
                };
                (0..entry.models.len())
                    .map(|alias| PickerItem::Model {
                        provider: *provider,
                        alias,
                    })
                    .collect()
            }
        }
    }

    fn item_text(&self, item: &PickerItem) -> String {
        match item {
            PickerItem::Provider(index) => self.all[*index].name.clone(),
            PickerItem::Model { provider, alias } => {
                self.all[*provider].models[*alias].alias.clone()
            }
        }
    }

    fn refilter(&mut self, query: &str) {
        let query = query.trim().to_ascii_lowercase();
        self.filtered = Self::rows(&self.stage, &self.all)
            .into_iter()
            .filter(|item| {
                query.is_empty() || self.item_text(item).to_ascii_lowercase().contains(&query)
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
    /// provider row confirms only when it is available AND has at least one
    /// model to offer.
    pub(crate) fn confirm_at(&mut self, index: IndexPath) -> Option<ConfirmedModel> {
        let item = self.item_at(index)?;
        match item {
            PickerItem::Model { provider, alias } => {
                let entry = self.all.get(provider)?;
                let model = entry.models.get(alias)?;
                Some(ConfirmedModel {
                    provider: entry.name.clone(),
                    alias: model.alias.clone(),
                })
            }
            PickerItem::Provider(index) => {
                let entry = self.all.get(index)?;
                if !entry.available || entry.models.is_empty() {
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
            PickerItem::Model { provider, alias } => {
                let model = &self.state.providers()[*provider].models[*alias];
                let color = if is_selected {
                    theme::readable_on(theme::text_primary(), theme::surface_selected())
                } else {
                    theme::text_primary()
                };
                (model.alias.clone(), String::new(), color)
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
        let label = if self.state.is_loading() {
            "Loading providers…"
        } else {
            "No providers configured"
        };
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
    use horizon_agent::wire::{ModelAlias, ProviderSummary};

    use super::{unavailable_reason, ConfirmedModel, PickerItem, PickerStage, PickerState};

    fn summary(
        name: &str,
        available: bool,
        default: bool,
        aliases: &[(&str, &str)],
    ) -> ProviderSummary {
        ProviderSummary {
            name: name.to_string(),
            base_url: None,
            api_key_env: format!("{name}_API_KEY"),
            models: aliases
                .iter()
                .map(|(alias, model)| ModelAlias {
                    alias: alias.to_string(),
                    model: model.to_string(),
                })
                .collect(),
            available,
            default,
        }
    }

    fn providers() -> Vec<ProviderSummary> {
        vec![
            summary(
                "openai",
                true,
                true,
                &[("gpt-4o", "gpt-4o-2024"), ("mini", "gpt-4o-mini")],
            ),
            summary("anthropic", false, false, &[("opus", "m-opus")]),
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
    fn confirming_an_available_provider_drills_into_its_models_in_order() {
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
        assert_eq!(
            state.items(),
            &[
                PickerItem::Model {
                    provider: 0,
                    alias: 0
                },
                PickerItem::Model {
                    provider: 0,
                    alias: 1
                },
            ],
            "the model stage lists the entry's aliases in file order"
        );
    }

    #[test]
    fn confirming_an_unavailable_or_model_less_provider_is_a_noop() {
        let mut state = PickerState::new();
        state.set_providers(providers());
        assert_eq!(state.confirm_at(IndexPath::new(1)), None);
        assert_eq!(
            state.stage(),
            &PickerStage::Providers,
            "a key-unavailable provider must not drill"
        );
        let mut state = PickerState::new();
        state.set_providers(vec![summary("empty", true, false, &[])]);
        assert_eq!(state.confirm_at(IndexPath::new(0)), None);
        assert_eq!(state.stage(), &PickerStage::Providers);
    }

    #[test]
    fn confirming_a_model_hands_back_the_provider_and_alias() {
        let mut state = PickerState::new();
        state.set_providers(providers());
        state.confirm_at(IndexPath::new(0));
        assert_eq!(
            state.confirm_at(IndexPath::new(1)),
            Some(ConfirmedModel {
                provider: "openai".to_string(),
                alias: "mini".to_string(),
            })
        );
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
        // The drill drops the providers-stage query: the modal clears the
        // search box, and the delegate's own filter resets with it.
        assert_eq!(
            state.items(),
            &[
                PickerItem::Model {
                    provider: 0,
                    alias: 0
                },
                PickerItem::Model {
                    provider: 0,
                    alias: 1
                },
            ]
        );
        state.refilter("mini");
        assert_eq!(
            state.items(),
            &[PickerItem::Model {
                provider: 0,
                alias: 1
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
        let mut entry = summary("anthropic", false, false, &[("opus", "m-opus")]);
        assert_eq!(
            unavailable_reason(&entry),
            "environment variable `anthropic_API_KEY` is not set"
        );
        entry.api_key_env = String::new();
        assert_eq!(unavailable_reason(&entry), "not configured");
    }
}
