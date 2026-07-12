//! `PlatformWindow` backed by a single winit `Window` + gpui_wgpu's
//! `WgpuRenderer`. Reuses gpui_wgpu's renderer wholesale — none of the
//! rendering pipeline is reimplemented here, only the glue that feeds it a
//! winit-owned surface and drains winit events into gpui's callbacks.
//!
//! IME, multi-window, and window decoration/state control (minimize, zoom,
//! move-by-drag, ...) are unimplemented no-ops per the leg-1 scope; see
//! docs/research/winit-backend-spike.md for which stubs actually got hit.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    px, AnyWindowHandle, Bounds, Capslock, Decorations, DevicePixels, DispatchEventResult,
    GpuSpecs, Modifiers, Pixels, PlatformAtlas, PlatformDisplay, PlatformInput,
    PlatformInputHandler, PromptButton, PromptLevel, RequestFrameOptions, ResizeEdge, Scene, Size,
    WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowControls,
    WindowDecorations, WindowParams,
};
use gpui_wgpu::{GpuContext, WgpuRenderer, WgpuSurfaceConfig};

#[derive(Default)]
pub(crate) struct WinitWindowCallbacks {
    pub(crate) request_frame: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    pub(crate) input: Option<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>,
    pub(crate) resize: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    pub(crate) active_status_change: Option<Box<dyn FnMut(bool)>>,
    pub(crate) should_close: Option<Box<dyn FnMut() -> bool>>,
    pub(crate) close: Option<Box<dyn FnOnce()>>,
    pub(crate) appearance_changed: Option<Box<dyn FnMut()>>,
}

pub(crate) struct WinitWindowState {
    pub(crate) renderer: WgpuRenderer,
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) scale_factor: f32,
    pub(crate) title: String,
    pub(crate) input_handler: Option<PlatformInputHandler>,
    pub(crate) is_active: bool,
    pub(crate) modifiers: Modifiers,
}

/// Shared with the winit `ApplicationHandler`, which drives `state` and
/// `callbacks` from `WindowEvent`s it receives for this window's id.
pub(crate) struct WinitWindowInner {
    pub(crate) window: Arc<winit::window::Window>,
    pub(crate) state: RefCell<WinitWindowState>,
    pub(crate) callbacks: RefCell<WinitWindowCallbacks>,
}

pub(crate) struct WinitPlatformWindow {
    pub(crate) inner: Rc<WinitWindowInner>,
    display: Rc<dyn PlatformDisplay>,
    #[allow(dead_code)]
    handle: AnyWindowHandle,
}

impl WinitPlatformWindow {
    pub(crate) fn new(
        handle: AnyWindowHandle,
        params: WindowParams,
        window: winit::window::Window,
        gpu_context: GpuContext,
        display: Rc<dyn PlatformDisplay>,
    ) -> anyhow::Result<Self> {
        let window = Arc::new(window);
        let physical_size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;

        // Evidence tap for the leg-1 spike (see
        // docs/research/winit-backend-spike.md criterion a): on Wayland,
        // GNOME/Mutter refuses server-side xdg decorations, so
        // winit falls back to its bundled sctk-adwaita client-side frame.
        // A non-zero outer/inner size delta is that frame's titlebar.
        log::info!(
            "window decoration evidence: is_decorated={} inner_size={:?} outer_size={:?}",
            window.is_decorated(),
            window.inner_size(),
            window.outer_size(),
        );

        let renderer_config = WgpuSurfaceConfig {
            size: Size {
                width: DevicePixels(physical_size.width as i32),
                height: DevicePixels(physical_size.height as i32),
            },
            transparent: false,
            preferred_present_mode: None,
        };

        let renderer = WgpuRenderer::new(gpu_context, &window, renderer_config, None)?;

        let logical_size = physical_size.to_logical::<f32>(scale_factor as f64);
        let bounds = Bounds {
            origin: params.bounds.origin,
            size: Size {
                width: px(logical_size.width),
                height: px(logical_size.height),
            },
        };

        let state = WinitWindowState {
            renderer,
            bounds,
            scale_factor,
            title: String::new(),
            input_handler: None,
            is_active: true,
            modifiers: Modifiers::default(),
        };

        let inner = Rc::new(WinitWindowInner {
            window,
            state: RefCell::new(state),
            callbacks: RefCell::new(WinitWindowCallbacks::default()),
        });

        Ok(Self {
            inner,
            display,
            handle,
        })
    }
}

impl raw_window_handle::HasWindowHandle for WinitPlatformWindow {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        self.inner.window.window_handle()
    }
}

impl raw_window_handle::HasDisplayHandle for WinitPlatformWindow {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        self.inner.window.display_handle()
    }
}

impl gpui::PlatformWindow for WinitPlatformWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        self.inner.state.borrow().bounds
    }

    fn is_maximized(&self) -> bool {
        self.inner.window.is_maximized()
    }

    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Windowed(self.bounds())
    }

    fn content_size(&self) -> Size<Pixels> {
        self.inner.state.borrow().bounds.size
    }

    fn resize(&mut self, size: Size<Pixels>) {
        let scale_factor = self.inner.state.borrow().scale_factor as f64;
        let _ = self.inner.window.request_inner_size(
            winit::dpi::LogicalSize::new(f32::from(size.width), f32::from(size.height))
                .to_physical::<u32>(scale_factor),
        );
    }

    fn scale_factor(&self) -> f32 {
        self.inner.state.borrow().scale_factor
    }

    fn appearance(&self) -> WindowAppearance {
        // winit exposes no light/dark query on Linux; gpui_linux itself
        // reads the freedesktop portal setting. Out of scope for leg 1.
        WindowAppearance::Dark
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(self.display.clone())
    }

    fn mouse_position(&self) -> gpui::Point<Pixels> {
        gpui::Point::default()
    }

    fn modifiers(&self) -> Modifiers {
        self.inner.state.borrow().modifiers
    }

    fn capslock(&self) -> Capslock {
        Capslock::default()
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.inner.state.borrow_mut().input_handler = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.inner.state.borrow_mut().input_handler.take()
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<futures::channel::oneshot::Receiver<usize>> {
        None
    }

    fn activate(&self) {
        self.inner.window.focus_window();
    }

    fn is_active(&self) -> bool {
        self.inner.state.borrow().is_active
    }

    fn is_hovered(&self) -> bool {
        false
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
    }

    fn set_title(&mut self, title: &str) {
        self.inner.state.borrow_mut().title = title.to_owned();
        self.inner.window.set_title(title);
    }

    fn set_background_appearance(&self, _background: WindowBackgroundAppearance) {}

    fn minimize(&self) {
        self.inner.window.set_minimized(true);
    }

    fn zoom(&self) {
        let maximized = self.inner.window.is_maximized();
        self.inner.window.set_maximized(!maximized);
    }

    fn toggle_fullscreen(&self) {
        let fullscreen = self.inner.window.fullscreen();
        self.inner.window.set_fullscreen(if fullscreen.is_some() {
            None
        } else {
            Some(winit::window::Fullscreen::Borderless(None))
        });
    }

    fn is_fullscreen(&self) -> bool {
        self.inner.window.fullscreen().is_some()
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.inner.callbacks.borrow_mut().request_frame = Some(callback);
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.inner.callbacks.borrow_mut().input = Some(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.inner.callbacks.borrow_mut().active_status_change = Some(callback);
    }

    fn on_hover_status_change(&self, _callback: Box<dyn FnMut(bool)>) {}

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.inner.callbacks.borrow_mut().resize = Some(callback);
    }

    fn on_moved(&self, _callback: Box<dyn FnMut()>) {}

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.inner.callbacks.borrow_mut().should_close = Some(callback);
    }

    fn on_hit_test_window_control(&self, _callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.inner.callbacks.borrow_mut().close = Some(callback);
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.inner.callbacks.borrow_mut().appearance_changed = Some(callback);
    }

    fn draw(&self, scene: &Scene) {
        self.inner.state.borrow_mut().renderer.draw(scene);
    }

    fn completed_frame(&self) {
        self.inner.window.pre_present_notify();
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.inner.state.borrow().renderer.sprite_atlas().clone()
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        self.inner
            .state
            .borrow()
            .renderer
            .supports_dual_source_blending()
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        Some(self.inner.state.borrow().renderer.gpu_specs())
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {
        // Leg 2 scope (docs/roadmap.md): IME preedit is not implemented.
    }

    fn request_decorations(&self, _decorations: WindowDecorations) {}

    fn show_window_menu(&self, _position: gpui::Point<Pixels>) {}

    fn start_window_move(&self) {
        let _ = self.inner.window.drag_window();
    }

    fn start_window_resize(&self, _edge: ResizeEdge) {}

    fn window_decorations(&self) -> Decorations {
        // sctk-adwaita CSD on Wayland, native chrome on X11/other backends —
        // both are winit/compositor-owned, so from gpui's point of view
        // the server (or winit acting on the client's behalf) always owns
        // decorations here.
        Decorations::Server
    }

    fn set_app_id(&mut self, _app_id: &str) {}

    fn window_controls(&self) -> WindowControls {
        WindowControls {
            fullscreen: true,
            maximize: true,
            minimize: true,
            window_menu: false,
        }
    }

    fn set_client_inset(&self, _inset: Pixels) {}
}
