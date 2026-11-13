//! The terminal view over a real shell on a PTY, at grids of several
//! numbers of cells.

use std::time::{Duration, Instant};

use horizon::terminal::{CSS, Terminal, view};
use horizon_terminal_core::TerminalSize;
use horizon_theme_css::Theme;
use keyboard_types::{Key, Modifiers, NamedKey};

use crate::{Frame, Scenario, Stage, character, key, named, support, wheel};

pub fn scenarios() -> Vec<Scenario> {
    // 80 by 24, 200 by 60, 400 by 120.
    const CELLS: &[usize] = &[1920, 12_000, 48_000];
    vec![
        Scenario {
            name: "terminal/output",
            unit: "cells",
            sizes: CELLS,
            run: output,
        },
        Scenario {
            name: "terminal/lines",
            unit: "cells",
            sizes: CELLS,
            run: lines,
        },
        Scenario {
            name: "terminal/typing",
            unit: "cells",
            sizes: CELLS,
            run: typing,
        },
        Scenario {
            name: "terminal/scrollback",
            unit: "cells",
            sizes: CELLS,
            run: scrollback,
        },
    ]
}

/// The most frames one wait is timed for.
const MOST_FRAMES: usize = 300;

struct Opened {
    stage: Stage,
    terminal: Terminal,
    columns: usize,
}

/// A terminal of `cells` cells, three tenths as tall as it is wide, at
/// the built-in font's cell.
fn open(cells: usize) -> Opened {
    let columns = (cells as f64 / 0.3).sqrt().round() as usize;
    let rows = columns * 3 / 10;
    let session = support::shell("sh", TerminalSize::new(columns as u16, rows as u16));
    let mut terminal = None;
    let stage = Stage::new(columns as u32 * 8, rows as u32 * 18, CSS, || {
        let (root, state) = view(session.into(), Theme::built_in().terminal());
        terminal = Some(state);
        root.focus();
        horizon_ui::el("div")
            .attr("style", "height: 100%; box-sizing: border-box;")
            .child(root)
    });
    let mut opened = Opened {
        stage,
        terminal: terminal.unwrap(),
        columns: 0,
    };
    opened.wait("a prompt", |text| text.trim_end().ends_with('$'));
    opened.columns = columns;
    opened
}

impl Opened {
    fn wait(&mut self, what: &str, met: impl Fn(&str) -> bool) {
        let terminal = self.terminal.clone();
        self.stage.until(what, || met(&terminal.text()));
    }

    fn run(&mut self, command: &str) {
        for typed in command.chars() {
            character(&self.stage.app, typed);
        }
        named(&self.stage.app, NamedKey::Enter);
    }

    /// Timed frames until `met`, keeping those in which a frame of the
    /// session's was taken in.
    fn frames_taking_in(&mut self, what: &str, met: impl Fn(&str) -> bool) {
        let started = Instant::now();
        let most = self.stage.frames.len() + MOST_FRAMES;
        loop {
            if self.stage.frames.len() >= most {
                return;
            }
            let before = self.terminal.frame_number();
            self.stage.frame(|_| {});
            if self.terminal.frame_number() == before {
                self.stage.frames.pop();
                std::thread::sleep(Duration::from_micros(200));
            }
            if met(&self.terminal.text()) {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "no {what} in:\n{}",
                self.terminal.text()
            );
        }
    }
}

/// A program printing as fast as it can: the frames of it that the view
/// gets to show.
fn output(cells: usize) -> Vec<Frame> {
    let mut opened = open(cells);
    // Lines as wide as the grid, so that a larger grid has more to draw,
    // each unlike the others, as what scrolls by is, and more of them
    // than are shown before the measuring stops.
    let line = "x".repeat(opened.columns - 14);
    opened.run(&format!(
        "seq -f '%012.0f {line}' 1 20000000; echo output-done"
    ));
    opened.frames_taking_in("the end of the output", |text| {
        text.lines().any(|line| line == "output-done")
    });
    // Not left printing under the scenarios that follow.
    key(
        &opened.stage.app,
        Key::Character("c".into()),
        Modifiers::CONTROL,
    );
    opened.stage.frames
}

/// A program printing a line at a time: each frame has the rows of the
/// one before it, a row up, and one new row.
fn lines(cells: usize) -> Vec<Frame> {
    let mut opened = open(cells);
    let line = "x".repeat(opened.columns - 14);
    // First a screenful, so that every frame timed is of a full screen.
    opened.run(&format!("seq -f '%012.0f {line}' 1 200; echo screen-full"));
    opened.wait("a full screen", |text| {
        text.lines().any(|line| line == "screen-full")
    });
    opened.run(&format!(
        "i=0; while [ $i -lt 300 ]; do echo \"$i {line}\"; sleep 0.005; i=$((i+1)); done; echo output-done"
    ));
    opened.frames_taking_in("the end of the output", |text| {
        text.lines().any(|line| line == "output-done")
    });
    opened.stage.frames
}

/// A character typed at the prompt, and the frame its echo arrives in.
fn typing(cells: usize) -> Vec<Frame> {
    let mut opened = open(cells);
    for count in 1..=100 {
        opened.stage.frame(|app| character(app, 'a'));
        // (The line wraps: counted, not looked for as one run.)
        opened.frames_taking_in("the echo", |text| {
            text.chars().filter(|typed| *typed == 'a').count() >= count
        });
    }
    opened.stage.frames
}

/// The wheel turned back through what scrolled off, three rows a step.
fn scrollback(cells: usize) -> Vec<Frame> {
    let mut opened = open(cells);
    opened.run("seq 1 5000; echo output-done");
    opened.wait("the end of the output", |text| {
        text.lines().any(|line| line == "output-done")
    });
    let middle = (opened.columns as f32 * 4., 100.);
    for _ in 0..100 {
        opened.stage.frame(|app| wheel(app, middle, 3. * 18.));
        // The rows asked of the session for where the view now is.
        std::thread::sleep(Duration::from_millis(2));
        opened.stage.frame(|_| {});
    }
    opened.stage.frames
}
