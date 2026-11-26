//! The wiki view: one page of the project wiki drawn as running text, with
//! each comment thread marked where it points. On a pane wide enough for
//! a margin, threads sit in a column beside the text, each at the top of
//! the block its passage starts in and below the thread before it; on a
//! narrower one they are in the text, after that block.
//!
//! A thread starts closed, as one line (its quote, how many comments,
//! whether an agent is replying), and opens on a click; an open thread
//! holds its field for a reply. Text selected in the page can be
//! commented on, which starts a thread. Threads whose passage left the
//! page are listed after it.
//!
//! Comments are written straight to the page's thread log; the daemon's
//! wiki flow notices them and answers through the thread's session, and
//! lists the comments it is answering beside the log. The page file, the
//! log and that list are watched (by the host, for a guest), so an answer
//! or an edit is drawn.
//!
//! The page as a view reads it is `horizon-wiki`'s `view_model`.
//!
//! There is a second layout, switched to from the header: each thread is
//! only its head line in the page, and the thread or new comment being
//! worked on is in a dialog over the view.
//!
//! Here is the view put together: the header, the page's blocks, the
//! margin and where its items are placed after a layout. What it keeps
//! and what is done to it is `state`, a thread's card and the dialog are
//! `thread`, a block's text with its marks is `render`, and the sheet is
//! `styles`.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::computed::Memo;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, With, WithUntracked};
use horizon_ui::{
    Callback, Component, El, dispatched, each_layout, el, prevent_default, relayout,
    stop_propagation,
};

use horizon_components::{Button, Chip, Notice, Press, Tone};
use horizon_wiki::view_model::{PlacedThread, WikiPage};
use keyboard_types::{Key, NamedKey};

mod render;
mod state;
mod styles;
mod thread;

pub use state::{CommentLayout, OpenUrl, State, Target};
use thread::{composer, dialog, thread, thread_key};

const NO_PAGE: &str = "No wiki page";
const LOADING: &str = "Loading";
const ORPHANS: &str = "No longer in the page";
const COMMENT_ON: &str = "Comment on";
const BACK: &str = "Back";
const COMMENT: &str = "Comment";
const TO_DIALOG: &str = "Comments in a dialog";
const TO_MARGIN: &str = "Comments in the margin";

/// The share of the width the margin takes.
const MARGIN_SHARE: f64 = 0.4;

/// The margin's width for `available` (the width the page's text and its
/// margin share), or `None` when the text beside it would get too narrow.
/// `unit` is what one pixel of the reference type size is drawn as.
fn margin_for(available: f64, unit: f64) -> Option<f64> {
    let margin = (available * MARGIN_SHARE)
        .clamp(240. * unit, 560. * unit)
        .round();
    (available - margin - 24. * unit >= 280. * unit).then_some(margin)
}

/// A page of a project's wiki, with the owner's comment threads anchored
/// in it. Links to other pages are followed in place, with a way back.
///
/// The page it opens on is a value: to show another from outside,
/// another is placed.
pub struct Wiki {
    /// The page's file it opens on.
    pub path: Option<PathBuf>,
    /// Hands a URL to the desktop.
    pub open_url: OpenUrl,
    /// Told of the page a link, or the way back, took the view to.
    pub on_navigate: Option<Callback<PathBuf>>,
}

impl Component for Wiki {
    fn view(self) -> El {
        build(self).0
    }
}

#[derive(Clone)]
enum Line {
    Block {
        index: usize,
        key: String,
    },
    /// The offer to comment on what is selected.
    Offer,
    New(Range<usize>),
    Thread {
        placed: Box<PlacedThread>,
        key: String,
    },
    Orphans,
}

impl Line {
    /// What names the line's place in the margin, whatever it shows there
    /// just now.
    fn slot(&self) -> String {
        match self {
            Line::Thread { placed, .. } => placed.thread.root.id.clone(),
            Line::New(_) | Line::Offer => "starting".to_string(),
            Line::Block { .. } | Line::Orphans => String::new(),
        }
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Builds the view, opened on `path`: a page's file in a project's wiki.
pub fn build(given: Wiki) -> (El, State) {
    let path = given.path.clone();
    let state = State::new(given);
    let (root, scroll) = (state.element, state.scroll);
    let held = state.held();
    if let Some(path) = path {
        state.show(path);
    }

    let bar = el("div")
        .class(styles::BAR)
        .when(
            move || !state.history.with(Vec::is_empty),
            move || {
                el("div")
                    .class(styles::BACK)
                    .text(BACK)
                    .on("click", move |_| state.back())
            },
        )
        .child(el("div").class(styles::SPACE))
        .child(
            el("div")
                .class(styles::LAYOUT)
                .text_dyn(move || {
                    match state.layout.get() {
                        CommentLayout::Margin => TO_DIALOG,
                        CommentLayout::Dialog => TO_MARGIN,
                    }
                    .to_string()
                })
                .on("click", move |_| state.switch_layout()),
        );
    let header = el("div")
        .class(styles::HEADER)
        .child(bar)
        .each(
            move || {
                let page = state.page.get();
                let title = page.as_ref().and_then(|page| page.document.title.clone());
                title.into_iter().collect::<Vec<_>>()
            },
            |title| title.clone(),
            |title| el("div").class(styles::HEADING).text(title),
        )
        .each(
            move || {
                let page = state.page.get();
                let fields: Vec<(String, String)> = page
                    .iter()
                    .flat_map(|page| page.document.fields.clone())
                    .filter(|(key, _)| key != "id")
                    .collect();
                (!fields.is_empty())
                    .then_some(fields)
                    .into_iter()
                    .collect::<Vec<_>>()
            },
            |fields| fields.clone(),
            |fields| {
                let row = el("div").class(styles::FIELDS);
                for (key, value) in fields {
                    row.child(Chip {
                        tone: Tone::Muted,
                        text: &format!("{key} {value}"),
                    });
                }
                row
            },
        );

    // A thread's card as a line, and what starts a comment: both tracked.
    let card = move |page: &WikiPage, placed: &PlacedThread| {
        let root = &placed.thread.root.id;
        let in_page = state.layout.get() == CommentLayout::Margin;
        let key = thread_key(
            placed,
            in_page && state.expanded.with(|expanded| expanded.contains(root)),
            state.focused.with(|focused| focused.as_ref() == Some(root)),
            state
                .target
                .with(|target| *target == Some(Target::Thread(root.clone()))),
            placed.replying(&page.replying),
        );
        Line::Thread {
            placed: Box::new(placed.clone()),
            key,
        }
    };
    let starting = move |page: &WikiPage| match state.target.get() {
        // (In the dialog, when that is where comments are written.)
        Some(Target::Selection(_)) if state.layout.get() == CommentLayout::Dialog => None,
        Some(Target::Selection(chars)) => {
            (page.block_of(chars.clone())).map(|block| (block, Line::New(chars)))
        }
        Some(Target::Thread(_)) => None,
        None => (state.selection.get())
            .and_then(|chars| page.block_of(chars))
            .map(|block| (block, Line::Offer)),
    };
    let key_of = |line: &Line| match line {
        Line::Block { key, .. } => format!("block {key}"),
        Line::Offer => "offer".to_string(),
        Line::New(chars) => format!("new {chars:?}"),
        Line::Thread { key, .. } => format!("thread {key}"),
        Line::Orphans => "orphans".to_string(),
    };
    let wide = Memo::new(move |_| {
        state.layout.get() == CommentLayout::Margin && state.margin.with(Option::is_some)
    });

    let drawing = held.clone();
    let draw: Rc<dyn Fn(&Line) -> El> = Rc::new(move |line| {
        let Some(page) = state.page.get_untracked() else {
            return el("div");
        };
        match line {
            Line::Block { index, .. } => {
                let focused = state.focused.get_untracked();
                let marks: Vec<render::Mark> = (page.threads.iter())
                    .filter_map(|placed| {
                        let lit = focused.as_ref() == Some(&placed.thread.root.id);
                        Some((placed.anchor.range()?, lit))
                    })
                    .collect();
                let Some(range) = page.blocks.get(*index).cloned() else {
                    return el("div");
                };
                let on_link: Rc<dyn Fn(&str)> = Rc::new(move |target| state.follow_link(target));
                let (element, pieces) = render::block(&page, range, &marks, on_link);
                drawing.pieces.borrow_mut().insert(*index, pieces);
                drawing.blocks.borrow_mut().insert(*index, element);
                element
            }
            // (Pressed, not clicked: the press itself would take the
            // selection it is an offer for.)
            Line::Offer => el("div")
                .class(styles::OFFER)
                .child(Button {
                    label: COMMENT,
                    // (Pressed, not clicked: the click itself would
                    // take the selection it is an offer for.)
                    press: Press::Down,
                    on_press: Some(Callback::new(move |()| {
                        state.comment_on_selection();
                        prevent_default();
                        stop_propagation();
                    })),
                    ..Default::default()
                })
                .child(el("span").class(styles::KEY).text("c")),
            Line::New(chars) => {
                let quote = one_line(&page.quote_of(chars.clone()));
                el("div")
                    .class(styles::NEW)
                    .child(
                        el("div").class(styles::QUOTED).child(
                            el("span")
                                .class(styles::QUOTE)
                                .text(&format!("{COMMENT_ON} “{quote}”")),
                        ),
                    )
                    .child(composer(state))
            }
            Line::Thread { placed, .. } => thread(state, placed, placed.replying(&page.replying)),
            Line::Orphans => el("div").class(styles::ORPHANS).text(ORPHANS),
        }
    });

    // The page's text: its blocks in order, each followed, when there is
    // no margin, by what is set after it.
    let text = el("div").class(styles::TEXT).each(
        move || {
            let Some(page) = state.page.get() else {
                return Vec::new();
            };
            let focused = state.focused.get();
            let starting = (!wide.get()).then(|| starting(&page)).flatten();
            let mut lines = Vec::new();
            for (index, block) in page.blocks.iter().enumerate() {
                // The block is drawn again when its text is another, or
                // the passages marked in it are.
                let within = page.logical_range(block.clone());
                let marks: Vec<String> = (page.threads.iter())
                    .filter_map(|placed| {
                        let chars = placed.anchor.range()?;
                        let within = within.as_ref()?;
                        (chars.start < within.end && within.start < chars.end).then(|| {
                            let lit = focused.as_ref() == Some(&placed.thread.root.id);
                            format!("{chars:?}{lit}")
                        })
                    })
                    .collect();
                let key = format!("{index} {} {marks:?}", &page.document.body[block.clone()]);
                lines.push(Line::Block { index, key });
                if wide.get() {
                    continue;
                }
                if let Some((_, line)) = starting.as_ref().filter(|(at, _)| *at == index) {
                    lines.push(line.clone());
                }
                for placed in &page.threads {
                    if placed.block == Some(index) {
                        lines.push(card(&page, placed));
                    }
                }
            }
            lines
        },
        key_of,
        {
            let draw = draw.clone();
            move |line| draw(line)
        },
    );

    // The margin: what starts a comment and the threads, in the order of
    // their blocks. Each is placed after layout, by its slot.
    let (listing, placing) = (held.clone(), held.clone());
    // (Without a margin the column is of no width, and the row has no gap
    // before it. It is not taken off the page with `display: none`: its
    // items change while it is away, and blitz-dom lays out a box whose
    // children changed while it was not displayed from the children it
    // had.)
    let margin = el("div")
        .class(styles::MARGIN)
        .style_dyn("width", move || {
            let width = state.margin.get().filter(|_| wide.get());
            format!("{}px", width.unwrap_or(0.))
        })
        .style_dyn("min-height", move || {
            let reach = if wide.get() { state.reach.get() } else { 0. };
            format!("{reach}px")
        })
        .each(
            move || {
                let page = state.page.get().filter(|_| wide.get());
                let mut lines: Vec<(usize, Line)> = (page.iter())
                    .flat_map(|page| {
                        (page.threads.iter())
                            .filter_map(|placed| Some((placed.block?, card(page, placed))))
                    })
                    .collect();
                if let Some((block, line)) = page.as_ref().and_then(|page| starting(page)) {
                    let at = (lines.iter())
                        .position(|(other, _)| *other >= block)
                        .unwrap_or(lines.len());
                    lines.insert(at, (block, line));
                }
                let beside: Vec<(usize, String)> = (lines.iter())
                    .map(|(block, line)| (*block, line.slot()))
                    .collect();
                // The elements of what left the margin are let go of.
                (listing.asides.borrow_mut())
                    .retain(|slot, _| beside.iter().any(|(_, kept)| kept == slot));
                *listing.beside.borrow_mut() = beside;
                lines.into_iter().map(|(_, line)| line).collect::<Vec<_>>()
            },
            key_of,
            {
                let draw = draw.clone();
                move |line| {
                    let slot = line.slot();
                    let at = slot.clone();
                    let item = el("div")
                        .class(styles::ASIDE)
                        .style_dyn("top", move || {
                            let top = state.tops.with(|tops| tops.get(&at).copied());
                            format!("{}px", top.unwrap_or(0.))
                        })
                        .child(draw(line));
                    placing.asides.borrow_mut().insert(slot, item);
                    item
                }
            },
        );

    let unit = el("div").class(styles::UNIT);
    let row = el("div")
        .class(styles::ROW)
        .class_dyn(move || (!wide.get()).then_some(styles::NARROW))
        .child(text)
        .child(margin);
    // After them, the threads whose passage has gone.
    scroll.child(unit).child(row).each(
        move || {
            let Some(page) = state.page.get() else {
                return Vec::new();
            };
            let orphans: Vec<Line> = (page.orphans())
                .map(|(_, placed)| card(&page, placed))
                .collect();
            let heading = (!orphans.is_empty()).then_some(Line::Orphans);
            heading.into_iter().chain(orphans).collect::<Vec<_>>()
        },
        key_of,
        move |line| draw(line),
    );

    // After each layout: the selection as it is now, and the passage to
    // bring into view.
    let laid = held.clone();
    each_layout(move |page| {
        if state.page.get_untracked().is_none() {
            return;
        }
        let selected = state.selected(page);
        if selected != state.selection.get_untracked() {
            state.selection.set(selected);
            relayout();
        }
        // Whether there is a margin, from the width the text and it share.
        let scale = page.size(unit).map_or(1., |(width, _)| width / 16.);
        let fits = (page.size(row)).and_then(|(width, _)| margin_for(width, scale.max(0.1)));
        if fits != state.margin.get_untracked() {
            state.margin.set(fits);
            relayout();
            return;
        }
        // Each item of the margin at the top of its block, or below the
        // item before it.
        if let Some(column) = fits.and_then(|_| page.bounds(margin)) {
            let (blocks, asides) = (laid.blocks.borrow(), laid.asides.borrow());
            let mut tops = HashMap::new();
            let mut cursor = 0.;
            for (block, slot) in laid.beside.borrow().iter() {
                let Some(item) = asides.get(slot).and_then(|item| page.bounds(*item)) else {
                    continue;
                };
                let beside = (blocks.get(block))
                    .and_then(|block| page.bounds(*block))
                    .map_or(0., |block| (block.y - column.y).max(0.));
                let top = beside.max(cursor);
                cursor = top + item.height + 8. * scale;
                tops.insert(slot.clone(), top);
            }
            let moved = state.tops.with_untracked(|placed| {
                placed.len() != tops.len()
                    || (tops.iter()).any(|(slot, top)| {
                        placed.get(slot).is_none_or(|was| (was - top).abs() > 0.5)
                    })
            });
            if moved || (state.reach.get_untracked() - cursor).abs() > 0.5 {
                state.tops.set(tops);
                state.reach.set(cursor);
                relayout();
            }
        }
        if let Some(index) = laid.reveal.take() {
            let block = laid.blocks.borrow().get(&index).copied();
            if let (Some(block), Some(view)) = (
                block.and_then(|block| page.bounds(block)),
                page.bounds(scroll),
            ) {
                let above = block.y - view.y;
                let below = (block.y + block.height) - (view.y + view.height);
                if above < 0. {
                    scroll.scroll_by(0., above);
                } else if below > 0. {
                    scroll.scroll_by(0., below);
                }
            }
        }
    });
    // The pieces and elements of blocks that are gone are let go of.
    Effect::new(move |_| {
        let count = state.page.get().map_or(0, |page| page.blocks.len());
        held.pieces.borrow_mut().retain(|index, _| *index < count);
        held.blocks.borrow_mut().retain(|index, _| *index < count);
    });

    root.on("keydown", move |event| {
        let DomEventData::KeyDown(key) = &event.data else {
            return;
        };
        if key.key == Key::Named(NamedKey::Escape) && state.target.get_untracked().is_some() {
            state.cancel();
            stop_propagation();
            return;
        }
        if key.modifiers.ctrl() || key.modifiers.alt() || key.modifiers.meta() {
            return;
        }
        // A line is three of the body's lines; a page most of the box.
        let scroll_by = move |lines: f64, pages: f64| {
            dispatched(move |page| {
                let height = page.size(scroll).map_or(0., |(_, height)| height);
                scroll.scroll_by(0., lines * 26. * 3. + pages * height * 0.8);
            });
        };
        let character = match &key.key {
            Key::Character(character) => character.as_str(),
            _ => "",
        };
        match (&key.key, character) {
            (_, "j") | (Key::Named(NamedKey::ArrowDown), _) => scroll_by(1., 0.),
            (_, "k") | (Key::Named(NamedKey::ArrowUp), _) => scroll_by(-1., 0.),
            (_, " ") | (Key::Named(NamedKey::PageDown), _) => scroll_by(0., 1.),
            (Key::Named(NamedKey::PageUp), _) => scroll_by(0., -1.),
            (_, "h") | (Key::Named(NamedKey::Backspace | NamedKey::ArrowLeft), _) => state.back(),
            (_, "c") => state.comment_on_selection(),
            // The next thread, or the one before, around the page's.
            (_, "n" | "p") => {
                let roots: Vec<String> = state.page.get_untracked().map_or_else(Vec::new, |page| {
                    (page.threads.iter())
                        .map(|placed| placed.thread.root.id.clone())
                        .collect()
                });
                if roots.is_empty() {
                    return;
                }
                let at = (state.focused.get_untracked())
                    .and_then(|root| roots.iter().position(|known| *known == root));
                let next = match (at, character) {
                    (None, "n") => 0,
                    (None, _) => roots.len() - 1,
                    (Some(index), "n") => (index + 1) % roots.len(),
                    (Some(index), _) => (index + roots.len() - 1) % roots.len(),
                };
                state.focus_thread(roots[next].clone());
            }
            _ => return,
        }
        prevent_default();
    })
    // The dialog, built again when what it shows is other.
    .each(
        move || {
            let shown = (state.layout.get() == CommentLayout::Dialog)
                .then(|| state.target.get())
                .flatten()
                .zip(state.page.get());
            let key = shown.map(|(target, page)| match target {
                Target::Selection(chars) => format!("new {chars:?}"),
                Target::Thread(root) => {
                    let placed = (page.threads.iter()).find(|placed| placed.thread.root.id == root);
                    let shows = placed.map(|placed| {
                        (
                            placed.thread.replies.len(),
                            placed.thread.last().id.clone(),
                            placed.replying(&page.replying),
                        )
                    });
                    format!("thread {root} {shows:?}")
                }
            });
            key.into_iter().collect::<Vec<_>>()
        },
        |key| key.clone(),
        move |_| dialog(state),
    )
    .child(header)
    .when(
        move || state.notice.with(Option::is_some),
        move || {
            Notice::Muted
                .view()
                .class(styles::NOTICE)
                .text_dyn(move || state.notice.get().unwrap_or_default())
        },
    )
    .each(
        move || {
            let stated = match (state.path.get(), state.error.get(), state.page.get()) {
                (None, ..) => Some((NO_PAGE.to_string(), false)),
                (_, Some(error), _) => Some((error, true)),
                (_, _, None) => Some((LOADING.to_string(), false)),
                _ => None,
            };
            stated.into_iter().collect::<Vec<_>>()
        },
        |stated| stated.clone(),
        |(text, error)| {
            el("div")
                .class((styles::STATE, error.then_some(styles::ERROR)))
                .text(text)
        },
    )
    .child(scroll);
    (root, state)
}
