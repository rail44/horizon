//! Horizon's agent pane on this stack.
//!
//!     cargo run --release --bin agent -- window [turns] [whole]
//!     cargo run --release --bin agent -- measure [scale]
//!     cargo run --release --bin agent -- picture out.png [idle|running] [scale]
//!
//! `window` shows a made-up session of `turns` ended turns (200 unless
//! given; with `whole`, every unit an element, in view or not, and
//! otherwise only those near the view) and keeps a turn streaming into it: text a few characters at a
//! time, a call, an approval that Enter or a click answers. `measure`
//! times what an update of the turn in progress costs against histories of
//! several lengths, with no window. `picture` draws one frame into a PNG.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyrender::ImageRenderer as _;
use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};
use reactive_graph::traits::Update;
use ui_foundation_spike::agent::sample::Session;
use ui_foundation_spike::agent::{Agent, Asked, CSS, Column, view_with};
use ui_foundation_spike::blitz_traits::events::{
    BlitzWheelDelta, BlitzWheelEvent, PointerCoords, UiEvent,
};
use ui_foundation_spike::blitz_traits::shell::{ColorScheme, Viewport};
use ui_foundation_spike::theme::{self, Theme};
use ui_foundation_spike::{App, Live};

const STREAMED: &str = "承知しました。まず `cargo clippy --workspace` の出力を確認します。\n\n\
警告は 3 件で、いずれも `src/board/list.rs` の未使用の束縛です。\n\n\
- `row_height` の古い定数への参照\n\
- 使われなくなった `use` 宣言\n\
- `let _ =` で捨てている戻り値\n\n\
順に直して、最後にもう一度 `clippy` と `nextest` を通します。";

fn open(asked: impl Fn(Asked) + 'static, column: Column) -> (App, Agent) {
    let mut agent = None;
    // The colors, the type scale and the font of this machine's config.
    let look = Theme::of_this_machine();
    let css = [theme::CSS, CSS].concat();
    let app = App::new(900, 700, &css, || {
        let (root, state) = view_with(asked, column);
        state.field.focus();
        agent = Some(state);
        theme::themed(move || look.clone(), root)
    });
    (app, agent.unwrap())
}

fn nodes(app: &App) -> usize {
    let document = app.document();
    let document = document.borrow();
    let mut pending = vec![document.root_node().id];
    let mut count = 0;
    while let Some(id) = pending.pop() {
        count += 1;
        pending.extend(document.get_node(id).unwrap().children.iter().copied());
    }
    count
}

fn median(mut samples: Vec<Duration>) -> (f64, f64) {
    samples.sort();
    let milliseconds = |duration: Duration| duration.as_secs_f64() * 1000.;
    (
        milliseconds(samples[samples.len() / 2]),
        milliseconds(*samples.last().unwrap()),
    )
}

fn measure(scale: f32) {
    println!("at a display scale of {scale}, every unit an element");
    measure_column(scale, Column::Whole, &[10, 50, 200, 400]);
    println!("at a display scale of {scale}, the units near the view elements");
    measure_column(scale, Column::Windowed, &[10, 50, 200, 400, 2000]);
}

fn measure_column(scale: f32, column: Column, lengths: &[usize]) {
    println!(
        "turns  units  nodes  first(ms) | per update of the turn in progress, median / worst (ms)"
    );
    println!(
        "                                | adapt+effects   resolve        paint(cpu)     | idle resolve | open a receipt | a wheel step"
    );
    for &turns in lengths {
        let mut session = Session::long(turns);
        let (app, agent) = open(|_| {}, column);
        let (width, height) = ((900. * scale) as u32, (700. * scale) as u32);
        (app.document().borrow_mut()).set_viewport(Viewport::new(
            width,
            height,
            scale,
            ColorScheme::Light,
        ));
        let started = Instant::now();
        session.show(&agent);
        app.run_effects();
        app.resolve();
        let first = started.elapsed();

        let mut cpu = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
        let mut pixels = Vec::new();
        let document = app.document();
        let mut paint = || {
            let started = Instant::now();
            cpu.render_to_vec(
                |painter| {
                    blitz_paint::paint_scene(
                        painter,
                        &mut document.borrow_mut(),
                        scale as f64,
                        width,
                        height,
                        0,
                        0,
                    )
                },
                &mut pixels,
            );
            started.elapsed()
        };
        paint();

        session.begin_turn("clippy の警告も直してください");
        session.show(&agent);
        app.run_effects();
        app.resolve();
        let (mut effects, mut resolves, mut paints) = (Vec::new(), Vec::new(), Vec::new());
        let characters: Vec<char> = STREAMED.chars().collect();
        for chunk in characters.chunks(4) {
            session.stream(&chunk.iter().collect::<String>());
            let started = Instant::now();
            session.show(&agent);
            app.run_effects();
            effects.push(started.elapsed());
            let started = Instant::now();
            app.resolve();
            resolves.push(started.elapsed());
            paints.push(paint());
        }
        let idle = (0..21)
            .map(|_| {
                let started = Instant::now();
                app.resolve();
                started.elapsed()
            })
            .collect();
        // A receipt of the first turn, far out of view.
        let key = session.receipt_keys(1)[0];
        let toggles = (0..20)
            .map(|_| {
                let started = Instant::now();
                agent.expanded.update(|expanded| {
                    if !expanded.remove(&key) {
                        expanded.insert(key);
                    }
                });
                app.run_effects();
                app.resolve();
                started.elapsed()
            })
            .collect();
        // The wheel, 400 px a step, up through the history.
        ui_foundation_spike::move_to(&app, 400., 300.);
        let scrolls = (0..30)
            .map(|_| {
                let started = Instant::now();
                app.dispatch(UiEvent::Wheel(BlitzWheelEvent {
                    delta: BlitzWheelDelta::Pixels(0., 400.),
                    coords: PointerCoords {
                        page_x: 400.,
                        page_y: 300.,
                        screen_x: 400.,
                        screen_y: 300.,
                        client_x: 400.,
                        client_y: 300.,
                    },
                    buttons: Default::default(),
                    mods: Default::default(),
                    element: Default::default(),
                }));
                app.resolve();
                started.elapsed()
            })
            .collect();
        let pair = |samples| {
            let (median, worst) = median(samples);
            format!("{median:5.2} / {worst:5.2}")
        };
        println!(
            "{turns:5}  {:5}  {:5}  {:8.1}  | {}   {}   {}  | {:5.2}        | {}  | {}",
            session.units(),
            nodes(&app),
            first.as_secs_f64() * 1000.,
            pair(effects),
            pair(resolves),
            pair(paints),
            median(idle).0,
            pair(toggles),
            pair(scrolls),
        );
    }
}

/// The sample of Horizon's own agent previews: a turn in progress waiting
/// on an approval, or with `idle` the session idle with the long turn's
/// edit and its failed command open, as the `agent-idle` preview has them.
fn picture(path: &str, idle: bool, scale: f32) {
    let session = Session::preview(!idle);
    let (app, agent) = open(|_| {}, Column::Windowed);
    // The page is 900 by 1185 CSS pixels: the pane Horizon's own preview
    // has in a window 900 by 1280.
    let (width, height) = ((900. * scale) as u32, (1185. * scale) as u32);
    (app.document().borrow_mut()).set_viewport(Viewport::new(
        width,
        height,
        scale,
        ColorScheme::Light,
    ));
    if idle {
        // The long turn's last receipt, its edit's diff and its failed
        // command's output.
        let key = *session.receipt_keys(1).last().expect("a receipt");
        agent.expanded.update(|expanded| {
            expanded.insert(key);
        });
        agent.open_bodies.update(|open| {
            open.extend([(key, 0), (key, 1)]);
        });
    }
    session.show(&agent);
    app.run_effects();
    app.resolve_at(0.3);
    let document = app.document();
    let mut renderer = anyrender_vello::VelloImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    renderer.render_to_vec(
        |scene| {
            blitz_paint::paint_scene(
                scene,
                &mut document.borrow_mut(),
                scale as f64,
                width,
                height,
                0,
                0,
            )
        },
        &mut pixels,
    );
    let file = std::fs::File::create(path).expect("the PNG file");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("a PNG header");
    writer.write_image_data(&pixels).expect("PNG data");
    println!("wrote {path}");
}

/// What the made-up session does next.
#[derive(Clone, Copy, PartialEq)]
enum Step {
    Idle(u32),
    Thinking(u32),
    Streaming(usize),
    Calling(u32),
    Waiting,
    Closing(usize),
}

fn window(turns: usize, column: Column) {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let log = asked.clone();
    let (app, agent) = open(move |asked| log.borrow_mut().push(asked), column);
    let mut session = Session::long(turns);
    session.show(&agent);

    let characters: Vec<char> = STREAMED.chars().collect();
    let step = Cell::new(Step::Idle(0));
    let started = Cell::new(false);
    let mut request = "clippy の警告も直してください".to_string();
    app.on_poll(move |waker| {
        if !started.replace(true) {
            // The clock: a poll every 30 ms.
            let waker = waker.clone();
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(30));
                    waker.wake_by_ref();
                }
            });
        }
        let asked: Vec<Asked> = asked.borrow_mut().drain(..).collect();
        let answered =
            (asked.iter()).any(|asked| matches!(asked, Asked::Approve(_) | Asked::Deny(_)));
        let stopped = asked.contains(&Asked::Stop);
        let sent = asked.iter().find_map(|asked| match asked {
            Asked::Send(text) => Some(text.clone()),
            _ => None,
        });
        let next = match step.get() {
            // A turn starts when the owner sends something, or by itself
            // after a pause.
            Step::Idle(ticks) => {
                if let Some(text) = sent {
                    request = text;
                }
                if ticks > 60 || asked.iter().any(|asked| matches!(asked, Asked::Send(_))) {
                    session.begin_turn(&request);
                    Step::Thinking(0)
                } else {
                    Step::Idle(ticks + 1)
                }
            }
            _ if stopped => {
                session.end_turn();
                Step::Idle(0)
            }
            Step::Thinking(ticks) if ticks < 40 => Step::Thinking(ticks + 1),
            Step::Thinking(_) => Step::Streaming(0),
            Step::Streaming(at) if at < characters.len() => {
                let end = (at + 4).min(characters.len());
                session.stream(&characters[at..end].iter().collect::<String>());
                Step::Streaming(end)
            }
            Step::Streaming(_) => {
                session.call("Run", "cargo clippy --workspace --all-targets", None);
                Step::Calling(0)
            }
            Step::Calling(ticks) if ticks < 50 => Step::Calling(ticks + 1),
            Step::Calling(_) => {
                session.finish_call();
                session.call(
                    "Write",
                    "~/.config/horizon/config.toml",
                    Some("リポジトリの外のファイルを書き換えます"),
                );
                Step::Waiting
            }
            Step::Waiting if answered => {
                session.finish_call();
                Step::Closing(0)
            }
            Step::Waiting => Step::Waiting,
            Step::Closing(at) if at < 120 => {
                let end = (at + 4).min(120);
                session.stream(&characters[at..end].iter().collect::<String>());
                Step::Closing(end)
            }
            Step::Closing(_) => {
                session.end_turn();
                Step::Idle(0)
            }
        };
        step.set(next);
        session.show(&agent);
    });

    blitz_shell::set_after_resolve(ui_foundation_spike::after_resolve);
    let event_loop = create_default_event_loop();
    let (proxy, events) = BlitzShellProxy::new(event_loop.create_proxy());
    let mut application = BlitzApplication::new(proxy, events);
    application.add_window(WindowConfig::new(
        Box::new(Live(app)),
        VelloWindowRenderer::new(),
    ));
    event_loop.run_app(application).expect("the event loop");
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("measure") => measure(
            (arguments.get(1))
                .map(|scale| scale.parse().expect("a display scale"))
                .unwrap_or(1.5),
        ),
        Some("picture") => picture(
            arguments.get(1).expect("a PNG path"),
            arguments.get(2).map(String::as_str) == Some("idle"),
            (arguments.get(3))
                .map(|scale| scale.parse().expect("a display scale"))
                .unwrap_or(1.5),
        ),
        Some("window") | None => window(
            (arguments.get(1))
                .map(|turns| turns.parse().expect("a number of turns"))
                .unwrap_or(200),
            match arguments.get(2).map(String::as_str) {
                Some("whole") => Column::Whole,
                _ => Column::Windowed,
            },
        ),
        Some(other) => panic!("window, measure or picture, not {other}"),
    }
}
