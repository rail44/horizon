//! The preview pane view: an `embedded_gpui::Surface` the loaded plugin
//! draws on, a status line, and a watch that reloads the plugin when its
//! artifact changes.
//!
//! A load failure is a pane state, never a shell failure: the error text
//! replaces the status line and the watch keeps running, so fixing the
//! build and rebuilding recovers without touching the pane.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use embedded_gpui::surface::{KeyEvent, ViewApiCaller as _};
use embedded_gpui::{PluginHost, PluginHostHandle as _, PluginOptions, Surface};
use gpui::{
    div, px, rgb, App, AppContext as _, Context, Entity, FocusHandle, Focusable, Global,
    InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent, ParentElement as _,
    PlatformInput, PlatformTextSystem, Render, Styled as _, Task, Window,
};
use gpui_component::v_flex;
use horizon_workspace::PaneId;

use crate::preview::host::{PreviewHostRoot, PreviewThemeSource};
use crate::preview::schema::{PreviewPlugin, PreviewPluginCaller as _};
use crate::preview::watch::{self, ArtifactWatch};
use crate::theme;

/// The most wall-clock time one guest turn may take before wasmtime traps
/// it. The first turn runs the guest's whole app init (its GPUI `App`,
/// gpui-component's setup, the first layout), which is far more than a
/// steady-state frame, so the budget is a watchdog rather than a latency
/// target.
const TURN_BUDGET: Duration = Duration::from_secs(5);

/// The most linear memory a preview guest may grow to.
const MEMORY_LIMIT: usize = 512 << 20;

/// The host's own glyph rasterizer, published at startup (`entry.rs`) so a
/// preview pane can hand it to each plugin: text a guest paints is then
/// shaped by the same text system as native text. gpui exposes the platform
/// text system only on the `Platform` object, which the shell has just once,
/// before the application runs.
pub(crate) struct PreviewTextSystem(pub(crate) Arc<dyn PlatformTextSystem>);

impl Global for PreviewTextSystem {}

/// What one preview pane shows. The shell keys these by pane id rather than
/// putting them in the workspace model: `ViewKind` stays `Copy` and the
/// persisted schema stays a bare tag.
#[derive(Clone)]
pub(crate) struct PreviewTarget {
    pub(crate) path: PathBuf,
    pub(crate) preview_name: String,
}

/// Which pane already shows `path`. `horizon preview` on a path that has a
/// pane reloads it instead of opening a duplicate; a pane with no artifact
/// yet (a restored one) has no entry here, so the command gives it one only
/// by being pointed at it explicitly.
pub(crate) fn preview_pane_for_path(
    targets: &HashMap<PaneId, PreviewTarget>,
    path: &Path,
) -> Option<PaneId> {
    targets
        .iter()
        .find(|(_, target)| target.path == path)
        .map(|(pane_id, _)| *pane_id)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// No artifact: a restored pane, until `horizon preview` points it at
    /// one.
    Empty,
    Loading,
    Loaded {
        previews: Vec<String>,
    },
    Failed(String),
}

/// The pane's one line of text. Separate from the view so the states a
/// human reads — including "this plugin has no preview by that name" — are
/// testable without a window.
pub(crate) fn status_line(status: &Status, path: Option<&Path>, preview_name: &str) -> String {
    let artifact = || {
        path.map(|path| path.display().to_string())
            .unwrap_or_else(|| "no artifact".to_string())
    };
    match status {
        Status::Empty => "no plugin loaded".to_string(),
        Status::Loading => format!("loading {}", artifact()),
        Status::Loaded { previews }
            if !previews.is_empty() && !previews.iter().any(|name| name == preview_name) =>
        {
            format!(
                "no preview named \"{preview_name}\" in this plugin (has: {})",
                previews.join(", ")
            )
        }
        Status::Loaded { .. } => format!("{preview_name} — {}", artifact()),
        Status::Failed(error) => format!("load failed: {error}"),
    }
}

pub(crate) struct PreviewPane {
    path: Option<PathBuf>,
    preview_name: String,
    status: Status,
    surface: Entity<Surface>,
    theme_source: Entity<PreviewThemeSource>,
    /// Dropped on reload; dropping it frees the wasmtime store, the guest's
    /// linear memory, and the instance's epoch ticker thread.
    host: Option<Entity<PluginHost>>,
    /// The root the guest reaches the host through, kept alive alongside it.
    _root: Option<Entity<PreviewHostRoot>>,
    focus_handle: FocusHandle,
    _watch: Option<ArtifactWatch>,
    _load: Option<Task<()>>,
    /// Belongs to this load; a reply from an old guest must not replace
    /// the next load's status after reload or retarget.
    _names: Option<Task<()>>,
}

impl PreviewPane {
    pub(crate) fn new(path: Option<PathBuf>, preview_name: String, cx: &mut Context<Self>) -> Self {
        let surface = cx.new(Surface::new);
        let theme_source = cx.new(PreviewThemeSource::new);
        let mut pane = Self {
            path: None,
            preview_name,
            status: Status::Empty,
            surface,
            theme_source,
            host: None,
            _root: None,
            focus_handle: cx.focus_handle(),
            _watch: None,
            _load: None,
            _names: None,
        };
        if let Some(path) = path {
            pane.set_target(path, cx);
        }
        pane
    }

    /// Point the pane at a (possibly different) artifact and preview, then
    /// reload. `horizon preview` on a path that already has a pane lands
    /// here instead of opening a second one.
    pub(crate) fn retarget(&mut self, path: PathBuf, preview_name: String, cx: &mut Context<Self>) {
        self.preview_name = preview_name;
        self.set_target(path, cx);
    }

    fn set_target(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // Also re-placed when there is no watch: the first attempt fails if
        // the artifact's directory does not exist yet, and pointing the pane
        // at the path again after a build is the retry.
        if self._watch.is_none() || self.path.as_deref() != Some(path.as_path()) {
            let watcher = cx.entity().downgrade();
            self._watch = watch::watch(path.clone(), cx, move |cx| {
                watcher.update(cx, |pane, cx| pane.reload(cx)).ok();
            });
        }
        self.path = Some(path);
        self.reload(cx);
    }

    /// Drop the loaded plugin and load the artifact again.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self._load = None;
        self._names = None;
        self.host = None;
        self._root = None;
        let Some(path) = self.path.clone() else {
            self.status = Status::Empty;
            cx.notify();
            return;
        };
        self.status = Status::Loading;
        cx.notify();
        let Some(text_system) = cx
            .try_global::<PreviewTextSystem>()
            .map(|global| global.0.clone())
        else {
            self.status = Status::Failed("no text system to shape guest text with".to_string());
            return;
        };
        let options = PluginOptions::new(text_system)
            .with_turn_budget(TURN_BUDGET)
            .with_memory_limit(MEMORY_LIMIT);
        let load = PluginHost::load(path, options, cx);
        self._load = Some(cx.spawn(async move |this, cx| {
            let loaded = load.await;
            let _ = this.update(cx, |pane, cx| match loaded {
                Ok(host) => pane.attach(host, cx),
                Err(error) => {
                    pane.status = Status::Failed(format!("{error:#}"));
                    cx.notify();
                }
            });
        }));
    }

    /// Bootstrap a freshly loaded plugin: install the host root, take the
    /// plugin's, and hand it this pane's surface to draw on.
    fn attach(&mut self, host: Entity<PluginHost>, cx: &mut Context<Self>) {
        let theme_ref = host.share(&self.theme_source, cx);
        let root = cx.new(|_| {
            PreviewHostRoot::new(
                self.theme_source.clone(),
                theme_ref,
                self.preview_name.clone(),
            )
        });
        host.share_root(&root, cx);
        let plugin = host.root::<PreviewPlugin>(cx);
        let surface_ref = host.share(&self.surface, cx);
        plugin.mount(surface_ref, cx);
        let names = plugin.preview_names(cx);
        self.host = Some(host);
        self._root = Some(root);
        self.status = Status::Loaded {
            previews: Vec::new(),
        };
        cx.notify();
        self.receive_preview_names(names, cx);
    }

    fn receive_preview_names(
        &mut self,
        names: embedded_gpui::Receipt<Vec<String>>,
        cx: &mut Context<Self>,
    ) {
        self._names = Some(cx.spawn(async move |this, cx| {
            let Ok(previews) = names.await else {
                return;
            };
            let _ = this.update(cx, |pane, cx| {
                pane.status = Status::Loaded { previews };
                cx.notify();
            });
        }));
    }

    /// Keys reach the guest through whichever of the two focus handles is
    /// focused: the surface's own (a click inside the preview) or this
    /// pane's (the shell's focus-follow after opening it). Forwarding only
    /// while this pane's handle is focused keeps the surface from seeing
    /// the same keystroke twice when it is the focused one and the event
    /// bubbles up here.
    fn forward_key(&self, input: PlatformInput, window: &Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        let Some(event) = KeyEvent::from_gpui(&input) else {
            return;
        };
        let view = self.surface.read(cx).view().cloned();
        if let Some(view) = view {
            view.key(event, cx);
        }
    }

    fn status_line(&self) -> String {
        status_line(&self.status, self.path.as_deref(), &self.preview_name)
    }

    fn status_color(&self) -> gpui::Hsla {
        match self.status {
            Status::Failed(_) => theme::danger(),
            _ => theme::text_muted(),
        }
    }
}

impl Focusable for PreviewPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PreviewPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(rgb(theme::background()))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.forward_key(PlatformInput::KeyDown(event.clone()), window, cx);
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                this.forward_key(PlatformInput::KeyUp(event.clone()), window, cx);
            }))
            .child(
                div()
                    .px(px(8.))
                    .py(px(4.))
                    .text_size(px(11.))
                    .text_color(self.status_color())
                    .child(self.status_line()),
            )
            .child(div().flex_1().child(self.surface.clone()))
    }
}

#[cfg(test)]
mod tests;
