//! The bar at the bottom of the window (board #84): the keys that work
//! where the keys are, each with what it does. Workspace mode's while it
//! is on, those of what is over the workspace while something is, and
//! otherwise the view's with the focus and the chords the configuration
//! binds. Each key is in a box of its own, its words after it; the bar
//! is one line, and what does not fit is left out whole from the end.
//! It is not pressed: what is pressed is the keys.

use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, Update, WithUntracked};
use horizon_ui::{El, Page, each_layout, el};
use horizon_workspace::Workspace;

use super::keymap::Bound;
use super::keys::MODE;
use super::{Modal, Shell};
use crate::plugin::Hint;

mod styles {
    horizon_ui::css_module! {"
    /* One line high: a line that wraps goes under its padding and out of
       sight, the hints that do not fit with it. A key's box is drawn by a
       shadow, which takes no room, so the line stays a line high. */
    .bar { flex: none; display: flex; flex-wrap: wrap; align-items: center;
           gap: var(--u8) var(--u12); height: var(--micro-line); overflow: hidden;
           padding: var(--u4) var(--u8); white-space: nowrap;
           font-size: var(--micro-size); line-height: var(--micro-line);
           color: var(--text-muted); background: var(--background);
           border-top: var(--line) solid var(--border); }
    .hint { display: flex; align-items: center; gap: var(--u4); }
    .keys { padding: 0 var(--u4); border-radius: var(--radius-small);
            font-weight: 600; color: var(--chip-muted-text);
            background: var(--chip-muted-surface);
            box-shadow: inset 0 0 0 var(--line) var(--border); }
    "}
}

/// A view an extension said the keys of, and what it said.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Hinted {
    view: El,
    hints: Vec<Hint>,
}

impl Shell {
    /// The bar, saying the keys of where the keys are after each layout:
    /// the focus moves with no signal of its own to follow.
    pub(super) fn bar(&self) -> El {
        let shell = *self;
        let shown = horizon_ui::reactive_graph::signal::RwSignal::new(Vec::<Hint>::new());
        each_layout(move |page| {
            let now = shell.hints_now(page);
            if shown.with_untracked(|was| *was != now) {
                shown.set(now);
            }
        });
        el("div").class(styles::BAR).each(
            move || shown.get().into_iter().enumerate().collect::<Vec<_>>(),
            |(index, hint)| (*index, hint.clone()),
            |(_, hint)| {
                el("span")
                    .class(styles::HINT)
                    .child(el("span").class(styles::KEYS).text(&hint.keys))
                    .child(el("span").text(&hint.label))
            },
        )
    }

    /// Says `hints` are the keys of the extension's view `view`, while the
    /// focus is in it; none takes them back.
    pub(super) fn hint(&self, view: El, hints: Vec<Hint>) {
        self.hinted.update(|hinted| {
            hinted.retain(|hinted| hinted.view != view);
            if !hints.is_empty() {
                hinted.push(Hinted { view, hints });
            }
        });
    }

    /// The keys of where the keys are now.
    fn hints_now(&self, page: &Page) -> Vec<Hint> {
        if let Some(modal) = self.modal.get_untracked() {
            let picked = |verb: &str, leave: &str| {
                vec![
                    Hint::new("↑/↓", "move"),
                    Hint::new("enter", verb),
                    Hint::new("esc", leave),
                ]
            };
            return match modal {
                Modal::Extension(_) => self.view_hints(page),
                Modal::Palette => picked("run", "close"),
                Modal::Chooser(_) | Modal::Finder(..) => picked("open", "close"),
                Modal::Models(_) => picked("choose", "back"),
                Modal::Layouts => picked("apply", "close"),
                Modal::MoveTo => picked("move", "close"),
            };
        }
        if self.mode.get_untracked() {
            // With no tab, what is typed searches the views the workspace
            // shows; `:` is still the commands'.
            if self.model.with_untracked(Workspace::tab_count) == 0 {
                return vec![Hint::new(":", "commands")];
            }
            return (MODE.iter())
                .filter(|group| group.here(*self))
                .map(|group| Hint::new(group.keys, group.label))
                .collect();
        }
        let mut hints = self.view_hints(page);
        if hints.is_empty() {
            let plugin = self.model.with_untracked(|model| {
                (model.cursor_pane_id())
                    .and_then(|pane| model.pane_plugin(pane))
                    .map(|plugin| plugin.name.clone())
            });
            hints = plugin.map_or_else(Vec::new, |name| super::plugins::hints(&name));
        }
        hints.extend(self.chord_hints());
        hints
    }

    /// The keys an extension's view with the focus said.
    fn view_hints(&self, page: &Page) -> Vec<Hint> {
        self.hinted.with_untracked(|hinted| {
            (hinted.iter())
                .find(|hinted| page.has_focus_within(hinted.view))
                .map(|hinted| hinted.hints.clone())
                .unwrap_or_default()
        })
    }

    /// The chords the configuration binds that work now: workspace mode's,
    /// and those of the commands that can be carried out, each command
    /// once.
    fn chord_hints(&self) -> Vec<Hint> {
        let kept = self.kept();
        let keymap = kept.keymap.borrow();
        let mut hints = vec![Hint::new(keymap.workspace_mode.label(), "workspace mode")];
        let mut said: Vec<&Bound> = Vec::new();
        for (chord, bound) in keymap.bindings() {
            if said.contains(&bound) {
                continue;
            }
            let label = match bound {
                Bound::OpenPalette => "commands".to_string(),
                Bound::Command(name) => match self.command(name) {
                    Some(command) if command.enabled => command.title.to_lowercase(),
                    _ => continue,
                },
            };
            said.push(bound);
            hints.push(Hint::new(chord.label(), label));
        }
        hints
    }
}
