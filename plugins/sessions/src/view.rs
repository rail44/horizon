//! The list's view: a project's sessions under its name, each a line of
//! its title, what it is, its state and since when, and a line under it
//! for its words where it has some; a band of the keys under them. What
//! the list does ([`Act`]) is done by its key, by a press on it in the
//! band, or, for what is done to a session, from the menu on the
//! session's row (its `⋯`, or a right-click on the row); a click on a row
//! goes to its session. `/` asks for the field a search is typed into.
//!
//! Before a session is ended the list asks, in an alert dialog where it
//! is seen and answered with the pointer or the keys alike: over the
//! whole workspace from a pane, which is narrow, and over the list itself
//! in the sheet, which is over the workspace already.
//!
//! The list is shown in a pane ([`pane`]), where it stays, and in a sheet
//! over the workspace ([`over`]), which going to a session leaves. The
//! two are the same list ([`list`]) told what differs ([`Shown`]).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use horizon_components::{
    AlertDialog, Icon, MenuButton, MenuItem, MenuLabel, ROW_SELECTED, list_row,
};
use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::guest::bindings::horizon::ui::session_control as control;
use horizon_ui::guest::bindings::horizon::ui::sessions::{self as host, Attention, Session};
use horizon_ui::guest::{self, Close, CursorTarget, modal, watch_sessions, watch_workspace};
use horizon_ui::keys::Key;
use horizon_ui::reactive_graph::computed::Memo;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::owner::on_cleanup;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, With, WithUntracked};
use horizon_ui::{
    Callback, Class, Component, El, each_layout, el, prevent_default, stop_propagation,
};
use keyboard_types::NamedKey;

use crate::list::{self, Group, Row};

mod styles {
    horizon_ui::css_module! {"
    .sessions { position: relative; display: flex; flex-direction: column; height: 100%;
                min-width: 0; outline: none; background: var(--background); color: var(--text);
                font-family: var(--font-family); font-size: var(--body-size);
                line-height: var(--body-line);
        .search { flex: none; height: var(--u32); display: flex; align-items: center;
                  padding: 0 var(--u8); border-bottom: var(--line) solid var(--border); }
        .search > * { flex: 1; min-width: 0; border: none; outline: none; padding: 0;
                      background: none; color: var(--text); font-family: var(--font-family);
                      font-size: var(--body-size); line-height: calc(var(--u4) * 5); }
        .rows { flex: 1; min-height: 0; overflow-y: scroll; padding: var(--u4); }
        .none { padding: var(--u24) var(--u12); text-align: center; color: var(--text-muted); }
        .band { display: flex; align-items: center; gap: var(--u8);
                padding: var(--u8) var(--u8) var(--u4); font-size: var(--meta-size);
                line-height: var(--meta-line); color: var(--text-muted); }
        .row { display: flex; align-items: flex-start; box-sizing: border-box;
               padding: var(--u4) var(--u8) var(--u4) 0;
               cursor: pointer; }
        /* The menu, and since when under it. */
        .side { flex: none; align-self: stretch; display: flex; flex-direction: column;
                align-items: flex-end; justify-content: space-between;
                margin-left: var(--u8); }
        .actions { opacity: 0; }
        .row:hover .actions, .row.show-actions .actions { opacity: 1; }
        .lead { flex: none; display: flex; width: var(--u16); color: var(--text-muted); }
        .lead.warning { color: var(--chip-warning-text); }
        .lead.accent { color: var(--chip-accent-text); }
        .lead.info { color: var(--chip-info-text); }
        .lead.danger { color: var(--chip-danger-text); }
        /* At work, its icon turns. */
        .turning { animation: turn 1s linear infinite; }
        .label { flex: 1; min-width: 0; display: flex; flex-direction: column; }
        .line { display: flex; align-items: center; gap: var(--u8); min-width: 0; }
        .title { flex: 0 1 auto; min-width: 0; overflow: hidden; white-space: nowrap;
                 text-overflow: ellipsis; }
        .title.away { color: var(--text-muted); }
        .time { text-align: right; white-space: nowrap;
                font-size: var(--micro-size); line-height: var(--micro-line);
                color: var(--text-muted); }
        .under { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap;
                 text-overflow: ellipsis; font-size: var(--meta-size);
                 line-height: var(--meta-line); color: var(--text-muted); }
        .asking { position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: 50;
                  display: flex; justify-content: center; align-items: center;
                  padding: var(--u24); }
        .dim { position: absolute; left: 0; top: 0; right: 0; bottom: 0;
               background: var(--background); opacity: 0.7; }
        .dialog { position: relative; border: var(--line) solid var(--border);
                  border-radius: var(--radius-large); overflow: hidden;
                  box-shadow: var(--shadow-modal); }
    }
    .over { width: 720px; height: 480px; display: flex; }
    .over > .sessions { flex: 1; }
    @keyframes turn { to { transform: rotate(1turn); } }
    "}
}

/// What the list does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    /// Goes to the chosen session.
    GoTo,
    /// Opens a terminal where the chosen session works.
    Terminal,
    /// Asks whether to end the chosen session.
    Terminate,
    /// Asks whether to end the chosen session and those derived from it.
    TerminateDerived,
    /// Chooses the next session that waits.
    NextWaiting,
    /// Asks for the search's field.
    Search,
}

/// What a row's menu does to its session, in order.
const ON_A_ROW: [Act; 4] = [
    Act::GoTo,
    Act::Terminal,
    Act::Terminate,
    Act::TerminateDerived,
];

impl Act {
    /// The key that does it.
    fn key(self) -> &'static str {
        match self {
            Self::GoTo => "Enter",
            Self::Terminal => "t",
            Self::Terminate => "x",
            Self::TerminateDerived => "X",
            Self::NextWaiting => "n",
            Self::Search => "/",
        }
    }

    /// What a row's menu calls it.
    fn named(self) -> &'static str {
        match self {
            Self::GoTo => "Go to",
            Self::Terminal => "Open a terminal there",
            Self::Terminate => "Terminate",
            Self::TerminateDerived => "Terminate with derived",
            Self::NextWaiting => "Next waiting",
            Self::Search => "Search",
        }
    }
}

/// Does an act.
type Run = Rc<dyn Fn(Act)>;

/// What differs with where the list is shown.
struct Shown {
    /// Whether the list asks over the whole workspace before it ends a
    /// session; where it does not, it asks over itself.
    asks_over_the_workspace: bool,
    /// Whether Escape takes a search back. Where it does not, Escape is
    /// passed on.
    escape_takes_a_search_back: bool,
    /// Done once a session is gone to, or a terminal opened where one
    /// works.
    gone: Rc<dyn Fn()>,
}

/// The list as a pane shows it: it stays as sessions are gone to, and
/// Escape takes a search back.
pub fn pane() -> El {
    list(Shown {
        asks_over_the_workspace: true,
        escape_takes_a_search_back: true,
        gone: Rc::new(|| {}),
    })
}

/// The list in a sheet over the workspace: going to a session leaves it,
/// and Escape is the sheet's, which leaves it too.
pub fn over(close: Close) -> El {
    let list = list(Shown {
        asks_over_the_workspace: false,
        escape_takes_a_search_back: false,
        gone: Rc::new(move || close.close()),
    });
    el("div").class(styles::OVER).child(list)
}

/// What the list asks before it ends anything.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Asked {
    session: String,
    title: String,
    /// How many sessions derived from it end with it, where they do.
    derived: Option<usize>,
}

impl Asked {
    /// The dialog that asks it: `answered` hears whether the session is
    /// to end, once it is answered.
    fn dialog(&self, answered: impl Fn(bool) + Clone + 'static) -> El {
        let title = match self.derived {
            Some(derived) => format!(
                "Terminate “{}” and the {derived} derived from it?",
                self.title
            ),
            None => format!("Terminate “{}”?", self.title),
        };
        let description = match self.derived {
            Some(_) => "It ends what runs in them.",
            None => "It ends what runs in it.",
        };
        let ended = answered.clone();
        AlertDialog {
            title: &title,
            description,
            action: "Terminate",
            action_key: Some("y"),
            cancel: "Keep",
            danger: true,
            on_action: Callback::new(move |_| ended(true)),
            on_cancel: Callback::new(move |_| answered(false)),
        }
        .view()
    }
}

#[derive(Clone, Copy)]
struct State {
    sessions: RwSignal<Vec<Session>>,
    /// What a search is for; nothing where none is made.
    query: RwSignal<String>,
    /// Whether the search's field has the keys.
    searching: RwSignal<bool>,
    /// The chosen session, by what the host names it.
    chosen: RwSignal<Option<String>>,
    asked: RwSignal<Option<Asked>>,
    /// The time the rows' times are told from.
    now: RwSignal<u64>,
}

fn now_ms() -> u64 {
    (SystemTime::now().duration_since(UNIX_EPOCH)).map_or(0, |since| since.as_millis() as u64)
}

impl State {
    fn chosen_session(&self) -> Option<Session> {
        let chosen = self.chosen.get_untracked()?;
        (self.sessions.get_untracked().into_iter()).find(|session| session.id == chosen)
    }

    /// How many sessions were derived from `session`, however far down.
    fn derived_from(&self, session: &str) -> usize {
        let sessions = self.sessions.get_untracked();
        let mut found = vec![session.to_string()];
        let mut at = 0;
        while at < found.len() {
            for other in &sessions {
                if other.parent.as_deref() == Some(found[at].as_str()) && !found.contains(&other.id)
                {
                    found.push(other.id.clone());
                }
            }
            at += 1;
        }
        found.len() - 1
    }
}

/// The color a session's state is said in.
fn tone(session: &Session) -> Class {
    match session
        .activity
        .as_ref()
        .and_then(|activity| activity.attention)
    {
        Some(Attention::Failed) => styles::DANGER,
        Some(Attention::Unseen) => styles::ACCENT,
        Some(_) => styles::WARNING,
        None => styles::INFO,
    }
}

/// A session's row. It is made once for a session and follows what the
/// session comes to be. A click goes to the session; its menu, on its `⋯`
/// or a right-click, does the rest to it. `within` is the list's scrolled
/// element, which the menu stays inside.
fn row(state: State, order: Memo<Vec<Row>>, id: String, rows: &Rows, run: Run, within: El) -> El {
    let named = id.clone();
    let this = Memo::new(move |_| {
        order.with(|order| order.iter().find(|row| row.session.id == named).cloned())
    });
    let said = move |say: fn(&Session) -> String| {
        move || {
            this.with(|row| {
                row.as_ref()
                    .map(|row| say(&row.session))
                    .unwrap_or_default()
            })
        }
    };
    let chosen = id.clone();
    let is_chosen = move || state.chosen.get().as_deref() == Some(chosen.as_str());
    let detail = move || {
        this.with(|row| {
            let activity = row.as_ref()?.session.activity.as_ref()?;
            activity.detail.clone()
        })
    };
    let picked = id.clone();
    // The menu is open on the chosen session.
    let open = RwSignal::new(false);
    let opened = id.clone();
    Effect::new(move |_| {
        if open.get() {
            state.chosen.set(Some(opened.clone()));
        }
    });
    // "With derived" is offered where some are derived from the session.
    let derived = id.clone();
    let has_derived = move || {
        state.sessions.with(|_| ());
        state.derived_from(&derived) > 0
    };
    let by_menu = run.clone();
    let menu = move |has_derived: &bool| {
        let acts: Vec<Act> = (ON_A_ROW.into_iter())
            .filter(|act| *act != Act::TerminateDerived || *has_derived)
            .collect();
        let items = (acts.iter())
            .map(|act| MenuItem {
                label: act.named().to_string(),
                key: Some(act.key().to_string()),
                danger: matches!(act, Act::Terminate | Act::TerminateDerived),
            })
            .collect();
        let run = by_menu.clone();
        MenuButton {
            label: MenuLabel::More,
            items,
            open,
            on_select: Callback::new(move |index: usize| run(acts[index])),
            within: Some(within),
        }
    };
    // Two lines, each there whatever the session is doing, so a row is as
    // tall whichever: its title, with what state it is in by an icon in
    // the place of its kind's; under it its kind, the state's word and its
    // words. At the row's end its menu, and since when under it. A session
    // no pane shows has its title muted.
    let tone_of = move || {
        this.with(|row| {
            let session = &row.as_ref()?.session;
            list::chip(session).map(|_| tone(session))
        })
    };
    let word = move || this.with(|row| list::chip(&row.as_ref()?.session).map(str::to_string));
    let terminal = move || {
        this.with(|row| {
            row.as_ref()
                .is_some_and(|row| row.session.kind == "terminal")
        })
    };
    let lead = el("span").class(styles::LEAD).class_dyn(tone_of).each(
        move || vec![(word(), terminal())],
        |(word, terminal)| (word.clone(), *terminal),
        |(word, terminal)| match (word.as_deref(), terminal) {
            (Some("working"), _) => Icon::CircleNotch.view().class(styles::TURNING),
            (Some("failed"), _) => Icon::XCircle.view(),
            (Some("unseen"), _) => Icon::Circle.view(),
            (Some(_), _) => Icon::Warning.view(),
            (None, true) => Icon::TerminalWindow.view(),
            (None, false) => Icon::Robot.view(),
        },
    );
    let title = el("span")
        .class(styles::TITLE)
        .class_dyn(move || {
            (this.with(|row| row.as_ref().is_some_and(|row| !row.session.attached)))
                .then_some(styles::AWAY)
        })
        .text_dyn(said(|session| session.title.clone()));
    let time = el("span").class(styles::TIME).text_dyn(move || {
        let now = state.now.get();
        this.with(|row| {
            let activity = row.as_ref()?.session.activity.as_ref()?;
            Some(list::ago(activity.since_ms, now))
        })
        .unwrap_or_default()
    });
    let said_under = move || {
        let kind = this.with(|row| row.as_ref().map(|row| list::kind(&row.session).to_string()));
        let parts: Vec<String> = [kind, word(), detail()].into_iter().flatten().collect();
        parts.join(" · ")
    };
    let first = el("div").class(styles::LINE).child(lead).child(title);
    let second = el("div")
        .class(styles::LINE)
        .child(el("span").class(styles::LEAD))
        .child(el("span").class(styles::UNDER).text_dyn(said_under));
    let element = list_row()
        .class(styles::ROW)
        .class_dyn(move || {
            let chosen = is_chosen();
            (
                chosen.then_some(ROW_SELECTED),
                chosen.then_some(styles::SHOW_ACTIONS),
            )
        })
        .child(
            el("div")
                .class(styles::LABEL)
                .style_dyn("margin-left", move || {
                    let depth = this.with(|row| row.as_ref().map_or(0, |row| row.depth));
                    format!("{}px", 8 + depth * 16)
                })
                .child(first)
                .child(second),
        )
        .child(
            el("div")
                .class(styles::SIDE)
                .child(el("div").class(styles::ACTIONS).each(
                    move || vec![has_derived()],
                    |has| *has,
                    menu,
                ))
                .child(time),
        )
        .on("click", move |_| {
            state.chosen.set(Some(picked.clone()));
            run(Act::GoTo);
        })
        .on("contextmenu", move |_| {
            open.set(true);
            prevent_default();
        });
    rows.borrow_mut().insert(id, element);
    element
}

/// The rows' elements, by their sessions: what a chosen row is brought
/// into view by.
type Rows = Rc<RefCell<HashMap<String, El>>>;

/// The session the workspace's cursor is on, or else the one the focus is
/// on: what the list's cursor follows. None where the cursor is on no
/// session, or the focus is on none.
fn followed() -> Option<String> {
    match guest::cursor() {
        Some(CursorTarget::Session(id)) => Some(id),
        Some(CursorTarget::Pane | CursorTarget::Strip) => None,
        None => guest::focus(),
    }
}

fn list(shown: Shown) -> El {
    let state = State {
        sessions: RwSignal::new(host::all()),
        query: RwSignal::new(String::new()),
        searching: RwSignal::new(false),
        chosen: RwSignal::new(followed()),
        asked: RwSignal::new(None),
        now: RwSignal::new(now_ms()),
    };
    // Watched for as long as the view is there: the effect holds the
    // watch. Read again now that they are watched, since a change between
    // the first reading and the watch would not be told.
    Effect::new(move |_| {
        let watching = watch_sessions(move || {
            state.sessions.set(host::all());
            state.now.set(now_ms());
        });
        state.sessions.set(host::all());
        watching
    });
    let groups = Memo::new(move |_| {
        state
            .sessions
            .with(|sessions| list::groups(sessions, &state.query.get()))
    });
    let order: Memo<Vec<Row>> = Memo::new(move |_| {
        groups.with(|groups| groups.iter().flat_map(|group| group.rows.clone()).collect())
    });
    // The cursor follows the workspace's cursor and focus to a session the
    // list shows, but while the focus is in the list: its own cursor is the
    // one shown then, and what the workspace says of it may be behind it.
    // (In workspace mode the keys are the shell's, not the list's.)
    // The focus is looked at after each layout, so the list follows again
    // once it has left the list too.
    let follow = move || {
        let Some(followed) = followed() else {
            return;
        };
        let shown =
            order.with_untracked(|order| order.iter().any(|row| row.session.id == followed));
        if shown && state.chosen.get_untracked().as_ref() != Some(&followed) {
            state.chosen.set(Some(followed));
        }
    };
    let has_keys = Rc::new(Cell::new(false));
    // (A session the workspace is on may be told of after it: the list
    // follows again when its rows change.)
    let keyed = has_keys.clone();
    Effect::new(move |_| {
        order.with(|_| ());
        if !keyed.get() {
            follow();
        }
    });
    let keyed = has_keys.clone();
    Effect::new(move |_| {
        let keyed = keyed.clone();
        watch_workspace(move || {
            if !keyed.get() {
                follow();
            }
        })
    });
    // The choice stays on a session there is: where the chosen one has
    // gone, on what took its place.
    Effect::new(move |before: Option<Vec<String>>| {
        let now: Vec<String> =
            order.with(|order| order.iter().map(|row| row.session.id.clone()).collect());
        let chosen = state.chosen.get_untracked();
        if !chosen.as_ref().is_some_and(|chosen| now.contains(chosen)) {
            let was = before
                .zip(chosen)
                .and_then(|(before, chosen)| before.iter().position(|session| *session == chosen));
            let at = was.unwrap_or(0).min(now.len().saturating_sub(1));
            state.chosen.set(now.get(at).cloned());
        }
        now
    });

    let step = move |by: isize, waiting: bool| {
        let rows = order.get_untracked();
        let Some(at) = (state.chosen.get_untracked())
            .and_then(|chosen| rows.iter().position(|row| row.session.id == chosen))
        else {
            return;
        };
        let count = rows.len() as isize;
        // The next that waits is looked for round the list; a step stops
        // at its ends.
        let next = match waiting {
            true => (1..=count)
                .map(|step| (at as isize + step).rem_euclid(count) as usize)
                .find(|at| list::waits(&rows[*at].session)),
            false => Some((at as isize + by).clamp(0, count - 1) as usize),
        };
        if let Some(next) = next {
            state.chosen.set(Some(rows[next].session.id.clone()));
        }
    };
    let ask = move |derived: bool| {
        let Some(session) = state.chosen_session() else {
            return;
        };
        let count = state.derived_from(&session.id);
        state.asked.set(Some(Asked {
            derived: (derived && count > 0).then_some(count),
            title: session.title,
            session: session.id,
        }));
    };
    // What was asked is answered: the session ended, or kept.
    let answer = move |end: bool| {
        if let Some(asked) = state.asked.get_untracked()
            && end
        {
            match asked.derived {
                Some(_) => control::terminate_with_derived(&asked.session),
                None => control::terminate(&asked.session),
            }
        }
        state.asked.set(None);
    };
    let gone = shown.gone.clone();
    let run: Run = Rc::new(move |act| match act {
        Act::GoTo => {
            if let Some(session) = state.chosen_session() {
                control::show(&session.id);
                gone();
            }
        }
        Act::Terminal => {
            if let Some(session) = state.chosen_session() {
                control::open_terminal(&session.id);
                gone();
            }
        }
        Act::Terminate => ask(false),
        Act::TerminateDerived => ask(true),
        Act::NextWaiting => step(0, true),
        Act::Search => state.searching.set(true),
    });

    let elements: Rows = Rows::default();
    let made = elements.clone();
    let by_rows = run.clone();
    let scrolled = el("div").class(styles::ROWS);
    let list = scrolled.each(
        move || groups.get(),
        |group: &Group| group.root.clone(),
        move |group: &Group| {
            let (root, made, run) = (group.root.clone(), made.clone(), by_rows.clone());
            let (named, counted) = (root.clone(), root.clone());
            let of = move |root: &str| {
                groups.with(|groups| groups.iter().find(|group| group.root == root).cloned())
            };
            el("div")
                .child(
                    el("div")
                        .class(styles::BAND)
                        .child(el("span").text_dyn(move || {
                            of(&named).map(|group| group.name).unwrap_or_default()
                        }))
                        .child(el("span").text_dyn(move || {
                            of(&counted).map_or(0, |group| group.rows.len()).to_string()
                        })),
                )
                .each(
                    move || of(&root).map(|group| group.rows).unwrap_or_default(),
                    |row: &Row| row.session.id.clone(),
                    move |row: &Row| {
                        let id = row.session.id.clone();
                        self::row(state, order, id, &made, run.clone(), scrolled)
                    },
                )
        },
    );
    let list = list.when(
        move || groups.with(Vec::is_empty),
        move || {
            el("div").class(styles::NONE).text_dyn(move || {
                match state.query.get().trim().is_empty() {
                    true => "No sessions".to_string(),
                    false => "No session is found".to_string(),
                }
            })
        },
    );
    // The chosen row is kept in view.
    let brought = RefCell::new(None::<String>);
    let held = elements.clone();
    each_layout(move |page| {
        let chosen = state.chosen.get_untracked();
        if *brought.borrow() == chosen {
            return;
        }
        *brought.borrow_mut() = chosen.clone();
        let row = chosen.and_then(|chosen| held.borrow().get(&chosen).copied());
        if let (Some(row), Some(view)) = (row.and_then(|row| page.bounds(row)), page.bounds(list)) {
            let above = row.y - view.y;
            let below = (row.y + row.height) - (view.y + view.height);
            if above < 0. {
                list.scroll_by(0., above);
            } else if below > 0. {
                list.scroll_by(0., below);
            }
        }
    });

    let root = el("div").class(styles::SESSIONS).attr("tabindex", "0");
    // Where the chosen session is, the workspace shows as workspace mode
    // does, while the focus is in the list.
    Effect::new(move |_| guest::set_cursor(root, state.chosen.get().as_deref()));
    each_layout(move |page| {
        let now = page.has_focus_within(root);
        if has_keys.replace(now) && !now {
            follow();
        }
    });
    let takes_back = shown.escape_takes_a_search_back;
    let field = move || {
        let input = el("input")
            .attr("type", "text")
            .attr("placeholder", "Search")
            .keys(vec![
                Key::new("enter", "done", move || {
                    state.searching.set(false);
                    root.focus();
                }),
                Key::when(
                    "esc",
                    if takes_back { "clear" } else { "close" },
                    move || {
                        if takes_back {
                            state.query.set(String::new());
                            state.searching.set(false);
                            root.focus();
                        }
                        takes_back
                    },
                ),
            ])
            .on("input", move |event| {
                if let DomEventData::Input(input) = &event.data {
                    state.query.set(input.value.clone());
                }
            })
            .set_value(&state.query.get_untracked());
        el("div")
            .class(styles::SEARCH)
            .child(match state.searching.get_untracked() {
                true => input.focus(),
                false => input,
            })
    };

    // Asked whether to end a session: over the whole workspace, or over
    // the list. The keys come back to the list once it is answered.
    let over_the_workspace = shown.asks_over_the_workspace;
    if over_the_workspace {
        let opened: Rc<RefCell<Option<Asked>>> = Rc::default();
        Effect::new(move |_| {
            let Some(asked) = state.asked.get() else {
                *opened.borrow_mut() = None;
                return;
            };
            if opened.borrow().as_ref() == Some(&asked) {
                return;
            }
            *opened.borrow_mut() = Some(asked.clone());
            modal(move |close| {
                // (Taken away without an answer -- a press outside it --
                // it is as if it were kept.)
                let left = asked.clone();
                on_cleanup(move || {
                    if state.asked.get_untracked().as_ref() == Some(&left) {
                        state.asked.set(None);
                    }
                });
                asked.dialog(move |end| {
                    answer(end);
                    close.close();
                })
            });
        });
    }
    let asking_over_the_list = move || {
        (state.asked.get().into_iter())
            .filter(|_| !over_the_workspace)
            .collect::<Vec<_>>()
    };
    let over_the_list = move |asked: &Asked| {
        let dialog = asked.dialog(move |end| {
            answer(end);
            root.focus();
        });
        el("div")
            .class(styles::ASKING)
            // (On the click, not as the button goes down: gone by then, it
            // would leave the click to the row under it.)
            .child(el("div").class(styles::DIM).on("click", move |_| {
                answer(false);
                root.focus();
                stop_propagation();
            }))
            .child(el("div").class(styles::DIALOG).child(dialog))
    };

    let root = root
        .when(
            move || state.searching.get() || !state.query.get().is_empty(),
            field,
        )
        .child(list)
        .each(asking_over_the_list, Clone::clone, over_the_list);

    // The list's keys, in the order they are wanted.
    let mut keys = vec![
        Key::group("j/k", "move")
            .bind("j", move || step(1, false))
            .bind("down", move || step(1, false))
            .bind("k", move || step(-1, false))
            .bind("up", move || step(-1, false)),
    ];
    for (spelled, chord, label, act) in [
        ("enter", "enter", "go to", Act::GoTo),
        ("t", "t", "terminal", Act::Terminal),
        ("x", "x", "terminate", Act::Terminate),
        ("X", "shift+x", "with derived", Act::TerminateDerived),
        ("n", "n", "next waiting", Act::NextWaiting),
        ("/", "/", "search", Act::Search),
    ] {
        let run = run.clone();
        keys.push(Key::group(spelled, label).bind(chord, move || run(act)));
    }
    // (Over the workspace, Escape is the sheet's, which leaves the list.)
    if !takes_back {
        keys.push(Key::when("esc", "close", || false));
    }
    root.keys(keys).on("keydown", move |event| {
        let DomEventData::KeyDown(key) = &event.data else {
            return;
        };
        state.now.set(now_ms());
        // (A search that stands is taken back.)
        let escape = key.key == keyboard_types::Key::Named(NamedKey::Escape);
        if escape && takes_back && !state.query.get_untracked().is_empty() {
            state.query.set(String::new());
            prevent_default();
            stop_propagation();
        }
    });
    root.focus()
}
