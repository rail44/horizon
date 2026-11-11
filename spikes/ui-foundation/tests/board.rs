//! Reordering the board list's rows by dragging, with the mouse's own
//! events.

use reactive_graph::traits::GetUntracked;
use ui_foundation_spike::blitz_dom::{BaseDocument, NodeId, local_name};
use ui_foundation_spike::board::{Board, CSS, Half, Task, view};
use ui_foundation_spike::{App, Pointer, click_at, pointer};

fn open() -> (App, Board) {
    let tasks = (1..=5)
        .map(|id| Task {
            id,
            title: format!("task {id}"),
            status: "open".to_string(),
        })
        .collect();
    let mut board = None;
    let app = App::new(600, 400, CSS, || {
        let (root, state) = view(tasks);
        board = Some(state);
        root
    });
    app.resolve();
    (app, board.unwrap())
}

fn collect(document: &BaseDocument, id: NodeId, class: &str, found: &mut Vec<NodeId>) {
    let node = document.get_node(id).expect("a node");
    if (node.attr(local_name!("class")))
        .is_some_and(|classes| classes.split(' ').any(|name| name == class))
    {
        found.push(id);
    }
    for child in &node.children {
        collect(document, *child, class, found);
    }
}

/// The boxes of the elements of `class`, in document order: x, y, width,
/// height.
fn boxes(app: &App, class: &str) -> Vec<(f32, f32, f32, f32)> {
    let document = app.document();
    let document = document.borrow();
    let mut found = Vec::new();
    collect(&document, document.root_element().id, class, &mut found);
    (found.into_iter())
        .map(|node| {
            let bounds = document.get_client_bounding_rect(node).expect("a box");
            (
                bounds.x as f32,
                bounds.y as f32,
                bounds.width as f32,
                bounds.height as f32,
            )
        })
        .collect()
}

fn order(board: &Board) -> Vec<u64> {
    (board.tasks.get_untracked().iter())
        .map(|task| task.id)
        .collect()
}

fn step(app: &App, x: f32, y: f32, what: Pointer) {
    pointer(app, x, y, what);
    app.resolve();
}

#[test]
fn a_row_is_carried_to_where_the_line_shows_and_dropped_there() {
    let (app, board) = open();
    let rows = boxes(&app, "row");
    assert_eq!(rows.len(), 5);
    // The style sheet's rules nested under the view's class apply.
    assert_eq!(rows[0].3, 28.);
    let middle = |row: usize| rows[row].1 + rows[row].3 / 2.;

    // Pressed, and not yet carried until it has moved.
    step(&app, 100., middle(0), Pointer::Move);
    step(&app, 100., middle(0), Pointer::Down);
    step(&app, 101., middle(0) + 1., Pointer::Drag);
    assert_eq!(board.carried.get_untracked(), None);
    assert!(boxes(&app, "ghost").is_empty());

    // Over its own row, and over the upper half of the one below it: a
    // release would move nothing, so there is no line.
    step(&app, 100., middle(0) + 8., Pointer::Drag);
    assert_eq!(board.carried.get_untracked(), Some(1));
    assert_eq!(board.indicator.get_untracked(), None);
    step(&app, 100., middle(1) - 6., Pointer::Drag);
    assert_eq!(board.indicator.get_untracked(), None);
    assert!(boxes(&app, "line").is_empty());

    // The lower half of the fourth row: the line is at its bottom edge,
    // and the row's copy is by the pointer.
    step(&app, 120., middle(3) + 6., Pointer::Drag);
    assert_eq!(board.indicator.get_untracked(), Some((4, Half::Below)));
    let line = boxes(&app, "line")[0];
    // (Inside the row's own one-pixel border, as an offset from a
    // positioned box is.)
    assert_eq!(line.1, rows[3].1 + rows[3].3 - 1.);
    let ghost = boxes(&app, "ghost")[0];
    assert_eq!((ghost.0, ghost.1), (132., middle(3) + 18.));

    // The upper half of the same row: the line moves to its top edge.
    step(&app, 120., middle(3) - 6., Pointer::Drag);
    assert_eq!(board.indicator.get_untracked(), Some((4, Half::Above)));
    assert_eq!(boxes(&app, "line").len(), 1);
    assert_eq!(boxes(&app, "line")[0].1, rows[3].1 - 1.);

    step(&app, 120., middle(3) - 6., Pointer::Up);
    assert_eq!(order(&board), [2, 3, 1, 4, 5]);
    assert_eq!(board.selected.get_untracked(), Some(1));
    assert_eq!(board.carried.get_untracked(), None);
    assert!(boxes(&app, "line").is_empty());
    assert!(boxes(&app, "ghost").is_empty());

    // Carried up again, past the top of the list's first row.
    let rows = boxes(&app, "row");
    step(&app, 100., middle(2), Pointer::Down);
    step(&app, 100., rows[0].1 + 3., Pointer::Drag);
    assert_eq!(board.indicator.get_untracked(), Some((2, Half::Above)));
    step(&app, 100., rows[0].1 + 3., Pointer::Up);
    assert_eq!(order(&board), [1, 2, 3, 4, 5]);
}

#[test]
fn a_click_selects_a_row_and_keys_move_the_selection() {
    let (app, board) = open();
    let rows = boxes(&app, "row");
    click_at(&app, 100., rows[2].1 + 10.);
    assert_eq!(board.selected.get_untracked(), Some(3));
    assert_eq!(order(&board), [1, 2, 3, 4, 5]);

    use ui_foundation_spike::blitz_traits::events::{BlitzKeyEvent, KeyState, UiEvent};
    let press = |character: &str| {
        app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
            key: keyboard_types::Key::Character(character.into()),
            code: keyboard_types::Code::KeyJ,
            modifiers: Default::default(),
            location: keyboard_types::Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
            text: None,
        }));
    };
    press("j");
    press("j");
    press("j");
    assert_eq!(board.selected.get_untracked(), Some(5));
    press("k");
    assert_eq!(board.selected.get_untracked(), Some(4));
}
