//! The whole shell over real shells on PTYs, with several numbers of
//! panes in its tab.

use horizon::workspace::{Opening, Shell, Spawn, css, view};
use horizon_terminal_core::TerminalSize;
use horizon_ui::reactive_graph::traits::WithUntracked;
use horizon_workspace::Workspace;
use horizon_workspace::commands::CommandId;
use keyboard_types::{Key, Modifiers, NamedKey};

use crate::{Frame, Scenario, Stage, character, key, named, support};

pub fn scenarios() -> Vec<Scenario> {
    const PANES: &[usize] = &[1, 4, 8];
    vec![
        Scenario {
            name: "shell/typing",
            unit: "panes",
            sizes: PANES,
            run: typing,
        },
        Scenario {
            name: "shell/resize",
            unit: "panes",
            sizes: PANES,
            run: resize,
        },
        Scenario {
            name: "shell/tab-switch",
            unit: "panes",
            sizes: PANES,
            run: tab_switch,
        },
        Scenario {
            name: "shell/palette",
            unit: "panes",
            sizes: PANES,
            run: palette,
        },
        Scenario {
            name: "shell/split",
            unit: "panes",
            sizes: PANES,
            run: split,
        },
    ]
}

const SIZE: (u32, u32) = (1600, 1000);

/// A shell whose tab has `panes` terminals, each at its prompt.
fn open(panes: usize) -> (Stage, Shell) {
    let mut shell = None;
    let mut stage = Stage::new(SIZE.0, SIZE.1, &css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let (root, built) = view(spawn, Opening::default());
        shell = Some(built);
        root
    });
    let shell = shell.unwrap();
    for split in 1..panes {
        shell.execute(match split % 2 {
            1 => CommandId::SplitRight,
            _ => CommandId::SplitDown,
        });
        stage.settle();
    }
    stage.until("every prompt", || {
        let shown = shell.model.with_untracked(|model: &Workspace| {
            (model.visible_panes().iter())
                .map(|pane| pane.id)
                .collect::<Vec<_>>()
        });
        shown.len() == panes
            && shown.iter().all(|pane| {
                (shell.terminal(*pane))
                    .is_some_and(|terminal| terminal.text().trim_end().ends_with('$'))
            })
    });
    (stage, shell)
}

/// A character typed into the active terminal, beside the others.
fn typing(panes: usize) -> Vec<Frame> {
    let (mut stage, _shell) = open(panes);
    for _ in 0..100 {
        stage.frame(|app| character(app, 'a'));
        // The echo.
        std::thread::sleep(std::time::Duration::from_millis(2));
        stage.frame(|_| {});
    }
    stage.frames
}

/// The window dragged narrower and wider, every terminal's grid with it.
fn resize(panes: usize) -> Vec<Frame> {
    let (mut stage, _shell) = open(panes);
    for step in 0..60u32 {
        let narrower = if step < 30 { step } else { 60 - step } * 10;
        stage.resize(SIZE.0 - narrower, SIZE.1 - narrower / 2);
        stage.frame(|_| {});
    }
    stage.frames
}

/// A second tab opened, and the two shown in turn.
fn tab_switch(panes: usize) -> Vec<Frame> {
    let (mut stage, shell) = open(panes);
    shell.execute(CommandId::NewTab);
    stage.settle();
    // In workspace mode, Tab shows the next tab.
    key(&stage.app, Key::Character("'".into()), Modifiers::CONTROL);
    stage.settle();
    for _ in 0..60 {
        stage.frame(|app| named(app, NamedKey::Tab));
    }
    stage.frames
}

/// The palette opened over the panes, a query typed into it, and closed.
fn palette(panes: usize) -> Vec<Frame> {
    let (mut stage, _shell) = open(panes);
    for _ in 0..10 {
        stage.frame(|app| key(app, Key::Character("'".into()), Modifiers::CONTROL));
        stage.frame(|app| character(app, ':'));
        for typed in "reload".chars() {
            stage.frame(|app| crate::commit(app, &typed.to_string()));
        }
        stage.frame(|app| named(app, NamedKey::Escape));
        stage.frame(|app| named(app, NamedKey::Escape));
    }
    stage.frames
}

/// One more pane split off a tab that has `panes`. Its `act` has the
/// start of a shell on a PTY in it, which is done here in the process:
/// in Horizon that is the terminal runtime's.
fn split(panes: usize) -> Vec<Frame> {
    let mut frames = Vec::new();
    for _ in 0..3 {
        let (mut stage, shell) = open(panes);
        stage.frame(|_| shell.execute(CommandId::SplitRight));
        frames.append(&mut stage.frames);
    }
    frames
}
