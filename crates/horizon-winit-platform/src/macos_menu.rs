//! macOS native application menu, backed by `muda` — the standard winit
//! companion for OS menu bars (winit itself draws no menus; see
//! docs/winit-backend-design.md's "macOS: native app menu" section).
//!
//! Implements the two `gpui::Platform` hooks gpui's own app startup drives
//! on every platform (`init_app_menus` in the pinned gpui checkout's
//! `crates/gpui/src/platform/app_menu.rs`, called unconditionally from
//! `App`'s constructor) that a menu-less stub would otherwise silently
//! swallow:
//!
//! - `Platform::set_menus` — build the native menu from gpui's
//!   `Menu`/`MenuItem` tree (`MacosMenuState::set_menus`, called from
//!   `platform.rs`).
//! - The click -> `Platform::on_app_menu_action` callback gpui registers
//!   to dispatch the clicked item's `Action` (`MacosMenuState::dispatch`,
//!   called from `platform.rs::dispatch_menu_action`, itself driven by the
//!   `muda::MenuEvent` forwarded through the winit event loop in
//!   `app_handler.rs` — see that module for the `MenuEvent::set_event_handler`
//!   wiring done once in `WinitPlatform::new`).
//!
//! Scope matches what Horizon actually sets today (`src/main.rs`): one
//! top-level "Horizon" menu holding a "Quit Horizon" action item. The
//! builder below walks the full `Menu`/`MenuItem` tree generally
//! (`Action`/`Separator`/`Submenu`) so it keeps working if Horizon adds
//! more menu items later; `MenuItem::SystemMenu` (macOS's OS-managed
//! Services menu) is the one variant left unimplemented — Horizon doesn't
//! set one, and muda has no direct hand-off for it.
//!
//! Accelerators are intentionally left unset on every muda item: gpui's
//! own keybinding dispatch already handles the one shortcut Horizon binds
//! today (`cmd-q` -> `Quit`, wired independently in `src/main.rs` via
//! `cx.bind_keys`) at the window level regardless of what the OS menu
//! shows, and deriving `muda::accelerator::Accelerator`s from gpui's
//! `Keymap` for arbitrary future actions is left for when Horizon actually
//! needs menu-displayed shortcuts.
//!
//! **Unbuilt on this host** (Linux, no macOS SDK) — see
//! docs/winit-backend-design.md's "Verification" section for what a Mac
//! build still needs to confirm.

use std::cell::RefCell;
use std::collections::HashMap;

use gpui::{Action, Menu, MenuItem};
use muda::{Menu as MudaMenu, MenuId, MenuItem as MudaMenuItem, PredefinedMenuItem, Submenu};

/// Owns the live native menu (must stay reachable for the OS to keep
/// showing it — see the `menu` field) and the click -> `Action` lookup
/// `dispatch` needs. One instance lives on `WinitPlatform` for the
/// process's lifetime.
#[derive(Default)]
pub(crate) struct MacosMenuState {
    actions: RefCell<HashMap<MenuId, Box<dyn Action>>>,
    // Retained so the native menu tree isn't dropped from under NSApp;
    // `set_menus` replaces it wholesale on every call (Horizon only ever
    // calls `cx.set_menus` once today, at startup).
    menu: RefCell<Option<MudaMenu>>,
    next_id: RefCell<u64>,
}

impl MacosMenuState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set_menus(&self, menus: Vec<Menu>) {
        self.actions.borrow_mut().clear();
        let root = MudaMenu::new();
        for menu in menus {
            let submenu = self.build_submenu(menu);
            if let Err(error) = root.append(&submenu) {
                log::warn!("horizon-winit-platform: failed to append app menu: {error}");
            }
        }
        root.init_for_nsapp();
        *self.menu.borrow_mut() = Some(root);
    }

    /// Invoked from `platform.rs::dispatch_menu_action` with the id off a
    /// `muda::MenuEvent` forwarded through the winit event loop, and the
    /// callback `Platform::on_app_menu_action` registered (gpui's own
    /// `init_app_menus` wires that to `cx.dispatch_action`).
    pub(crate) fn dispatch(&self, id: &MenuId, callback: &mut dyn FnMut(&dyn Action)) {
        if let Some(action) = self.actions.borrow().get(id) {
            callback(action.as_ref());
        }
    }

    fn build_submenu(&self, menu: Menu) -> Submenu {
        let submenu = Submenu::new(&menu.name, !menu.disabled);
        for item in menu.items {
            self.append_item(&submenu, item);
        }
        submenu
    }

    fn append_item(&self, parent: &Submenu, item: MenuItem) {
        let result = match item {
            MenuItem::Separator => parent.append(&PredefinedMenuItem::separator()),
            MenuItem::Submenu(menu) => parent.append(&self.build_submenu(menu)),
            MenuItem::SystemMenu(_) => {
                // The OS-managed Services menu; Horizon doesn't set one
                // today and muda has no direct equivalent hand-off.
                return;
            }
            MenuItem::Action {
                name,
                action,
                disabled,
                ..
            } => {
                let id = self.fresh_id();
                let muda_item = MudaMenuItem::with_id(id.clone(), &name, !disabled, None);
                let result = parent.append(&muda_item);
                self.actions.borrow_mut().insert(id, action);
                result
            }
        };
        if let Err(error) = result {
            log::warn!("horizon-winit-platform: failed to append menu item: {error}");
        }
    }

    fn fresh_id(&self) -> MenuId {
        let mut next = self.next_id.borrow_mut();
        let id = MenuId::new(format!("horizon-menu-{}", *next));
        *next += 1;
        id
    }
}
