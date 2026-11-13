//! What the frames of a scenario come to, and the two things looked for
//! in it: a frame that takes longer than a display shows one for, and a
//! frame that costs more the more there is to show.

use std::fmt::Write;
use std::time::Duration;

use crate::{Frame, Scenario};

/// A display at 60 Hz shows a frame for this long.
const A_FRAME: Duration = Duration::from_micros(16_667);
/// A scenario whose median frame at its largest size is this many times
/// that at its smallest is marked as growing with the content.
const GROWS: f64 = 2.0;
/// Below this a median frame is too short for its growth to matter.
const WORTH_COMPARING: Duration = Duration::from_micros(500);

#[derive(Clone, Copy)]
pub struct Spread {
    pub median: Duration,
    pub p95: Duration,
    pub max: Duration,
}

impl Spread {
    fn of(mut times: Vec<Duration>) -> Self {
        times.sort();
        let at = |part: f64| {
            let position = ((times.len() as f64 - 1.) * part).round() as usize;
            times.get(position).copied().unwrap_or_default()
        };
        Self {
            median: at(0.5),
            p95: at(0.95),
            max: at(1.0),
        }
    }
}

pub struct Measured {
    pub name: &'static str,
    pub unit: &'static str,
    pub size: usize,
    pub frames: usize,
    /// Drawn into a window: the display's refresh interval, and how many
    /// frames took longer than it.
    pub late: Option<(Duration, usize)>,
    pub act: Spread,
    pub layout: Spread,
    pub scene: Spread,
    pub total: Spread,
}

impl Measured {
    pub fn of(
        scenario: &Scenario,
        size: usize,
        frames: &[Frame],
        refresh: Option<Duration>,
    ) -> Self {
        let spread = |of: fn(&Frame) -> Duration| Spread::of(frames.iter().map(of).collect());
        let late = refresh.map(|refresh| {
            let late = frames.iter().filter(|frame| frame.total() > refresh);
            (refresh, late.count())
        });
        Self {
            name: scenario.name,
            unit: scenario.unit,
            size,
            frames: frames.len(),
            late,
            act: spread(|frame| frame.act),
            layout: spread(|frame| frame.layout),
            scene: spread(|frame| frame.scene),
            total: spread(Frame::total),
        }
    }
}

fn milliseconds(time: Duration) -> f64 {
    time.as_secs_f64() * 1000.
}

/// What is wrong with a scenario's frames at one size, given those at its
/// smallest.
fn marks(measured: &Measured, smallest: &Measured) -> Vec<String> {
    let mut marks = Vec::new();
    // Drawn into a window, a frame is held against the display it was
    // drawn for.
    if let Some((_, late)) = measured.late {
        if late > 0 {
            marks.push(format!("{late} over a refresh"));
        }
        return marks;
    }
    if measured.total.p95 > A_FRAME {
        marks.push("over a frame".to_string());
    }
    let (now, then) = (measured.total.median, smallest.total.median);
    if measured.size > smallest.size && now > WORTH_COMPARING && !then.is_zero() {
        let times = now.as_secs_f64() / then.as_secs_f64();
        if times >= GROWS {
            let larger = measured.size as f64 / smallest.size as f64;
            marks.push(format!("grows x{times:.1} at x{larger:.0} the size"));
        }
    }
    marks
}

fn smallest<'a>(all: &'a [Measured], of: &Measured) -> &'a Measured {
    (all.iter())
        .filter(|other| other.name == of.name)
        .min_by_key(|other| other.size)
        .expect("the scenario itself")
}

/// The times in milliseconds: the median of each part, then the median,
/// 95th percentile and longest of the whole frame.
pub fn table(all: &[Measured]) -> String {
    let mut table = String::new();
    let shown = all.iter().find_map(|measured| measured.late);
    if let Some((refresh, _)) = shown {
        let _ = writeln!(
            table,
            "drawn into a window; the display shows a frame for {:.2} ms",
            milliseconds(refresh)
        );
    }
    let _ = writeln!(
        table,
        "{:<28} {:>14} {:>6}  {:>8} {:>8} {:>8}  {:>8} {:>8} {:>8}  ",
        "scenario",
        "size",
        "frames",
        "act",
        "layout",
        if shown.is_some() { "render" } else { "scene" },
        "median",
        "p95",
        "max"
    );
    for measured in all {
        let _ = writeln!(
            table,
            "{:<28} {:>14} {:>6}  {:>8.3} {:>8.3} {:>8.3}  {:>8.3} {:>8.3} {:>8.3}  {}",
            measured.name,
            format!("{} {}", measured.size, measured.unit),
            measured.frames,
            milliseconds(measured.act.median),
            milliseconds(measured.layout.median),
            milliseconds(measured.scene.median),
            milliseconds(measured.total.median),
            milliseconds(measured.total.p95),
            milliseconds(measured.total.max),
            marks(measured, smallest(all, measured)).join(", "),
        );
    }
    table
}

pub fn json(all: &[Measured]) -> String {
    let spread = |spread: &Spread| {
        format!(
            r#"{{"median_ms":{:.4},"p95_ms":{:.4},"max_ms":{:.4}}}"#,
            milliseconds(spread.median),
            milliseconds(spread.p95),
            milliseconds(spread.max)
        )
    };
    let rows: Vec<String> = (all.iter())
        .map(|measured| {
            let marks: Vec<String> = (marks(measured, smallest(all, measured)).iter())
                .map(|mark| format!("{mark:?}"))
                .collect();
            format!(
                r#"  {{"scenario":{:?},"size":{},"unit":{:?},"frames":{},"late":{},"act":{},"layout":{},"scene":{},"total":{},"marks":[{}]}}"#,
                measured.name,
                measured.size,
                measured.unit,
                measured.frames,
                match measured.late {
                    Some((_, late)) => late.to_string(),
                    None => "null".to_string(),
                },
                spread(&measured.act),
                spread(&measured.layout),
                spread(&measured.scene),
                spread(&measured.total),
                marks.join(","),
            )
        })
        .collect();
    format!("[\n{}\n]\n", rows.join(",\n"))
}
