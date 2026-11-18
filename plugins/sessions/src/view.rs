//! The list's view: a project's sessions under its name, each a line of
//! its title, what it is, its state and since when, with its words under
//! it where it has some; a band of the
//! keys under them. A typed key is a command of the list's, and `/` asks
//! for the field a search is typed into.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use horizon_components::{Chip, Tone};
use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::guest::bindings::horizon::ui::session_control as control;
use horizon_ui::guest::bindings::horizon::ui::sessions::{self as host, Attention, Session};
use horizon_ui::guest::watch_sessions;
use horizon_ui::reactive_graph::computed::Memo;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, With};
use horizon_ui::{El, each_layout, el, prevent_default, stop_propagation};
use keyboard_types::{Key, NamedKey};

use crate::list::{self, Group, Row};

mod styles {
    horizon_ui::css_module! {"
    .sessions { display: flex; flex-direction: column; height: 100%; min-width: 0;
                outline: none; background: var(--background); color: var(--text);
                font-family: var(--font-family); font-size: var(--body-size);
                line-height: var(--body-line);
        .search { flex: none; height: 32px; display: flex; align-items: center;
                  padding: 0 var(--u12); border-bottom: var(--line) solid var(--border); }
        .search > * { flex: 1; min-width: 0; border: none; outline: none; padding: 0;
                      background: none; color: var(--text); font-family: var(--font-family);
                      font-size: var(--body-size); line-height: calc(var(--u4) * 5); }
        .rows { flex: 1; min-height: 0; overflow-y: scroll; }
        .none { padding: var(--u24) var(--u12); text-align: center; color: var(--text-muted); }
        .band { display: flex; align-items: center; gap: var(--u8);
                padding: var(--u4) var(--u12); font-size: var(--meta-size);
                line-height: var(--meta-line); color: var(--text-muted);
                background: var(--band); }
        .row { display: flex; align-items: flex-start; box-sizing: border-box;
               padding: var(--u8) var(--u12) var(--u8) 0; cursor: pointer;
               border: 1px solid transparent; }
        .row:hover { background: var(--band); }
        .row.selected { background: var(--selection); }
        .bar { flex: none; align-self: stretch; width: 2px;
               margin: calc(var(--u8) * -1) 0; }
        .row.selected .bar { background: var(--accent); }
        .label { flex: 1; min-width: 0; display: flex; flex-direction: column; }
        .headline { display: flex; align-items: baseline; gap: var(--u8); }
        .title { flex: 0 1 auto; min-width: 0; overflow: hidden; white-space: nowrap;
                 text-overflow: ellipsis; }
        .kind { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap;
                font-size: var(--micro-size); line-height: var(--micro-line);
                color: var(--text-muted); }
        .time { flex: none; width: var(--u28); text-align: right;
                font-size: var(--micro-size); line-height: var(--micro-line);
                color: var(--text-muted); }
        .meta { overflow: hidden; white-space: nowrap; text-overflow: ellipsis;
                font-size: var(--micro-size); line-height: var(--micro-line);
                color: var(--text-muted); }
        .foot { flex: none; display: flex; flex-wrap: wrap; align-items: center;
                gap: 0 var(--u12); padding: var(--u4) var(--u12);
                border-top: var(--line) solid var(--border); font-size: var(--micro-size);
                line-height: var(--micro-line); color: var(--text-muted); }
        .foot.asked { color: var(--danger); }
        .hint { display: flex; align-items: center; gap: var(--u4); }
        .key { font-weight: 600; color: var(--text); }
    }
    .sessions:focus .row.selected { border-color: var(--accent); }
    "}
}

/// The keys in the band, and what each does.
const KEYS: [(&str, &str); 6] = [
    ("Enter", "Go to"),
    ("t", "Terminal"),
    ("x", "Terminate"),
    ("X", "With derived"),
    ("n", "Next waiting"),
    ("/", "Search"),
];

/// What the list asks before it ends anything.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Asked {
    session: String,
    title: String,
    /// How many sessions derived from it end with it, where they do.
    derived: Option<usize>,
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

fn tone(session: &Session) -> Tone {
    match session
        .activity
        .as_ref()
        .and_then(|activity| activity.attention)
    {
        Some(Attention::Failed) => Tone::Danger,
        Some(Attention::Unseen) => Tone::Accent,
        Some(_) => Tone::Warning,
        None => Tone::Info,
    }
}

/// A session's row. It is made once for a session and follows what the
/// session comes to be.
fn row(state: State, order: Memo<Vec<Row>>, id: String, rows: &Rows) -> El {
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
    let (picked, shown) = (id.clone(), id.clone());
    // The title's line says everything of the session but its words,
    // which are under it where there are some. What state it is in is
    // the chip's to say, and nothing else of the row's.
    let headline = el("div")
        .class(styles::HEADLINE)
        .child(
            el("span")
                .class(styles::TITLE)
                .text_dyn(said(|session| session.title.clone())),
        )
        .child(
            el("span")
                .class(styles::KIND)
                .text_dyn(said(|session| list::kind(session).to_string())),
        )
        .each(
            move || {
                this.with(|row| {
                    let session = &row.as_ref()?.session;
                    Some((list::chip(session)?, tone(session)))
                })
                .into_iter()
                .collect()
            },
            |(word, tone)| (*word, *tone as u8),
            |(word, tone)| Chip {
                tone: *tone,
                text: word,
            },
        )
        .child(el("span").class(styles::TIME).text_dyn(move || {
            let now = state.now.get();
            this.with(|row| {
                let activity = row.as_ref()?.session.activity.as_ref()?;
                Some(list::ago(activity.since_ms, now))
            })
            .unwrap_or_default()
        }));
    let element = el("div")
        .class(styles::ROW)
        .class_dyn(move || is_chosen().then_some(styles::SELECTED))
        .child(el("span").class(styles::BAR))
        .child(
            el("div")
                .class(styles::LABEL)
                .style_dyn("margin-left", move || {
                    let depth = this.with(|row| row.as_ref().map_or(0, |row| row.depth));
                    format!("{}px", 12 + depth * 16)
                })
                .child(headline)
                .when(
                    move || detail().is_some(),
                    move || {
                        el("div")
                            .class(styles::META)
                            .text_dyn(move || detail().unwrap_or_default())
                    },
                ),
        )
        .on("click", move |_| state.chosen.set(Some(picked.clone())))
        .on("dblclick", move |_| control::show(&shown));
    rows.borrow_mut().insert(id, element);
    element
}

/// The rows' elements, by their sessions: what a chosen row is brought
/// into view by.
type Rows = Rc<RefCell<HashMap<String, El>>>;

pub fn view() -> El {
    let state = State {
        sessions: RwSignal::new(host::all()),
        query: RwSignal::new(String::new()),
        searching: RwSignal::new(false),
        chosen: RwSignal::new(None),
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

    let elements: Rows = Rows::default();
    let made = elements.clone();
    let list = el("div").class(styles::ROWS).each(
        move || groups.get(),
        |group: &Group| group.root.clone(),
        move |group: &Group| {
            let (root, made) = (group.root.clone(), made.clone());
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
                    move |row: &Row| self::row(state, order, row.session.id.clone(), &made),
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
    let field = move || {
        let input = el("input")
            .attr("type", "text")
            .attr("placeholder", "Search")
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
    let foot = move || match state.asked.get() {
        Some(asked) => {
            let question = match asked.derived {
                Some(derived) => format!(
                    "Terminate “{}” and the {derived} derived from it? It ends what runs in them.",
                    asked.title
                ),
                None => format!("Terminate “{}”? It ends what runs in it.", asked.title),
            };
            el("div")
                .class((styles::FOOT, styles::ASKED))
                .child(el("span").text(&question))
                .children([("y", "Terminate"), ("Esc", "Keep")].map(hint))
        }
        None => el("div").class(styles::FOOT).children(KEYS.map(hint)),
    };

    let root = root
        .when(
            move || state.searching.get() || !state.query.get().is_empty(),
            field,
        )
        .child(list)
        .each(
            move || vec![state.asked.get()],
            Clone::clone,
            move |_| foot(),
        );

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
    root.on("keydown", move |event| {
        let DomEventData::KeyDown(key) = &event.data else {
            return;
        };
        if key.is_composing || key.modifiers.ctrl() || key.modifiers.alt() || key.modifiers.meta() {
            return;
        }
        state.now.set(now_ms());
        let character = match &key.key {
            Key::Character(character) => character.as_str(),
            _ => "",
        };
        // Asked whether to end a session, the keys answer.
        if let Some(asked) = state.asked.get_untracked() {
            if character == "y" {
                match asked.derived {
                    Some(_) => control::terminate_with_derived(&asked.session),
                    None => control::terminate(&asked.session),
                }
            }
            state.asked.set(None);
            prevent_default();
            stop_propagation();
            return;
        }
        // In the search's field, the keys are what is searched for.
        if state.searching.get_untracked() {
            match &key.key {
                Key::Named(NamedKey::Enter) => state.searching.set(false),
                Key::Named(NamedKey::Escape) => {
                    state.query.set(String::new());
                    state.searching.set(false);
                }
                _ => return,
            }
            root.focus();
            prevent_default();
            stop_propagation();
            return;
        }
        let ask = |derived: bool| {
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
        match (&key.key, character) {
            (_, "j") | (Key::Named(NamedKey::ArrowDown), _) => step(1, false),
            (_, "k") | (Key::Named(NamedKey::ArrowUp), _) => step(-1, false),
            (_, "n") => step(0, true),
            (Key::Named(NamedKey::Enter), _) => {
                if let Some(session) = state.chosen_session() {
                    control::show(&session.id);
                }
            }
            (_, "t") => {
                if let Some(session) = state.chosen_session() {
                    control::open_terminal(&session.id);
                }
            }
            (_, "x") => ask(false),
            (_, "X") => ask(true),
            (_, "/") => state.searching.set(true),
            // (A search that stands is taken back.)
            (Key::Named(NamedKey::Escape), _) if !state.query.get_untracked().is_empty() => {
                state.query.set(String::new());
            }
            _ => return,
        }
        prevent_default();
        stop_propagation();
    });
    root.focus()
}

fn hint((key, does): (&str, &str)) -> El {
    el("span")
        .class(styles::HINT)
        .child(el("span").class(styles::KEY).text(key))
        .child(el("span").text(does))
}
