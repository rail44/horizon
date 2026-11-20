//! What runs a plugin built as a wasm component, for whatever shows one:
//! a preview (`preview.rs`) and an extension. The plugin has the view's
//! signals, effects and handlers; the document is this shell's. What the
//! plugin's `El` and `Page` do arrives here as calls (`horizon-ui`'s
//! `plugin.wit`) and is done in this document, on nodes the plugin made
//! and no others. An event that reaches one of those nodes is told to the
//! plugin.
//!
//! A plugin is driven by one future that goes on for as long as it does
//! ([`Loaded`]): the store's `run_concurrent`, around a loop that takes
//! this shell's calls into the plugin from a queue and makes them. The
//! plugin's own tasks (a file's changes waited on) go on whenever that
//! future is polled. A wake means one thing, that the future is to be
//! polled: the page is woken, and its effects poll it. A call's answer is
//! wanted where the call is made, so the call polls the future until it
//! has it.
//!
//! A plugin that traps is let go of. It traps when it takes more memory
//! than [`MEMORY_LIMIT`], or one poll of its future goes on for longer
//! than [`CALL_LIMIT`].

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread::Thread;
use std::time::{Duration, Instant};

use horizon_ui::blitz_dom::NodeId;
use horizon_ui::blitz_traits::events::{DomEvent, DomEventData};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set};
use horizon_ui::{El, dispatched, el, host, prevent_default, relayout, stop_propagation};
use keyboard_types::Key;
use wasmtime::component::{
    Accessor, Component, HasSelf, Instance, Linker, Resource, ResourceTable,
};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder, UpdateDeadline};

use crate::wake::Wake;
use horizon_theme::seed::{AccentValue, HueSlot, Seed};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "crates/horizon-ui/wit",
    world: "view",
    imports: {
        "horizon:ui/files.[method]watch.changed": async | store | trappable,
        "horizon:ui/theme.save": async | store | trappable,
        "horizon:ui/sessions.[method]watch.changed": async | store | trappable,
        "horizon:ui/modal.[method]modal.closed": async | store | trappable,
        default: trappable,
    },
    // Every call is made through the accessor, beside the plugin's tasks.
    exports: { default: async | store },
    // (What a plugin is told of the sessions is compared with what it was
    // told before.)
    additional_derives: [PartialEq],
    with: {
        "horizon:ui/files.file": File,
        "horizon:ui/files.working-directory": WorkingDirectory,
        "horizon:ui/files.watch": Watching,
        "horizon:ui/sessions.watch": SessionsWatching,
        "horizon:ui/modal.modal": ModalShown,
    },
});

use self::exports::horizon::ui::events::{EventData, KeyEvent, PointerEvent};
use self::horizon::ui::document::{Bounds, Host};

/// A file the person chose, or the command line named, as a plugin holds
/// it.
pub struct File(pub PathBuf);

/// Where a pane was asked for, as a plugin holds it.
pub struct WorkingDirectory(pub PathBuf);

/// A file a plugin watches, for as long as the plugin holds this; nothing
/// where the file cannot be watched.
pub struct Watching(Option<Arc<crate::watch::Kept>>);

/// The sessions a plugin watches, for as long as the plugin holds this.
pub struct SessionsWatching(tokio::sync::watch::Receiver<u64>);

pub use self::horizon::ui::sessions::{
    Activity as SessionActivity, Attention as SessionAttention, Project as SessionProject, Session,
};

/// The workspace's sessions as plugins are given them: whoever has the
/// sessions says what they are whenever they change, and a plugin reads
/// them and waits for the next change.
pub struct SessionsFeed {
    sessions: Mutex<Vec<Session>>,
    changes: tokio::sync::watch::Sender<u64>,
}

impl Default for SessionsFeed {
    fn default() -> Self {
        Self {
            sessions: Mutex::default(),
            changes: tokio::sync::watch::Sender::new(0),
        }
    }
}

impl SessionsFeed {
    /// Says what the sessions are; the plugins that wait are told where
    /// that is not what they were.
    pub fn set(&self, sessions: Vec<Session>) {
        let mut kept = self.sessions.lock().unwrap();
        if *kept != sessions {
            *kept = sessions;
            self.changes.send_modify(|change| *change += 1);
        }
    }

    fn all(&self) -> Vec<Session> {
        self.sessions.lock().unwrap().clone()
    }
}

/// What a plugin asks the host to do to a session, named as the host
/// named it to the plugin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionAsk {
    Show(String),
    Terminate(String),
    TerminateWithDerived(String),
    OpenTerminal(String),
}

/// What the host does with the workspace's sessions for a plugin.
#[derive(Clone)]
pub struct SessionAsks {
    /// What the sessions are.
    pub feed: Arc<SessionsFeed>,
    /// Does to a session what a plugin asked.
    pub act: Rc<dyn Fn(SessionAsk)>,
}

/// A view a plugin has over the workspace, for as long as the plugin
/// holds this.
pub struct ModalShown {
    node: NodeId,
    /// Whether the view has been taken away.
    closed: tokio::sync::watch::Receiver<bool>,
}

/// Says that a view is no longer over the workspace.
pub type ModalClosed = tokio::sync::watch::Sender<bool>;

/// What the host does to put a plugin's view over the workspace.
#[derive(Clone)]
pub struct ModalAsks {
    /// Puts a view there; what it is given is told (`true`) once the view
    /// is there no longer.
    pub open: Rc<dyn Fn(NodeId, ModalClosed)>,
    /// Takes a view away, if it is the one there.
    pub close: Rc<dyn Fn(NodeId)>,
}

/// What the host does for a plugin beyond the document, as whoever shows
/// the plugin does it.
#[derive(Clone, Default)]
pub struct Asks {
    /// Hands a URL to the desktop.
    pub open_url: Option<OpenUrl>,
    /// The title of the plugin's pane.
    pub set_title: Option<SetTitle>,
    /// The shell's theme.
    pub theme: Option<ThemeAsks>,
    /// The workspace's sessions.
    pub sessions: Option<SessionAsks>,
    /// A view of the plugin's over the workspace.
    pub modal: Option<ModalAsks>,
}

/// What the host does with the shell's theme for a plugin.
#[derive(Clone)]
pub struct ThemeAsks {
    /// The seed of the theme as it is, and the file one is saved to.
    pub now: Rc<dyn Fn() -> (Seed, Option<PathBuf>)>,
    /// Draws everything in the theme a seed makes.
    pub apply: Rc<dyn Fn(&Seed)>,
    /// Writes a seed to the configuration, and says where.
    pub save: SaveSeed,
}

/// Writes a seed to the configuration, and says where.
pub type SaveSeed = Rc<dyn Fn(&Seed) -> Result<PathBuf, String>>;

/// Hands a URL to the desktop.
pub type OpenUrl = Rc<dyn Fn(&str)>;
/// Says the title of a plugin's pane.
pub type SetTitle = Rc<dyn Fn(String)>;

/// What a plugin asks of the host beyond the document.
enum Asked {
    OpenUrl(String),
    SetTitle(String),
    ApplyTheme(Seed),
    /// The answer goes back to the plugin's task that waits for it.
    SaveTheme(Seed, tokio::sync::oneshot::Sender<Result<String, String>>),
    Session(SessionAsk),
    OpenModal(NodeId, ModalClosed),
    CloseModal(NodeId),
}

/// What of the host's beyond the document a plugin may use, as its
/// manifest's `uses` names it: an interface, all of it (`sessions`), or one
/// function or resource of one (`session-control.terminate`,
/// `workspace.cursor`). The rest of the host's -- the document, its
/// events, the pane, the desktop, the files it is lent -- every plugin
/// uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Uses(Vec<String>);

/// The interfaces held to `uses`, and what is in each.
const USED: &[(&str, &[&str])] = &[
    ("theme", &["current", "path", "apply", "save"]),
    ("sessions", &["all", "watch"]),
    (
        "session-control",
        &[
            "show",
            "terminate",
            "terminate-with-derived",
            "open-terminal",
        ],
    ),
    ("modal", &["modal"]),
];

impl Uses {
    /// The names a manifest says, each one of the host's.
    pub fn new(names: Vec<String>) -> Result<Self, String> {
        for name in &names {
            let (interface, item) = match name.split_once('.') {
                Some((interface, item)) => (interface, Some(item)),
                None => (name.as_str(), None),
            };
            let known = (USED.iter())
                .find(|(of, _)| *of == interface)
                .is_some_and(|(_, items)| item.is_none_or(|item| items.contains(&item)));
            if !known {
                return Err(format!("`uses` names {name:?}, which the host has not"));
            }
        }
        Ok(Self(names))
    }

    /// Whether `item` of `interface` is used: the interface is named, or
    /// it is.
    fn allows(&self, interface: &str, item: &str) -> bool {
        (self.0.iter()).any(|name| match name.split_once('.') {
            Some((of, named)) => of == interface && named == item,
            None => name == interface,
        })
    }
}

/// The most memory a plugin may take.
pub const MEMORY_LIMIT: usize = 512 << 20;
/// The longest one poll of a plugin's future may take: a call into it, or
/// what its tasks do when they are woken.
pub const CALL_LIMIT: Duration = Duration::from_secs(5);

/// The DOM events a plugin may listen for, as the names the layer keeps.
const EVENTS: &[&str] = &[
    "click",
    "dblclick",
    "contextmenu",
    "pointerdown",
    "pointermove",
    "pointerup",
    "mousedown",
    "mousemove",
    "mouseup",
    "keydown",
    "keyup",
    "keypress",
    "input",
];

/// What a plugin's store keeps.
pub struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    /// What the plugin leaves for this shell to do.
    left: Arc<Mutex<Left>>,
    /// The nodes the plugin made: the only ones it may name.
    owned: HashSet<NodeId>,
    /// The theme's seed and the file one is saved to, as they were when
    /// the plugin was started, if whoever shows it gives it the theme.
    theme: Option<(Seed, Option<PathBuf>)>,
    /// The workspace's sessions, if whoever shows it gives them.
    sessions: Option<Arc<SessionsFeed>>,
    /// Whether whoever shows it lets it put a view over the workspace.
    modal: bool,
    /// What of the above its manifest says it uses.
    uses: Uses,
}

/// What a plugin leaves for this shell to do once the poll it was left in
/// is over: the store is the future's, and this is what the two share.
#[derive(Default)]
struct Left {
    /// What the plugin asked of the host beyond the document.
    asked: Vec<Asked>,
    /// Where the plugin asked to hear an event.
    listens: Vec<(NodeId, &'static str, bool)>,
    /// Effects of the plugin's wait to be run.
    effects_wait: bool,
    /// Whether the plugin has work for after a layout, if it said.
    layout_work: Option<bool>,
}

const BUSY: &str = "the document cannot be changed from work after a layout or an event";

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl State {
    /// A node the plugin names, if it is one of its own.
    fn own(&self, node: u64) -> wasmtime::Result<NodeId> {
        let node = NodeId::from_u64(node);
        if self.owned.contains(&node) {
            Ok(node)
        } else {
            Err(wasmtime::Error::msg("not a node of this plugin"))
        }
    }

    /// The same, for a change that the document has to be free for.
    fn change(&self, node: u64) -> wasmtime::Result<NodeId> {
        if !host::free() {
            return Err(wasmtime::Error::msg(BUSY));
        }
        self.own(node)
    }

    /// `item` of `interface`, if the plugin's manifest says it uses it.
    fn may(&self, interface: &str, item: &str) -> wasmtime::Result<()> {
        match self.uses.allows(interface, item) {
            true => Ok(()),
            false => Err(wasmtime::Error::msg(format!(
                "{interface}.{item} is not in this plugin's uses"
            ))),
        }
    }

    fn made(&mut self, make: impl FnOnce() -> NodeId) -> wasmtime::Result<u64> {
        if !host::free() {
            return Err(wasmtime::Error::msg(BUSY));
        }
        let node = make();
        self.owned.insert(node);
        Ok(node.as_u64())
    }
}

impl Host for State {
    fn create_element(&mut self, tag: String) -> wasmtime::Result<u64> {
        self.made(|| el(&tag).0)
    }

    fn create_text(&mut self, text: String) -> wasmtime::Result<u64> {
        self.made(|| host::create_text(&text))
    }

    fn create_comment(&mut self, text: String) -> wasmtime::Result<u64> {
        self.made(|| host::create_comment(&text))
    }

    fn set_attribute(&mut self, node: u64, name: String, value: String) -> wasmtime::Result<()> {
        El(self.change(node)?).attr(&name, &value);
        Ok(())
    }

    fn set_style(&mut self, node: u64, property: String, value: String) -> wasmtime::Result<()> {
        host::set_style(self.change(node)?, &property, &value);
        Ok(())
    }

    fn adopt_sheet(&mut self, id: String, css: String) -> wasmtime::Result<()> {
        host::adopt_sheet(&id, &css);
        Ok(())
    }

    fn change_classes(&mut self, node: u64, remove: String, add: String) -> wasmtime::Result<()> {
        host::change_classes(self.change(node)?, &remove, &add);
        Ok(())
    }

    fn set_text(&mut self, node: u64, text: String) -> wasmtime::Result<()> {
        host::set_text(self.change(node)?, &text);
        Ok(())
    }

    fn append(&mut self, parent: u64, child: u64) -> wasmtime::Result<()> {
        host::append(self.change(parent)?, self.own(child)?);
        Ok(())
    }

    fn insert_before(&mut self, marker: u64, nodes: Vec<u64>) -> wasmtime::Result<()> {
        let marker = self.change(marker)?;
        let nodes = (nodes.into_iter())
            .map(|node| self.own(node))
            .collect::<wasmtime::Result<Vec<_>>>()?;
        host::insert_before(marker, &nodes);
        Ok(())
    }

    fn remove(&mut self, node: u64) -> wasmtime::Result<Vec<u64>> {
        let gone = host::remove(self.own(node)?);
        for node in &gone {
            self.owned.remove(node);
        }
        Ok(gone.into_iter().map(|node| node.as_u64()).collect())
    }

    fn detach(&mut self, node: u64) -> wasmtime::Result<()> {
        El(self.own(node)?).detach();
        Ok(())
    }

    fn scroll_to(&mut self, node: u64, x: f64, y: f64) -> wasmtime::Result<()> {
        El(self.own(node)?).scroll_to(x, y);
        Ok(())
    }

    fn scroll_by(&mut self, node: u64, x: f64, y: f64) -> wasmtime::Result<()> {
        El(self.own(node)?).scroll_by(x, y);
        Ok(())
    }

    fn set_value(&mut self, node: u64, text: String) -> wasmtime::Result<()> {
        El(self.own(node)?).set_value(&text);
        Ok(())
    }

    fn focus(&mut self, node: u64, within: bool) -> wasmtime::Result<()> {
        let element = El(self.own(node)?);
        if within {
            element.focus_within();
        } else {
            element.focus();
        }
        Ok(())
    }

    fn listen(&mut self, node: u64, event: String, capture: bool) -> wasmtime::Result<()> {
        let element = El(self.own(node)?);
        let Some(event) = EVENTS.iter().find(|known| **known == event).copied() else {
            // (An event that does not cross: the plugin never hears it.)
            return Ok(());
        };
        (self.left.lock().unwrap().listens).push((element.0, event, capture));
        Ok(())
    }

    fn relayout(&mut self) -> wasmtime::Result<()> {
        relayout();
        Ok(())
    }

    fn effects_wait(&mut self) -> wasmtime::Result<()> {
        self.left.lock().unwrap().effects_wait = true;
        Ok(())
    }

    fn layout_work(&mut self, any: bool) -> wasmtime::Result<()> {
        self.left.lock().unwrap().layout_work = Some(any);
        Ok(())
    }

    fn bounds_of(&mut self, node: u64) -> wasmtime::Result<Option<Bounds>> {
        let element = El(self.own(node)?);
        Ok(host::lent(|page| page.bounds(element))
            .flatten()
            .map(|bounds| Bounds {
                x: bounds.x,
                y: bounds.y,
                width: bounds.width,
                height: bounds.height,
            }))
    }

    fn size_of(&mut self, node: u64) -> wasmtime::Result<Option<(f64, f64)>> {
        let element = El(self.own(node)?);
        Ok(host::lent(|page| page.size(element)).flatten())
    }

    fn under(&mut self, x: f32, y: f32) -> wasmtime::Result<Vec<u64>> {
        let under = host::lent(|page| page.under(x, y)).unwrap_or_default();
        Ok((under.into_iter())
            .filter(|element| self.owned.contains(&element.0))
            .map(|element| element.0.as_u64())
            .collect())
    }

    fn scrolled(&mut self, node: u64) -> wasmtime::Result<Option<(f64, f64)>> {
        let element = El(self.own(node)?);
        Ok(host::lent(|page| page.scrolled(element)).flatten())
    }

    fn lines(&mut self, node: u64) -> wasmtime::Result<Option<u32>> {
        let element = El(self.own(node)?);
        Ok(host::lent(|page| page.lines(element))
            .flatten()
            .map(|lines| lines as u32))
    }

    fn has_focus(&mut self, node: u64) -> wasmtime::Result<bool> {
        let element = El(self.own(node)?);
        Ok(host::lent(|page| page.has_focus(element)).unwrap_or(false))
    }

    fn has_focus_within(&mut self, node: u64) -> wasmtime::Result<bool> {
        let element = El(self.own(node)?);
        Ok(host::lent(|page| page.has_focus_within(element)).unwrap_or(false))
    }

    fn selection(&mut self) -> wasmtime::Result<Vec<self::horizon::ui::document::Selected>> {
        let selection = host::lent(|page| page.selection()).unwrap_or_default();
        // (What is selected in another view's elements is not this
        // plugin's to hear of.)
        Ok((selection.into_iter())
            .filter(|(element, ..)| self.owned.contains(&element.0))
            .map(
                |(element, start, end)| self::horizon::ui::document::Selected {
                    node: element.0.as_u64(),
                    start: start as u32,
                    end: end as u32,
                },
            )
            .collect())
    }

    fn inline_text(&mut self, node: u64) -> wasmtime::Result<Option<String>> {
        let element = El(self.own(node)?);
        Ok(host::lent(|page| page.inline_text(element)).flatten())
    }
}

impl self::horizon::ui::desktop::Host for State {
    fn open_url(&mut self, url: String) -> wasmtime::Result<()> {
        (self.left.lock().unwrap().asked).push(Asked::OpenUrl(url));
        Ok(())
    }
}

impl self::horizon::ui::pane::Host for State {
    fn set_title(&mut self, title: String) -> wasmtime::Result<()> {
        (self.left.lock().unwrap().asked).push(Asked::SetTitle(title));
        Ok(())
    }
}

/// A seed as the boundary carries it.
fn carried(seed: &Seed) -> self::horizon::ui::theme::Seed {
    use self::horizon::ui::theme::Accent;
    self::horizon::ui::theme::Seed {
        surface_base: seed.surface_base,
        hues: seed.hues.to_vec(),
        accent: match seed.accent {
            AccentValue::Slot(slot) => {
                let place = HueSlot::ALL.iter().position(|other| *other == slot);
                Accent::Hue(place.unwrap_or(0) as u8)
            }
            AccentValue::Hex(color) => Accent::Color(color),
        },
        text_contrast: seed.text_contrast,
    }
}

/// The seed a plugin handed over; what is not one is the plugin's fault.
fn seed_of(carried: self::horizon::ui::theme::Seed) -> wasmtime::Result<Seed> {
    use self::horizon::ui::theme::Accent;
    let none = |what: &str| wasmtime::Error::msg(format!("a seed with {what}"));
    let hues: [u32; 6] = (carried.hues.try_into()).map_err(|_| none("other than six hues"))?;
    let accent = match carried.accent {
        Accent::Hue(place) => AccentValue::Slot(
            *(HueSlot::ALL.get(place as usize)).ok_or_else(|| none("an accent that is no hue"))?,
        ),
        Accent::Color(color) => AccentValue::Hex(color),
    };
    Ok(Seed {
        surface_base: carried.surface_base,
        hues,
        accent,
        text_contrast: Seed::clamp_contrast(carried.text_contrast),
    })
}

impl self::horizon::ui::theme::Host for State {
    fn current(&mut self) -> wasmtime::Result<self::horizon::ui::theme::Seed> {
        self.may("theme", "current")?;
        let (seed, _) = (self.theme.as_ref())
            .ok_or_else(|| wasmtime::Error::msg("the theme is not given to this plugin"))?;
        Ok(carried(seed))
    }

    fn path(&mut self) -> wasmtime::Result<Option<String>> {
        self.may("theme", "path")?;
        let path = (self.theme.as_ref()).and_then(|(_, path)| path.as_ref());
        Ok(path.map(|path| path.to_string_lossy().into_owned()))
    }

    fn apply(&mut self, seed: self::horizon::ui::theme::Seed) -> wasmtime::Result<()> {
        self.may("theme", "apply")?;
        let seed = seed_of(seed)?;
        (self.left.lock().unwrap().asked).push(Asked::ApplyTheme(seed));
        Ok(())
    }
}

impl<T: Send> self::horizon::ui::theme::HostWithStore<T> for HasSelf<State> {
    async fn save(
        accessor: &Accessor<T, Self>,
        seed: self::horizon::ui::theme::Seed,
    ) -> wasmtime::Result<Result<String, String>> {
        let seed = seed_of(seed)?;
        let (answer, answered) = tokio::sync::oneshot::channel();
        accessor.with(|mut access| -> wasmtime::Result<()> {
            let state = access.get();
            state.may("theme", "save")?;
            (state.left.lock().unwrap().asked).push(Asked::SaveTheme(seed, answer));
            Ok(())
        })?;
        // (Whoever shows the plugin answers once the poll this was asked
        // in is over.)
        Ok(answered
            .await
            .unwrap_or_else(|_| Err("the theme is not given to this plugin".to_string())))
    }
}

impl State {
    fn sessions(&self) -> wasmtime::Result<&Arc<SessionsFeed>> {
        (self.sessions.as_ref())
            .ok_or_else(|| wasmtime::Error::msg("the sessions are not given to this plugin"))
    }

    fn ask_of_session(&mut self, item: &str, ask: SessionAsk) -> wasmtime::Result<()> {
        self.may("session-control", item)?;
        self.sessions()?;
        (self.left.lock().unwrap().asked).push(Asked::Session(ask));
        Ok(())
    }
}

impl self::horizon::ui::sessions::Host for State {
    fn all(&mut self) -> wasmtime::Result<Vec<Session>> {
        self.may("sessions", "all")?;
        Ok(self.sessions()?.all())
    }
}

impl self::horizon::ui::sessions::HostWatch for State {
    fn new(&mut self) -> wasmtime::Result<Resource<SessionsWatching>> {
        self.may("sessions", "watch")?;
        let changes = self.sessions()?.changes.subscribe();
        Ok(self.table.push(SessionsWatching(changes))?)
    }

    fn drop(&mut self, watch: Resource<SessionsWatching>) -> wasmtime::Result<()> {
        self.table.delete(watch)?;
        Ok(())
    }
}

impl<T: Send> self::horizon::ui::sessions::HostWatchWithStore<T> for HasSelf<State> {
    async fn changed(
        accessor: &Accessor<T, Self>,
        watch: Resource<SessionsWatching>,
    ) -> wasmtime::Result<()> {
        // What was seen stays the resource's: a change between two waits
        // is told at the second.
        let mut changes = accessor.with(|mut access| -> wasmtime::Result<_> {
            Ok(access.get().table.get(&watch)?.0.clone())
        })?;
        // (A feed that has gone says nothing more: the wait goes on.)
        if changes.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
        accessor.with(|mut access| -> wasmtime::Result<_> {
            access.get().table.get_mut(&watch)?.0 = changes;
            Ok(())
        })
    }
}

impl self::horizon::ui::session_control::Host for State {
    fn show(&mut self, id: String) -> wasmtime::Result<()> {
        self.ask_of_session("show", SessionAsk::Show(id))
    }

    fn terminate(&mut self, id: String) -> wasmtime::Result<()> {
        self.ask_of_session("terminate", SessionAsk::Terminate(id))
    }

    fn terminate_with_derived(&mut self, id: String) -> wasmtime::Result<()> {
        self.ask_of_session(
            "terminate-with-derived",
            SessionAsk::TerminateWithDerived(id),
        )
    }

    fn open_terminal(&mut self, id: String) -> wasmtime::Result<()> {
        self.ask_of_session("open-terminal", SessionAsk::OpenTerminal(id))
    }
}

impl self::horizon::ui::modal::Host for State {}

impl self::horizon::ui::modal::HostModal for State {
    fn new(&mut self, node: u64) -> wasmtime::Result<Resource<ModalShown>> {
        self.may("modal", "modal")?;
        if !self.modal {
            return Err(wasmtime::Error::msg(
                "a view over the workspace is not given to this plugin",
            ));
        }
        let node = self.own(node)?;
        let (told, closed) = tokio::sync::watch::channel(false);
        (self.left.lock().unwrap().asked).push(Asked::OpenModal(node, told));
        Ok(self.table.push(ModalShown { node, closed })?)
    }

    fn drop(&mut self, modal: Resource<ModalShown>) -> wasmtime::Result<()> {
        let shown = self.table.delete(modal)?;
        (self.left.lock().unwrap().asked).push(Asked::CloseModal(shown.node));
        Ok(())
    }
}

impl<T: Send> self::horizon::ui::modal::HostModalWithStore<T> for HasSelf<State> {
    async fn closed(
        accessor: &Accessor<T, Self>,
        modal: Resource<ModalShown>,
    ) -> wasmtime::Result<()> {
        let mut closed = accessor.with(|mut access| -> wasmtime::Result<_> {
            Ok(access.get().table.get(&modal)?.closed.clone())
        })?;
        // (Whoever would say so having gone, it is not there either.)
        let _ = closed.wait_for(|closed| *closed).await;
        Ok(())
    }
}

impl self::horizon::ui::files::Host for State {}

impl self::horizon::ui::files::HostWatch for State {
    fn new(&mut self, path: String) -> wasmtime::Result<Resource<Watching>> {
        Ok(self
            .table
            .push(Watching(crate::watch::kept(PathBuf::from(path))))?)
    }

    fn drop(&mut self, watch: Resource<Watching>) -> wasmtime::Result<()> {
        self.table.delete(watch)?;
        Ok(())
    }
}

impl<T: Send> self::horizon::ui::files::HostWatchWithStore<T> for HasSelf<State> {
    async fn changed(
        accessor: &Accessor<T, Self>,
        watch: Resource<Watching>,
    ) -> wasmtime::Result<Result<(), String>> {
        let watch = accessor.with(|mut access| -> wasmtime::Result<_> {
            Ok(access.get().table.get(&watch)?.0.clone())
        })?;
        let Some(watch) = watch else {
            return Ok(Err("the file cannot be watched".to_string()));
        };
        watch.next().await;
        Ok(Ok(()))
    }
}

impl self::horizon::ui::files::HostFile for State {
    fn path(&mut self, file: Resource<File>) -> wasmtime::Result<String> {
        Ok(self.table.get(&file)?.0.to_string_lossy().into_owned())
    }

    fn drop(&mut self, file: Resource<File>) -> wasmtime::Result<()> {
        self.table.delete(file)?;
        Ok(())
    }
}

impl self::horizon::ui::files::HostWorkingDirectory for State {
    fn path(&mut self, directory: Resource<WorkingDirectory>) -> wasmtime::Result<String> {
        Ok(self.table.get(&directory)?.0.to_string_lossy().into_owned())
    }

    fn drop(&mut self, directory: Resource<WorkingDirectory>) -> wasmtime::Result<()> {
        self.table.delete(directory)?;
        Ok(())
    }
}

impl State {
    /// `path`, as a file the plugin may be given.
    pub fn file(&mut self, path: PathBuf) -> wasmtime::Result<Resource<File>> {
        Ok(self.table.push(File(path))?)
    }

    /// `path`, as where a pane was asked for.
    pub fn working_directory(
        &mut self,
        path: PathBuf,
    ) -> wasmtime::Result<Resource<WorkingDirectory>> {
        Ok(self.table.push(WorkingDirectory(path))?)
    }
}

/// What crosses of an event's payload.
fn payload(event: &DomEvent) -> EventData {
    match &event.data {
        DomEventData::KeyDown(key) | DomEventData::KeyUp(key) | DomEventData::KeyPress(key) => {
            let (named, character) = match &key.key {
                Key::Named(named) => (Some(named.to_string()), None),
                Key::Character(character) => (None, Some(character.clone())),
            };
            EventData::Key(KeyEvent {
                named,
                character,
                modifiers: key.modifiers.bits(),
                repeat: key.is_auto_repeating,
                composing: key.is_composing,
                text: key.text.as_ref().map(ToString::to_string),
            })
        }
        DomEventData::Input(input) => EventData::Input(input.value.clone()),
        DomEventData::PointerDown(pointer)
        | DomEventData::PointerMove(pointer)
        | DomEventData::PointerUp(pointer)
        | DomEventData::MouseDown(pointer)
        | DomEventData::MouseMove(pointer)
        | DomEventData::MouseUp(pointer)
        | DomEventData::Click(pointer)
        | DomEventData::DoubleClick(pointer)
        | DomEventData::ContextMenu(pointer) => EventData::Pointer(PointerEvent {
            page_x: pointer.page_x(),
            page_y: pointer.page_y(),
            client_x: pointer.client_x(),
            client_y: pointer.client_y(),
            button: pointer.button as u8,
            buttons: pointer.buttons.bits(),
            modifiers: pointer.mods.bits(),
        }),
        _ => EventData::Other,
    }
}

/// Ends wasm that goes on for longer than [`CALL_LIMIT`]: the engine's
/// epoch is moved on while a poll of a plugin's future outlasts it, and
/// the store, whose deadline is the next epoch, traps. The thread sleeps
/// between polls.
struct Watchdog {
    /// Told `true` as a poll begins and `false` as it ends.
    polls: mpsc::Sender<bool>,
}

impl Watchdog {
    fn over(engine: Engine) -> Self {
        let (polls, told) = mpsc::channel::<bool>();
        std::thread::spawn(move || {
            // Until the plugin is let go of, and the sender with it.
            while let Ok(began) = told.recv() {
                if !began {
                    continue;
                }
                match told.recv_timeout(CALL_LIMIT) {
                    Ok(_) => {}
                    Err(RecvTimeoutError::Timeout) => engine.increment_epoch(),
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        Self { polls }
    }
}

thread_local! {
    /// Whether this thread is polling a plugin's future, or in a call that
    /// will poll it again before it is left.
    static POLLING: Cell<bool> = const { Cell::new(false) };
}

/// What a plugin's future is polled with: something wants it polled.
struct Woken {
    asked: AtomicBool,
    page: Wake,
    /// The UI thread, which may be waiting for a call's answer.
    thread: Thread,
}

impl std::task::Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.asked.store(true, Ordering::Release);
        // The thread that is polling polls again before it leaves, so the
        // page has nothing to be woken for; from anywhere else, the page's
        // effects are what polls.
        if POLLING.with(Cell::get) {
            return;
        }
        self.thread.unpark();
        self.page.wake();
    }
}

/// A call into a plugin, made through the accessor.
pub type Calling<'a, R> = Pin<Box<dyn Future<Output = wasmtime::Result<R>> + 'a>>;

/// A plugin as a call is made into it.
pub struct Plugin {
    pub instance: Instance,
    events: View,
}

impl Plugin {
    /// The plugin's exports every plugin has.
    fn events(&self) -> &exports::horizon::ui::events::Guest {
        self.events.horizon_ui_events()
    }
}

type Job = Box<dyn for<'a> FnOnce(&'a Plugin, &'a Accessor<State>) -> Calling<'a, ()>>;

/// The calls asked for and not yet taken, and who waits for the next.
#[derive(Default)]
struct Jobs {
    asked: VecDeque<Job>,
    waiting: Option<Waker>,
}

/// Comes back with the next call asked for.
struct Next(Rc<RefCell<Jobs>>);

impl Future for Next {
    type Output = Job;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Job> {
        let mut jobs = self.0.borrow_mut();
        match jobs.asked.pop_front() {
            Some(job) => Poll::Ready(job),
            None => {
                jobs.waiting = Some(context.waker().clone());
                Poll::Pending
            }
        }
    }
}

type Driving = Pin<Box<dyn Future<Output = wasmtime::Result<()>>>>;

/// A plugin in use.
pub struct Loaded {
    /// The plugin's whole life, its store with it: gone once it has ended.
    driving: RefCell<Option<Driving>>,
    jobs: Rc<RefCell<Jobs>>,
    left: Arc<Mutex<Left>>,
    asks: Asks,
    watchdog: Watchdog,
    woken: Arc<Woken>,
    /// When the poll under way began, for the store's epoch callback.
    began: Arc<Mutex<Option<Instant>>>,
    /// What the plugin said when it trapped; nothing is asked of it after.
    failed: RwSignal<Option<String>>,
    /// Whether the future is being polled: it is not polled, and the
    /// plugin not called, from inside that.
    polling: Cell<bool>,
    /// Effects of the plugin's wait to be run, at the page's next.
    effects_wait: Cell<bool>,
    /// Whether the plugin has work for after a layout.
    layout_work: Cell<bool>,
    /// The views it has put over the workspace: one still there is taken
    /// away when the plugin goes, and each goes with it.
    over: RefCell<Vec<NodeId>>,
}

impl Drop for Loaded {
    fn drop(&mut self) {
        if !host::there() {
            return;
        }
        for node in self.over.take() {
            if let Some(modal) = &self.asks.modal {
                (modal.close)(node);
            }
            host::remove(node);
        }
    }
}

impl Loaded {
    /// Whether the plugin has trapped, or ended.
    pub fn gone(&self) -> bool {
        // (The signal goes with the pane; a handler may outlast it a moment.)
        self.failed.try_get_untracked().flatten().is_some() || self.driving.borrow().is_none()
    }

    /// Polls the future once, under the limit.
    fn poll(&self) {
        let mut driving = self.driving.borrow_mut();
        let Some(future) = driving.as_mut() else {
            return;
        };
        *self.began.lock().unwrap() = Some(Instant::now());
        let _ = self.watchdog.polls.send(true);
        let waker = Waker::from(self.woken.clone());
        let polled = future.as_mut().poll(&mut Context::from_waker(&waker));
        let _ = self.watchdog.polls.send(false);
        *self.began.lock().unwrap() = None;
        if let Poll::Ready(ended) = polled {
            *driving = None;
            let said = match ended {
                Ok(()) => "the plugin ended".to_string(),
                Err(error) => format!("{error:#}"),
            };
            self.failed.try_set(Some(said));
        }
    }

    /// Polls the future for as long as something asks for it.
    fn settle(&self) {
        while self.driving.borrow().is_some() && self.woken.asked.swap(false, Ordering::AcqRel) {
            self.poll();
        }
    }

    /// Does what the plugin left for this shell to do.
    fn take_left(self: &Rc<Self>) {
        let Left {
            asked,
            listens,
            effects_wait,
            layout_work,
        } = std::mem::take(&mut *self.left.lock().unwrap());
        // Its effects are run when the page's are: the page is woken for
        // them.
        if effects_wait && !self.effects_wait.replace(true) {
            self.woken.page.wake();
        }
        if let Some(any) = layout_work {
            self.layout_work.set(any);
        }
        for asked in asked {
            match asked {
                Asked::OpenUrl(url) => {
                    if let Some(open_url) = &self.asks.open_url {
                        open_url(&url);
                    }
                }
                Asked::SetTitle(title) => {
                    if let Some(set_title) = &self.asks.set_title {
                        set_title(title);
                    }
                }
                Asked::ApplyTheme(seed) => {
                    if let Some(theme) = &self.asks.theme {
                        (theme.apply)(&seed);
                    }
                }
                // (With no theme given, the answer is dropped, which the
                // plugin's task hears.)
                Asked::SaveTheme(seed, answer) => {
                    if let Some(theme) = &self.asks.theme {
                        let saved = (theme.save)(&seed);
                        let _ = answer.send(saved.map(|path| path.to_string_lossy().into_owned()));
                    }
                }
                Asked::Session(ask) => {
                    if let Some(sessions) = &self.asks.sessions {
                        (sessions.act)(ask);
                    }
                }
                Asked::OpenModal(node, closed) => {
                    if let Some(modal) = &self.asks.modal {
                        self.over.borrow_mut().push(node);
                        (modal.open)(node, closed);
                    }
                }
                Asked::CloseModal(node) => {
                    if let Some(modal) = &self.asks.modal {
                        (modal.close)(node);
                    }
                }
            }
        }
        // A handler holds the plugin loosely: the nodes may outlast it.
        for (node, event, capture) in listens {
            let loaded = Rc::downgrade(self);
            let handler = move |happened: &DomEvent| {
                if let Some(loaded) = loaded.upgrade() {
                    loaded.handle(node, event, capture, happened);
                }
            };
            if capture {
                El(node).on_capture(event, handler);
            } else {
                El(node).on(event, handler);
            }
        }
    }

    /// Marks this thread as polling for as long as `work` runs, and does
    /// what the plugin left afterwards; `None` from inside a poll.
    fn polled<R>(self: &Rc<Self>, work: impl FnOnce() -> R) -> Option<R> {
        if self.polling.replace(true) {
            return None;
        }
        let before = POLLING.replace(true);
        let answer = work();
        POLLING.set(before);
        self.polling.set(false);
        self.take_left();
        Some(answer)
    }

    /// Calls into the plugin and waits here for the answer; `None` if the
    /// plugin has trapped, traps, or is being polled.
    pub fn call<R: 'static>(
        self: &Rc<Self>,
        call: impl for<'a> FnOnce(&'a Plugin, &'a Accessor<State>) -> Calling<'a, R> + 'static,
    ) -> Option<R> {
        if self.gone() {
            return None;
        }
        let answer = Rc::new(RefCell::new(None::<R>));
        let told = answer.clone();
        let job: Job = Box::new(move |plugin, accessor| {
            Box::pin(async move {
                *told.borrow_mut() = Some(call(plugin, accessor).await?);
                Ok(())
            })
        });
        self.polled(|| {
            let waiting = {
                let mut jobs = self.jobs.borrow_mut();
                jobs.asked.push_back(job);
                jobs.waiting.take()
            };
            if let Some(waiting) = waiting {
                waiting.wake();
            }
            self.woken.asked.store(true, Ordering::Release);
            let answered = loop {
                if self.driving.borrow().is_none() {
                    break None;
                }
                if self.woken.asked.swap(false, Ordering::AcqRel) {
                    self.poll();
                } else {
                    // The answer waits on something outside this thread
                    // (a file read), which unparks it.
                    POLLING.set(false);
                    std::thread::park_timeout(Duration::from_millis(50));
                    POLLING.set(true);
                }
                if let Some(answer) = answer.borrow_mut().take() {
                    break Some(answer);
                }
            };
            self.settle();
            answered
        })
        .flatten()
    }

    /// Polls the future if something asked for it: the plugin's tasks go
    /// on.
    fn drive(self: &Rc<Self>) {
        if self.woken.asked.load(Ordering::Acquire) && !self.gone() {
            self.polled(|| self.settle());
        }
    }

    fn handle(self: &Rc<Self>, node: NodeId, event: &str, capture: bool, happened: &DomEvent) {
        let data = payload(happened);
        let target = happened.target.as_u64();
        let event = event.to_string();
        let handled = self.call(move |plugin, accessor| {
            Box::pin(async move {
                (plugin.events())
                    .call_handle(accessor, node.as_u64(), target, event, capture, data)
                    .await
            })
        });
        let Some(handled) = handled else {
            return;
        };
        if handled.prevent_default {
            prevent_default();
        }
        if handled.stop_propagation {
            stop_propagation();
        }
        if handled.asks_page {
            let loaded = self.clone();
            dispatched(move |page| {
                host::lend(page, || loaded.after_dispatch());
            });
        }
    }
}

impl Loaded {
    /// What follows an event: the work that asked the page.
    pub fn after_dispatch(self: &Rc<Self>) -> Option<()> {
        self.call(|plugin, accessor| Box::pin(plugin.events().call_after_dispatch(accessor)))
    }

    /// The work that follows a layout.
    pub fn after_layout(self: &Rc<Self>) -> Option<()> {
        self.call(|plugin, accessor| Box::pin(plugin.events().call_after_layout(accessor)))
    }

    /// Runs the plugin's effects.
    pub fn run_effects(self: &Rc<Self>) -> Option<()> {
        self.call(|plugin, accessor| Box::pin(plugin.events().call_run_effects(accessor)))
    }

    /// Has the plugin follow this page: its work after each layout, and
    /// its tasks and its effects when the page's effects run.
    pub fn follow(self: &Rc<Self>) {
        let (laid_out, effects) = (self.clone(), self.clone());
        horizon_ui::each_layout(move |page| {
            if laid_out.layout_work.get() {
                host::lend(page, || laid_out.after_layout());
            }
        });
        // (Its effects are run when it said some wait, and while running
        // them leaves more waiting.)
        horizon_ui::each_effects(move || {
            effects.drive();
            while effects.effects_wait.replace(false) && !effects.gone() {
                effects.run_effects();
            }
        });
    }
}

/// A plugin's file compiled: what takes a while, and can be done off the
/// UI thread.
#[derive(Clone)]
pub struct Compiled {
    pub engine: Engine,
    pub component: Component,
}

/// Compiles the plugin at `path`.
pub fn compile(path: &Path) -> Result<Compiled, String> {
    let said = |error: wasmtime::Error| format!("{error:#}");
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.wasm_component_model_async(true);
    config.epoch_interruption(true);
    config.cache(compiled_cache());
    let engine = Engine::new(&config).map_err(said)?;
    let component = Component::from_file(&engine, path).map_err(said)?;
    Ok(Compiled { engine, component })
}

/// Where a plugin's file compiled is kept, so that the same file is not
/// compiled again by the next process that reads it:
/// `$XDG_CACHE_HOME/horizon/wasm`, else `~/.cache/horizon/wasm`. One per
/// process, as each starts a thread of its own; none if there is no
/// such directory to make.
fn compiled_cache() -> Option<wasmtime::Cache> {
    static CACHE: std::sync::OnceLock<Option<wasmtime::Cache>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let named = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
            let home = match named("XDG_CACHE_HOME") {
                Some(value) => PathBuf::from(value),
                None => PathBuf::from(named("HOME")?).join(".cache"),
            };
            let mut config = wasmtime::CacheConfig::new();
            config.with_directory(home.join("horizon").join("wasm"));
            wasmtime::Cache::new(config).ok()
        })
        .clone()
}

/// What the host offers a plugin: WASI, and the interfaces of
/// `horizon:ui`.
fn linker(engine: &Engine) -> wasmtime::Result<Linker<State>> {
    let mut linker = Linker::<State>::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    View::add_to_linker::<_, HasSelf<State>>(&mut linker, |state| state)?;
    Ok(linker)
}

/// Holds what `compiled` imports to what the host offers: an import the
/// host does not have, or has with another type, is said. A component
/// that asks for less than is offered is taken.
pub fn offered(compiled: &Compiled) -> Result<(), String> {
    let said = |error: wasmtime::Error| format!("{error:#}");
    let linker = linker(&compiled.engine).map_err(said)?;
    (linker.instantiate_pre(&compiled.component))
        .map(drop)
        .map_err(said)
}

/// The plugin `compiled` in a store of its own, given `wasi` and `asks`,
/// its tasks started; `wake` wakes the page when its future is to be
/// polled, and `failed` is told what it said if it traps.
pub fn instantiate(
    compiled: &Compiled,
    wasi: WasiCtx,
    uses: Uses,
    asks: Asks,
    wake: Wake,
    failed: RwSignal<Option<String>>,
) -> wasmtime::Result<Rc<Loaded>> {
    let linker = linker(&compiled.engine)?;
    let left = Arc::new(Mutex::new(Left::default()));
    let mut store = Store::new(
        &compiled.engine,
        State {
            wasi,
            table: ResourceTable::new(),
            left: left.clone(),
            limits: StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).build(),
            owned: HashSet::new(),
            theme: asks.theme.as_ref().map(|theme| (theme.now)()),
            sessions: asks.sessions.as_ref().map(|sessions| sessions.feed.clone()),
            modal: asks.modal.is_some(),
            uses,
        },
    );
    store.limiter(|state| &mut state.limits);
    // Another store's poll may move the shared engine's epoch on: this
    // store's wasm is ended only once its own poll has gone on too long.
    let began = Arc::new(Mutex::new(None::<Instant>));
    let polled = began.clone();
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(move |_| match *polled.lock().unwrap() {
        Some(began) if began.elapsed() >= CALL_LIMIT => {
            Err(wasmtime::Error::msg("the plugin took too long"))
        }
        _ => Ok(UpdateDeadline::Continue(1)),
    });
    let jobs = Rc::new(RefCell::new(Jobs::default()));
    let taken = jobs.clone();
    let component = compiled.component.clone();
    // The store is the future's own: it is borrowed for as long as the
    // plugin lives, by the one thing that drives it.
    let driving: Driving = Box::pin(async move {
        let mut store = store;
        let instance = linker.instantiate_async(&mut store, &component).await?;
        let events = View::new(&mut store, &instance)?;
        let plugin = Plugin { instance, events };
        store
            .run_concurrent(async |accessor| -> wasmtime::Result<()> {
                // What runs the plugin's own tasks: it comes back, they
                // go on.
                plugin.events().call_tasks(accessor).await?;
                loop {
                    let job = Next(taken.clone()).await;
                    job(&plugin, accessor).await?;
                }
            })
            .await?
    });
    let loaded = Rc::new(Loaded {
        driving: RefCell::new(Some(driving)),
        jobs,
        left,
        asks,
        watchdog: Watchdog::over(compiled.engine.clone()),
        woken: Arc::new(Woken {
            asked: AtomicBool::new(false),
            page: wake,
            thread: std::thread::current(),
        }),
        began,
        failed,
        polling: Cell::new(false),
        effects_wait: Cell::new(false),
        layout_work: Cell::new(false),
        over: RefCell::default(),
    });
    // The plugin is made by the future's first polls: a call that asks
    // nothing of it is answered once it is there.
    match loaded.call(|_, _| Box::pin(async { Ok(()) })) {
        Some(()) => Ok(loaded),
        None => {
            let said = failed.try_get_untracked().flatten();
            Err(wasmtime::Error::msg(said.unwrap_or_else(|| {
                "the plugin could not be made".to_string()
            })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_names_an_interface_whole_or_one_thing_of_it() {
        let uses = Uses::new(vec![
            "sessions".to_string(),
            "session-control.show".to_string(),
        ])
        .unwrap();
        assert!(uses.allows("sessions", "all"));
        assert!(uses.allows("sessions", "watch"));
        assert!(uses.allows("session-control", "show"));
        assert!(!uses.allows("session-control", "terminate"));
        assert!(!uses.allows("theme", "current"));
        assert!(!Uses::default().allows("modal", "modal"));
        // What the host has not is refused, an interface or a thing of one.
        for name in ["clipboard", "sessions.end", "session-control.show.now"] {
            assert!(Uses::new(vec![name.to_string()]).is_err(), "{name}");
        }
    }

    #[test]
    fn a_feed_tells_those_who_wait_only_of_what_changed() {
        let named = |title: &str| Session {
            id: "s-1".to_string(),
            kind: "terminal".to_string(),
            title: title.to_string(),
            attached: true,
            directory: None,
            project: None,
            parent: None,
            activity: None,
        };
        let feed = SessionsFeed::default();
        let mut changes = feed.changes.subscribe();
        assert!(!changes.has_changed().unwrap());

        feed.set(vec![named("notes")]);
        assert!(changes.has_changed().unwrap());
        changes.mark_unchanged();
        assert_eq!(feed.all(), [named("notes")]);

        // The same said again is no change.
        feed.set(vec![named("notes")]);
        assert!(!changes.has_changed().unwrap());
        feed.set(vec![named("letters")]);
        assert!(changes.has_changed().unwrap());
    }

    /// What the host says of a component written as text.
    fn offered_to(name: &str, component: &str) -> Result<(), String> {
        let file = std::env::temp_dir().join(format!(
            "horizon-offered-{name}-{}.wasm",
            std::process::id()
        ));
        std::fs::write(&file, wat::parse_str(component).unwrap()).unwrap();
        let compiled = compile(&file);
        let _ = std::fs::remove_file(&file);
        offered(&compiled.unwrap_or_else(|error| panic!("{error}")))
    }

    #[test]
    fn what_a_component_imports_is_held_to_what_the_host_offers() {
        // Less than is offered: one function of one interface.
        let fewer = r#"(component
            (import "horizon:ui/desktop" (instance
                (export "open-url" (func (param "url" string))))))"#;
        assert_eq!(offered_to("fewer", fewer), Ok(()));

        // A function the host has with another type.
        let retyped = r#"(component
            (import "horizon:ui/desktop" (instance
                (export "open-url" (func (param "url" u32))))))"#;
        let said = offered_to("retyped", retyped).unwrap_err();
        assert!(said.contains("`open-url` has the wrong type"), "{said}");
        assert!(said.contains("expected `string` found `u32`"), "{said}");

        // A function the host does not have, and an interface it does not.
        let missing = r#"(component
            (import "horizon:ui/desktop" (instance
                (export "open-elsewhere" (func (param "url" string))))))"#;
        let said = offered_to("missing", missing).unwrap_err();
        assert!(said.contains("open-elsewhere"), "{said}");
        let unknown = r#"(component
            (import "horizon:ui/no-such" (instance
                (export "anything" (func)))))"#;
        let said = offered_to("unknown", unknown).unwrap_err();
        assert!(said.contains("horizon:ui/no-such"), "{said}");
    }
}
