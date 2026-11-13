//! The agent view over a made-up session, with histories of several
//! lengths.

use horizon::agent::view;
use horizon_theme_css::{Theme, themed};

use crate::sample::{Given, Session};
use crate::{Frame, Scenario, Stage, commit, wheel};

pub fn scenarios() -> Vec<Scenario> {
    const TURNS: &[usize] = &[10, 100, 1000];
    vec![
        Scenario {
            name: "agent/show",
            unit: "turns",
            sizes: TURNS,
            run: show,
        },
        Scenario {
            name: "agent/streaming",
            unit: "turns",
            sizes: TURNS,
            run: streaming,
        },
        Scenario {
            name: "agent/typing",
            unit: "turns",
            sizes: TURNS,
            run: typing,
        },
        Scenario {
            name: "agent/scroll",
            unit: "turns",
            sizes: TURNS,
            run: scroll,
        },
    ]
}

/// The view on a page, with what it is given to show.
fn open() -> (Stage, Given) {
    // (Made on the page: a signal is its page's.)
    let mut given = None;
    let stage = Stage::new(900, 700, horizon_theme_css::CSS, || {
        let shown = Given::new_local(None);
        given = Some(shown);
        let (root, state) = view(shown.into(), |_| {});
        state.field.focus();
        themed(Theme::built_in, root)
    });
    (stage, given.unwrap())
}

fn opened_on(session: &Session) -> (Stage, Given) {
    let (mut stage, shown) = open();
    session.show(shown);
    // The list is built for what is in view over a few layouts.
    for _ in 0..4 {
        stage.settle();
    }
    (stage, shown)
}

/// A session's whole history handed to a view that had none: what
/// attaching to a session costs.
fn show(turns: usize) -> Vec<Frame> {
    let session = Session::long(turns);
    let mut frames = Vec::new();
    for _ in 0..5 {
        let (mut stage, shown) = open();
        stage.frame(|_| session.show(shown));
        frames.append(&mut stage.frames);
    }
    frames
}

/// An answer arriving a few words at a time after the history.
fn streaming(turns: usize) -> Vec<Frame> {
    let mut session = Session::long(turns);
    let (mut stage, shown) = opened_on(&session);
    session.begin_turn("もう一度、同じ箇所を確認してほしい");
    session.show(shown);
    stage.settle();
    for chunk in 0..300 {
        session.stream(&format!("確認した内容の {chunk} 番目です。"));
        stage.frame(|_| session.show(shown));
    }
    stage.frames
}

/// Text typed into the message field under the history.
fn typing(turns: usize) -> Vec<Frame> {
    let session = Session::long(turns);
    let (mut stage, _agent) = opened_on(&session);
    for _ in 0..100 {
        stage.frame(|app| commit(app, "あ"));
    }
    stage.frames
}

/// The wheel turned back through the history.
fn scroll(turns: usize) -> Vec<Frame> {
    let session = Session::long(turns);
    let (mut stage, _agent) = opened_on(&session);
    for _ in 0..200 {
        stage.frame(|app| wheel(app, (450., 300.), 120.));
    }
    stage.frames
}
