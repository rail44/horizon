//! The window's event loop: blitz-shell's application, with what Horizon
//! needs of the window that blitz-shell does not do, from around it rather
//! than from inside it. It is built the way dioxus-native wraps the same
//! application: the window's events and the shell's own are seen here
//! first, and what is not Horizon's goes on to blitz-shell.
//!
//! What it adds:
//!
//! - Each frame is laid out by `horizon-ui` ([`horizon_ui::frame`]), and
//!   painted by [`redraw`] here instead of by blitz-shell's
//!   `View::redraw`, which lays the document out itself just before it
//!   paints: the work that follows a layout would come too late, and the
//!   layout `horizon-ui` rounds to the display's pixels would be rounded
//!   to whole CSS pixels again.
//! - The input method is enabled with its cursor area declared as a
//!   capability ([`InputMethodShell`]), without which winit discards every
//!   cursor area it is told later.
//! - The window's taking and losing the desktop's focus reaches
//!   [`crate::window`].
//! - The primary selection is set and pasted from ([`crate::primary`]).

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use anyrender::{RenderContext, WindowRenderer};
use anyrender_vello::VelloWindowRenderer;
use blitz_dom::Document;
use blitz_paint::paint_scene;
use blitz_shell::{BlitzApplication, BlitzShellEvent, BlitzShellProvider, BlitzShellProxy, View};
use blitz_shell::{WindowConfig, create_default_event_loop};
use blitz_traits::events::UiEvent;
use blitz_traits::shell::{ClipboardError, FileDialogFilter, ShellProvider};
use winit::application::ApplicationHandler;
use winit::cursor::CursorIcon;
use winit::data_transfer::{DataTransferSendBuilder, TypeHint};
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, AsyncRequestSerial};
use winit::window::{ImeCapabilities, ImeEnableRequest, ImeRequest, ImeRequestData, WindowId};

type Renderer = VelloWindowRenderer;

/// Opens `document` in a window and runs the event loop until the window
/// closes.
pub fn run(document: Box<dyn Document>) {
    let event_loop = create_default_event_loop();
    let (proxy, events) = BlitzShellProxy::new(event_loop.create_proxy());
    PROXY.set(Some(proxy.clone()));
    crate::primary::connect(set_primary_selection, request_primary_selection);
    let application = Application {
        inner: BlitzApplication::new(proxy, events),
        pending: Some(WindowConfig::new(document, Renderer::new())),
        primary_reads: HashMap::new(),
    };
    event_loop.run_app(application).expect("the event loop");
}

thread_local! {
    /// What the primary selection is reached through.
    static PROXY: std::cell::RefCell<Option<BlitzShellProxy>> =
        const { std::cell::RefCell::new(None) };
}

/// What Horizon asks of the window, sent as blitz-shell's embedder event.
enum Request {
    SetPrimary(String),
    PastePrimary,
}

fn send(request: Request) {
    PROXY.with_borrow(|proxy| {
        if let Some(proxy) = proxy {
            proxy.send_event(BlitzShellEvent::embedder_event(request));
        }
    });
}

fn set_primary_selection(text: String) {
    send(Request::SetPrimary(text));
}

fn request_primary_selection() {
    send(Request::PastePrimary);
}

struct Application {
    inner: BlitzApplication<Renderer>,
    /// The window, until the event loop can make it.
    pending: Option<WindowConfig<Renderer>>,
    /// The primary selection asked for, by the window it is pasted into.
    primary_reads: HashMap<AsyncRequestSerial, WindowId>,
}

impl Application {
    fn request(&mut self, event_loop: &dyn ActiveEventLoop, request: &Request) {
        let Some(primary) = event_loop.primary_selection_ext() else {
            return;
        };
        match request {
            Request::SetPrimary(text) => {
                let data = DataTransferSendBuilder::new(text.clone())
                    .with_type(TypeHint::Plaintext, |text, _| Some(text.clone()))
                    .build();
                let _ = primary.set_primary_selection(data);
            }
            Request::PastePrimary => {
                // Pasted into the window there is: Horizon has one.
                let Some(&window_id) = self.inner.windows.keys().next() else {
                    return;
                };
                if let Ok(Some(id)) = primary.primary_selection()
                    && let Ok(serial) = event_loop.fetch_data_transfer(id, &TypeHint::Plaintext)
                {
                    self.primary_reads.insert(serial, window_id);
                }
            }
        }
    }

    /// The primary selection asked for has arrived: it is handed over as
    /// the clipboard's is, a paste. False if it was not asked for here.
    fn primary_arrived(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::DataTransferReceived { serial, value, .. } = event else {
            return false;
        };
        let Some(&window_id) = self.primary_reads.get(serial) else {
            return false;
        };
        let text = value.try_as_string();
        if (text.as_ref()).is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock) {
            return true;
        }
        self.primary_reads.remove(serial);
        if let Ok(text) = text
            && let Some(view) = self.inner.windows.get_mut(&window_id)
        {
            view.doc.handle_ui_event(UiEvent::Paste(text));
            view.request_redraw();
            (self.inner.proxy).send_event(BlitzShellEvent::Poll { window_id });
        }
        true
    }
}

impl ApplicationHandler for Application {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        // Made here rather than by blitz-shell, as dioxus-native makes its
        // own: the shell the document is given is replaced before the
        // document is first laid out and can enable the input method.
        // blitz-shell resumes it below with the windows it has.
        if let Some(config) = self.pending.take() {
            let mut view = View::init(config, event_loop, &self.inner.proxy);
            let doc_id = view.doc.id();
            let shell =
                BlitzShellProvider::new(view.window.clone(), self.inner.proxy.clone(), doc_id);
            let shell = InputMethodShell {
                window: view.window.clone(),
                inner: shell,
            };
            view.doc.inner_mut().set_shell_provider(Arc::new(shell));
            self.inner.windows.insert(view.window_id(), view);
        }
        self.inner.can_create_surfaces(event_loop);
    }

    fn destroy_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.destroy_surfaces(event_loop);
    }

    fn resumed(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.resumed(event_loop);
    }

    fn suspended(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.suspended(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        match &event {
            WindowEvent::RedrawRequested => {
                if let Some(view) = self.inner.windows.get_mut(&window_id) {
                    redraw(view);
                }
                (self.inner.proxy).send_event(BlitzShellEvent::Poll { window_id });
                return;
            }
            // Passed on too: blitz-shell's accessibility follows it.
            WindowEvent::Focused(focused) => crate::window::set_active(*focused),
            event if self.primary_arrived(event) => return,
            _ => {}
        }
        self.inner.window_event(event_loop, window_id, event);
    }

    fn proxy_wake_up(&mut self, event_loop: &dyn ActiveEventLoop) {
        while let Ok(event) = self.inner.event_queue.try_recv() {
            match event {
                BlitzShellEvent::Embedder(event) => {
                    let event: &dyn Any = &*event;
                    if let Some(request) = event.downcast_ref::<Request>() {
                        self.request(event_loop, request);
                    }
                }
                event => self.inner.handle_blitz_shell_event(event_loop, event),
            }
        }
    }
}

/// Lays the document out with `horizon-ui` and paints it.
///
/// A copy of blitz-shell's `View::redraw` at the revision `Cargo.toml`
/// pins, with `horizon_ui::frame` where that resolves the document: when
/// Blitz is moved to another revision, compare this with that revision's
/// `View::redraw` and carry its changes over.
fn redraw(view: &mut View<Renderer>) {
    let animation_time = view.current_animation_time();
    horizon_ui::frame(animation_time);

    let is_visible = view.is_visible;
    let mut inner = view.doc.inner_mut();

    // Unregister resources (e.g. textures) from dropped custom widget nodes
    for id in inner.take_pending_resource_deallocations() {
        view.renderer.unregister_resource(id);
    }

    let (width, height) = inner.viewport().window_size;
    let scale = inner.viewport().scale_f64();
    let is_animating = inner.is_animating();
    let is_blocked = inner.has_pending_critical_resources();
    let insets = view.safe_area_insets;

    if !is_blocked && is_visible {
        view.renderer.render(|scene| {
            paint_scene(
                scene,
                &mut inner,
                scale,
                width,
                height,
                insets.left,
                insets.top,
            )
        });
    }

    drop(inner);

    if !is_blocked && is_visible && is_animating {
        view.request_redraw();
    }
}

/// blitz-shell's shell for a window, but for the input method: enabled
/// with its cursor area declared as a capability, with a first value.
/// Without the capability winit discards every later cursor area, and the
/// input method's candidates are placed without regard to the caret.
/// Upstream's own fix for this is DioxusLabs/blitz#844; once a Blitz with
/// it is in, blitz-shell's shell can be given as it is.
struct InputMethodShell {
    window: Arc<dyn winit::window::Window>,
    inner: BlitzShellProvider,
}

impl ShellProvider for InputMethodShell {
    fn set_ime_enabled(&self, is_enabled: bool) {
        if !is_enabled {
            return self.inner.set_ime_enabled(false);
        }
        let capabilities = ImeCapabilities::new().with_cursor_area();
        let data = ImeRequestData::default().with_cursor_area(
            LogicalPosition::new(0.0, 0.0).into(),
            LogicalSize::new(1.0, 1.0).into(),
        );
        if let Some(request) = ImeEnableRequest::new(capabilities, data) {
            let _ = self.window.request_ime_update(ImeRequest::Enable(request));
        }
    }

    // The rest is blitz-shell's.
    fn request_redraw(&self) {
        self.inner.request_redraw();
    }
    fn set_cursor(&self, icon: Option<CursorIcon>) {
        self.inner.set_cursor(icon);
    }
    fn set_window_title(&self, title: String) {
        self.inner.set_window_title(title);
    }
    fn set_ime_cursor_area(&self, x: f32, y: f32, width: f32, height: f32) {
        self.inner.set_ime_cursor_area(x, y, width, height);
    }
    fn request_clipboard_text(&self) -> Result<(), ClipboardError> {
        self.inner.request_clipboard_text()
    }
    fn set_clipboard_text(&self, text: String) -> Result<(), ClipboardError> {
        self.inner.set_clipboard_text(text)
    }
    fn open_file_dialog(
        &self,
        multiple: bool,
        filter: Option<FileDialogFilter>,
    ) -> Vec<std::path::PathBuf> {
        self.inner.open_file_dialog(multiple, filter)
    }
    fn request_window_close(&self) {
        self.inner.request_window_close();
    }
    fn set_window_minimized(&self, minimized: bool) {
        self.inner.set_window_minimized(minimized);
    }
    fn set_window_maximized(&self, maximized: bool) {
        self.inner.set_window_maximized(maximized);
    }
    fn is_window_maximized(&self) -> bool {
        self.inner.is_window_maximized()
    }
    fn set_window_decorations(&self, decorations: bool) {
        self.inner.set_window_decorations(decorations);
    }
    fn drag_window(&self) {
        self.inner.drag_window();
    }
}
