//! A preview: a view of this shell that is still being written, built
//! into a wasm plugin (`../preview-plugin`) and shown from it.
//!
//! The plugin has the view's signals, effects and handlers; the document
//! is this shell's. What the plugin's `El` and `Page` do arrives here as
//! calls (`horizon-ui`'s `plugin.wit`) and is done in this document, on
//! nodes the plugin made and no others. An event that reaches one of
//! those nodes is told to the plugin, and the plugin's effects run when
//! this shell's do.
//!
//! A plugin that traps is let go of, and the pane says what it said.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;

use horizon_ui::blitz_dom::NodeId;
use horizon_ui::blitz_traits::events::{DomEvent, DomEventData};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set};
use horizon_ui::{
    El, dispatched, each_effects, each_layout, el, host, prevent_default, relayout,
    stop_propagation,
};
use keyboard_types::Key;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../ui/wit",
    world: "plugin",
    imports: { default: trappable },
});

use self::horizon::ui::document::{Bounds, Host};

/// The most memory a plugin may take.
const MEMORY_LIMIT: usize = 512 << 20;

pub const CSS: &str = "
    .preview { height: 100%; min-height: 0; display: flex; flex-direction: column; }
    .preview > .failed { padding: var(--u16); color: var(--danger);
                         font-size: var(--meta-size); line-height: var(--meta-line);
                         white-space: pre-wrap; }
";

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

struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
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

/// A plugin in use.
struct Loaded {
    store: RefCell<Store<State>>,
    plugin: Plugin,
    /// What the plugin said when it trapped; nothing is asked of it after.
    failed: RwSignal<Option<String>>,
    /// Whether a call into the plugin is under way: it is not called
    /// again from inside one.
    calling: Cell<bool>,
}

impl Loaded {
    /// Calls into the plugin, unless it has trapped or is being called.
    fn call<R>(
        self: &Rc<Self>,
        call: impl FnOnce(&Plugin, &mut Store<State>) -> wasmtime::Result<R>,
    ) -> Option<R> {
        // (The signal goes with the pane; a handler may outlast it a moment.)
        let failed = self.failed.try_get_untracked().flatten().is_some();
        if failed || self.calling.replace(true) {
            return None;
        }
        let (answer, listens) = {
            let mut store = self.store.borrow_mut();
            let answer = call(&self.plugin, &mut store);
            (answer, std::mem::take(&mut store.data_mut().listens))
        };
        self.calling.set(false);
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

    fn handle(self: &Rc<Self>, node: NodeId, event: &str, capture: bool, happened: &DomEvent) {
        let data = payload(happened);
        let target = happened.target.as_u64();
        let handled = self.call(|plugin, store| {
            plugin.call_handle(store, node.as_u64(), target, event, capture, &data)
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
                host::lend(page, || {
                    loaded.call(|plugin, store| plugin.call_after_dispatch(store))
                });
            });
        }
    }
}

fn load(path: &Path, failed: RwSignal<Option<String>>) -> wasmtime::Result<Rc<Loaded>> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_file(&engine, path)?;
    let mut linker = Linker::<State>::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    Plugin::add_to_linker::<_, HasSelf<State>>(&mut linker, |state| state)?;
    let mut store = Store::new(
        &engine,
        State {
            // No grants: no files, no sockets, no environment.
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).build(),
            owned: HashSet::new(),
            listens: Vec::new(),
        },
    );
    store.limiter(|state| &mut state.limits);
    let plugin = Plugin::instantiate(&mut store, &component, &linker)?;
    Ok(Rc::new(Loaded {
        store: RefCell::new(store),
        plugin,
        failed,
        calling: Cell::new(false),
    }))
}

/// Builds the preview `name` of the plugin at `path`.
pub fn view(path: &Path, name: &str) -> El {
    let failed = RwSignal::new(None::<String>);
    let root = el("div").class("preview").when(
        move || failed.get().is_some(),
        move || {
            el("div")
                .class("failed")
                .text_dyn(move || failed.get().unwrap_or_default())
        },
    );
    let loaded = match load(path, failed) {
        Ok(loaded) => loaded,
        Err(error) => {
            failed.set(Some(format!("{error:#}")));
            return root;
        }
    };
    // The plugin's style sheet, with the pane: its rules are its views'.
    if let Some(css) = loaded.call(|plugin, store| plugin.call_css(store)) {
        root.child(el("style").text(&css));
    }
    match loaded.call(|plugin, store| plugin.call_mount(store, name)) {
        Some(Some(mounted)) => {
            root.child(El(NodeId::from_u64(mounted)));
        }
        Some(None) => failed.set(Some(format!("the plugin has no preview named `{name}`"))),
        None => {}
    }
    let (laid_out, effects) = (loaded.clone(), loaded);
    each_layout(move |page| {
        host::lend(page, || {
            laid_out.call(|plugin, store| plugin.call_after_layout(store))
        });
    });
    each_effects(move || {
        effects.call(|plugin, store| plugin.call_run_effects(store));
    });
    root
}
