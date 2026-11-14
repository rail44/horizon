//! What runs a plugin built as a wasm component, for whatever shows one:
//! a preview (`preview.rs`) and an extension. The plugin has the view's
//! signals, effects and handlers; the document is this shell's. What the
//! plugin's `El` and `Page` do arrives here as calls (`horizon-ui`'s
//! `plugin.wit`) and is done in this document, on nodes the plugin made
//! and no others. An event that reaches one of those nodes is told to the
//! plugin.
//!
//! A plugin that traps is let go of. It traps when it takes more memory
//! than [`MEMORY_LIMIT`], or a call into it goes on for longer than
//! [`CALL_LIMIT`].

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use horizon_ui::blitz_dom::NodeId;
use horizon_ui::blitz_traits::events::{DomEvent, DomEventData};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set};
use horizon_ui::{El, dispatched, el, host, prevent_default, relayout, stop_propagation};
use keyboard_types::Key;
use wasmtime::component::{Component, HasSelf, Instance, Linker, Resource, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

/// No grants: no files, no sockets, no environment.
pub fn no_grants() -> WasiCtx {
    wasmtime_wasi::WasiCtxBuilder::new().build()
}

wasmtime::component::bindgen!({
    path: "crates/horizon-ui/wit",
    world: "view",
    imports: { default: trappable },
    with: { "horizon:ui/host.file": File },
});

use self::exports::horizon::ui::events::{EventData, KeyEvent, PointerEvent};
use self::horizon::ui::document::{Bounds, Host};

/// A file the person chose, or the command line named, as a plugin holds
/// it.
pub struct File(pub PathBuf);

/// What the host does for a plugin beyond the document, as whoever shows
/// the plugin does it.
#[derive(Clone, Default)]
pub struct Asks {
    /// Hands a URL to the desktop.
    pub open_url: Option<OpenUrl>,
    /// The title of the plugin's pane.
    pub set_title: Option<SetTitle>,
}

/// Hands a URL to the desktop.
pub type OpenUrl = Rc<dyn Fn(&str)>;
/// Says the title of a plugin's pane.
pub type SetTitle = Rc<dyn Fn(String)>;

/// What a plugin asks of the host beyond the document.
enum Asked {
    OpenUrl(String),
    SetTitle(String),
}

/// The most memory a plugin may take.
pub const MEMORY_LIMIT: usize = 512 << 20;
/// The longest one call into a plugin may take.
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
    /// What the plugin asked of the host since it was last called: done
    /// once the call is back, on the UI thread.
    asked: Vec<Asked>,
    /// The nodes the plugin made: the only ones it may name.
    owned: HashSet<NodeId>,
    /// Where the plugin asked to hear an event since it was last called:
    /// the handlers are made once the call is back.
    listens: Vec<(NodeId, &'static str, bool)>,
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
        self.listens.push((element.0, event, capture));
        Ok(())
    }

    fn relayout(&mut self) -> wasmtime::Result<()> {
        relayout();
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
}

impl self::horizon::ui::host::Host for State {
    fn open_url(&mut self, url: String) -> wasmtime::Result<()> {
        self.asked.push(Asked::OpenUrl(url));
        Ok(())
    }

    fn set_title(&mut self, title: String) -> wasmtime::Result<()> {
        self.asked.push(Asked::SetTitle(title));
        Ok(())
    }
}

impl self::horizon::ui::host::HostFile for State {
    fn path(&mut self, file: Resource<File>) -> wasmtime::Result<String> {
        Ok(self.table.get(&file)?.0.to_string_lossy().into_owned())
    }

    fn drop(&mut self, file: Resource<File>) -> wasmtime::Result<()> {
        self.table.delete(file)?;
        Ok(())
    }
}

impl State {
    /// `path`, as a file the plugin may be given.
    pub fn file(&mut self, path: PathBuf) -> wasmtime::Result<Resource<File>> {
        Ok(self.table.push(File(path))?)
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

/// Ends a call into a plugin that goes on for longer than
/// [`CALL_LIMIT`]: the engine's epoch is moved on, and the call, made with
/// that epoch as its deadline, traps. The thread sleeps between calls.
struct Watchdog {
    /// Told `true` as a call begins and `false` as it ends.
    calls: mpsc::Sender<bool>,
}

impl Watchdog {
    fn over(engine: Engine) -> Self {
        let (calls, told) = mpsc::channel::<bool>();
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
        Self { calls }
    }

    /// Runs `call`, a call into the plugin of `store`, under the limit.
    fn timed<R>(&self, store: &mut Store<State>, call: impl FnOnce(&mut Store<State>) -> R) -> R {
        store.set_epoch_deadline(1);
        let _ = self.calls.send(true);
        let answer = call(store);
        let _ = self.calls.send(false);
        answer
    }
}

/// A plugin in use.
pub struct Loaded {
    store: RefCell<Store<State>>,
    instance: Instance,
    events: View,
    asks: Asks,
    watchdog: Watchdog,
    /// What the plugin said when it trapped; nothing is asked of it after.
    failed: RwSignal<Option<String>>,
    /// Whether a call into the plugin is under way: it is not called
    /// again from inside one.
    calling: Cell<bool>,
}

impl Loaded {
    /// Calls into the plugin, unless it has trapped or is being called.
    pub fn call<R>(
        self: &Rc<Self>,
        call: impl FnOnce(&Instance, &mut Store<State>) -> wasmtime::Result<R>,
    ) -> Option<R> {
        // (The signal goes with the pane; a handler may outlast it a moment.)
        let failed = self.failed.try_get_untracked().flatten().is_some();
        if failed || self.calling.replace(true) {
            return None;
        }
        let (answer, listens, asked) = {
            let mut store = self.store.borrow_mut();
            let answer = (self.watchdog).timed(&mut store, |store| call(&self.instance, store));
            let state = store.data_mut();
            (
                answer,
                std::mem::take(&mut state.listens),
                std::mem::take(&mut state.asked),
            )
        };
        self.calling.set(false);
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
        match answer {
            Ok(answer) => Some(answer),
            Err(error) => {
                self.failed.try_set(Some(format!("{error:#}")));
                None
            }
        }
    }

    /// The plugin's exports every plugin has.
    fn events(&self) -> &exports::horizon::ui::events::Guest {
        self.events.horizon_ui_events()
    }

    fn handle(self: &Rc<Self>, node: NodeId, event: &str, capture: bool, happened: &DomEvent) {
        let data = payload(happened);
        let target = happened.target.as_u64();
        let loaded = self.clone();
        let handled = self.call(|_, store| {
            (loaded.events()).call_handle(store, node.as_u64(), target, event, capture, &data)
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
        let loaded = self.clone();
        self.call(|_, store| loaded.events().call_after_dispatch(store))
    }

    /// The work that follows a layout.
    pub fn after_layout(self: &Rc<Self>) -> Option<()> {
        let loaded = self.clone();
        self.call(|_, store| loaded.events().call_after_layout(store))
    }

    /// Runs the plugin's effects.
    pub fn run_effects(self: &Rc<Self>) -> Option<()> {
        let loaded = self.clone();
        self.call(|_, store| loaded.events().call_run_effects(store))
    }

    /// Has the plugin follow this page: its work after each layout, its
    /// effects when the page's run.
    pub fn follow(self: &Rc<Self>) {
        let (laid_out, effects) = (self.clone(), self.clone());
        horizon_ui::each_layout(move |page| {
            host::lend(page, || laid_out.after_layout());
        });
        horizon_ui::each_effects(move || {
            effects.run_effects();
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
    config.epoch_interruption(true);
    let engine = Engine::new(&config).map_err(said)?;
    let component = Component::from_file(&engine, path).map_err(said)?;
    Ok(Compiled { engine, component })
}

/// The plugin `compiled` in a store of its own, given `wasi` and `asks`;
/// `failed` is told what it said if it traps.
pub fn instantiate(
    compiled: &Compiled,
    wasi: WasiCtx,
    asks: Asks,
    failed: RwSignal<Option<String>>,
) -> wasmtime::Result<Rc<Loaded>> {
    let mut linker = Linker::<State>::new(&compiled.engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    View::add_to_linker::<_, HasSelf<State>>(&mut linker, |state| state)?;
    let mut store = Store::new(
        &compiled.engine,
        State {
            wasi,
            table: ResourceTable::new(),
            asked: Vec::new(),
            limits: StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).build(),
            owned: HashSet::new(),
            listens: Vec::new(),
        },
    );
    store.limiter(|state| &mut state.limits);
    let watchdog = Watchdog::over(compiled.engine.clone());
    let instance = watchdog.timed(&mut store, |store| {
        linker.instantiate(store, &compiled.component)
    })?;
    let events = View::new(&mut store, &instance)?;
    Ok(Rc::new(Loaded {
        store: RefCell::new(store),
        instance,
        events,
        asks,
        watchdog,
        failed,
        calling: Cell::new(false),
    }))
}
