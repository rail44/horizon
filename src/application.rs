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
//!   `View::redraw` -- the first one by [`complete_resume`] instead of by
//!   its `View::complete_resume` -- which lay the document out themselves
//!   just before they paint: the work that follows a layout would come too late, and the
//!   layout `horizon-ui` rounds to the display's pixels would be rounded
//!   to whole CSS pixels again.
//! - The input method is enabled with its cursor area and the kind of text
//!   declared as capabilities ([`InputMethodShell`]), without which winit
//!   discards every cursor area it is told later and never tells the kind.
//! - The window's taking and losing the desktop's focus reaches
//!   [`crate::window`].
//! - The primary selection is set and pasted from ([`crate::primary`]).
//! - A frame drawn for nothing but a CSS animation is drawn on every
//!   [`crate::window::animation_every_nth_refresh`]th refresh of the
//!   display, not on every one: the whole window is drawn for it. A frame
//!   for anything else is drawn as soon as it is asked for.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::task::{Wake, Waker};
use std::time::{Duration, Instant};

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
use winit::event_loop::{ActiveEventLoop, AsyncRequestSerial, ControlFlow};
use winit::window::{
    ImeCapabilities, ImeEnableRequest, ImeHint, ImePurpose, ImeRequest, ImeRequestData, WindowId,
};

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
        animation_frames: HashMap::new(),
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
    /// When each window whose page is animating draws its next frame for
    /// the animation, if nothing has it drawn before then.
    animation_frames: HashMap<WindowId, Instant>,
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
                told: Mutex::default(),
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
                    let started = Instant::now();
                    if redraw(view) {
                        let next = started + animation_interval(refresh_millihertz(view));
                        self.animation_frames.insert(window_id, next);
                    } else {
                        self.animation_frames.remove(&window_id);
                    }
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

    /// The windows whose animation's next frame is due are drawn; the event
    /// loop sleeps until the next that is not.
    fn about_to_wait(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.about_to_wait(event_loop);
        let now = Instant::now();
        let due: Vec<WindowId> = (self.animation_frames.iter())
            .filter(|(_, at)| **at <= now)
            .map(|(window, _)| *window)
            .collect();
        for window_id in due {
            self.animation_frames.remove(&window_id);
            if let Some(view) = self.inner.windows.get(&window_id) {
                view.request_redraw();
            }
        }
        event_loop.set_control_flow(match self.animation_frames.values().min() {
            Some(next) => ControlFlow::WaitUntil(*next),
            None => ControlFlow::Wait,
        });
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
                // The first frame is painted here too, laid out as every
                // other is.
                BlitzShellEvent::ResumeReady { window_id } => {
                    if let Some(view) = self.inner.windows.get_mut(&window_id) {
                        complete_resume(view);
                    }
                    (self.inner.proxy).send_event(BlitzShellEvent::Poll { window_id });
                }
                event => self.inner.handle_blitz_shell_event(event_loop, event),
            }
        }
    }
}

/// Finishes the window's renderer, once it is ready, and paints the first
/// frame.
///
/// A copy of blitz-shell's `View::complete_resume` at the revision
/// `Cargo.toml` pins, with `horizon_ui::frame` where that resolves the
/// document, as [`redraw`] has it: compare it with the new revision's when
/// Blitz is moved. The waker it gives the window is blitz-shell's
/// `create_waker`'s, which blitz-shell does not export.
fn complete_resume(view: &mut View<Renderer>) {
    if !view.renderer.complete_resume() {
        return;
    }
    let window_id = view.window_id();

    // Resync the renderer to the current viewport: resizes that arrived
    // while the renderer was pending did not reach it.
    let animation_time = view.current_animation_time();
    horizon_ui::frame(animation_time);
    let mut inner = view.doc.inner_mut();
    let (width, height) = inner.viewport().window_size;
    let scale = inner.viewport().scale_f64();
    let insets = view.safe_area_insets;

    inner.can_create_surfaces(&mut view.renderer as _);

    // The render surface covers the entire window, including the safe area
    view.renderer.set_size(
        width + insets.left + insets.right,
        height + insets.top + insets.bottom,
    );

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
    drop(inner);

    view.waker = Some(Waker::from(Arc::new(PollOnWake {
        proxy: view.proxy.clone(),
        window_id,
    })));
}

/// Wakes the window's document to be polled.
struct PollOnWake {
    proxy: BlitzShellProxy,
    window_id: WindowId,
}

impl Wake for PollOnWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let window_id = self.window_id;
        self.proxy.send_event(BlitzShellEvent::Poll { window_id });
    }
}

/// Lays the document out with `horizon-ui` and paints it. Whether the page
/// is animating, visible and not waiting on a resource: the frame its
/// animation draws next is the caller's to ask for.
///
/// A copy of blitz-shell's `View::redraw` at the revision `Cargo.toml`
/// pins, with `horizon_ui::frame` where that resolves the document, and
/// with the next frame for an animation returned rather than asked for at
/// once: when Blitz is moved to another revision, compare this with that
/// revision's `View::redraw` and carry its changes over.
fn redraw(view: &mut View<Renderer>) -> bool {
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

    !is_blocked && is_visible && is_animating
}

/// The display's refresh rate under the window, in millihertz, if the
/// desktop says.
fn refresh_millihertz(view: &View<Renderer>) -> Option<u32> {
    let mode = view.window.current_monitor()?.current_video_mode()?;
    Some(mode.refresh_rate_millihertz()?.get())
}

/// How long after a frame the next one an animation alone asks for is
/// drawn: [`crate::window::animation_every_nth_refresh`] refreshes of a
/// display refreshing at `millihertz` (60 Hz where that is not known).
fn animation_interval(millihertz: Option<u32>) -> Duration {
    every_nth_refresh(millihertz, crate::window::animation_every_nth_refresh())
}

fn every_nth_refresh(millihertz: Option<u32>, every: u32) -> Duration {
    let millihertz = millihertz.filter(|rate| *rate > 0).unwrap_or(60_000);
    Duration::from_secs_f64(f64::from(every.max(1)) * 1000. / f64::from(millihertz))
}

/// blitz-shell's shell for a window, but for the input method: enabled
/// with its cursor area declared as a capability, with a first value.
/// Without the capability winit discards every later cursor area, and the
/// input method's candidates are placed without regard to the caret.
/// Upstream's own fix for this is DioxusLabs/blitz#844.
///
/// The kind of text ([`horizon_ui::input_purpose`]) is declared and told
/// the same way, with every cursor area. Over Wayland, winit sends the
/// compositor a kind only when it is declared; GNOME's input method
/// otherwise goes on with the kind the last window that sent one gave it,
/// and a password field's takes its IBus input sources away from the
/// switcher while this window has the focus.
///
/// A cursor area and kind the input method was last told are not told it
/// again ([`Told`]): the document tells them after every layout, and
/// neither mutter nor GNOME Shell drops a repeat, so each would reach
/// GNOME Shell's script and IBus once a frame.
struct InputMethodShell {
    window: Arc<dyn winit::window::Window>,
    inner: BlitzShellProvider,
    told: Mutex<Told>,
}

/// Where the text is and its kind, as the input method was last told
/// them since it was last turned on or off.
#[derive(Default)]
struct Told(Option<((f32, f32, f32, f32), ImePurpose)>);

impl Told {
    /// Whether `now` is news to the input method, kept as what it was told
    /// if so.
    fn news(&mut self, now: ((f32, f32, f32, f32), ImePurpose)) -> bool {
        let news = self.0 != Some(now);
        self.0 = Some(now);
        news
    }

    /// Turning the input method on tells it an area of its own, and off
    /// lets go of what it was told.
    fn forget(&mut self) {
        self.0 = None;
    }
}

/// The kind of text what has the focus takes, in winit's words.
fn purpose() -> ImePurpose {
    match horizon_ui::input_purpose() {
        horizon_ui::InputPurpose::Normal => ImePurpose::Normal,
        horizon_ui::InputPurpose::Terminal => ImePurpose::Terminal,
    }
}

impl ShellProvider for InputMethodShell {
    fn set_ime_enabled(&self, is_enabled: bool) {
        self.told.lock().unwrap().forget();
        if !is_enabled {
            return self.inner.set_ime_enabled(false);
        }
        let capabilities = ImeCapabilities::new()
            .with_cursor_area()
            .with_hint_and_purpose();
        let data = ImeRequestData::default()
            .with_cursor_area(
                LogicalPosition::new(0.0, 0.0).into(),
                LogicalSize::new(1.0, 1.0).into(),
            )
            .with_hint_and_purpose(ImeHint::NONE, purpose());
        if let Some(request) = ImeEnableRequest::new(capabilities, data) {
            let _ = self.window.request_ime_update(ImeRequest::Enable(request));
        }
    }

    fn set_ime_cursor_area(&self, x: f32, y: f32, width: f32, height: f32) {
        let purpose = purpose();
        if !(self.told.lock().unwrap()).news(((x, y, width, height), purpose)) {
            return;
        }
        let data = ImeRequestData::default()
            .with_cursor_area(
                LogicalPosition::new(x, y).into(),
                LogicalSize::new(width, height).into(),
            )
            .with_hint_and_purpose(ImeHint::NONE, purpose);
        let _ = self.window.request_ime_update(ImeRequest::Update(data));
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

#[cfg(test)]
mod tests {
    use winit::window::ImePurpose;

    use super::Told;

    #[test]
    fn the_input_method_is_told_what_changed_and_again_once_turned_on() {
        let caret = (10., 20., 8., 16.);
        let mut told = Told::default();
        assert!(told.news((caret, ImePurpose::Terminal)));
        // The next frame's layout, with nothing moved.
        assert!(!told.news((caret, ImePurpose::Terminal)));
        // The caret moved, then the focus went to a text field there.
        assert!(told.news(((18., 20., 8., 16.), ImePurpose::Terminal)));
        assert!(told.news(((18., 20., 8., 16.), ImePurpose::Normal)));
        // Turned on again, the input method has an area of its own.
        told.forget();
        assert!(told.news(((18., 20., 8., 16.), ImePurpose::Normal)));
    }

    #[test]
    fn an_animation_is_drawn_on_every_nth_refresh_of_the_display() {
        use std::time::Duration;

        use super::every_nth_refresh;

        let close = |interval: Duration, milliseconds: f64| {
            (interval.as_secs_f64() * 1000. - milliseconds).abs() < 1e-6
        };
        // Every third refresh of a 60 Hz display: 20 frames a second.
        assert!(close(every_nth_refresh(Some(60_000), 3), 50.));
        // Of a 120 Hz one: 40.
        assert!(close(every_nth_refresh(Some(120_000), 3), 25.));
        // A display that does not say is taken to refresh at 60 Hz.
        assert!(close(every_nth_refresh(None, 3), 50.));
        assert!(close(every_nth_refresh(Some(0), 3), 50.));
        // Every refresh is the least.
        assert!(close(every_nth_refresh(Some(60_000), 0), 1000. / 60.));
    }
}
