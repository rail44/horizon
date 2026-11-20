//! The image viewer (`plugins/image`): a `file://` link to an image,
//! clicked in a terminal with the platform's key held, is opened with it,
//! over the workspace; a link to a file no extension opens is left alone,
//! and a web page's goes to the desktop as before.
//!
//! The extension is the one cargo builds (`./scripts/extensions.sh
//! build`).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use horizon::workspace::{Opening, Shell, Spawn, css, view};
use horizon_terminal_core::TerminalSize;
use horizon_ui::App;
use horizon_ui::blitz_dom::{NodeId, local_name};
use horizon_ui::blitz_traits::events::{
    BlitzKeyEvent, BlitzPointerEvent, BlitzPointerId, KeyState, MouseEventButton,
    MouseEventButtons, PointerCoords, UiEvent,
};
use horizon_ui::reactive_graph::traits::WithUntracked;

use keyboard_types::{Key, Modifiers, NamedKey};

mod support;

/// A PNG three pixels wide and two high.
const PNG: [u8; 74] = [
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 3, 0, 0, 0, 2, 8, 6, 0,
    0, 0, 157, 116, 102, 26, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240, 31,
    134, 25, 144, 57, 0, 155, 126, 11, 245, 114, 176, 185, 60, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
    96, 130,
];

/// What a link's text is: wide enough to be pressed on wherever the row
/// begins.
const LINK: &str = "the-link-the-link-the-link-the-link";

struct Opened {
    app: App,
    shell: Shell,
    /// The web pages handed to the desktop.
    to_the_desktop: Rc<RefCell<Vec<String>>>,
    /// The element the shell's view returned: what the tests look under.
    root: NodeId,
}

/// A shell with the extensions it comes with, and its terminal.
fn open() -> Opened {
    let extensions =
        horizon::extension::own_in(&Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug"));
    assert!(
        (extensions.iter()).any(|file| file.ends_with("horizon_image_plugin.wasm")),
        "{extensions:?}"
    );
    let to_the_desktop = Rc::new(RefCell::new(Vec::new()));
    let handed = to_the_desktop.clone();
    let (mut shell, mut top) = (None, None);
    let app = App::new(1200, 800, "Test", css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let (root, built) = view(
            spawn,
            Opening {
                extensions,
                open_url: Some(Rc::new(move |url: &str| {
                    handed.borrow_mut().push(url.to_string())
                })),
                ..Opening::default()
            },
        );
        (shell, top) = (Some(built), Some(root.0));
        root
    });
    app.resolve();
    let opened = Opened {
        app,
        shell: shell.unwrap(),
        to_the_desktop,
        root: top.unwrap(),
    };
    opened.until("the extensions", || opened.shell.extensions_read());

    opened.until("a prompt", || opened.screen().trim_end().ends_with('$'));
    opened
}

impl Opened {
    fn until(&self, what: &str, met: impl Fn() -> bool) {
        let started = Instant::now();
        while !met() {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "no {what} in:\n{}",
                self.screen()
            );
            std::thread::sleep(Duration::from_millis(5));
            self.app.run_effects();
            self.app.resolve();
        }
    }

    fn all(&self, class: &str) -> Vec<NodeId> {
        horizon_ui::testing::find(&self.app, self.root, class)
    }

    /// The box of `node` on the page: x, y, width, height.
    fn bounds(&self, node: NodeId) -> (f32, f32, f32, f32) {
        let document = self.app.document();
        let document = document.borrow();
        let bounds = document.get_client_bounding_rect(node).expect("a box");
        (
            bounds.x as f32,
            bounds.y as f32,
            bounds.width as f32,
            bounds.height as f32,
        )
    }

    /// The text of the terminal the cursor's pane shows.
    fn screen(&self) -> String {
        let pane = (self.shell.model).with_untracked(|model| model.cursor_pane_id());
        (pane.and_then(|pane| self.shell.terminal(pane)))
            .map_or_else(String::new, |terminal| terminal.text())
    }

    fn key(&self, key: Key) {
        let text = match &key {
            Key::Character(text) => Some(text.as_str().into()),
            _ => None,
        };
        self.app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
            text,
            key,
            code: keyboard_types::Code::Unidentified,
            modifiers: Modifiers::default(),
            location: keyboard_types::Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
        }));
        self.app.run_effects();
        self.app.resolve();
    }

    /// Runs `command` in the terminal, as typed.
    fn run(&self, command: &str) {
        for character in command.chars() {
            self.key(Key::Character(character.to_string()));
        }
        self.key(Key::Named(NamedKey::Enter));
    }

    /// Writes a link to `uri` on the terminal's first row, and presses on
    /// it with the platform's key held.
    fn follow(&self, uri: &str) {
        // (`%` begins a conversion in printf's format.)
        let uri = uri.replace('%', "%%");
        self.run(&format!(
            r"clear; printf '\033]8;;{uri}\033\\{LINK}\033]8;;\033\\\n'"
        ));
        self.until("the link", || {
            self.screen().lines().next() == Some(LINK) && self.screen().trim_end().ends_with('$')
        });
        let terminal = self.all("terminal");
        let (x, y, _, _) = self.bounds(*terminal.first().expect("the terminal"));
        let (x, y) = (x + 40., y + 6.);
        let event = |buttons| BlitzPointerEvent {
            id: BlitzPointerId::Mouse,
            is_primary: true,
            coords: PointerCoords {
                page_x: x,
                page_y: y,
                screen_x: x,
                screen_y: y,
                client_x: x,
                client_y: y,
            },
            button: MouseEventButton::Main,
            buttons,
            mods: Modifiers::META,
            details: Default::default(),
            element: Default::default(),
            active_pointers: Default::default(),
        };
        self.app
            .dispatch(UiEvent::PointerMove(event(MouseEventButtons::None)));
        self.app
            .dispatch(UiEvent::PointerDown(event(MouseEventButtons::Primary)));
        self.app
            .dispatch(UiEvent::PointerUp(event(MouseEventButtons::None)));
        self.app.run_effects();
        self.app.resolve();
    }

    /// The images over the workspace.
    fn images(&self) -> Vec<NodeId> {
        (self.all("sheet").into_iter())
            .flat_map(|sheet| horizon_ui::testing::find(&self.app, sheet, "image"))
            .collect()
    }

    fn src(&self, image: NodeId) -> String {
        let document = self.app.document();
        let document = document.borrow();
        let node = document.get_node(image).expect("the image");
        node.attr(local_name!("src"))
            .unwrap_or_default()
            .to_string()
    }
}

/// The page painted into pixels: the color at a point of it.
fn painted(opened: &Opened) -> impl Fn(usize, usize) -> [u8; 3] {
    use anyrender::ImageRenderer as _;

    let (width, height) = (1200, 800);
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    let document = opened.app.document();
    renderer.render_to_vec(
        |scene| {
            blitz_paint::paint_scene(scene, &mut document.borrow_mut(), 1.0, width, height, 0, 0)
        },
        &mut pixels,
    );
    if let Some(out) = std::env::var_os("HORIZON_IMAGE_OUT") {
        let mut ppm = format!("P6 {width} {height} 255\n").into_bytes();
        for pixel in pixels.chunks(4) {
            ppm.extend_from_slice(&pixel[..3]);
        }
        std::fs::write(out, ppm).unwrap();
    }
    move |x, y| {
        let at = (y * width as usize + x) * 4;
        [pixels[at], pixels[at + 1], pixels[at + 2]]
    }
}

/// A directory of the test's own with `files` written in it.
fn directory(name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("hz-image-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    for (file, bytes) in files {
        std::fs::write(directory.join(file), bytes).unwrap();
    }
    directory
}

#[test]
fn a_link_to_an_image_is_opened_over_the_workspace_and_escape_takes_it_away() {
    let directory = directory("over", &[("shot one.png", &PNG)]);
    let opened = open();
    let file = directory.join("shot one.png");
    opened.follow(&format!("file://{}", file.display()).replace(' ', "%20"));

    opened.until("the image", || opened.images().len() == 1);
    let image = opened.images()[0];
    let src = opened.src(image);
    assert!(
        src.starts_with(&format!(
            "file://{}?",
            directory.join("shot%20one.png").display()
        )),
        "{src}"
    );
    // Drawn at its own size once it is read.
    opened.until("the image's size", || {
        let (_, _, width, height) = opened.bounds(image);
        (width, height) == (3., 2.)
    });
    assert_eq!(opened.shell.commanded(), 1);

    opened.key(Key::Named(NamedKey::Escape));
    opened.until("the image gone", || opened.images().is_empty());
    opened.until("the instance let go of", || opened.shell.commanded() == 0);
    // Opened again, it is fetched again: the file may have been written
    // since.
    opened.follow(&format!("file://{}", file.display()).replace(' ', "%20"));
    opened.until("the image again", || opened.images().len() == 1);
    assert_ne!(opened.src(opened.images()[0]), src);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn an_image_larger_than_the_window_is_made_smaller_to_fit_its_shape_kept() {
    let large = br#"<svg xmlns="http://www.w3.org/2000/svg" width="3000" height="2000">
        <rect width="3000" height="2000" fill="red"/></svg>"#;
    let directory = directory("large", &[("large.svg", large)]);
    let opened = open();
    opened.follow(&format!("file://{}", directory.join("large.svg").display()));
    opened.until("the image", || opened.images().len() == 1);
    let image = opened.images()[0];
    opened.until("the image read", || opened.bounds(image).2 > 0.);
    // Within the window, which is 1200 by 800, and as large as it is let
    // be there.
    let (x, y, width, height) = opened.bounds(image);
    assert!(x >= 0. && x + width <= 1200., "{x} {width}");
    assert!(y >= 0. && y + height <= 800., "{y} {height}");
    assert!((width / height - 1.5).abs() < 0.01, "{width} {height}");
    assert!(height > 400., "{height}");
    // And drawn there.
    let middle = painted(&opened)((x + width / 2.) as usize, (y + height / 2.) as usize);
    assert_eq!(middle, [255, 0, 0]);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn a_link_to_a_file_no_extension_opens_is_left_alone_and_a_web_page_goes_to_the_desktop() {
    let directory = directory("alone", &[("notes.txt", b"words")]);
    let opened = open();
    opened.follow(&format!("file://{}", directory.join("notes.txt").display()));
    opened.follow("file://elsewhere.example/tmp/shot.png");
    for _ in 0..20 {
        opened.app.run_effects();
        opened.app.resolve();
    }
    assert!(opened.images().is_empty());
    assert_eq!(opened.shell.commanded(), 0);
    assert!(opened.to_the_desktop.borrow().is_empty());

    opened.follow("https://example.com/x");
    assert_eq!(*opened.to_the_desktop.borrow(), ["https://example.com/x"]);
    let _ = std::fs::remove_dir_all(&directory);
}
