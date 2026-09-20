//! The guest half: the plugin's entry point and its root object.
//!
//! `preview-plugin/` is a three-line crate that registers [`PreviewGuest`]
//! with `embedded_gpui::register_plugin!`; everything the plugin does lives
//! here, so a preview added to [`crate::preview::registry`] ships without
//! touching the plugin crate.

use embedded_gpui::surface::SurfaceApi;
use embedded_gpui::{open_view, root, share_root, shared, Plugin, Ref, Remote};
use gpui::{prelude::*, App, AssetSource, Context, Entity, Subscription, WindowHandle};
use horizon_config::{RawConfig, RawThemeConfig};

use crate::preview::registry::{self, PreviewRoot};
use crate::preview::sample::PreviewIcons;
use crate::preview::schema::{
    PreviewHost, PreviewHostCaller as _, PreviewPlugin, PreviewThemeApi, PreviewThemeApiCaller as _,
};

pub struct PreviewGuest {
    /// The registry holds the root weakly; this keeps it alive for the
    /// component's lifetime.
    _root: Entity<GuestRoot>,
}

impl Plugin for PreviewGuest {
    fn new(cx: &mut App) -> Self {
        gpui_component::init(cx);
        let host = root::<PreviewHost>();
        let root_entity = cx.new(|_| GuestRoot {
            host: host.clone(),
            windows: Vec::new(),
            theme: None,
            _theme_observer: None,
        });
        share_root(&root_entity, cx);

        let weak_root = root_entity.downgrade();
        cx.spawn(async move |cx| {
            let receipt = cx.update(|cx| host.theme(cx));
            let theme = match receipt.await {
                Ok(theme) => theme,
                Err(error) => {
                    embedded_gpui::log::error!("preview: host theme unavailable: {error:#}");
                    return;
                }
            };
            cx.update(|cx| {
                // `observe` fires once on subscription, so the first apply
                // rides the same path as every later change.
                let source = theme.clone();
                let target = weak_root.clone();
                let observer = theme.observe(cx, move |cx| {
                    let receipt = source.theme_json(cx);
                    let target = target.clone();
                    cx.spawn(async move |cx| {
                        let json: String = match receipt.await {
                            Ok(json) => json,
                            Err(error) => {
                                embedded_gpui::log::error!("preview: theme read failed: {error:#}");
                                return;
                            }
                        };
                        cx.update(|cx| {
                            target
                                .update(cx, |root, cx| root.apply_theme(&json, cx))
                                .ok();
                        });
                    })
                    .detach();
                });
                weak_root
                    .update(cx, |root, _| {
                        root.theme = Some(theme);
                        root._theme_observer = Some(observer);
                    })
                    .ok();
            });
        })
        .detach();

        Self { _root: root_entity }
    }

    fn assets() -> Option<Box<dyn AssetSource>> {
        Some(Box::new(PreviewIcons))
    }
}

struct GuestRoot {
    host: Remote<PreviewHost>,
    /// One per mounted surface; kept so a theme change can repaint them.
    windows: Vec<WindowHandle<PreviewRoot>>,
    /// Held so the host's theme object stays connected.
    theme: Option<Remote<PreviewThemeApi>>,
    _theme_observer: Option<Subscription>,
}

impl GuestRoot {
    /// Resolves the shell's `[theme]` section into this guest's own scheme
    /// and gpui-component theme — the same two calls the shell makes for
    /// itself — then repaints every mounted preview.
    fn apply_theme(&mut self, json: &str, cx: &mut Context<Self>) {
        let theme = match serde_json::from_str::<RawThemeConfig>(json) {
            Ok(theme) => theme,
            Err(error) => {
                embedded_gpui::log::error!("preview: unreadable theme input: {error:#}");
                return;
            }
        };
        crate::theme::reload_from(&RawConfig {
            theme,
            ..RawConfig::default()
        });
        crate::theme::apply_gpui_component_theme(cx);
        self.windows
            .retain(|window| window.update(cx, |_, window, _| window.refresh()).is_ok());
    }

    fn open(&mut self, surface: Ref<SurfaceApi>, name: &str, cx: &mut Context<Self>) {
        let Some(entry) = registry::preview(name) else {
            embedded_gpui::log::error!("preview: no preview named {name:?} in this plugin");
            return;
        };
        let build = entry.build;
        match open_view(surface, cx, move |window, cx| {
            let child = build(window, cx);
            cx.new(|_| PreviewRoot::new(child))
        }) {
            Ok(handle) => self.windows.push(handle),
            Err(error) => embedded_gpui::log::error!("preview: open_view failed: {error:#}"),
        }
    }
}

#[shared]
impl PreviewPlugin for GuestRoot {
    fn preview_names(&mut self, _cx: &mut Context<Self>) -> Vec<String> {
        registry::previews()
            .iter()
            .map(|preview| preview.name.to_string())
            .collect()
    }

    fn mount(&mut self, surface: Ref<SurfaceApi>, cx: &mut Context<Self>) {
        let receipt = self.host.selected_preview(cx);
        cx.spawn(async move |this, cx| {
            let name = match receipt.await {
                Ok(name) => name,
                Err(error) => {
                    embedded_gpui::log::error!("preview: no preview selection: {error:#}");
                    return;
                }
            };
            cx.update(|cx| {
                this.update(cx, |root, cx| root.open(surface, &name, cx))
                    .ok();
            });
        })
        .detach();
    }
}
