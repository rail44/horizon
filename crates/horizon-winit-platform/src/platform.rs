//! `Platform` implementation injected into gpui via
//! `Application::with_platform`. Modeled closely on `gpui_web::WebPlatform`
//! (the smallest existing `Platform` impl in the zed tree) with the window
//! system swapped for winit; window creation additionally needs
//! `active_loop` because, unlike a browser `Window` or an X11/Wayland
//! connection, winit's `ActiveEventLoop` is only reachable from inside an
//! `ApplicationHandler` callback.
//!
//! Most methods below are no-op stubs — credentials, path prompts, screen
//! capture, multi-window/multi-display. See docs/winit-backend-design.md
//! for which stubs are load-bearing (ported from
//! docs/research/winit-backend-spike.md §8's "what actually gets called"
//! inventory) versus genuinely out of scope for this crate. `set_menus`/
//! `activate` are real (not stubs) on macOS — see `macos_menu.rs` and the
//! design doc's "macOS: native app menu" section; both stay documented
//! no-ops on Linux and Windows.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use futures::channel::oneshot;
use gpui::{
    Action, AnyWindowHandle, BackgroundExecutor, ClipboardItem, CursorStyle, DummyKeyboardMapper,
    ForegroundExecutor, Keymap, Menu, MenuItem, PathPromptOptions, Platform, PlatformDisplay,
    PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem, PlatformWindow, Task,
    ThermalState, WindowAppearance, WindowParams,
};
use gpui_wgpu::{CosmicTextSystem, GpuContext};
use winit::event_loop::{EventLoop, EventLoopProxy};

use crate::active_loop::with_active_loop;
use crate::app_handler::{WinitAppHandler, WinitUserEvent};
use crate::clipboard::WinitClipboard;
use crate::cursor::cursor_style_to_icon;
use crate::dispatcher::WinitDispatcher;
use crate::display::WinitDisplay;
#[cfg(target_os = "macos")]
use crate::macos_menu::MacosMenuState;
use crate::window::WinitPlatformWindow;
use crate::window::WinitWindowInner;
#[cfg(target_os = "macos")]
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};

struct WinitKeyboardLayout;

impl PlatformKeyboardLayout for WinitKeyboardLayout {
    fn id(&self) -> &str {
        "us"
    }

    fn name(&self) -> &str {
        "US"
    }
}

pub(crate) struct WinitPlatform {
    background_executor: BackgroundExecutor,
    foreground_executor: ForegroundExecutor,
    text_system: Arc<dyn PlatformTextSystem>,
    pub(crate) dispatcher: Arc<WinitDispatcher>,
    event_loop: RefCell<Option<EventLoop<WinitUserEvent>>>,
    gpu_context: GpuContext,
    display: Rc<dyn PlatformDisplay>,
    pub(crate) windows: RefCell<Vec<Rc<WinitWindowInner>>>,
    active_window: Cell<Option<AnyWindowHandle>>,
    clipboard: WinitClipboard,
    // Set by `Platform::on_app_menu_action` (gpui's own `init_app_menus`
    // wires this to `cx.dispatch_action`); invoked from
    // `dispatch_menu_action` on macOS, the only platform that ever
    // produces a menu click. Kept unconditional (not `#[cfg(macos)]`)
    // since it's a trivial field and `on_app_menu_action` itself is a
    // normal cross-platform `Platform` method.
    #[allow(clippy::type_complexity)]
    app_menu_action_callback: RefCell<Option<Box<dyn FnMut(&dyn Action)>>>,
    #[cfg(target_os = "macos")]
    macos_menu: MacosMenuState,
}

impl WinitPlatform {
    pub(crate) fn new() -> Self {
        let mut builder = EventLoop::with_user_event();
        // `ActivationPolicy::Regular` gives the process a normal Dock
        // icon/menu-bar identity and activates it on launch — without
        // this, a process with no bundle Info.plist (like a bare `cargo
        // run` binary) can end up with no way to become the active app.
        // See docs/winit-backend-design.md's "macOS: native app menu"
        // section for what this does and doesn't cover.
        #[cfg(target_os = "macos")]
        builder.with_activation_policy(ActivationPolicy::Regular);
        let event_loop: EventLoop<WinitUserEvent> =
            builder.build().expect("failed to build winit event loop");
        let proxy: EventLoopProxy<WinitUserEvent> = event_loop.create_proxy();

        // Forward muda's global menu-click channel into the winit event
        // loop as a user event, per muda's own documented winit
        // integration (its README's "Note for winit or tao users") — this
        // both wakes the loop (if it's parked in `ControlFlow::Wait`) and
        // gets the click onto the thread `dispatch_menu_action` expects to
        // run on. Registered once per process, matching `platform()`'s own
        // "call this once" contract.
        #[cfg(target_os = "macos")]
        {
            let menu_proxy = proxy.clone();
            muda::MenuEvent::set_event_handler(Some(move |event: muda::MenuEvent| {
                let _ = menu_proxy.send_event(WinitUserEvent::MenuEvent(event));
            }));
        }

        let dispatcher = Arc::new(WinitDispatcher::new(proxy));
        let background_executor = BackgroundExecutor::new(dispatcher.clone());
        let foreground_executor = ForegroundExecutor::new(dispatcher.clone());

        let text_system: Arc<dyn PlatformTextSystem> =
            Arc::new(CosmicTextSystem::new("sans-serif"));

        Self {
            background_executor,
            foreground_executor,
            text_system,
            dispatcher,
            event_loop: RefCell::new(Some(event_loop)),
            gpu_context: Rc::new(RefCell::new(None)),
            display: Rc::new(WinitDisplay::new()),
            windows: RefCell::new(Vec::new()),
            active_window: Cell::new(None),
            clipboard: WinitClipboard::new(),
            app_menu_action_callback: RefCell::new(None),
            #[cfg(target_os = "macos")]
            macos_menu: MacosMenuState::new(),
        }
    }

    /// Routes a `muda::MenuEvent`'s id (forwarded through the event loop as
    /// `WinitUserEvent::MenuEvent`, see `app_handler.rs::user_event`) to
    /// whatever `Action` `set_menus` associated with it, then into
    /// whatever callback `Platform::on_app_menu_action` registered (gpui's
    /// `init_app_menus` wires that to `cx.dispatch_action`).
    #[cfg(target_os = "macos")]
    pub(crate) fn dispatch_menu_action(&self, id: &muda::MenuId) {
        let mut callback = self.app_menu_action_callback.borrow_mut();
        if let Some(callback) = callback.as_mut() {
            self.macos_menu.dispatch(id, callback);
        }
    }
}

impl Platform for WinitPlatform {
    fn background_executor(&self) -> BackgroundExecutor {
        self.background_executor.clone()
    }

    fn foreground_executor(&self) -> ForegroundExecutor {
        self.foreground_executor.clone()
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.text_system.clone()
    }

    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        let event_loop = self
            .event_loop
            .borrow_mut()
            .take()
            .expect("Platform::run called more than once");

        let mut handler = WinitAppHandler::new(self, on_finish_launching);
        event_loop
            .run_app(&mut handler)
            .expect("winit event loop exited with an error");
    }

    fn quit(&self) {
        // No `ActiveEventLoop` handle is retained outside callbacks (see
        // active_loop.rs). Horizon's own quit path
        // (`src/main.rs`'s `Quit` action) calls `cx.quit()`, which tears
        // down every window first — `WindowEvent::CloseRequested`'s
        // handler (app_handler.rs) calls `ActiveEventLoop::exit()` once the
        // last one closes, so the process still exits cleanly without this
        // method doing anything itself.
        log::trace!("Platform::quit called");
    }

    fn restart(&self, _binary_path: Option<PathBuf>) {}

    fn activate(&self, _ignoring_other_apps: bool) {
        // No winit API brings the whole app (as opposed to one window) to
        // the front post-launch — `ActivationPolicy::Regular` (set once at
        // event-loop build time, see `new()`) already gets launch-time
        // activation; this focuses whatever window(s) exist right now,
        // which is what a later re-activate (e.g. a future Dock-icon
        // reopen handler) would want. See docs/winit-backend-design.md's
        // "macOS: native app menu" section for the fuller rationale and
        // what a real `NSApp.activate(ignoringOtherApps:)` call would add.
        #[cfg(target_os = "macos")]
        for window in self.windows.borrow().iter() {
            window.window.focus_window();
        }
        log::trace!("Platform::activate called");
    }

    fn hide(&self) {
        log::trace!("Platform::hide called");
    }

    fn hide_other_apps(&self) {
        log::trace!("Platform::hide_other_apps called");
    }

    fn unhide_other_apps(&self) {
        log::trace!("Platform::unhide_other_apps called");
    }

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        vec![self.display.clone()]
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(self.display.clone())
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        self.active_window.get()
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        params: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        let title = params
            .titlebar
            .as_ref()
            .and_then(|titlebar| titlebar.title.clone())
            .map(|title| title.to_string())
            .unwrap_or_default();

        let attrs = winit::window::WindowAttributes::default()
            .with_title(title)
            .with_inner_size(winit::dpi::LogicalSize::new(
                f32::from(params.bounds.size.width),
                f32::from(params.bounds.size.height),
            ))
            .with_decorations(true);

        let window =
            with_active_loop(|event_loop| event_loop.create_window(attrs)).ok_or_else(|| {
                anyhow::anyhow!(
                    "open_window called outside a winit ApplicationHandler callback \
                     (no ActiveEventLoop available)"
                )
            })??;

        let platform_window = WinitPlatformWindow::new(
            handle,
            params,
            window,
            self.gpu_context.clone(),
            self.display.clone(),
        )?;

        self.windows
            .borrow_mut()
            .push(Rc::clone(&platform_window.inner));
        self.active_window.set(Some(handle));

        Ok(Box::new(platform_window))
    }

    fn window_appearance(&self) -> WindowAppearance {
        WindowAppearance::Dark
    }

    fn open_url(&self, url: &str) {
        log::info!("open_url (not implemented on the winit backend): {url}");
    }

    fn on_open_urls(&self, _callback: Box<dyn FnMut(Vec<String>)>) {}

    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn prompt_for_paths(
        &self,
        _options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        let (tx, rx) = oneshot::channel();
        tx.send(Err(anyhow::anyhow!(
            "prompt_for_paths is not implemented on the winit backend"
        )))
        .ok();
        rx
    }

    fn prompt_for_new_path(
        &self,
        _directory: &Path,
        _suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        let (tx, rx) = oneshot::channel();
        tx.send(Err(anyhow::anyhow!(
            "prompt_for_new_path is not implemented on the winit backend"
        )))
        .ok();
        rx
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        false
    }

    fn reveal_path(&self, _path: &Path) {}

    fn open_with_system(&self, _path: &Path) {}

    fn on_quit(&self, _callback: Box<dyn FnMut()>) {}

    fn on_reopen(&self, _callback: Box<dyn FnMut()>) {}

    fn on_system_wake(&self, _callback: Box<dyn FnMut()>) {}

    fn set_menus(&self, menus: Vec<Menu>, _keymap: &Keymap) {
        // `_keymap` is unused: see macos_menu.rs's module doc for why menu
        // items don't carry OS-displayed accelerators today.
        #[cfg(target_os = "macos")]
        self.macos_menu.set_menus(menus);
        #[cfg(not(target_os = "macos"))]
        {
            // Documented no-op on Linux (sctk-adwaita's CSD carries no
            // menu bar) and Windows (out of scope — see
            // docs/winit-backend-design.md).
            let _ = menus;
        }
    }

    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {}

    fn on_app_menu_action(&self, callback: Box<dyn FnMut(&dyn Action)>) {
        // Cross-platform storage (see the field doc); only ever driven by
        // a real click on macOS (`dispatch_menu_action`).
        *self.app_menu_action_callback.borrow_mut() = Some(callback);
    }

    fn on_will_open_app_menu(&self, _callback: Box<dyn FnMut()>) {}

    fn on_validate_app_menu_command(&self, _callback: Box<dyn FnMut(&dyn Action) -> bool>) {}

    fn thermal_state(&self) -> ThermalState {
        ThermalState::Nominal
    }

    fn on_thermal_state_change(&self, _callback: Box<dyn FnMut()>) {}

    fn app_path(&self) -> Result<PathBuf> {
        std::env::current_exe().map_err(Into::into)
    }

    fn path_for_auxiliary_executable(&self, _name: &str) -> Result<PathBuf> {
        Err(anyhow::anyhow!(
            "path_for_auxiliary_executable is not implemented on the winit backend"
        ))
    }

    fn set_cursor_style(&self, style: CursorStyle) {
        let icon = cursor_style_to_icon(style);
        for window in self.windows.borrow().iter() {
            window.set_cursor_icon(icon);
        }
    }

    fn hide_cursor_until_mouse_moves(&self) {
        // Out of scope for this crate's milestone (task brief item 2b is
        // cursor *styles*, not auto-hide) — see docs/winit-backend-design.md.
        log::trace!("Platform::hide_cursor_until_mouse_moves called");
    }

    fn is_cursor_visible(&self) -> bool {
        true
    }

    fn should_auto_hide_scrollbars(&self) -> bool {
        false
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        self.clipboard.read()
    }

    fn write_to_clipboard(&self, item: ClipboardItem) {
        self.clipboard.write(item);
    }

    // The cfg on these four must mirror `gpui::Platform`'s own gates
    // exactly: the trait only declares primary-selection methods on
    // Linux/FreeBSD and find-pasteboard methods on macOS.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn read_from_primary(&self) -> Option<ClipboardItem> {
        self.clipboard.read_primary()
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn write_to_primary(&self, item: ClipboardItem) {
        self.clipboard.write_primary(item);
    }

    // The find pasteboard backs macOS's system-wide "Use Selection for
    // Find" state; arboard exposes no NSFindPboard API and no Horizon
    // surface feeds it, so stub it the same way clipboard.rs stubs
    // primary selection off-Linux.
    #[cfg(target_os = "macos")]
    fn read_from_find_pasteboard(&self) -> Option<ClipboardItem> {
        None
    }

    #[cfg(target_os = "macos")]
    fn write_to_find_pasteboard(&self, _item: ClipboardItem) {}

    fn write_credentials(&self, _url: &str, _username: &str, _password: &[u8]) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "credential storage is not implemented on the winit backend"
        )))
    }

    fn read_credentials(&self, _url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        Task::ready(Ok(None))
    }

    fn delete_credentials(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "credential storage is not implemented on the winit backend"
        )))
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(WinitKeyboardLayout)
    }

    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        Rc::new(DummyKeyboardMapper)
    }

    fn on_keyboard_layout_change(&self, _callback: Box<dyn FnMut()>) {}
}
