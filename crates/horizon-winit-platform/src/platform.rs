//! `Platform` implementation injected into gpui via
//! `Application::with_platform`. Modeled closely on `gpui_web::WebPlatform`
//! (the smallest existing `Platform` impl in the zed tree) with the window
//! system swapped for winit; window creation additionally needs
//! `active_loop` because, unlike a browser `Window` or an X11/Wayland
//! connection, winit's `ActiveEventLoop` is only reachable from inside an
//! `ApplicationHandler` callback.
//!
//! Most methods below are no-op stubs — menus, credentials, path prompts,
//! screen capture, multi-window/multi-display. See
//! docs/winit-backend-design.md for which stubs are load-bearing (ported
//! from docs/research/winit-backend-spike.md §8's "what actually gets
//! called" inventory) versus genuinely out of scope for this crate.

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
use crate::window::WinitPlatformWindow;
use crate::window::WinitWindowInner;

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
}

impl WinitPlatform {
    pub(crate) fn new() -> Self {
        let event_loop: EventLoop<WinitUserEvent> = EventLoop::with_user_event()
            .build()
            .expect("failed to build winit event loop");
        let proxy: EventLoopProxy<WinitUserEvent> = event_loop.create_proxy();

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

    fn set_menus(&self, _menus: Vec<Menu>, _keymap: &Keymap) {}

    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {}

    fn on_app_menu_action(&self, _callback: Box<dyn FnMut(&dyn Action)>) {}

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

    fn read_from_primary(&self) -> Option<ClipboardItem> {
        self.clipboard.read_primary()
    }

    fn write_to_primary(&self, item: ClipboardItem) {
        self.clipboard.write_primary(item);
    }

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
