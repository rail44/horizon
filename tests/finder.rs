//! The file finder: typing narrows the list, the arrows move the choice,
//! Enter takes the file chosen.

use std::cell::RefCell;
use std::rc::Rc;

use horizon::finder::Finder;
use horizon_theme_css::{Theme, themed};
use horizon_ui::blitz_traits::events::{BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{App, Callback, Component};
use keyboard_types::{Key, Modifiers, NamedKey};

struct Opened {
    app: App,
    picked: Rc<RefCell<Vec<String>>>,
}

fn open(files: Vec<String>, theme: fn() -> Theme, size: (u32, u32)) -> Opened {
    let picked = Rc::new(RefCell::new(Vec::new()));
    let told = picked.clone();
    let app = App::new(size.0, size.1, horizon_theme_css::CSS, move || {
        let finder = Finder {
            files: Signal::stored(files),
            listing: Signal::stored(false),
            picked: Callback::new(move |path| told.borrow_mut().push(path)),
            left: Callback::new(|()| {}),
        };
        themed(theme, finder.view())
    });
    app.resolve();
    Opened { app, picked }
}

impl Opened {
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
        self.app.resolve();
    }

    fn type_text(&self, text: &str) {
        for character in text.chars() {
            self.key(Key::Character(character.to_string()));
        }
    }

    fn text(&self) -> String {
        horizon_ui::testing::text(&self.app)
    }
}

fn files() -> Vec<String> {
    ["README.md", "docs/roadmap.md", "docs/research/roads.md"]
        .map(String::from)
        .to_vec()
}

#[test]
fn typing_narrows_the_list_and_enter_takes_the_file_chosen() {
    let opened = open(files(), Theme::built_in, (800, 600));
    assert!(opened.text().contains("3/3"), "{}", opened.text());
    opened.type_text("road");
    let text = opened.text();
    assert!(text.contains("2/3"), "{text}");
    assert!(!text.contains("README"), "{text}");
    // The shorter path is first; the arrow moves to the other.
    opened.key(Key::Named(NamedKey::ArrowDown));
    opened.key(Key::Named(NamedKey::Enter));
    assert_eq!(*opened.picked.borrow(), ["docs/research/roads.md"]);
}

/// Paints the finder over this repository's Markdown files in this
/// machine's theme (or over the paths in the file `HORIZON_FINDER_FILES`
/// names, one a line) to the file `HORIZON_FINDER_OUT` names (a PPM), with
/// `HORIZON_FINDER_QUERY` typed: a picture for a person to look at. Run by
/// hand.
#[test]
#[ignore = "writes a picture for a person: set HORIZON_FINDER_OUT"]
fn paint_the_finder() {
    use anyrender::ImageRenderer as _;

    let out = std::env::var("HORIZON_FINDER_OUT").expect("HORIZON_FINDER_OUT");
    let query = std::env::var("HORIZON_FINDER_QUERY").unwrap_or_default();
    let listed = match std::env::var_os("HORIZON_FINDER_FILES") {
        Some(list) => std::fs::read_to_string(list).expect("the list of files"),
        None => {
            let listed = std::process::Command::new("git")
                .args(["ls-files", "*.md"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .expect("git");
            String::from_utf8(listed.stdout).expect("paths")
        }
    };
    let files: Vec<String> = listed.lines().map(String::from).collect();
    let (width, height) = (600u32, 440u32);
    let opened = open(files, Theme::of_this_machine, (width, height));
    opened.type_text(&query);
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    let document = opened.app.document();
    renderer.render_to_vec(
        |scene| {
            blitz_paint::paint_scene(scene, &mut document.borrow_mut(), 1.0, width, height, 0, 0)
        },
        &mut pixels,
    );
    let mut picture = format!("P6\n{width} {height}\n255\n").into_bytes();
    for [red, green, blue, _] in pixels.as_chunks::<4>().0 {
        picture.extend_from_slice(&[*red, *green, *blue]);
    }
    std::fs::write(out, picture).expect("the picture written");
}
