//! A window on the desktop that the scenarios' frames are drawn into, by
//! the renderer Horizon's own window uses: `--window`.
//!
//! The window is `blitz-shell`'s, opened and kept alive by pumping its
//! event loop between frames. A frame is drawn into it by hand, the way
//! `blitz-shell` redraws: the scene built from the stage's document and
//! given to the renderer, which presents it.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyrender::{RenderContext, WindowRenderer};
use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};
use horizon_ui::blitz_dom::{
    BaseDocument, DocGuard, DocGuardMut, Document, DocumentConfig, LocalName, QualName, ns,
};
use winit::dpi::PhysicalSize;
use winit::event_loop::EventLoop;
use winit::event_loop::pump_events::EventLoopExtPumpEvents;
use winit::window::WindowAttributes;

/// What the window holds as its own document: nothing. The scenarios'
/// documents stay the stages', at the sizes the stages gave them.
struct Blank(BaseDocument);

impl Document for Blank {
    fn inner(&self) -> DocGuard<'_> {
        DocGuard::Ref(&self.0)
    }

    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        DocGuardMut::Ref(&mut self.0)
    }
}

struct Shown {
    event_loop: EventLoop,
    application: BlitzApplication<VelloWindowRenderer>,
}

thread_local! {
    static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
}

impl Shown {
    fn pump(&mut self) {
        let _ = (self.event_loop).pump_app_events(Some(Duration::ZERO), &mut self.application);
    }

    fn active(&self) -> bool {
        (self.application.windows.values()).any(|view| view.renderer.is_active())
    }
}

/// Opens the window and waits for its renderer.
pub fn open() {
    let event_loop = create_default_event_loop();
    let (proxy, events) = BlitzShellProxy::new(event_loop.create_proxy());
    let mut application = BlitzApplication::new(proxy, events);
    let attributes = WindowAttributes::default()
        .with_title("Horizon scenarios")
        .with_surface_size(PhysicalSize::new(900, 700));
    let mut blank = BaseDocument::new(DocumentConfig::default());
    let html = (blank.mutate()).create_element(
        QualName::new(None, ns!(html), LocalName::from("html")),
        Vec::new(),
    );
    let top = blank.root_node().id;
    blank.mutate().append_children(top, &[html]);
    application.add_window(WindowConfig::with_attributes(
        Box::new(Blank(blank)),
        VelloWindowRenderer::new(),
        attributes,
    ));
    let mut shown = Shown {
        event_loop,
        application,
    };
    let started = Instant::now();
    while !shown.active() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the window's renderer did not start"
        );
        let _ = (shown.event_loop)
            .pump_app_events(Some(Duration::from_millis(10)), &mut shown.application);
    }
    SHOWN.set(Some(shown));
}

pub fn is_open() -> bool {
    SHOWN.with_borrow(Option::is_some)
}

/// How long the window's display shows a frame for.
pub fn refresh_interval() -> Duration {
    let millihertz = SHOWN.with_borrow(|shown| {
        let view = shown.as_ref()?.application.windows.values().next()?;
        let mode = view.window.current_monitor()?.current_video_mode()?;
        mode.refresh_rate_millihertz()
    });
    match millihertz {
        Some(millihertz) => Duration::from_secs_f64(1000. / millihertz.get() as f64),
        None => Duration::from_micros(16_667),
    }
}

/// Asks for the window to be `width` by `height`, and the size it then
/// is: the desktop decides (a window that is maximized or tiled stays as
/// it is, and one larger than the screen is not given).
pub fn fit(width: u32, height: u32) -> (u32, u32) {
    SHOWN.with_borrow_mut(|shown| {
        let shown = shown.as_mut().expect("the window");
        let size = |shown: &Shown| {
            let view = (shown.application.windows.values().next()).expect("the window's view");
            let size = view.window.surface_size();
            (size.width, size.height)
        };
        if size(shown) != (width, height) {
            let view = (shown.application.windows.values().next()).expect("the window's view");
            let _ = (view.window).request_surface_size(PhysicalSize::new(width, height).into());
            // The desktop's answer arrives as events.
            let started = Instant::now();
            while size(shown) != (width, height) && started.elapsed() < Duration::from_millis(300) {
                let _ = (shown.event_loop)
                    .pump_app_events(Some(Duration::from_millis(10)), &mut shown.application);
            }
        }
        let fitted = size(shown);
        // (A desktop that scales by a fraction gives a pixel more or less.)
        if fitted.0.abs_diff(width) > 2 || fitted.1.abs_diff(height) > 2 {
            eprintln!(
                "  asked for {width}x{height}, the window is {}x{}",
                fitted.0, fitted.1
            );
        }
        fitted
    })
}

/// Draws `document` into the window, and how long that took: the scene,
/// the GPU's work on it, and its presentation. The surface drawn into is
/// always as large as the window is: the desktop ends a program whose
/// surface is not the size its window was given.
pub fn render(document: &Rc<RefCell<BaseDocument>>, width: u32, height: u32) -> Duration {
    SHOWN.with_borrow_mut(|shown| {
        let shown = shown.as_mut().expect("the window");
        shown.pump();
        let view = (shown.application.windows.values_mut().next()).expect("the window's view");
        let surface = view.window.surface_size();
        view.renderer.set_size(surface.width, surface.height);
        let mut document = document.borrow_mut();
        for resource in document.take_pending_resource_deallocations() {
            view.renderer.unregister_resource(resource);
        }
        let started = Instant::now();
        view.renderer.render(|scene| {
            blitz_paint::paint_scene(scene, &mut document, 1.0, width, height, 0, 0);
        });
        started.elapsed()
    })
}
