//! A real shell in the terminal view, in a window:
//!
//!     cargo run --release --bin live_terminal
//!
//! It runs `$SHELL` (or `sh`). Keys, the input method, Ctrl+Shift+C and
//! Ctrl+Shift+V, a drag to select, the wheel and the window's size all go
//! to it. With `picture out.png` it runs a few commands in `sh` and draws
//! the screen into the PNG; with `measure` it times a long output, with no
//! window either way.

use std::time::{Duration, Instant};

use anyrender::ImageRenderer as _;
use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};
use horizon_terminal_core::TerminalSize;
use ui_foundation_spike::blitz_traits::events::{BlitzKeyEvent, KeyState, UiEvent};
use ui_foundation_spike::blitz_traits::shell::{ColorScheme, Viewport};
use ui_foundation_spike::pty::Session;
use ui_foundation_spike::terminal::{CSS, Terminal, view};
use ui_foundation_spike::{App, Live};

fn open(program: &str) -> (App, Terminal) {
    let session = Session::spawn(program, &[], TerminalSize::new(80, 24)).expect("a shell");
    let mut terminal = None;
    let app = App::new(1000, 600, CSS, || {
        let (root, state) = view(session);
        terminal = Some(state);
        root.focus()
    });
    (app, terminal.unwrap())
}

/// Pastes `command` and presses Enter.
fn run(app: &App, command: &str) {
    app.dispatch(UiEvent::Paste(command.to_string()));
    app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
        key: keyboard_types::Key::Named(keyboard_types::NamedKey::Enter),
        code: keyboard_types::Code::Enter,
        modifiers: Default::default(),
        location: keyboard_types::Location::Standard,
        is_auto_repeating: false,
        is_composing: false,
        state: KeyState::Pressed,
        text: None,
    }));
}

fn wait(app: &App, terminal: &Terminal, met: impl Fn(&str) -> bool) {
    let started = Instant::now();
    loop {
        app.resolve();
        if met(&terminal.text()) || started.elapsed() > Duration::from_secs(20) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn picture(path: &str) {
    let (app, terminal) = open("sh");
    let (scale, width, height) = (1.5, 1500, 900);
    (app.document().borrow_mut()).set_viewport(Viewport::new(
        width,
        height,
        scale,
        ColorScheme::Light,
    ));
    wait(&app, &terminal, |text| text.trim_end().ends_with('$'));
    // Wide characters beside narrow ones, with a bar that lines up when
    // every character sits on its cell; then colors.
    run(
        &app,
        "printf '日本語のテキスト|\\nabcdefghijklmnop|\\nあaいbうc|\\n'",
    );
    run(
        &app,
        "printf '\\033[31mred \\033[32mgreen \\033[34mblue \\033[1;35mbold \\033[0;7minverse\\033[0m \\033[48;5;220m 256 \\033[0m \\033[38;2;255;100;0mtruecolor\\033[0m\\n'",
    );
    run(&app, "ls --color=always /");
    run(&app, "echo done-$((40+2))");
    wait(&app, &terminal, |text| text.contains("done-42"));
    app.resolve();
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

/// A long output, taken in and painted frame by frame as a window would.
fn measure() {
    let (app, terminal) = open("sh");
    let (scale, width, height) = (1.5, 1500, 900);
    (app.document().borrow_mut()).set_viewport(Viewport::new(
        width,
        height,
        scale,
        ColorScheme::Light,
    ));
    wait(&app, &terminal, |text| text.trim_end().ends_with('$'));
    let document = app.document();
    let mut renderer = anyrender_vello::VelloImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    let commands = [
        "seq 1 200000",
        "yes 'the quick brown fox 日本語' | head -n 200000",
    ];
    for (index, command) in commands.into_iter().enumerate() {
        // A mark the command's own text does not hold, to know its end by.
        let mark = format!("finished-{}", index + 2);
        run(&app, &format!("{command}; echo finished-$(({index}+2))"));
        let started = Instant::now();
        let (mut frames, mut resolves, mut paints) = (0, Vec::new(), Vec::new());
        loop {
            let resolve = Instant::now();
            app.resolve();
            resolves.push(resolve.elapsed());
            let paint = Instant::now();
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
            paints.push(paint.elapsed());
            frames += 1;
            if terminal.text().contains(&mark) || started.elapsed().as_secs() > 60 {
                break;
            }
        }
        let median = |mut samples: Vec<Duration>| {
            samples.sort();
            samples[samples.len() / 2].as_secs_f64() * 1000.
        };
        let (columns, rows) = terminal.size();
        println!(
            "{command}\n  {columns}x{rows} at {scale}: {:.2} s, {frames} frames drawn; a frame: layout {:.2} ms, paint and read back {:.2} ms (median)",
            started.elapsed().as_secs_f64(),
            median(resolves),
            median(paints),
        );
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("picture") => return picture(arguments.get(1).expect("a PNG path")),
        Some("measure") => return measure(),
        _ => {}
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
    let (app, terminal) = open(&shell);
    let started = std::cell::Cell::new(false);
    app.on_poll(move |waker| {
        // A frame from the session wakes the window to draw it.
        if !started.replace(true) {
            terminal.set_waker(waker.clone());
        }
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
