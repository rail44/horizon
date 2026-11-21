//! The terminal view over a real shell on a PTY: what is typed, composed,
//! pasted, dragged and scrolled reaches the shell, and what the shell
//! prints is the view's frame.

use std::time::{Duration, Instant};

use std::cell::RefCell;
use std::rc::Rc;

use horizon::terminal::{Session, Terminal, TerminalView, build};
use horizon_terminal_core::TerminalSize;
use horizon_terminal_core::TerminalUpdate;
use horizon_theme_css::TerminalLook;
use horizon_theme_css::Theme;
use horizon_ui::App;
use horizon_ui::blitz_traits::events::{
    BlitzImeEvent, BlitzKeyEvent, BlitzWheelDelta, BlitzWheelEvent, KeyState, PointerCoords,
    UiEvent,
};
use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::Set;
use horizon_ui::testing::{Pointer, move_to, pointer};

mod support;

struct Opened {
    app: App,
    terminal: Terminal,
    /// What the view was given: the session, the look it follows, and
    /// where the links it opened went.
    session: Rc<Session>,
    look: RwSignal<TerminalLook>,
    opened_urls: Rc<RefCell<Vec<String>>>,
}

fn open() -> Opened {
    open_at("")
}

/// The view in a box with `style`: where on the page it is.
fn open_at(style: &str) -> Opened {
    let session: Rc<Session> = support::shell("sh", TerminalSize::new(80, 24)).into();
    // (Made on the page: a signal is its page's.)
    let mut followed = None;
    let opened_urls = Rc::new(RefCell::new(Vec::new()));
    let (given, log) = (session.clone(), opened_urls.clone());
    let mut terminal = None;
    let app = App::new(900, 540, "Test", "", || {
        let drags = horizon_components::Drags::default();
        let look = RwSignal::new(Theme::built_in().terminal());
        followed = Some(look);
        let (root, state) = build(TerminalView {
            session: given,
            look: look.into(),
            open_link: Some(Rc::new(move |url| log.borrow_mut().push(url.to_string()))),
            drags: drags.clone(),
        });
        terminal = Some(state);
        root.focus();
        // The page around the view: where a drag that leaves it is heard.
        let around = horizon_ui::el("div")
            .attr("style", "width: 100%; height: 100%;")
            .child(
                horizon_ui::el("div")
                    .attr(
                        "style",
                        &format!("height: 100%; box-sizing: border-box; {style}"),
                    )
                    .child(root),
            );
        drags.wire(around)
    });
    app.resolve();
    let opened = Opened {
        app,
        terminal: terminal.unwrap(),
        session,
        look: followed.unwrap(),
        opened_urls,
    };
    // The shell's first prompt.
    opened.wait("a prompt", |text| text.trim_end().ends_with('$'));
    opened
}

impl Opened {
    /// Lays out (which takes the session's frames in) until `met` holds of
    /// the screen's text.
    fn wait(&self, what: &str, met: impl Fn(&str) -> bool) -> String {
        let started = Instant::now();
        loop {
            self.app.resolve();
            let text = self.terminal.text();
            if met(&text) {
                return text;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no {what} in:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// As [`Self::wait`], and until the shell is back at its prompt after
    /// it: what is typed next is typed at the prompt, as a person types.
    /// Typed while the command before it still runs, it is echoed at once,
    /// in among what that command writes.
    fn done(&self, what: &str, met: impl Fn(&str) -> bool) -> String {
        self.wait(what, |text| met(text) && text.trim_end().ends_with('$'))
    }

    /// Lays out until `met` holds.
    fn until(&self, what: &str, met: impl Fn() -> bool) {
        let started = Instant::now();
        loop {
            self.app.resolve();
            if met() {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no {what} in:\n{}",
                self.terminal.text()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn key(&self, key: keyboard_types::Key, modifiers: keyboard_types::Modifiers) {
        let text = match &key {
            keyboard_types::Key::Character(text) if modifiers.is_empty() => Some(text.as_str()),
            keyboard_types::Key::Character(text)
                if modifiers == keyboard_types::Modifiers::SHIFT =>
            {
                Some(text.as_str())
            }
            _ => None,
        };
        self.app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
            text: text.map(Into::into),
            key,
            code: keyboard_types::Code::Unidentified,
            modifiers,
            location: keyboard_types::Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
        }));
    }

    /// Types `text` a key at a time, as a US keyboard would produce it.
    fn type_text(&self, text: &str) {
        for character in text.chars() {
            let shifted = character.is_ascii_uppercase() || "$()*|\"_:~{}<>?!&".contains(character);
            let modifiers = match shifted {
                true => keyboard_types::Modifiers::SHIFT,
                false => keyboard_types::Modifiers::default(),
            };
            self.key(
                keyboard_types::Key::Character(character.to_string()),
                modifiers,
            );
        }
    }

    fn enter(&self) {
        self.key(
            keyboard_types::Key::Named(keyboard_types::NamedKey::Enter),
            Default::default(),
        );
    }

    fn run(&self, command: &str) {
        self.type_text(command);
        self.enter();
    }

    fn ime(&self, event: BlitzImeEvent) {
        self.app.dispatch(UiEvent::Ime(event));
    }

    /// The row whose text is exactly `text`.
    fn row_of(&self, text: &str) -> usize {
        (self.terminal.text().lines())
            .position(|line| line == text)
            .unwrap_or_else(|| panic!("no row {text:?} in:\n{}", self.terminal.text()))
    }

    /// The middle of a cell, on the page.
    fn cell(&self, row: usize, column: usize) -> (f32, f32) {
        // As many cells as fit were counted off the box's 900 pixels: a
        // cell is at most this wide, and the few cells asked for here are
        // within it either way.
        let (columns, _) = self.terminal.size();
        let width = 900. / (columns as f32 + 0.5);
        ((column as f32 + 0.5) * width, (row as f32 + 0.5) * 18.)
    }

    fn wheel(&self, up: f64) {
        move_to(&self.app, 300., 200.);
        self.app.dispatch(UiEvent::Wheel(BlitzWheelEvent {
            delta: BlitzWheelDelta::Pixels(0., up),
            coords: PointerCoords {
                page_x: 300.,
                page_y: 200.,
                screen_x: 300.,
                screen_y: 200.,
                client_x: 300.,
                client_y: 200.,
            },
            buttons: Default::default(),
            mods: Default::default(),
            element: Default::default(),
        }));
    }
}

#[test]
fn what_is_typed_runs_and_a_chord_interrupts() {
    let opened = open();
    // Shifted characters and all: the shell does the sum.
    opened.run("echo Sum-$((6*7))");
    opened.wait("the sum", |text| text.contains("Sum-42"));

    // `cat` gives back each line; Ctrl+C ends it, and the shell is there
    // again.
    opened.run("cat");
    opened.run("again");
    opened.wait("cat's copy", |text| text.matches("again").count() == 2);
    opened.key(
        keyboard_types::Key::Character("c".into()),
        keyboard_types::Modifiers::CONTROL,
    );
    // (The shell drops what is typed before it is back at its prompt.)
    opened.wait("the prompt", |text| text.trim_end().ends_with('$'));
    opened.run("echo after-$((1+1))");
    opened.done("the shell again", |text| text.contains("after-2"));

    // Backspace takes out what is before the cursor. An arrow reaches the
    // shell as its escape sequence, which this shell, having no line
    // editing, shows as it came.
    opened.type_text("echo abX");
    opened.key(
        keyboard_types::Key::Named(keyboard_types::NamedKey::Backspace),
        Default::default(),
    );
    opened.run("cd");
    opened.wait("the edited line", |text| {
        text.lines().any(|line| line == "abcd")
    });
    opened.key(
        keyboard_types::Key::Named(keyboard_types::NamedKey::ArrowLeft),
        Default::default(),
    );
    opened.wait("the arrow's sequence", |text| text.contains("^[[D"));
}

#[test]
fn what_the_input_method_composes_is_shown_and_what_it_commits_is_sent() {
    let opened = open();
    opened.type_text("echo ");
    opened.wait("the command", |text| text.contains("echo"));

    opened.ime(BlitzImeEvent::Preedit(
        "にほんご".to_string(),
        Some((0, 12)),
    ));
    assert_eq!(opened.terminal.preedit(), "にほんご");
    // While it composes, a key is the input method's, not the shell's.
    opened.type_text("x");
    opened.ime(BlitzImeEvent::Preedit(String::new(), None));
    opened.ime(BlitzImeEvent::Commit("日本語".to_string()));
    assert_eq!(opened.terminal.preedit(), "");
    opened.enter();
    let text = opened.wait("the committed text, twice", |text| {
        text.matches("日本語").count() == 2
    });
    assert!(!text.contains("にほんご"), "{text}");
    assert!(!text.contains('x'), "{text}");
    assert!(text.lines().any(|line| line == "日本語"), "{text}");
}

#[test]
fn a_paste_goes_to_the_shell() {
    let opened = open();
    opened
        .app
        .dispatch(UiEvent::Paste("echo pasted-$((2*3))".to_string()));
    opened.enter();
    opened.wait("what the pasted command prints", |text| {
        text.contains("pasted-6")
    });
}

#[test]
fn the_shell_is_told_the_size_of_the_box() {
    let opened = open();
    let (columns, rows) = opened.terminal.size();
    assert_eq!(rows, 30);
    assert!((100..=150).contains(&columns), "{columns}");
    opened.run("stty size");
    opened.done("the size", |text| {
        text.lines().any(|line| line == format!("{rows} {columns}"))
    });

    (opened.app.document().borrow_mut()).set_viewport(Viewport::new(
        600,
        360,
        1.0,
        ColorScheme::Light,
    ));
    opened.app.resolve();
    let (narrower, shorter) = opened.terminal.size();
    assert_eq!(shorter, 20);
    assert!(narrower < columns);
    opened.run("stty size");
    opened.wait("the new size", |text| {
        text.lines()
            .any(|line| line == format!("{shorter} {narrower}"))
    });
}

#[test]
fn a_drag_selects_cells_and_copying_takes_them() {
    let opened = open();
    opened.run("echo select-me-please");
    opened.wait("the line", |text| {
        text.lines().any(|line| line == "select-me-please")
    });
    let row = opened.row_of("select-me-please");

    let (x, y) = opened.cell(row, 0);
    pointer(&opened.app, x, y, Pointer::Move);
    pointer(&opened.app, x, y, Pointer::Down);
    let (x, y) = opened.cell(row, 5);
    pointer(&opened.app, x, y, Pointer::Drag);
    pointer(&opened.app, x, y, Pointer::Up);
    let started = Instant::now();
    let selection = loop {
        opened.app.resolve();
        if let Some(selection) = opened.terminal.frame().and_then(|frame| frame.selection) {
            break selection;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "no selection");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!((selection.start.row, selection.start.col), (row, 0));
    assert_eq!((selection.end.row, selection.end.col), (row, 5));

    // (The chord for it is the shell's: `tests/workspace.rs`.)
    opened.terminal.copy_selection();
    let started = Instant::now();
    let copied = loop {
        opened.app.resolve();
        if let Some(copied) = opened.terminal.take_copied() {
            break copied;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "nothing copied"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(copied, "select");
}

#[test]
fn the_wheel_goes_back_through_what_scrolled_off() {
    let opened = open();
    opened.run("seq 1 200");
    opened.wait("the last number", |text| {
        text.lines().any(|line| line == "200")
    });
    assert!(!opened.terminal.text().lines().any(|line| line == "150"));

    // Up by forty rows.
    opened.wheel(40. * 18.);
    opened.wait("what scrolled off", |text| {
        text.lines().any(|line| line == "150") && !text.lines().any(|line| line == "200")
    });
    // The history is scrolled here, over a window of it the view holds:
    // by a part of a row too, with the rows the same.
    assert_eq!(opened.terminal.scrolled_into(), Some(0.));
    let before = opened.terminal.text();
    opened.wheel(9.);
    opened.app.resolve();
    assert_eq!(opened.terminal.scrolled_into(), Some(0.5));
    let nudged = opened.terminal.text();
    assert_eq!(
        nudged.lines().nth(1),
        before.lines().next(),
        "a row further into history at the top, the row that was the top under it"
    );
    opened.wheel(-9.);
    opened.app.resolve();
    assert_eq!(opened.terminal.text(), before);
    // Far up, past the window held: more of the history is asked for.
    // (A turn ends at the edge of the window held; the next goes on in
    // the one that arrives.)
    let started = Instant::now();
    while !opened.terminal.text().lines().any(|line| line == "1") {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no first numbers in:\n{}",
            opened.terminal.text()
        );
        opened.wheel(20. * 18.);
        opened.app.resolve();
        std::thread::sleep(Duration::from_millis(5));
    }
    // What the shell goes on printing does not move the view.
    let held = opened.terminal.text();
    opened.type_text("echo below");
    opened.enter();
    std::thread::sleep(Duration::from_millis(200));
    opened.app.resolve();
    assert_eq!(opened.terminal.text(), held);

    opened.wheel(-400. * 18.);
    opened.wait("the last number again", |text| {
        text.lines().any(|line| line == "200")
    });
    assert_eq!(opened.terminal.scrolled_into(), None);
}

#[test]
fn the_session_says_when_the_shell_has_gone_and_the_view_does_not_take_that() {
    let opened = open();
    let gone = |opened: &Opened| {
        (opened.session.take_reports().iter())
            .any(|report| matches!(report, TerminalUpdate::Exited))
    };
    assert!(!gone(&opened));
    opened.run("exit");
    let started = Instant::now();
    // (The view takes in what is its own at each layout.)
    while !gone(&opened) {
        opened.app.resolve();
        assert!(started.elapsed() < Duration::from_secs(10), "still running");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_tab_is_a_blank_cell_and_what_follows_it_is_at_the_tab_stop() {
    let opened = open();
    opened.run("printf 'a\\tb\\n'");
    let text = opened.wait("the line", |text| {
        text.lines().any(|line| line.starts_with("a "))
    });
    assert!(text.lines().any(|line| line == "a       b"), "{text}");
    assert!(!text.contains('\t'), "{text}");
}

#[test]
fn the_screen_is_painted_and_not_only_held() {
    use anyrender::ImageRenderer as _;

    let opened = open();
    opened.run("echo painted");
    opened.wait("the line", |text| {
        text.lines().any(|line| line == "painted")
    });
    let (width, height) = (900, 540);
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    let document = opened.app.document();
    renderer.render_to_vec(
        |scene| {
            blitz_paint::paint_scene(scene, &mut document.borrow_mut(), 1.0, width, height, 0, 0)
        },
        &mut pixels,
    );
    // A renderer with no GPU device keeps no rows: the screen is drawn whole.
    assert!(!opened.terminal.painted_from_kept_rows());
    // The screen's own background fills the page, and the text on it is
    // pixels of its foreground.
    let look = Theme::built_in().terminal();
    let (rgba, _) = pixels.as_chunks::<4>();
    let colors: Vec<[u8; 3]> = (rgba.iter())
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect();
    let of = |color: [u8; 3]| colors.iter().filter(|pixel| **pixel == color).count();
    let filled = of(look.background);
    assert!(
        filled > colors.len() / 2,
        "{filled} of {} pixels are the background",
        colors.len()
    );
    let lit = of(look.foreground);
    assert!(lit > 20, "{lit} pixels of text");

    // Scrolled back by rows and half a row, the history is painted: the
    // rows of numbers, the top one cut by the box's edge.
    opened.run("seq 1 100");
    opened.wait("the last number", |text| {
        text.lines().any(|line| line == "100")
    });
    opened.wheel(40. * 18. + 9.);
    let started = Instant::now();
    while opened.terminal.scrolled_into() != Some(0.5) {
        assert!(started.elapsed() < Duration::from_secs(10), "not scrolled");
        opened.app.resolve();
        std::thread::sleep(Duration::from_millis(5));
    }
    renderer.render_to_vec(
        |scene| {
            blitz_paint::paint_scene(scene, &mut document.borrow_mut(), 1.0, width, height, 0, 0)
        },
        &mut pixels,
    );
    let (rgba, _) = pixels.as_chunks::<4>();
    let text_in = |rows: std::ops::Range<usize>| {
        (rgba[rows.start * width as usize..rows.end * width as usize].iter())
            .filter(|pixel| [pixel[0], pixel[1], pixel[2]] == look.foreground)
            .count()
    };
    assert!(text_in(0..height as usize) > 200, "the numbers");
    // The first whole row starts half a row down: its glyphs are in the
    // band a row would not have reached into from the top.
    assert!(text_in(9..27) > text_in(0..9), "a row half a row down");
}

#[test]
fn a_second_press_selects_the_word_and_a_third_the_line() {
    let opened = open();
    opened.run("echo one two three");
    opened.wait("the line", |text| {
        text.lines().any(|line| line == "one two three")
    });
    let row = opened.row_of("one two three");
    let (x, y) = opened.cell(row, 5);
    let press = || {
        pointer(&opened.app, x, y, Pointer::Down);
        pointer(&opened.app, x, y, Pointer::Up);
    };
    // The columns selected in the row, once they are `wanted`.
    let selected = |what: &str, wanted: &dyn Fn(usize, usize) -> bool| {
        let started = Instant::now();
        loop {
            opened.app.resolve();
            let selection = opened.terminal.frame().and_then(|frame| frame.selection);
            if let Some(selection) = selection
                && selection.start.row == row
                && wanted(selection.start.col, selection.end.col)
            {
                return;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "no {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    pointer(&opened.app, x, y, Pointer::Move);
    press();
    press();
    selected("word", &|from, to| (from, to) == (4, 6));
    press();
    selected("line", &|from, to| from == 0 && to >= 12);
}

/// A press of `button` with `mods` held, at a point of the page.
fn press_with(
    app: &App,
    x: f32,
    y: f32,
    button: horizon_ui::blitz_traits::events::MouseEventButton,
    mods: keyboard_types::Modifiers,
) {
    use horizon_ui::blitz_traits::events::{BlitzPointerEvent, BlitzPointerId, MouseEventButtons};
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
        button,
        buttons,
        mods,
        details: Default::default(),
        element: Default::default(),
        active_pointers: Default::default(),
    };
    app.dispatch(UiEvent::PointerDown(event(MouseEventButtons::Primary)));
    app.dispatch(UiEvent::PointerUp(event(MouseEventButtons::None)));
}

#[test]
fn a_program_that_asks_the_colors_is_told_the_ones_it_is_drawn_in() {
    let opened = open();
    let look = Theme::built_in().terminal();
    // The answer to OSC 11 comes in as if typed; the tty echoes it.
    opened.run(r"printf '\033]11;?\007'; sleep 0.5");
    let [r, g, b] = look.background;
    let told = format!("11;rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}");
    opened.wait("the background's color", |text| text.contains(&told));

    // (The answer is on the shell's line: Enter has the shell be done
    // with it.)
    opened.enter();
    opened.wait("the shell again", |text| text.trim_end().ends_with('$'));

    // Another look: another answer.
    let mut other = look.clone();
    other.background = [0x12, 0x34, 0x56];
    opened.look.set(other);
    opened.app.run_effects();
    opened.run(r"printf '\033]11;?\007'; sleep 0.5");
    opened.wait("the new background's color", |text| {
        text.contains("11;rgb:1212/3434/5656")
    });
}

/// What a link is clicked with to be followed: Cmd on macOS, Ctrl
/// elsewhere.
fn link_key() -> keyboard_types::Modifiers {
    if cfg!(target_os = "macos") {
        keyboard_types::Modifiers::META
    } else {
        keyboard_types::Modifiers::CONTROL
    }
}

#[test]
fn a_link_clicked_with_ctrl_or_cmd_is_opened() {
    use horizon_ui::blitz_traits::events::MouseEventButton;

    let opened = open();
    let opened_urls = opened.opened_urls.clone();
    opened.run(r"printf '\033]8;;https://example.com/x\033\\a-link\033]8;;\033\\ plain\n'");
    opened.wait("the link", |text| {
        text.lines().any(|line| line == "a-link plain")
    });
    let row = opened.row_of("a-link plain");
    let key = link_key();

    // On the link, with the key: opened, and nothing selected.
    let (x, y) = opened.cell(row, 2);
    pointer(&opened.app, x, y, Pointer::Move);
    press_with(&opened.app, x, y, MouseEventButton::Main, key);
    assert_eq!(*opened_urls.borrow(), ["https://example.com/x"]);
    // Beside the link, or without the key: not.
    let (x, y) = opened.cell(row, 9);
    pointer(&opened.app, x, y, Pointer::Move);
    press_with(&opened.app, x, y, MouseEventButton::Main, key);
    let (x, y) = opened.cell(row, 2);
    pointer(&opened.app, x, y, Pointer::Move);
    press_with(
        &opened.app,
        x,
        y,
        MouseEventButton::Main,
        Default::default(),
    );
    // Nor with Super off macOS: the desktop keeps it for itself.
    if !cfg!(target_os = "macos") {
        press_with(
            &opened.app,
            x,
            y,
            MouseEventButton::Main,
            keyboard_types::Modifiers::META,
        );
    }
    assert_eq!(opened_urls.borrow().len(), 1);
}

/// What a link is followed with is whoever placed the view's to say
/// (the shell opens a web page with the desktop and a file with an
/// extension): the view hands over a link of any scheme.
#[test]
fn a_link_of_any_scheme_is_handed_to_whoever_placed_the_view() {
    use horizon_ui::blitz_traits::events::MouseEventButton;

    let opened = open();
    opened.run(
        r"printf '\033]8;;file:///tmp/a.png\033\\a-file\033]8;;\033\\\n\033]8;;vscode://x\033\\a-scheme\033]8;;\033\\\n'",
    );
    opened.wait("the links", |text| {
        text.lines().any(|line| line == "a-file") && text.lines().any(|line| line == "a-scheme")
    });
    for line in ["a-file", "a-scheme"] {
        let (x, y) = opened.cell(opened.row_of(line), 2);
        pointer(&opened.app, x, y, Pointer::Move);
        press_with(&opened.app, x, y, MouseEventButton::Main, link_key());
    }
    assert_eq!(
        *opened.opened_urls.borrow(),
        ["file:///tmp/a.png", "vscode://x"]
    );
}

#[test]
fn a_selection_is_the_primary_selection_and_the_middle_button_pastes_it() {
    use std::sync::Mutex;

    use horizon_ui::blitz_traits::events::MouseEventButton;

    static SET: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static ASKED: Mutex<usize> = Mutex::new(0);
    horizon::primary::connect(
        |text| SET.lock().unwrap().push(text),
        || *ASKED.lock().unwrap() += 1,
    );

    let opened = open();
    opened.run("echo select-me-please");
    opened.wait("the line", |text| {
        text.lines().any(|line| line == "select-me-please")
    });
    let row = opened.row_of("select-me-please");
    let (x, y) = opened.cell(row, 0);
    pointer(&opened.app, x, y, Pointer::Move);
    pointer(&opened.app, x, y, Pointer::Down);
    let (x, y) = opened.cell(row, 5);
    pointer(&opened.app, x, y, Pointer::Drag);
    pointer(&opened.app, x, y, Pointer::Up);
    let started = Instant::now();
    while SET.lock().unwrap().last().map(String::as_str) != Some("select") {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "not the primary selection: {:?}",
            SET.lock().unwrap()
        );
        opened.app.resolve();
        std::thread::sleep(Duration::from_millis(5));
    }

    press_with(
        &opened.app,
        x,
        y,
        MouseEventButton::Auxiliary,
        Default::default(),
    );
    assert_eq!(*ASKED.lock().unwrap(), 1);
}

#[test]
fn block_characters_fill_their_cells_with_no_seam_between_them() {
    use anyrender::ImageRenderer as _;

    let opened = open();
    // Two rows of three full blocks, then a row of lower halves, and the
    // art a program draws with such characters.
    opened.run(r"clear; printf '\342\226\210\342\226\210\342\226\210\n\342\226\210\342\226\210\342\226\210\n\342\226\204\342\226\204\342\226\204\n'; printf ' \342\226\220\342\226\233\342\226\210\342\226\210\342\226\210\342\226\234\342\226\214\n\342\226\235\342\226\234\342\226\210\342\226\210\342\226\210\342\226\210\342\226\210\342\226\233\342\226\230\n  \342\226\230\342\226\230 \342\226\235\342\226\235\n'");
    opened.wait("the blocks", |text| text.contains("▘▘ ▝▝"));
    let row = opened.row_of("███");
    let (width, height) = (900, 540);
    let mut renderer = anyrender_vello::VelloImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    let document = opened.app.document();
    renderer.render_to_vec(
        |scene| {
            blitz_paint::paint_scene(scene, &mut document.borrow_mut(), 1.0, width, height, 0, 0)
        },
        &mut pixels,
    );
    // (For a person to look at: the page as raw RGBA.)
    if let Some(path) = std::env::var_os("HORIZON_TEST_DUMP") {
        std::fs::write(path, &pixels).unwrap();
    }
    let look = Theme::built_in().terminal();
    let at = |x: usize, y: usize| {
        let pixel = (y * width as usize + x) * 4;
        [pixels[pixel], pixels[pixel + 1], pixels[pixel + 2]]
    };
    // The box of the six full blocks: from the first cell's left edge to
    // the third's right, two rows tall.
    let (left, top) = opened.cell(row, 0);
    let (right, _) = opened.cell(row, 2);
    let half = (right - left) / 4.;
    // (The cells' edges are on whole pixels: the last column of pixels
    // the estimate reaches may be the next cell's.)
    let (x0, x1) = (
        (left - half).round() as usize,
        (right + half).round() as usize - 1,
    );
    let (y0, y1) = ((top - 9.).round() as usize, (top + 27.).round() as usize);
    let mut other = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            if at(x, y) != look.foreground {
                other += 1;
            }
        }
    }
    assert_eq!(other, 0, "of {} pixels", (x1 - x0) * (y1 - y0));
    // The lower halves under them: the row's top half is the background,
    // its lower half the foreground, across all three cells.
    let middle = (x0 + x1) / 2;
    assert_eq!(at(middle, y1 + 3), look.background);
    assert_eq!(at(middle, y1 + 14), look.foreground);
    assert_eq!(at(x0 + 1, y1 + 14), look.foreground);
    assert_eq!(at(x1 - 2, y1 + 14), look.foreground);
}

#[test]
fn blocks_have_no_seam_in_a_view_that_is_not_on_whole_pixels() {
    use anyrender::ImageRenderer as _;

    // A display that is not at one device pixel to the CSS pixel, and a
    // view whose box starts at an odd CSS pixel: its place on the display
    // is part of a device pixel in, as a pane's beside a split is.
    let scale = 1.5;
    let opened = open_at("padding: 21px 0 0 31px;");
    {
        use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};
        let document = opened.app.document();
        (document.borrow_mut()).set_viewport(Viewport::new(900, 540, scale, ColorScheme::Light));
    }
    opened.app.resolve();
    opened.run(r"clear; printf '\342\226\210\342\226\210\342\226\210\342\226\210\n\342\226\210\342\226\210\342\226\210\342\226\210\n\342\226\210\342\226\210\342\226\210\342\226\210\n'; printf ' \342\226\220\342\226\233\342\226\210\342\226\210\342\226\210\342\226\234\342\226\214\n\342\226\235\342\226\234\342\226\210\342\226\210\342\226\210\342\226\210\342\226\210\342\226\233\342\226\230\n  \342\226\230\342\226\230 \342\226\235\342\226\235\n'");
    opened.wait("the blocks", |text| text.contains("▘▘ ▝▝"));
    opened.app.resolve();
    let (width, height) = (900, 540);
    let mut renderer = anyrender_vello::VelloImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    let document = opened.app.document();
    renderer.render_to_vec(
        |scene| {
            let mut document = document.borrow_mut();
            blitz_paint::paint_scene(scene, &mut document, scale as f64, width, height, 0, 0)
        },
        &mut pixels,
    );
    if let Some(path) = std::env::var_os("HORIZON_TEST_DUMP") {
        std::fs::write(path, &pixels).unwrap();
    }
    let look = Theme::built_in().terminal();
    let at = |x: usize, y: usize| {
        let pixel = (y * width as usize + x) * 4;
        [pixels[pixel], pixels[pixel + 1], pixels[pixel + 2]]
    };
    // The blocks' box: where the page is the foreground, in the rows the
    // blocks are in (above the prompt's own text).
    let inked: Vec<(usize, usize)> = (0..105)
        .flat_map(|y| (0..300).map(move |x| (x, y)))
        .filter(|(x, y)| at(*x, *y) == look.foreground)
        .collect();
    let (x0, x1) = (
        inked.iter().map(|(x, _)| *x).min().unwrap(),
        inked.iter().map(|(x, _)| *x).max().unwrap(),
    );
    let (y0, y1) = (
        inked.iter().map(|(_, y)| *y).min().unwrap(),
        inked.iter().map(|(_, y)| *y).max().unwrap(),
    );
    assert!(x1 - x0 > 20 && y1 - y0 > 40, "{x0}..{x1} {y0}..{y1}");
    // Inside it every pixel is the foreground: no line where two cells
    // meet.
    let seams: Vec<(usize, usize)> = (y0..=y1)
        .flat_map(|y| (x0..=x1).map(move |x| (x, y)))
        .filter(|(x, y)| at(*x, *y) != look.foreground)
        .collect();
    assert!(
        seams.is_empty(),
        "{} pixels, the first at {:?}",
        seams.len(),
        seams[0]
    );
}

/// The page as it is painted: each pixel's color, a row of pixels after
/// another, 900 across.
fn painted(opened: &Opened) -> Vec<[u8; 3]> {
    use anyrender::ImageRenderer as _;

    let (width, height) = (900, 540);
    let mut renderer = anyrender_vello::VelloImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    let document = opened.app.document();
    renderer.render_to_vec(
        |scene| {
            blitz_paint::paint_scene(scene, &mut document.borrow_mut(), 1.0, width, height, 0, 0)
        },
        &mut pixels,
    );
    let (rgba, _) = pixels.as_chunks::<4>();
    (rgba.iter())
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

/// The most pixels of `color` any one row of pixels has, among those of
/// the screen's row `row`.
fn longest_line_of(pixels: &[[u8; 3]], row: usize, color: [u8; 3]) -> usize {
    (row * 18..(row + 1) * 18)
        .map(|y| {
            (pixels[y * 900..(y + 1) * 900].iter())
                .filter(|pixel| **pixel == color)
                .count()
        })
        .max()
        .unwrap_or(0)
}

#[test]
fn text_is_underlined_and_struck_through_in_the_colors_it_was_written_with() {
    let opened = open();
    // Six cells underlined in red, six struck through in blue, six with
    // two lines under them in the text's own color.
    opened.run(
        r"clear; printf '\033[4;58;2;255;0;0munders\033[0m\n\033[9;38;2;0;0;255mstruck\033[0m\n\033[4:2;38;2;0;160;0mdouble\033[0m\n'",
    );
    opened.done("the first rows", |text| {
        text.lines().any(|line| line == "double")
    });
    // (And the other lines there are, for a person to look at.)
    opened.run(
        r"printf '\033[4:3;58;2;255;0;0mcurled\033[0m \033[4:4mdotted\033[0m \033[4:5mdashed\033[0m \033[3mitalic\033[0m\n'",
    );
    opened.wait("the rows", |text| text.contains("dashed italic"));
    let pixels = painted(&opened);
    if let Some(path) = std::env::var_os("HORIZON_TEST_DUMP") {
        std::fs::write(path, pixels.as_flattened()).unwrap();
    }
    let (first, second) = (opened.cell(0, 0).0, opened.cell(0, 1).0);
    // (A line's two ends are on whole pixels: a pixel short at each.)
    let six_cells = ((second - first) * 6.) as usize - 2;

    let under = longest_line_of(&pixels, opened.row_of("unders"), [255, 0, 0]);
    assert!(under >= six_cells, "{under} pixels of a red line");
    let through = longest_line_of(&pixels, opened.row_of("struck"), [0, 0, 255]);
    assert!(through >= six_cells, "{through} pixels of a blue line");
    // Two lines: two rows of pixels that are the color all the way across.
    let row = opened.row_of("double");
    let lines = (row * 18..(row + 1) * 18)
        .filter(|y| {
            let across = &pixels[y * 900..(y + 1) * 900];
            across.iter().filter(|pixel| **pixel == [0, 160, 0]).count() >= six_cells
        })
        .count();
    assert_eq!(lines, 2, "rows of pixels that are a line");
    // And none where nothing was asked for.
    let plain = longest_line_of(&pixels, opened.row_of("double") + 1, [0, 160, 0]);
    assert_eq!(plain, 0);
}

#[test]
fn italics_are_not_drawn_as_upright_text() {
    let opened = open();
    opened.run(r"clear; printf 'llllllll\n\033[3mllllllll\033[0m\n'");
    opened.wait("the rows", |text| {
        text.lines().filter(|line| *line == "llllllll").count() == 2
    });
    let pixels = painted(&opened);
    let row = opened.row_of("llllllll");
    let band = |row: usize| &pixels[row * 18 * 900..(row + 1) * 18 * 900];
    assert!(band(row) != band(row + 1), "the two rows are the same");
}

#[test]
fn the_colors_a_program_sets_are_the_ones_it_is_drawn_in() {
    let opened = open();
    // The background (OSC 11) and the palette's red (OSC 4).
    opened.run(
        r"clear; printf '\033]11;rgb:10/20/30\007\033]4;1;rgb:aa/bb/cc\007\033[41m      \033[0m\n'; echo set",
    );
    opened.wait("the line", |text| text.lines().any(|line| line == "set"));
    let started = Instant::now();
    loop {
        let pixels = painted(&opened);
        let of = |color: [u8; 3]| pixels.iter().filter(|pixel| **pixel == color).count();
        if of([0x10, 0x20, 0x30]) > pixels.len() / 2 && of([0xaa, 0xbb, 0xcc]) > 6 * 5 * 18 {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{} pixels of the background set and {} of the red set",
            of([0x10, 0x20, 0x30]),
            of([0xaa, 0xbb, 0xcc]),
        );
        opened.app.resolve();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_drag_goes_on_outside_the_view_and_ends_where_the_button_comes_up() {
    // The view in a part of the page, with page to its right and below.
    let opened = open_at("width: 450px; height: 270px;");
    opened.run("echo select-me-please");
    opened.wait("the line", |text| {
        text.lines().any(|line| line == "select-me-please")
    });
    let row = opened.row_of("select-me-please");
    let (columns, rows) = opened.terminal.size();
    let width = 450. / (columns as f32 + 0.5);
    let cell = |column: usize| ((column as f32 + 0.5) * width, (row as f32 + 0.5) * 18.);
    // The selection, once it is `wanted` (or whatever it is by then).
    let selection = |wanted: Option<((usize, usize), (usize, usize))>| {
        let started = Instant::now();
        loop {
            opened.app.resolve();
            let selection = opened.terminal.frame().and_then(|frame| frame.selection);
            let ends = selection.map(|s| ((s.start.row, s.start.col), (s.end.row, s.end.col)));
            if ends == wanted || started.elapsed() > Duration::from_secs(5) {
                return ends;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };

    let (x, y) = cell(2);
    pointer(&opened.app, x, y, Pointer::Move);
    pointer(&opened.app, x, y, Pointer::Down);
    let (x, y) = cell(5);
    pointer(&opened.app, x, y, Pointer::Drag);
    let inside = Some(((row, 2), (row, 5)));
    assert_eq!(selection(inside), inside);

    // Out of the box, to the right of it and below it: the selection
    // goes to the screen's last cell, the nearest to the pointer.
    pointer(&opened.app, 700., 400., Pointer::Drag);
    let outside = Some(((row, 2), (rows - 1, columns - 1)));
    assert_eq!(selection(outside), outside);

    // The button comes up out there: a move back in, with no button
    // held, selects nothing more.
    pointer(&opened.app, 700., 400., Pointer::Up);
    let (x, y) = cell(9);
    pointer(&opened.app, x, y, Pointer::Move);
    for _ in 0..20 {
        opened.app.resolve();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(selection(outside), outside);

    // A drag back from where it began takes in both its ends.
    let (x, y) = cell(8);
    pointer(&opened.app, x, y, Pointer::Down);
    let (x, y) = cell(4);
    pointer(&opened.app, x, y, Pointer::Drag);
    pointer(&opened.app, x, y, Pointer::Up);
    let backwards = Some(((row, 4), (row, 8)));
    assert_eq!(selection(backwards), backwards);
}

#[test]
fn a_notch_of_the_wheel_is_one_report_to_a_program_that_asked_for_the_mouse() {
    let opened = open();
    // A program that asks for the mouse, in SGR's form, and reads what it
    // is sent: the terminal's own echo shows each report as it is written.
    opened.run(r"clear; printf '\033[?1000h\033[?1006h'; cat");
    // The wheel is the program's once the frame the view has says it
    // asked for the mouse: before that it scrolls the history, and nothing
    // is reported. (The program asks some time after the screen is
    // cleared, as long after as the machine takes to start it.)
    opened.until("the mouse asked for", || {
        (opened.terminal.frame()).is_some_and(|frame| frame.mouse_reporting)
    });

    move_to(&opened.app, 300., 200.);
    opened.app.dispatch(UiEvent::Wheel(BlitzWheelEvent {
        delta: BlitzWheelDelta::Lines(0., 1.),
        coords: PointerCoords {
            page_x: 300.,
            page_y: 200.,
            screen_x: 300.,
            screen_y: 200.,
            client_x: 300.,
            client_y: 200.,
        },
        buttons: Default::default(),
        mods: Default::default(),
        element: Default::default(),
    }));
    opened.wait("the report", |text| text.contains("[<64;"));
    // (Time for more of them to come, were there more.)
    std::thread::sleep(Duration::from_millis(200));
    let text = opened.wait("the report", |text| text.contains("[<64;"));
    assert_eq!(text.matches("[<64;").count(), 1, "in:\n{text}");
}

/// Counts the times it is woken.
struct Woken(std::sync::atomic::AtomicUsize);

impl std::task::Wake for Woken {
    fn wake(self: std::sync::Arc<Self>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn the_wheel_over_the_window_held_has_the_page_drawn_again() {
    use std::sync::atomic::Ordering;

    let opened = open();
    opened.run("seq 1 100");
    opened.wait("the last number", |text| {
        text.lines().any(|line| line == "100")
    });
    // Back into the history: a window of it is held from here on.
    opened.wheel(10. * 18.);
    let started = Instant::now();
    while opened.terminal.scrolled_into().is_none() {
        assert!(started.elapsed() < Duration::from_secs(10), "not scrolled");
        opened.app.resolve();
        std::thread::sleep(Duration::from_millis(5));
    }

    // The shell is quiet: nothing arrives to wake the page. A turn of the
    // wheel moves the view over the window it holds, and the page is asked
    // to draw that.
    let woken = std::sync::Arc::new(Woken(Default::default()));
    opened.terminal.set_waker(woken.clone().into());
    std::thread::sleep(Duration::from_millis(100));
    let before = woken.0.load(Ordering::SeqCst);
    opened.wheel(9.);
    assert!(
        woken.0.load(Ordering::SeqCst) > before,
        "the page was not asked to draw the view where the wheel put it"
    );
    opened.app.resolve();
    assert_eq!(opened.terminal.scrolled_into(), Some(0.5));
}

/// More output than a view is sent: the daemon stops sending
/// while the view is behind and sends the terminal's state when it has
/// caught up, and the view ends with the screen the shell has.
#[test]
fn a_flood_of_output_ends_with_the_screen_the_shell_has() {
    let opened = open();
    let states_before = opened.session.states_taken();
    let started = Instant::now();
    // About 5.4 MB: past what a view may be sent and not take in (2 MB)
    // while it takes in nothing, and short enough not to hold the
    // machine's cores from the tests beside it.
    opened.run("yes flooding | head -n 600000; echo flood-over");
    // The view takes nothing in for a while: it falls behind.
    std::thread::sleep(Duration::from_millis(1500));
    let text = loop {
        opened.app.resolve();
        let text = opened.terminal.text();
        // (The prompt follows the mark, a moment after it.)
        if text.lines().any(|line| line == "flood-over") && text.trim_end().ends_with('$') {
            break text;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the flood did not end ({} states, {} bytes taken) in:\n{text}",
            opened.session.states_taken(),
            opened.session.bytes_taken(),
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    let took = started.elapsed();
    // The last of the flood, then the mark, then the prompt.
    let lines: Vec<&str> = text.lines().collect();
    let mark = lines.iter().position(|line| *line == "flood-over").unwrap();
    assert!(mark > 1, "in:\n{text}");
    assert_eq!(lines[mark - 1], "flooding", "in:\n{text}");
    assert!(lines[mark + 1].trim_end().ends_with('$'), "in:\n{text}");
    let again = opened.session.states_taken() - states_before;
    println!(
        "5.4 MB of output in {took:?}; the state was sent again {again} times; {} bytes of output were taken in",
        opened.session.bytes_taken(),
    );
    assert!(again > 0, "the view never fell behind");

    // And the terminal goes on from there: what is typed next runs.
    opened.run("echo after-the-flood");
    opened.wait("the next command", |text| {
        text.lines().any(|line| line == "after-the-flood")
    });
}

/// A synchronized update the program never closes is let go by the
/// daemon's clock, and the view lets go where the daemon did, with no
/// output after it to say so.
#[test]
fn an_update_held_and_never_let_go_is_shown_when_the_daemon_lets_it_go() {
    let opened = open();
    // (`\154` is `l`: the line typed is not the text held.)
    opened.run(r"printf '\033[?2026hhe\154d-text'; sleep 3");
    let started = Instant::now();
    opened.wait("the held text", |text| text.contains("held-text"));
    // Long before `sleep` ends and the prompt comes.
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// The page painted on the GPU with Vello, the way the window paints it:
/// a terminal keeps its rows as pixels on that renderer's device.
fn paint_on_gpu(
    opened: &Opened,
    renderer: &mut anyrender_vello::VelloImageRenderer,
    scale: f32,
    (width, height): (u32, u32),
) -> Vec<u8> {
    use anyrender::ImageRenderer as _;
    opened.app.resolve();
    let mut pixels = Vec::new();
    let document = opened.app.document();
    renderer.render_to_vec(
        |scene| {
            let mut document = document.borrow_mut();
            blitz_paint::paint_scene(scene, &mut document, scale as f64, width, height, 0, 0)
        },
        &mut pixels,
    );
    pixels
}

/// The screen after `change`, painted from the rows a renderer kept of it
/// before (`setup`'s screen painted first), and painted anew on another
/// renderer, at `scale`: the two must be the same pixels.
fn kept_rows_paint_what_painting_anew_paints(
    what: &str,
    scale: f32,
    setup: impl Fn(&Opened),
    change: impl Fn(&Opened),
) {
    use anyrender::ImageRenderer as _;

    let opened = open();
    let size = ((900. * scale).round() as u32, (540. * scale).round() as u32);
    (opened.app.document().borrow_mut()).set_viewport(Viewport::new(
        size.0,
        size.1,
        scale,
        ColorScheme::Light,
    ));
    opened.app.resolve();
    setup(&opened);
    let mut kept = anyrender_vello::VelloImageRenderer::new(size.0, size.1);
    paint_on_gpu(&opened, &mut kept, scale, size);
    change(&opened);
    let from_kept = paint_on_gpu(&opened, &mut kept, scale, size);
    // The window lets go of its renderer this way before it takes another
    // (`blitz-shell`'s suspend): what was kept on the one before is gone.
    assert!(
        opened.terminal.painted_from_kept_rows(),
        "{what} at {scale}"
    );
    opened.app.document().borrow_mut().destroy_surfaces();
    let mut another = anyrender_vello::VelloImageRenderer::new(size.0, size.1);
    let anew = paint_on_gpu(&opened, &mut another, scale, size);
    assert!(
        opened.terminal.painted_from_kept_rows(),
        "{what} at {scale}"
    );
    let (kept, anew) = (from_kept.as_chunks::<4>().0, anew.as_chunks::<4>().0);
    let differing: Vec<usize> = (0..kept.len())
        .filter(|at| kept[*at] != anew[*at])
        .collect();
    let first = (differing.first()).map(|at| {
        let place = (at % size.0 as usize, at / size.0 as usize);
        (place, kept[*at], anew[*at])
    });
    // The two renderers may differ by a little at a few edges of glyphs:
    // with the glyphs' hinting off they are the same pixels, so what is
    // left is Vello's hinted glyphs, which a renderer that drew other text
    // before draws a shade apart. A row in the wrong slot, or a slot not
    // drawn, is a whole row's text apart.
    let far = (differing.iter()).find(|at| {
        let (kept, anew) = (kept[**at], anew[**at]);
        (0..3).any(|channel| kept[channel].abs_diff(anew[channel]) > 32)
    });
    assert!(
        far.is_none() && differing.len() < 50,
        "{what} at {scale}: {} pixels differ, the first (place, kept, anew) {first:?}",
        differing.len()
    );
    // Something was painted: the screen's text.
    let look = Theme::built_in().terminal();
    let lit = (anew.iter())
        .filter(|pixel| [pixel[0], pixel[1], pixel[2]] == look.foreground)
        .count();
    assert!(lit > 20, "{what} at {scale}: {lit} pixels of text");
}

/// Waits for a screen of the numbers 1 to `last`, at the prompt.
fn numbers(opened: &Opened, last: usize) {
    opened.run(&format!("seq 1 {last}"));
    opened.done("the last number", |text| {
        text.lines().any(|line| line == last.to_string())
    });
}

#[test]
fn rows_kept_after_output_scrolls_paint_the_screen_painted_anew() {
    for scale in [1.25, 1.5] {
        kept_rows_paint_what_painting_anew_paints(
            "output that scrolls",
            scale,
            |opened| numbers(opened, 30),
            |opened| {
                opened.run("seq 31 33");
                opened.done("the new numbers", |text| {
                    text.lines().any(|line| line == "33")
                });
            },
        );
    }
}

#[test]
fn rows_kept_after_part_of_the_screen_scrolls_paint_the_screen_painted_anew() {
    for scale in [1.25, 1.5] {
        kept_rows_paint_what_painting_anew_paints(
            "a scroll of part of the screen",
            scale,
            |opened| numbers(opened, 30),
            |opened| {
                // Rows 3 to 12 scroll up by three, the rest stay.
                opened.run(
                    r"printf '\033[3;12r\033[12;1H\n\n\n\033[r\033[99;1H'; echo scrolled-part",
                );
                opened.done("the scroll", |text| {
                    text.lines().any(|line| line == "scrolled-part")
                });
            },
        );
    }
}

#[test]
fn rows_kept_while_scrolled_back_into_a_row_paint_the_history_painted_anew() {
    for scale in [1.25, 1.5] {
        kept_rows_paint_what_painting_anew_paints(
            "history scrolled back",
            scale,
            |opened| numbers(opened, 100),
            |opened| {
                opened.wheel(40. * 18. + 9.);
                opened.until("the history", || {
                    opened
                        .terminal
                        .scrolled_into()
                        .is_some_and(|into| into > 0.)
                });
            },
        );
    }
}

#[test]
fn rows_kept_after_a_selection_paint_the_screen_painted_anew() {
    for scale in [1.25, 1.5] {
        kept_rows_paint_what_painting_anew_paints(
            "a selection",
            scale,
            |opened| {
                opened.run("echo select-me-please");
                opened.done("the line", |text| {
                    text.lines().any(|line| line == "select-me-please")
                });
            },
            |opened| {
                let row = opened.row_of("select-me-please");
                let (x, y) = opened.cell(row, 0);
                pointer(&opened.app, x, y, Pointer::Move);
                pointer(&opened.app, x, y, Pointer::Down);
                let (x, y) = opened.cell(row, 5);
                pointer(&opened.app, x, y, Pointer::Drag);
                pointer(&opened.app, x, y, Pointer::Up);
                opened.until("the selection", || {
                    (opened.terminal.frame()).is_some_and(|frame| frame.selection.is_some())
                });
            },
        );
    }
}
