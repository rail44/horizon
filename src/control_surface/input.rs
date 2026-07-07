use crate::app::command_actions::CommandActionState;
use crate::app::keymap::palette_accepts_text_input;
use floem::keyboard::{Key, KeyEvent, NamedKey};
use floem::prelude::*;

use crate::control_surface::actions::{
    close_palette, execute_palette_selection, move_palette_selection, update_palette_query,
    PaletteActionState,
};
use crate::control_surface::PaletteStage;

#[derive(Clone)]
pub(crate) struct ControlInputState {
    pub(crate) command: CommandActionState,
    pub(crate) palette_open: RwSignal<bool>,
    pub(crate) palette_query: RwSignal<String>,
    pub(crate) palette_selection: RwSignal<usize>,
}

impl ControlInputState {
    pub(crate) fn palette_action_state(self) -> PaletteActionState {
        PaletteActionState {
            command: self.command,
            palette_open: self.palette_open,
            palette_query: self.palette_query,
            palette_selection: self.palette_selection,
        }
    }
}

/// `Event::KeyDown` entry point for the command palette -- the control
/// surface is Commands-only now that the Tab-switching workspace overview
/// is gone (`docs/plans/application-ui/01-session-manager.md`; session
/// management moved to its own modal, `control_surface::view::
/// session_manager`).
pub(crate) fn handle_control_key(key_event: &KeyEvent, state: ControlInputState) -> bool {
    let workspace = state.command.workspace();
    let frames = state.command.frames();
    let palette_stage = state.command.palette.palette_stage;
    match &key_event.key.logical_key {
        Key::Named(NamedKey::Escape) => {
            handle_escape(
                state.palette_open,
                state.palette_query,
                state.palette_selection,
                palette_stage,
            );
            true
        }
        Key::Named(NamedKey::Enter) => {
            execute_palette_selection(state.palette_action_state());
            true
        }
        // Tab walks the list like ArrowDown (Shift+Tab like ArrowUp) --
        // picker convention; Tab lost its old Commands/Workspace toggle
        // role when the overview retired, freeing it for this.
        Key::Named(NamedKey::Tab) => {
            let step = if key_event.modifiers.shift() { -1 } else { 1 };
            move_palette_selection(
                workspace,
                frames,
                state.palette_query,
                state.palette_selection,
                palette_stage,
                step,
            );
            true
        }
        Key::Named(NamedKey::ArrowUp) => {
            move_palette_selection(
                workspace,
                frames,
                state.palette_query,
                state.palette_selection,
                palette_stage,
                -1,
            );
            true
        }
        Key::Named(NamedKey::ArrowDown) => {
            move_palette_selection(
                workspace,
                frames,
                state.palette_query,
                state.palette_selection,
                palette_stage,
                1,
            );
            true
        }
        Key::Named(NamedKey::Backspace) => {
            update_palette_query(
                workspace,
                frames,
                state.palette_query,
                state.palette_selection,
                palette_stage,
                |query| {
                    query.pop();
                },
            );
            true
        }
        Key::Named(NamedKey::Space) => {
            update_palette_query(
                workspace,
                frames,
                state.palette_query,
                state.palette_selection,
                palette_stage,
                |query| {
                    query.push(' ');
                },
            );
            true
        }
        Key::Character(text) if palette_accepts_text_input(key_event.modifiers) => {
            update_palette_query(
                workspace,
                frames,
                state.palette_query,
                state.palette_selection,
                palette_stage,
                |query| {
                    query.push_str(text.as_str());
                },
            );
            true
        }
        _ => false,
    }
}

/// `Key::Escape`'s palette behavior, factored out of [`handle_control_key`]
/// so it's unit-testable without constructing a real (floem/winit-backed)
/// `KeyEvent` (see `app::keymap`'s `Chord::matches` doc comment for why that
/// can't be built from a plain struct literal outside floem itself). One
/// step back per keypress: from the second-stage view chooser, back to
/// Commands (query/selection reset, palette left open); from Commands,
/// closes the palette outright, exactly as it always has -- see
/// `docs/roadmap.md`'s "Placement-first session creation".
fn handle_escape(
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    palette_stage: RwSignal<PaletteStage>,
) {
    match palette_stage.get_untracked() {
        PaletteStage::ViewChooser { .. } => {
            palette_query.set(String::new());
            palette_selection.set(0);
            palette_stage.set(PaletteStage::Commands);
        }
        PaletteStage::Commands => {
            close_palette(palette_open, palette_query, palette_stage);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_surface::Placement;

    #[test]
    fn escape_pops_the_view_chooser_back_to_commands_without_closing() {
        let open = RwSignal::new(true);
        let query = RwSignal::new("term".to_string());
        let selection = RwSignal::new(2);
        let stage = RwSignal::new(PaletteStage::ViewChooser {
            placement: Placement::SplitPane,
        });

        handle_escape(open, query, selection, stage);

        assert!(
            open.get_untracked(),
            "Escape from the chooser must not close the palette"
        );
        assert_eq!(stage.get_untracked(), PaletteStage::Commands);
        assert_eq!(query.get_untracked(), "");
        assert_eq!(selection.get_untracked(), 0);
    }

    #[test]
    fn escape_closes_the_palette_from_the_commands_stage() {
        let open = RwSignal::new(true);
        let query = RwSignal::new("split".to_string());
        let selection = RwSignal::new(1);
        let stage = RwSignal::new(PaletteStage::Commands);

        handle_escape(open, query, selection, stage);

        assert!(!open.get_untracked());
        assert_eq!(query.get_untracked(), "");
        assert_eq!(stage.get_untracked(), PaletteStage::Commands);
    }

    #[test]
    fn escape_twice_from_a_chooser_opened_by_a_bound_key_ends_up_closed() {
        // Mirrors a bound key opening the chooser directly (`open_view_chooser`,
        // not the Commands stage first) -- Escape must still pop to Commands
        // before a second Escape closes, per the "consistency over restoring
        // whatever query preceded it" decision.
        let open = RwSignal::new(false);
        let query = RwSignal::new(String::new());
        let selection = RwSignal::new(0);
        let stage = RwSignal::new(PaletteStage::Commands);
        crate::control_surface::open_view_chooser(
            crate::control_surface::OpenPaletteState {
                palette_open: open,
                palette_query: query,
                palette_selection: selection,
                palette_stage: stage,
                palette_focus_request: RwSignal::new(0),
            },
            Placement::NewTab,
        );
        assert!(open.get_untracked());

        handle_escape(open, query, selection, stage);
        assert!(open.get_untracked(), "first Escape only pops to Commands");
        assert_eq!(stage.get_untracked(), PaletteStage::Commands);

        handle_escape(open, query, selection, stage);
        assert!(!open.get_untracked(), "second Escape closes the palette");
    }
}
