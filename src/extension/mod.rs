//! A view drawn by an extension: a WASI component (`extensions/`) that
//! describes its elements through `gpui-wit-plugin` and calls Horizon
//! through `extensions/wit/host.wit`.
//!
//! [`ExtensionView`] is what a pane holds. It gives the plugin its file
//! access and its host data, and carries out what the plugin asked of the
//! application once each call into it has returned.

mod components;
mod host;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::*;
use gpui_wit_plugin::wasmtime::component::Component;
use gpui_wit_plugin::wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};
use gpui_wit_plugin::{PluginView, Returned, Runtime};
use serde_json::{json, Value};

use self::host::{HostData, Request};
use crate::file_watch::{self, FileWatch};
use crate::markdown::DocumentChanged;

/// The runtime every extension view shares, and the components loaded so
/// far, by the file each came from.
struct Extensions {
    runtime: Rc<Runtime>,
    components: HashMap<PathBuf, Component>,
}

impl Global for Extensions {}

fn extensions(cx: &mut App) -> Result<&mut Extensions, String> {
    if !cx.has_global::<Extensions>() {
        let mut runtime = Runtime::new().map_err(|error| format!("{error:#}"))?;
        host::add_to_linker(runtime.linker()).map_err(|error| format!("{error:#}"))?;
        components::register(&mut runtime);
        cx.set_global(Extensions {
            runtime: Rc::new(runtime),
            components: HashMap::new(),
        });
    }
    Ok(cx.global_mut::<Extensions>())
}

pub(crate) struct ExtensionView {
    plugin: Entity<PluginView>,
    /// Off only in tests: a real watch runs on a thread of its own, which
    /// gpui's deterministic test scheduler rejects.
    watch_files: bool,
    _watch: Option<FileWatch>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DocumentChanged> for ExtensionView {}

impl ExtensionView {
    /// Loads the extension at `wasm` and opens it on `document`.
    pub(crate) fn new(
        wasm: &Path,
        document: Option<PathBuf>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<Self>, String> {
        Self::build(wasm, document, true, window, cx)
    }

    fn build(
        wasm: &Path,
        document: Option<PathBuf>,
        watch_files: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<Self>, String> {
        let extensions = extensions(cx)?;
        let runtime = extensions.runtime.clone();
        let component = match extensions.components.get(wasm) {
            Some(component) => component.clone(),
            None => {
                let component = (runtime.load(wasm))
                    .map_err(|error| format!("{}: {error:#}", wasm.display()))?;
                (extensions.components).insert(wasm.to_path_buf(), component.clone());
                component
            }
        };
        // An extension reads any file the user can, and writes none.
        let mut wasi = WasiCtxBuilder::new();
        wasi.inherit_stdio()
            .preopened_dir("/", "/", DirPerms::READ, FilePerms::READ)
            .map_err(|error| format!("{error:#}"))?;
        let data = HostData {
            document,
            requests: Vec::new(),
        };
        let instance = runtime
            .instantiate(&component, wasi.build(), Box::new(data))
            .map_err(|error| format!("{}: {error:#}", wasm.display()))?;
        let plugin = cx.new(|cx| PluginView::new(&runtime, instance, cx));
        window.focus(&plugin.focus_handle(cx), cx);
        Ok(cx.new(|cx| {
            let returned = cx.subscribe(&plugin, |view: &mut Self, _, _: &Returned, cx| {
                view.carry_out(cx);
            });
            let [theme, font_size] = crate::preview::observe_look(cx, |view: &mut Self, cx| {
                view.tell("look-changed", Value::Null, cx);
            });
            let mut view = Self {
                plugin,
                watch_files,
                _watch: None,
                _subscriptions: vec![returned, theme, font_size],
            };
            // The plugin's first call returned before anything subscribed.
            view.carry_out(cx);
            view
        }))
    }

    /// Points the view at `path` from outside (the CLI).
    pub(crate) fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.tell("open", json!(path.to_string_lossy()), cx);
    }

    /// Tells the plugin's view something, as `extensions/wit/host.wit`
    /// lists.
    fn tell(&self, name: &'static str, payload: Value, cx: &mut App) {
        tell(&self.plugin, name, payload, cx);
    }

    /// Does what the plugin's calls asked of the application.
    fn carry_out(&mut self, cx: &mut Context<Self>) {
        let requests = self.plugin.update(cx, |plugin, _| {
            let data = plugin.state().data.downcast_mut::<HostData>();
            data.map(|data| std::mem::take(&mut data.requests))
        });
        for request in requests.unwrap_or_default() {
            match request {
                Request::SetDocument(path) => cx.emit(DocumentChanged(path)),
                Request::OpenUrl(url) => cx.open_url(&url),
                Request::Watch(path) if self.watch_files => {
                    let plugin = self.plugin.downgrade();
                    self._watch = file_watch::watch(path, cx, move |cx| {
                        if let Some(plugin) = plugin.upgrade() {
                            tell(&plugin, "file-changed", Value::Null, cx);
                        }
                    });
                }
                Request::Watch(_) => {}
            }
        }
    }
}

fn tell(plugin: &Entity<PluginView>, name: &'static str, payload: Value, cx: &mut App) {
    plugin.update(cx, |plugin, cx| plugin.dispatch(0, name, &payload, cx));
}

impl Focusable for ExtensionView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.plugin.focus_handle(cx)
    }
}

impl Render for ExtensionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(crate::keystroke_input::keystroke_only(
                &self.plugin.focus_handle(cx),
            ))
            .child(self.plugin.clone())
    }
}

#[cfg(test)]
mod tests;
