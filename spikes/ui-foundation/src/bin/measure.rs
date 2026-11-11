//! Times one label changing in a list of rows, stage by stage, without a
//! window: `cargo run --release --bin measure`.

use std::time::{Duration, Instant};

use anyrender::ImageRenderer as _;
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, Set};
use ui_foundation_spike::{App, el};

const CSS: &str = "
    body { margin: 0; font-size: 14px; }
    .list { display: flex; flex-direction: column; gap: 4px; padding: 8px; }
    .group { display: flex; flex-direction: column; gap: 4px; }
    .row { display: flex; gap: 8px; }
    .label { flex: 1; }
    .on { color: #356df3; }
";

struct Scene {
    app: App,
    labels: Vec<RwSignal<String>>,
    marks: Vec<RwSignal<bool>>,
}

/// `rows` rows, each a label and a fixed word; in groups of `group` rows
/// when it is not 0.
fn build(rows: usize, group: usize) -> Scene {
    let labels: Vec<RwSignal<String>> = (0..rows)
        .map(|row| RwSignal::new(format!("row {row}")))
        .collect();
    let marks: Vec<RwSignal<bool>> = (0..rows).map(|_| RwSignal::new(false)).collect();
    let row = |index: usize| {
        let (label, mark) = (labels[index], marks[index]);
        el("div")
            .class("row")
            .child(
                el("span")
                    .class_dyn(move || {
                        if mark.get() {
                            "label on".to_owned()
                        } else {
                            "label".to_owned()
                        }
                    })
                    .text_dyn(move || label.get()),
            )
            .child(el("span").text("remove"))
    };
    let app = App::new(800, 600, CSS, || {
        let mut list = el("div").class("list");
        if group == 0 {
            for index in 0..rows {
                list = list.child(row(index));
            }
        } else {
            for start in (0..rows).step_by(group) {
                let mut part = el("div").class("group");
                for index in start..(start + group).min(rows) {
                    part = part.child(row(index));
                }
                list = list.child(part);
            }
        }
        list
    });
    Scene { app, labels, marks }
}

fn micros(mut times: Vec<Duration>) -> f64 {
    times.sort();
    times[times.len() / 2].as_secs_f64() * 1e6
}

fn main() {
    println!("microseconds, median of 31");
    for (incremental, group) in [(true, 0), (true, 50), (false, 0)] {
        println!(
            "incremental layout {}, {}:",
            if incremental { "on" } else { "off" },
            if group == 0 {
                "rows under one parent".to_owned()
            } else {
                format!("rows in groups of {group}")
            }
        );
        for rows in [100usize, 1_000, 3_000, 10_000] {
            let started = Instant::now();
            let scene = build(rows, group);
            let built = started.elapsed();
            scene
                .app
                .document()
                .borrow_mut()
                .set_incremental_layout(incremental);
            let started = Instant::now();
            scene.app.resolve();
            let first = started.elapsed();

            let mut still = Vec::new();
            for _ in 0..31 {
                let started = Instant::now();
                scene.app.resolve();
                still.push(started.elapsed());
            }

            // The text of one label: the signal's write, then the document.
            let (mut writes, mut texts) = (Vec::new(), Vec::new());
            for round in 0..31 {
                let started = Instant::now();
                scene.labels[rows / 2].set(format!("row {} changed {round}", rows / 2));
                scene.app.run_effects();
                writes.push(started.elapsed());
                let started = Instant::now();
                scene.app.resolve();
                texts.push(started.elapsed());
            }

            // The color of one label: a class, so style and no layout.
            let mut colors = Vec::new();
            for round in 0..31 {
                scene.marks[rows / 2].set(round % 2 == 0);
                scene.app.run_effects();
                let started = Instant::now();
                scene.app.resolve();
                colors.push(started.elapsed());
            }
            // Painting the 800 by 600 at the top, with the CPU rasterizer.
            let document = scene.app.document();
            let mut cpu = anyrender_vello_cpu::VelloCpuImageRenderer::new(800, 600);
            let mut paints = Vec::new();
            let mut pixels = Vec::new();
            for _ in 0..11 {
                let started = Instant::now();
                cpu.render_to_vec(
                    |painter| {
                        blitz_paint::paint_scene(
                            painter,
                            &mut document.borrow_mut(),
                            1.0,
                            800,
                            600,
                            0,
                            0,
                        )
                    },
                    &mut pixels,
                );
                paints.push(started.elapsed());
            }
            println!(
                "  {rows:6} rows: build {:9.0} | first resolve {:9.0} | unchanged {:7.1} | signal write {:5.1} | one text {:9.1} | one color {:9.1} | paint {:7.1}",
                built.as_secs_f64() * 1e6,
                first.as_secs_f64() * 1e6,
                micros(still),
                micros(writes),
                micros(texts),
                micros(colors),
                micros(paints),
            );
        }
    }
}
