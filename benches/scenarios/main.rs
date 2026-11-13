//! Scenarios that time the shell's views a frame at a time, with no
//! window: what a key, a wheel step, a chunk of output or of an answer
//! costs before the page can be drawn, and how that cost moves as what is
//! shown grows.
//!
//! A frame here is the three things a window does between an input and a
//! picture, each timed on its own:
//!
//! - `act`: the input handed to the document, its handlers, and the
//!   signals' effects (the writes to the document);
//! - `layout`: `App::resolve` (style, layout, text, and the work that
//!   follows a layout);
//! - `scene`: `blitz_paint::paint_scene` into a recording scene (what is
//!   drawn, as commands).
//!
//! Turning the commands into pixels is the GPU's in a window and is not
//! timed here. With `--window` the frames are drawn into a window on the
//! desktop instead (`window.rs`), and the third part is `render`: the
//! scene, the GPU's work on it and its presentation, which waits for the
//! display.
//!
//! ```sh
//! cargo bench --bench scenarios                # every scenario
//! cargo bench --bench scenarios -- agent/      # those whose name has `agent/`
//! cargo bench --bench scenarios -- --json out.json
//! cargo bench --bench scenarios -- --window    # drawn into a window
//! ```

use std::time::{Duration, Instant};

use anyrender::PaintScene;
use horizon_ui::blitz_traits::events::{
    BlitzImeEvent, BlitzKeyEvent, BlitzWheelDelta, BlitzWheelEvent, KeyState, PointerCoords,
    UiEvent,
};
use horizon_ui::{App, El};
use keyboard_types::{Key, Modifiers, NamedKey};

mod agent;
mod board;
mod markdown;
mod report;
#[allow(dead_code)]
#[path = "../../tests/agent/sample.rs"]
mod sample;
mod shell;
#[allow(dead_code)]
#[path = "../../tests/support/mod.rs"]
mod support;
mod terminal;
mod window;

/// What one frame took.
#[derive(Clone, Copy, Default)]
pub struct Frame {
    pub act: Duration,
    pub layout: Duration,
    /// The scene's commands; drawn into a window, those and the GPU's work
    /// and the presentation.
    pub scene: Duration,
}

impl Frame {
    pub fn total(&self) -> Duration {
        self.act + self.layout + self.scene
    }
}

/// A view in a document of its own, and the frames timed on it.
pub struct Stage {
    pub app: App,
    size: (u32, u32),
    scene: anyrender::Scene,
    pub frames: Vec<Frame>,
}

impl Stage {
    /// A view in a document of `width` by `height`; drawn into a window,
    /// of the size the window can be given.
    pub fn new(width: u32, height: u32, css: &str, view: impl FnOnce() -> El) -> Self {
        let (width, height) = match window::is_open() {
            true => window::fit(width, height),
            false => (width, height),
        };
        let app = App::new(width, height, css, view);
        let mut stage = Self {
            app,
            size: (width, height),
            scene: anyrender::Scene::new(),
            frames: Vec::new(),
        };
        stage.settle();
        stage
    }

    /// Draws the page, and how long that took.
    fn paint(&mut self) -> Duration {
        let document = self.app.document();
        let (width, height) = self.size;
        if window::is_open() {
            return window::render(&document, width, height);
        }
        let started = Instant::now();
        self.scene.reset();
        blitz_paint::paint_scene(
            &mut self.scene,
            &mut document.borrow_mut(),
            1.0,
            width,
            height,
            0,
            0,
        );
        started.elapsed()
    }

    /// One frame around `act`, timed.
    pub fn frame(&mut self, act: impl FnOnce(&App)) {
        let started = Instant::now();
        act(&self.app);
        self.app.run_effects();
        let act = started.elapsed();
        let started = Instant::now();
        self.app.resolve();
        let layout = started.elapsed();
        let scene = self.paint();
        self.frames.push(Frame { act, layout, scene });
    }

    /// A frame that is not timed: what a scenario does to get to where it
    /// measures from.
    pub fn settle(&mut self) {
        self.app.run_effects();
        self.app.resolve();
        self.paint();
    }

    /// Untimed frames until `met` holds.
    pub fn until(&mut self, what: &str, met: impl Fn() -> bool) {
        let started = Instant::now();
        loop {
            self.settle();
            if met() {
                return;
            }
            assert!(started.elapsed() < Duration::from_secs(30), "no {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Untimed frames until the page's text has `text` in it.
    pub fn until_text(&mut self, what: &str, text: &str) {
        let document = self.app.document();
        self.until(what, || {
            let document = document.borrow();
            document.root_element().text_content().contains(text)
        });
    }

    /// The window at another size.
    pub fn resize(&mut self, width: u32, height: u32) {
        use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};
        let (width, height) = match window::is_open() {
            true => window::fit(width, height),
            false => (width, height),
        };
        self.size = (width, height);
        let document = self.app.document();
        (document.borrow_mut()).set_viewport(Viewport::new(width, height, 1.0, ColorScheme::Light));
    }
}

pub fn key(app: &App, key: Key, modifiers: Modifiers) {
    let text = match &key {
        Key::Character(text) if (modifiers - Modifiers::SHIFT).is_empty() => {
            Some(text.as_str().into())
        }
        _ => None,
    };
    app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
        text,
        key,
        code: keyboard_types::Code::Unidentified,
        modifiers,
        location: keyboard_types::Location::Standard,
        is_auto_repeating: false,
        is_composing: false,
        state: KeyState::Pressed,
    }));
}

pub fn character(app: &App, character: char) {
    key(
        app,
        Key::Character(character.to_string()),
        Modifiers::default(),
    );
}

pub fn named(app: &App, named: NamedKey) {
    key(app, Key::Named(named), Modifiers::default());
}

/// Text as an input method commits it into the focused field.
pub fn commit(app: &App, text: &str) {
    app.dispatch(UiEvent::Ime(BlitzImeEvent::Commit(text.to_string())));
}

/// The wheel turned at a point of the page, `up` pixels towards the top.
pub fn wheel(app: &App, at: (f32, f32), up: f64) {
    horizon_ui::testing::move_to(app, at.0, at.1);
    app.dispatch(UiEvent::Wheel(BlitzWheelEvent {
        delta: BlitzWheelDelta::Pixels(0., up),
        coords: PointerCoords {
            page_x: at.0,
            page_y: at.1,
            screen_x: at.0,
            screen_y: at.1,
            client_x: at.0,
            client_y: at.1,
        },
        buttons: Default::default(),
        mods: Default::default(),
        element: Default::default(),
    }));
}

/// A scenario: frames timed with the content at each of `sizes`.
pub struct Scenario {
    pub name: &'static str,
    /// What a size counts.
    pub unit: &'static str,
    pub sizes: &'static [usize],
    pub run: fn(usize) -> Vec<Frame>,
}

fn scenarios() -> Vec<Scenario> {
    let mut all = Vec::new();
    all.extend(terminal::scenarios());
    all.extend(agent::scenarios());
    all.extend(board::scenarios());
    all.extend(markdown::scenarios());
    all.extend(shell::scenarios());
    all
}

fn main() {
    let mut filter = None;
    let mut json = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--json" => json = arguments.next(),
            "--window" => window::open(),
            // What `cargo bench` passes to every bench target.
            "--bench" => {}
            "--list" => {
                for scenario in scenarios() {
                    println!("{} ({})", scenario.name, scenario.unit);
                }
                return;
            }
            _ => filter = Some(argument),
        }
    }
    // Drawn into a window, a frame is on time when it is done within the
    // display's refresh after the one before it.
    let shown = window::is_open().then(window::refresh_interval);
    let mut measured = Vec::new();
    for scenario in scenarios() {
        if filter
            .as_ref()
            .is_some_and(|filter| !scenario.name.contains(filter.as_str()))
        {
            continue;
        }
        for &size in scenario.sizes {
            eprintln!("{} at {size} {}", scenario.name, scenario.unit);
            let frames = (scenario.run)(size);
            measured.push(report::Measured::of(&scenario, size, &frames, shown));
        }
    }
    print!("{}", report::table(&measured));
    if let Some(path) = json {
        std::fs::write(&path, report::json(&measured)).expect("the JSON file written");
    }
}
