//! The agent view over a made-up session, with histories of several
//! lengths.

use horizon::agent::{Agent, CSS, view};
use horizon_theme_css::{Theme, themed};

use crate::sample::Session;
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

fn open() -> (Stage, Agent) {
    let mut agent = None;
    let css = [horizon_theme_css::CSS, CSS].concat();
    let stage = Stage::new(900, 700, &css, || {
        let (root, state) = view(|_| {});
        state.field.focus();
        agent = Some(state);
        themed(Theme::built_in, root)
    });
    (stage, agent.unwrap())
}

fn opened_on(session: &Session) -> (Stage, Agent) {
    let (mut stage, agent) = open();
    session.show(&agent);
    // The list is built for what is in view over a few layouts.
    for _ in 0..4 {
        stage.settle();
    }
    (stage, agent)
}

/// A session's whole history handed to a view that had none: what
/// attaching to a session costs.
fn show(turns: usize) -> Vec<Frame> {
    let session = Session::long(turns);
    let mut frames = Vec::new();
    for _ in 0..5 {
        let (mut stage, agent) = open();
        stage.frame(|_| session.show(&agent));
        frames.append(&mut stage.frames);
    }
    frames
}

/// An answer arriving a few words at a time after the history.
fn streaming(turns: usize) -> Vec<Frame> {
    let mut session = Session::long(turns);
    let (mut stage, agent) = opened_on(&session);
    session.begin_turn("もう一度、同じ箇所を確認してほしい");
    session.show(&agent);
    stage.settle();
    for chunk in 0..300 {
        session.stream(&format!("確認した内容の {chunk} 番目です。"));
        stage.frame(|_| session.show(&agent));
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
