//! The bar at the bottom of the window (board #84): the keys that work
//! where the keys are, each with what it does. Workspace mode's while it
//! is on; otherwise those the elements the focus is in take
//! (`horizon_ui::keys`, a plugin's view's as the shell's own), and, but
//! over the workspace, the chords the configuration binds. Each key is in
//! a box of its own, its words after it; the bar is one line, and what
//! does not fit is left out whole from the end. It is not pressed: what
//! is pressed is the keys.

use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, WithUntracked};
use horizon_ui::{El, Page, each_layout, el};
use horizon_workspace::Workspace;

use super::Shell;
use super::keymap::Bound;
use super::keys::MODE;

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
    /* As faint as a pane's tag. */
    .keys { padding: 0 var(--u4); border-radius: var(--radius);
            color: var(--text-muted); background: var(--band);
            box-shadow: inset 0 0 0 var(--line) var(--surface-chrome); }
    "}
}

/// A key the bar says, spelled as it is there, and what it does.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Hint {
    keys: String,
    label: String,
}

impl Hint {
    fn new(keys: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            keys: keys.into(),
            label: label.into(),
        }
    }
}

impl Shell {
    /// The bar, saying the keys of where the keys are after each layout:
    /// the focus moves with no signal of its own to follow.
    pub(super) fn bar(&self) -> El {
        let shell = *self;
        let shown = RwSignal::new(Vec::<Hint>::new());
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

    /// The keys of where the keys are now.
    fn hints_now(&self, page: &Page) -> Vec<Hint> {
        // Workspace mode's are the shell's own, under a sheet over the
        // workspace too, which has the keys then.
        if self.mode.get_untracked() && self.modal.get_untracked().is_none() {
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
        let mut hints: Vec<Hint> = (page.focused_keys().into_iter())
            .map(|key| Hint::new(key.spelled, key.label))
            .collect();
        if self.modal.get_untracked().is_none() {
            hints.extend(self.chord_hints());
        }
        hints
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
