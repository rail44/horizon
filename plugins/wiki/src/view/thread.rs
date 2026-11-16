//! A comment thread as the wiki view draws it: its card, closed or open,
//! the field for a comment, and the dialog a thread is read in.

use std::rc::Rc;

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set, With};
use horizon_ui::{El, el, prevent_default, stop_propagation};

use horizon_components::{Chip, Tone};
use horizon_wiki::anchor::Anchor;
use horizon_wiki::annotation::{AgentType, Annotation};
use horizon_wiki::view_model::PlacedThread;
use keyboard_types::{Key, NamedKey};

use super::state::{State, Target};
use super::{COMMENT_ON, one_line, styles};

const CHANGED_SINCE: &str = "changed since";
const TO_SEND: &str = "to send, Esc to cancel";
const REPLYING: &str = "Replying";
const CLOSE: &str = "Close";

/// The field for a comment, with what was typed so far in it.
pub(super) fn composer(state: State) -> El {
    let field = el("textarea").attr("rows", "2");
    let typed = state.typed.get_untracked();
    if !typed.is_empty() {
        field.set_value(&typed);
    }
    field
        .on("input", move |event| {
            if let DomEventData::Input(input) = &event.data {
                state.typed.set(input.value.clone());
            }
        })
        .on("keydown", move |event| {
            let DomEventData::KeyDown(key) = &event.data else {
                return;
            };
            match &key.key {
                Key::Named(NamedKey::Enter) if !key.modifiers.shift() && !key.is_composing => {
                    state.send();
                    prevent_default();
                }
                Key::Named(NamedKey::Escape) => state.cancel(),
                _ => {}
            }
            // The field's keys are its own.
            stop_propagation();
        })
        .focus();
    el("div")
        .class(styles::COMPOSER)
        .child(
            el("div")
                .class(styles::FIELD)
                .child(horizon_components::Field {
                    input: field,
                    prompt: None,
                }),
        )
        .child(
            el("div")
                .class(styles::SEND)
                .child(el("span").class(styles::KEY).text("Enter"))
                .child(el("span").class(styles::HINT).text(TO_SEND)),
        )
}

pub(super) fn post(annotation: &Annotation) -> El {
    let person = match annotation.creator.kind {
        AgentType::Person => Some(styles::PERSON),
        AgentType::Software => None,
    };
    let when = annotation.created.replace('T', " ").replace('Z', "");
    el("div")
        .class((styles::POST, person))
        .child(horizon_components::markdown::render(
            &annotation.body.value,
            horizon_components::markdown::Headings::AsText,
            Rc::new(|_| {}),
        ))
        .child(el("div").class(styles::WHEN).text(&when))
}

/// What a thread's card shows, as text: the card is built again when any
/// of it changes.
pub(super) fn thread_key(
    placed: &PlacedThread,
    open: bool,
    focused: bool,
    editing: bool,
    replying: bool,
) -> String {
    let changed = matches!(placed.anchor, Anchor::Moved { exact: false, .. });
    format!(
        "{} {open} {focused} {editing} {replying} {changed} {} {} {}",
        placed.thread.root.id,
        placed.thread.replies.len(),
        placed.thread.last().id,
        placed.quote()
    )
}

pub(super) fn replying_line() -> El {
    el("div")
        .class(styles::REPLYING)
        .child(el("div").class(styles::BAR))
        .child(el("span").text(REPLYING))
}

/// The thread or new comment being worked on, over the view: the whole
/// quote, the posts and the field. Escape, `Close` or a press outside
/// closes it.
pub(super) fn dialog(state: State) -> El {
    let scrim = el("div").class(styles::SCRIM).on("pointerdown", move |_| {
        state.cancel();
    });
    let Some(page) = state.page.get_untracked() else {
        return scrim;
    };
    let (quote, posts, replying) = match state.target.get_untracked() {
        Some(Target::Selection(chars)) => (
            format!("{COMMENT_ON} “{}”", page.quote_of(chars)),
            Vec::new(),
            false,
        ),
        Some(Target::Thread(root)) => match state.placed(&root) {
            Some(placed) => {
                let posts: Vec<Annotation> = std::iter::once(&placed.thread.root)
                    .chain(&placed.thread.replies)
                    .cloned()
                    .collect();
                (
                    format!("“{}”", placed.quote()),
                    posts,
                    placed.replying(&page.replying),
                )
            }
            None => return scrim,
        },
        None => return scrim,
    };
    let list = el("div")
        .class(styles::POSTS)
        .children(posts.iter().map(post));
    if replying {
        list.child(replying_line());
    }
    scrim.child(
        el("div")
            .class(styles::DIALOG)
            // A press in the dialog is not a press outside it.
            .on("pointerdown", |_| stop_propagation())
            .child(
                el("div")
                    .class(styles::TOP)
                    .child(el("div").class(styles::PASSAGE).text(&quote))
                    .child(el("span").class(styles::KEY).text("Esc"))
                    .child(
                        el("div")
                            .class(styles::CLOSE)
                            .text(CLOSE)
                            .on("click", move |_| state.cancel()),
                    ),
            )
            .child(list)
            .child(composer(state)),
    )
}

pub(super) fn thread(state: State, placed: &PlacedThread, replying: bool) -> El {
    let root = placed.thread.root.id.clone();
    let open = !state.in_dialog() && state.expanded.with(|expanded| expanded.contains(&root));
    let focused = state.focused.get_untracked().as_ref() == Some(&root);
    let editing = state.target.get_untracked() == Some(Target::Thread(root.clone()));
    let posts: Vec<&Annotation> = std::iter::once(&placed.thread.root)
        .chain(&placed.thread.replies)
        .collect();
    let count = match posts.len() {
        1 => "1 comment".to_string(),
        count => format!("{count} comments"),
    };
    let toggled = root.clone();
    let head = el("div")
        .class(styles::HEAD)
        .child(el("span").text(if open { "▾" } else { "▸" }))
        .child(
            el("span")
                .class(styles::QUOTE)
                .text(&format!("“{}”", one_line(&placed.quote()))),
        )
        .child(el("span").class(styles::COUNT).text(&count))
        .on("click", move |_| state.toggle_thread(toggled.clone()));
    if matches!(placed.anchor, Anchor::Moved { exact: false, .. }) {
        head.child(Chip {
            tone: Tone::Muted,
            text: CHANGED_SINCE,
        });
    }
    if replying && !open {
        head.child(Chip {
            tone: Tone::Accent,
            text: REPLYING,
        });
    }
    let card = el("div")
        .class((styles::THREAD, focused.then_some(styles::FOCUSED)))
        .child(head);
    if open {
        card.children(posts.iter().map(|annotation| post(annotation)));
        if replying {
            card.child(replying_line());
        }
        if editing {
            card.child(composer(state));
        }
    }
    card
}
