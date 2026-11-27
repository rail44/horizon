//! The controls a view places (a slider, a select, a color picker),
//! driven with the document's own events, and looked at as pixels where
//! what matters is what is painted.

use std::cell::RefCell;
use std::rc::Rc;

use horizon_components::{
    AlertDialog, ColorPicker, MenuButton, MenuItem, MenuLabel, ROW_SELECTED, Range, Select, Slider,
    list_row,
};
use horizon_theme_css::{Theme, themed};
use horizon_ui::blitz_dom::NodeId;
use horizon_ui::blitz_traits::events::{BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set};
use horizon_ui::testing::{Pointer, click_at, pointer};
use horizon_ui::{App, Callback, Component, El, el};
use keyboard_types::{Key, Modifiers, NamedKey};

const HOST: &str = "
    .host { position: relative; width: 400px; height: 400px; padding: 20px;
            box-sizing: border-box; background: var(--background); }
    .other { width: 40px; height: 20px; margin-top: 240px; outline: none; }
    .within { width: 200px; height: 200px; overflow-y: scroll; }
    .filler { height: 170px; }
";

/// A page with what `build` makes on it, and beside it something else
/// that takes the focus.
fn open(build: impl FnOnce() -> El) -> App {
    open_with(build)
}

/// [`open`], for a test whose own `open` is a signal.
fn open_with(build: impl FnOnce() -> El) -> App {
    let sheet = [horizon_theme_css::CSS, HOST].concat();
    let app = App::new(400, 400, "Test", &sheet, || {
        let host = el("div")
            .class("host")
            .child(build())
            .child(el("div").class("other").attr("tabindex", "0"));
        themed(Theme::built_in, host)
    });
    app.resolve();
    app
}

/// The elements of a class on the page, in document order. (The whole
/// page: what a test puts on it is the component it checks, and an
/// element of its own with no sheet's class.)
fn all(app: &App, class: &str) -> Vec<NodeId> {
    let top = app.document().borrow().root_element().id;
    horizon_ui::testing::find(app, top, class)
}

/// A node's box: x, y, width, height.
fn bounds(app: &App, node: NodeId) -> (f32, f32, f32, f32) {
    let document = app.document();
    let document = document.borrow();
    let bounds = document.get_client_bounding_rect(node).expect("a box");
    (
        bounds.x as f32,
        bounds.y as f32,
        bounds.width as f32,
        bounds.height as f32,
    )
}

/// A point of a node's box, by its shares of the width and the height.
fn point(app: &App, node: NodeId, across: f32, down: f32) -> (f32, f32) {
    let (x, y, width, height) = bounds(app, node);
    (x + width * across, y + height * down)
}

fn click(app: &App, node: NodeId, across: f32, down: f32) {
    let (x, y) = point(app, node, across, down);
    click_at(app, x, y);
    app.resolve();
}

fn key(app: &App, key: Key, modifiers: Modifiers) {
    app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
        key,
        code: keyboard_types::Code::Unidentified,
        modifiers,
        location: keyboard_types::Location::Standard,
        is_auto_repeating: false,
        is_composing: false,
        state: KeyState::Pressed,
        text: None,
    }));
    app.resolve();
}

fn named(app: &App, named: NamedKey) {
    key(app, Key::Named(named), Modifiers::default());
}

/// The page as pixels, and the color at a point of it.
fn painted(app: &App) -> impl Fn(f32, f32) -> [u8; 3] {
    use anyrender::ImageRenderer as _;

    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(400, 400);
    let mut pixels = Vec::new();
    let document = app.document();
    renderer.render_to_vec(
        |scene| blitz_paint::paint_scene(scene, &mut document.borrow_mut(), 1.0, 400, 400, 0, 0),
        &mut pixels,
    );
    move |x: f32, y: f32| {
        let at = (y as usize * 400 + x as usize) * 4;
        [pixels[at], pixels[at + 1], pixels[at + 2]]
    }
}

#[test]
fn list_rows_draw_an_accent_border_for_selection_and_a_band_on_hover() {
    let app = open(|| {
        let row = || list_row().attr("style", "width: 100px; height: 28px;");
        el("div")
            .attr("style", "--accent: #ff0000; --band: #00ff00;")
            .child(row())
            .child(row().class(ROW_SELECTED))
    });
    let rows = all(&app, "list-row");
    let [accent, band] = [[255, 0, 0], [0, 255, 0]];
    let ground = Theme::built_in().terminal().background;
    let color = painted(&app);
    let top = |row| point(&app, row, 0.5, 0.0);
    let middle = |row| point(&app, row, 0.5, 0.5);
    let at = |paint: &dyn Fn(f32, f32) -> [u8; 3], (x, y)| paint(x, y);
    assert_eq!(at(&color, top(rows[0])), ground);
    assert_eq!(at(&color, top(rows[1])), accent);
    assert_eq!(at(&color, point(&app, rows[1], 0.0, 0.0)), ground);
    assert_eq!(at(&color, middle(rows[1])), ground);

    for row in rows.iter().copied() {
        let (x, y) = middle(row);
        pointer(&app, x, y, Pointer::Move);
        app.resolve();
        let color = painted(&app);
        assert_eq!(color(x, y), band);
        assert_eq!(at(&color, top(rows[1])), accent);
    }
}

#[test]
fn a_slider_follows_a_press_a_drag_and_the_keys_in_its_steps() {
    let value = RwSignal::new(5.);
    let heard = Rc::new(RefCell::new(Vec::new()));
    let log = heard.clone();
    let range = Range {
        min: 0.,
        max: 10.,
        step: 0.5,
    };
    let app = open(|| {
        Slider {
            value,
            range,
            on_change: Callback::new(move |to| log.borrow_mut().push(to)),
        }
        .view()
    });
    let track = all(&app, "track")[0];
    // The thumb is where the value is along the track.
    let (track_x, _, track_width, _) = bounds(&app, track);
    let thumb_middle = |app: &App| {
        let (x, _, width, _) = bounds(app, all(app, "thumb")[0]);
        x + width / 2.
    };
    assert!((thumb_middle(&app) - (track_x + track_width / 2.)).abs() < 1.);

    // A press goes to the step nearest the point, and a drag goes on
    // from there wherever the pointer is.
    let (x, y) = point(&app, track, 0.26, 0.5);
    pointer(&app, x, y, Pointer::Move);
    pointer(&app, x, y, Pointer::Down);
    app.resolve();
    assert_eq!(value.get_untracked(), 2.5);
    let (x, _) = point(&app, track, 0.8, 0.5);
    pointer(&app, x, y + 120., Pointer::Drag);
    app.resolve();
    assert_eq!(value.get_untracked(), 8.);
    pointer(&app, x, y + 120., Pointer::Up);
    let (x, _) = point(&app, track, 0.1, 0.5);
    pointer(&app, x, y + 120., Pointer::Move);
    app.resolve();
    assert_eq!(value.get_untracked(), 8.);
    assert!((thumb_middle(&app) - (track_x + track_width * 0.8)).abs() < 1.);

    // The press gave it the keys.
    named(&app, NamedKey::ArrowRight);
    assert_eq!(value.get_untracked(), 8.5);
    key(&app, Key::Named(NamedKey::ArrowLeft), Modifiers::SHIFT);
    assert_eq!(value.get_untracked(), 3.5);
    named(&app, NamedKey::End);
    assert_eq!(value.get_untracked(), 10.);
    named(&app, NamedKey::ArrowRight);
    named(&app, NamedKey::Home);
    assert_eq!(value.get_untracked(), 0.);
    assert_eq!(*heard.borrow(), [2.5, 8., 8.5, 3.5, 10., 0.]);
}

#[test]
fn a_select_opens_its_options_and_one_is_chosen_by_click_or_key() {
    let chosen = RwSignal::new(0usize);
    let heard = Rc::new(RefCell::new(Vec::new()));
    let log = heard.clone();
    let options = ["red", "green", "custom"].map(String::from).to_vec();
    let app = open(|| {
        Select {
            options,
            chosen,
            on_change: Callback::new(move |index| log.borrow_mut().push(index)),
        }
        .view()
    });
    let text = |app: &App, node: NodeId| {
        let document = app.document();
        let document = document.borrow();
        document.get_node(node).unwrap().text_content()
    };
    let trigger = all(&app, "trigger")[0];
    assert!(text(&app, trigger).contains("red"));
    assert!(all(&app, "option").is_empty());

    click(&app, trigger, 0.5, 0.5);
    let listed = all(&app, "option");
    assert_eq!(listed.len(), 3);
    click(&app, listed[2], 0.5, 0.5);
    assert_eq!(chosen.get_untracked(), 2);
    assert!(all(&app, "option").is_empty());
    assert!(text(&app, trigger).contains("custom"));

    // From the keyboard: Enter opens on the chosen one, the arrows move,
    // Enter chooses; Escape leaves it as it was.
    named(&app, NamedKey::Enter);
    assert_eq!(all(&app, "active"), vec![all(&app, "option")[2]]);
    named(&app, NamedKey::ArrowUp);
    named(&app, NamedKey::Enter);
    assert_eq!(chosen.get_untracked(), 1);
    named(&app, NamedKey::Enter);
    named(&app, NamedKey::ArrowUp);
    named(&app, NamedKey::Escape);
    assert_eq!(chosen.get_untracked(), 1);
    assert!(all(&app, "option").is_empty());

    // A press elsewhere shuts the list.
    click(&app, trigger, 0.5, 0.5);
    assert_eq!(all(&app, "option").len(), 3);
    click(&app, all(&app, "other")[0], 0.5, 0.5);
    app.resolve();
    assert!(all(&app, "option").is_empty());
    assert_eq!(*heard.borrow(), [2, 1]);
}

#[test]
fn a_menu_button_opens_its_items_and_one_is_done_by_click_or_key() {
    let open = RwSignal::new(false);
    let heard = Rc::new(RefCell::new(Vec::new()));
    let log = heard.clone();
    let clicked = Rc::new(RefCell::new(0));
    let behind = clicked.clone();
    let item = |label: &str, key: &str| MenuItem {
        label: label.to_string(),
        key: Some(key.to_string()),
        danger: label == "Terminate",
    };
    let app = open_with(|| {
        // What it is in hears its own clicks, not the menu's.
        el("div")
            .on("click", move |_| *behind.borrow_mut() += 1)
            .child(
                MenuButton {
                    label: MenuLabel::More,
                    items: vec![
                        item("Go to", "Enter"),
                        item("Terminal", "t"),
                        item("Terminate", "x"),
                    ],
                    open,
                    on_select: Callback::new(move |index| log.borrow_mut().push(index)),
                    within: None,
                }
                .view(),
            )
    });
    let trigger = all(&app, "trigger")[0];
    assert!(all(&app, "item").is_empty());

    click(&app, trigger, 0.5, 0.5);
    let listed = all(&app, "item");
    assert_eq!(listed.len(), 3);
    assert_eq!(all(&app, "danger"), vec![listed[2]]);
    click(&app, listed[1], 0.5, 0.5);
    assert!(!open.get_untracked());
    assert!(all(&app, "item").is_empty());
    assert_eq!(*clicked.borrow(), 0);

    // From the keyboard: Enter opens on the first, the arrows move, Enter
    // does it; Escape shuts it and does nothing.
    named(&app, NamedKey::Enter);
    assert_eq!(all(&app, "active"), vec![all(&app, "item")[0]]);
    named(&app, NamedKey::End);
    named(&app, NamedKey::ArrowUp);
    named(&app, NamedKey::Enter);
    named(&app, NamedKey::ArrowDown);
    named(&app, NamedKey::Escape);
    assert!(all(&app, "item").is_empty());

    // Opened from elsewhere, it takes the keys.
    click(&app, all(&app, "other")[0], 0.5, 0.5);
    open.set(true);
    app.run_effects();
    app.resolve();
    named(&app, NamedKey::ArrowDown);
    named(&app, NamedKey::ArrowDown);
    named(&app, NamedKey::Enter);

    // A press elsewhere shuts it.
    click(&app, trigger, 0.5, 0.5);
    assert_eq!(all(&app, "item").len(), 3);
    click(&app, all(&app, "other")[0], 0.5, 0.5);
    app.resolve();
    assert!(all(&app, "item").is_empty());
    assert_eq!(*heard.borrow(), [1, 1, 2]);
}

#[test]
fn a_menu_button_s_three_dots_are_drawn_in_the_middle_of_its_box() {
    let app = open_with(|| {
        MenuButton {
            label: MenuLabel::More,
            items: Vec::new(),
            open: RwSignal::new(false),
            on_select: Callback::new(|_| {}),
            within: None,
        }
        .view()
    });
    let trigger = all(&app, "trigger")[0];
    let (_, _, width, height) = bounds(&app, trigger);
    assert_eq!(width, height);
    // The middle dot is in the middle of the box; the dots are in a row
    // across it, the same either side, with none above or below them.
    let color = painted(&app);
    let at = |across: f32, down: f32| {
        let (x, y) = point(&app, trigger, across, down);
        color(x, y)
    };
    let ground = at(0.5, 0.1);
    assert_ne!(at(0.5, 0.5), ground);
    assert_eq!(at(0.5, 0.9), ground);
    let drawn: Vec<f32> = (0..20)
        .filter(|step| at(*step as f32 / 20. + 0.025, 0.5) != ground)
        .map(|step| step as f32 + 0.5)
        .collect();
    assert!(drawn.len() >= 3, "{drawn:?}");
    let middle = drawn.iter().sum::<f32>() / drawn.len() as f32;
    assert!((middle - 10.).abs() <= 1., "{drawn:?}");
}

#[test]
fn a_menu_button_opens_upward_where_below_would_pass_what_it_is_in() {
    let open = RwSignal::new(false);
    let app = open_with(|| {
        let within = el("div").class("within");
        within.child(el("div").class("filler")).child(
            MenuButton {
                label: MenuLabel::Text("More"),
                items: ["one", "two", "three"]
                    .map(|label| MenuItem {
                        label: label.to_string(),
                        ..Default::default()
                    })
                    .to_vec(),
                open,
                on_select: Callback::new(|_| {}),
                within: Some(within),
            }
            .view(),
        )
    });
    click(&app, all(&app, "trigger")[0], 0.5, 0.5);
    app.resolve();
    let (_, trigger_y, _, _) = bounds(&app, all(&app, "trigger")[0]);
    let (_, items_y, _, items_height) = bounds(&app, all(&app, "items")[0]);
    assert!(
        items_y + items_height <= trigger_y + 0.5,
        "{items_y} {items_height} {trigger_y}"
    );
}

#[test]
fn an_alert_dialog_is_answered_by_its_buttons_and_its_keys() {
    let heard = Rc::new(RefCell::new(Vec::new()));
    let (done, kept) = (heard.clone(), heard.clone());
    let behind = Rc::new(RefCell::new(0));
    let keys_behind = behind.clone();
    let app = open_with(|| {
        // What is behind it hears none of its keys.
        el("div")
            .on("keydown", move |_| *keys_behind.borrow_mut() += 1)
            .child(
                AlertDialog {
                    title: "Terminate “build”?",
                    description: "It ends what runs in it.",
                    action: "Terminate",
                    action_key: Some("y"),
                    cancel: "Keep",
                    danger: true,
                    on_action: Callback::new(move |_| done.borrow_mut().push("action")),
                    on_cancel: Callback::new(move |_| kept.borrow_mut().push("cancel")),
                }
                .view(),
            )
    });
    let answers = all(&app, "answer");
    assert_eq!(answers.len(), 2);
    // It opens on the button that keeps things as they are.
    assert_eq!(all(&app, "at"), vec![answers[0]]);
    named(&app, NamedKey::Enter);
    // Tab goes to the action, and back.
    named(&app, NamedKey::Tab);
    assert_eq!(all(&app, "at"), vec![answers[1]]);
    named(&app, NamedKey::Enter);
    named(&app, NamedKey::Tab);
    assert_eq!(all(&app, "at"), vec![answers[0]]);
    // Escape keeps; the action's key does it; another key does nothing.
    named(&app, NamedKey::Escape);
    key(&app, Key::Character("y".into()), Modifiers::default());
    key(&app, Key::Character("j".into()), Modifiers::default());
    // The buttons are pressed by the pointer too.
    click(&app, all(&app, "button")[1], 0.5, 0.5);
    click(&app, all(&app, "button")[0], 0.5, 0.5);
    assert_eq!(
        *heard.borrow(),
        ["cancel", "action", "cancel", "action", "action", "cancel"]
    );
    assert_eq!(*behind.borrow(), 0);
}

#[test]
fn a_color_picker_chooses_on_its_area_and_its_bar_of_hues() {
    let value = RwSignal::new([255, 0, 0]);
    let heard = Rc::new(RefCell::new(Vec::new()));
    let log = heard.clone();
    let app = open(|| {
        ColorPicker {
            value,
            on_change: Callback::new(move |color| log.borrow_mut().push(color)),
        }
        .view()
    });
    assert!(all(&app, "panel").is_empty());
    click(&app, all(&app, "swatch")[0], 0.5, 0.5);
    assert_eq!(all(&app, "panel").len(), 1);
    let (area, bar) = (all(&app, "area")[0], all(&app, "hue")[0]);

    // What is painted: the hues along the bar, and on the area the hue
    // at the top right, white at the top left and black along the foot.
    let at = painted(&app);
    let sample = |node: NodeId, across: f32, down: f32| {
        let (x, y) = point(&app, node, across, down);
        at(x, y)
    };
    let [r, g, b] = sample(bar, 1. / 6., 0.2);
    assert!(r > 200 && g > 200 && b < 60, "yellow, not {:?}", [r, g, b]);
    let [r, g, b] = sample(bar, 0.5, 0.2);
    assert!(r < 60 && g > 200 && b > 200, "cyan, not {:?}", [r, g, b]);
    // (Short of the corner itself, where the knob is for a pure hue.)
    let [r, g, b] = sample(area, 0.85, 0.03);
    assert!(r > 220 && g < 60 && b < 60, "the hue, not {:?}", [r, g, b]);
    let [r, g, b] = sample(area, 0.03, 0.03);
    assert!(r > 220 && g > 200 && b > 200, "white, not {:?}", [r, g, b]);
    let [r, g, b] = sample(area, 0.5, 0.97);
    assert!(r < 30 && g < 30 && b < 30, "black, not {:?}", [r, g, b]);

    // The middle of the area: half the saturation, half the brightness.
    click(&app, area, 0.5, 0.5);
    let [r, g, b] = value.get_untracked();
    assert!((r as i32 - 128).abs() <= 3 && (g as i32 - 64).abs() <= 3 && g == b);
    // A third of the way along the bar: green, at the same two.
    click(&app, bar, 1. / 3., 0.5);
    let [r, g, b] = value.get_untracked();
    assert!((g as i32 - 128).abs() <= 3 && (r as i32 - 64).abs() <= 3 && r == b);
    assert_eq!(heard.borrow().len(), 2);
    // The swatch is the color.
    let at = painted(&app);
    let (x, y) = point(&app, all(&app, "swatch")[0], 0.5, 0.5);
    let (shown, chosen) = (at(x, y), value.get_untracked());
    let near = (shown.iter().zip(&chosen)).all(|(shown, chosen)| shown.abs_diff(*chosen) <= 2);
    assert!(near, "{shown:?} for {chosen:?}");

    // A press elsewhere shuts the panel.
    click(&app, all(&app, "other")[0], 0.5, 0.5);
    app.resolve();
    assert!(all(&app, "panel").is_empty());
}

#[test]
fn a_color_picker_is_worked_from_the_keyboard() {
    let value = RwSignal::new([255, 0, 0]);
    let heard = Rc::new(RefCell::new(Vec::new()));
    let log = heard.clone();
    let app = open(|| {
        ColorPicker {
            value,
            on_change: Callback::new(move |color| log.borrow_mut().push(color)),
        }
        .view()
    });
    // The swatch has the keys (a click on it, and its panel shut again).
    click(&app, all(&app, "swatch")[0], 0.5, 0.5);
    named(&app, NamedKey::Escape);
    assert!(all(&app, "panel").is_empty());

    // Enter opens the panel with the keys on the area: the arrows move
    // the color across it and up it.
    named(&app, NamedKey::Enter);
    app.resolve();
    assert_eq!(all(&app, "panel").len(), 1);
    named(&app, NamedKey::ArrowLeft);
    let [r, g, b] = value.get_untracked();
    assert!(
        r == 255 && g == b && (1..=4).contains(&g),
        "{:?}",
        [r, g, b]
    );
    key(&app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    let [r, g, b] = value.get_untracked();
    assert!((228..=232).contains(&r) && g == b, "{:?}", [r, g, b]);
    // At the area's edge a key does nothing more.
    for _ in 0..3 {
        key(&app, Key::Named(NamedKey::ArrowRight), Modifiers::SHIFT);
    }
    let [r, g, b] = value.get_untracked();
    assert!(g == 0 && b == 0 && r > 200, "{:?}", [r, g, b]);

    // Tab goes on to the bar of hues, where right turns the hue.
    named(&app, NamedKey::Tab);
    app.resolve();
    let before = value.get_untracked();
    for _ in 0..12 {
        key(&app, Key::Named(NamedKey::ArrowRight), Modifiers::SHIFT);
    }
    let [r, g, b] = value.get_untracked();
    assert_ne!([r, g, b], before);
    assert!(g > 200 && r < 30 && b < 30, "green, not {:?}", [r, g, b]);
    assert!(heard.borrow().len() >= 4);

    // Escape shuts the panel, from wherever in it the keys are.
    named(&app, NamedKey::Escape);
    app.resolve();
    assert!(all(&app, "panel").is_empty());
}

#[test]
fn a_button_is_pressed_by_a_click_and_by_enter_or_space_and_leaves_the_keys_where_they_were() {
    use horizon_components::Button;

    let presses = Rc::new(RefCell::new(0));
    let counted = presses.clone();
    let app = open(move || {
        Button {
            label: "Save",
            on_press: Some(Callback::new(move |()| *counted.borrow_mut() += 1)),
            ..Default::default()
        }
        .view()
    });
    let button = all(&app, "button")[0];
    let focused = |app: &App| app.document().borrow().get_focussed_node_id();
    let other = all(&app, "other")[0];
    // The keys are elsewhere, and a press with the pointer leaves them there.
    El(other).focus();
    app.resolve();
    click(&app, button, 0.5, 0.5);
    assert_eq!(*presses.borrow(), 1);
    assert_eq!(focused(&app), Some(other));
    // With the keys on it, Enter and Space press it.
    El(button).focus();
    app.resolve();
    named(&app, NamedKey::Enter);
    key(&app, Key::Character(" ".into()), Modifiers::default());
    assert_eq!(*presses.borrow(), 3);
    // It is a button to whoever asks what is on the page.
    let document = app.document();
    let document = document.borrow();
    let element = document.get_node(button).unwrap().element_data().unwrap();
    assert_eq!(&*element.name.local, "button");
}

/// Paints the gallery of the components in this machine's theme, to the file
/// `HORIZON_GALLERY_OUT` names (a PPM): a picture for a person to look
/// at. Run by hand.
#[test]
#[ignore = "writes a picture for a person: set HORIZON_GALLERY_OUT"]
fn paint_the_gallery() {
    use anyrender::ImageRenderer as _;

    let out = std::env::var_os("HORIZON_GALLERY_OUT").expect("HORIZON_GALLERY_OUT");
    let (width, height) = (900u32, 840u32);
    let app = App::new(width, height, "Test", horizon_theme_css::CSS, || {
        themed(Theme::of_this_machine, horizon_components::gallery())
    });
    app.resolve();
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    let document = app.document();
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
