//! The terminal view over a real shell on a PTY: what is typed, composed,
//! pasted, dragged and scrolled reaches the shell, and what the shell
//! prints is the view's frame.

use std::time::{Duration, Instant};

use horizon::terminal::{CSS, Terminal, view};
use horizon_terminal_core::TerminalSize;
use horizon_theme_css::Theme;
use horizon_ui::App;
use horizon_ui::blitz_traits::events::{
    BlitzImeEvent, BlitzKeyEvent, BlitzWheelDelta, BlitzWheelEvent, KeyState, PointerCoords,
    UiEvent,
};
use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};
use horizon_ui::testing::{Pointer, move_to, pointer};

mod support;

struct Opened {
    app: App,
    terminal: Terminal,
}

fn open() -> Opened {
    open_at("")
}

/// The view in a box with `style`: where on the page it is.
fn open_at(style: &str) -> Opened {
    let session = support::shell("sh", TerminalSize::new(80, 24));
    let mut terminal = None;
    let app = App::new(900, 540, CSS, || {
        let (root, state) = view(session.into(), Theme::built_in().terminal());
        terminal = Some(state);
        root.focus();
        horizon_ui::el("div")
            .attr(
                "style",
                &format!("height: 100%; box-sizing: border-box; {style}"),
            )
            .child(root)
    });
    app.resolve();
    let opened = Opened {
        app,
        terminal: terminal.unwrap(),
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
    opened.wait("the shell again", |text| text.contains("after-2"));

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
    opened.wait("the size", |text| {
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
fn a_drag_selects_cells_and_the_copy_chord_copies_them() {
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

    opened.key(
        keyboard_types::Key::Character("C".into()),
        keyboard_types::Modifiers::CONTROL | keyboard_types::Modifiers::SHIFT,
    );
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
fn the_view_knows_when_the_shell_has_gone() {
    let opened = open();
    assert!(!opened.terminal.exited());
    opened.run("exit");
    let started = Instant::now();
    while !opened.terminal.exited() {
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
    opened.terminal.set_look(other);
    opened.run(r"printf '\033]11;?\007'; sleep 0.5");
    opened.wait("the new background's color", |text| {
        text.contains("11;rgb:1212/3434/5656")
    });
}

#[test]
fn a_link_clicked_with_the_platform_s_key_is_opened() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use horizon_ui::blitz_traits::events::MouseEventButton;

    let opened = open();
    let opened_urls = Rc::new(RefCell::new(Vec::new()));
    let log = opened_urls.clone();
    (opened.terminal).on_open_url(Rc::new(move |url| log.borrow_mut().push(url.to_string())));
    opened.run(r"printf '\033]8;;https://example.com/x\033\\a-link\033]8;;\033\\ plain\n'");
    opened.wait("the link", |text| {
        text.lines().any(|line| line == "a-link plain")
    });
    let row = opened.row_of("a-link plain");
    let meta = keyboard_types::Modifiers::META;

    // On the link, with the key: opened, and nothing selected.
    let (x, y) = opened.cell(row, 2);
    pointer(&opened.app, x, y, Pointer::Move);
    press_with(&opened.app, x, y, MouseEventButton::Main, meta);
    assert_eq!(*opened_urls.borrow(), ["https://example.com/x"]);
    // Beside the link, or without the key: not.
    let (x, y) = opened.cell(row, 9);
    pointer(&opened.app, x, y, Pointer::Move);
    press_with(&opened.app, x, y, MouseEventButton::Main, meta);
    let (x, y) = opened.cell(row, 2);
    pointer(&opened.app, x, y, Pointer::Move);
    press_with(
        &opened.app,
        x,
        y,
        MouseEventButton::Main,
        Default::default(),
    );
    assert_eq!(opened_urls.borrow().len(), 1);
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
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
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
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
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
