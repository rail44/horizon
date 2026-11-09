//! A window showing an A2UI surface next to its live data model and the
//! messages sent to a mock agent.
//!
//! ```sh
//! cargo run -p a2ui-gpui --example login [path/to/script.jsonl]
//! ```
//!
//! Without an argument it loads `examples/login.jsonl`. The mock agent
//! answers `login_submitted` the way a real one would answer over the wire:
//! with `updateDataModel` messages (a status line, a history row appended to
//! a list template, and a cleared password field).

use a2ui_gpui::protocol::ClientBody;
use a2ui_gpui::{ActionEvent, SurfaceView};
use gpui::{
    div, prelude::*, px, size, App, Bounds, Context, Entity, Subscription, Window, WindowBounds,
    WindowOptions,
};
use gpui_component::{h_flex, v_flex, ActiveTheme as _, Root};
use serde_json::{json, Value};

const DEFAULT_SCRIPT: &str = include_str!("login.jsonl");

struct Demo {
    surface: Entity<SurfaceView>,
    sent: Vec<String>,
    errors: Vec<String>,
    _actions: Subscription,
}

impl Demo {
    fn new(script: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let surface = cx.new(|_| SurfaceView::new());
        let mut errors = Vec::new();
        surface.update(cx, |view, cx| {
            for line in script.lines().filter(|line| !line.trim().is_empty()) {
                if let Err(error) = view.apply_json(line, window, cx) {
                    errors.push(error.to_string());
                }
            }
        });
        let actions = cx.subscribe_in(
            &surface,
            window,
            |demo, surface, event: &ActionEvent, window, cx| {
                demo.sent
                    .push(serde_json::to_string(&event.message).unwrap_or_default());
                for reply in mock_agent(event) {
                    surface.update(cx, |view, cx| {
                        if let Err(error) = view.apply_json(&reply, window, cx) {
                            demo.errors.push(error.to_string());
                        }
                    });
                }
                cx.notify();
            },
        );
        Self {
            surface,
            sent: Vec::new(),
            errors,
            _actions: actions,
        }
    }
}

/// What an agent would send back for each action this demo knows.
fn mock_agent(event: &ActionEvent) -> Vec<String> {
    let ClientBody::Action(action) = &event.message.body;
    if action.name != "login_submitted" {
        return Vec::new();
    }
    let user = action
        .context
        .get("user")
        .and_then(Value::as_str)
        .unwrap_or("");
    let remember = action.context.get("remember") == Some(&Value::Bool(true));
    let update = |path: &str, value: Value| {
        json!({"version": "1.0", "updateDataModel": {
            "surfaceId": action.surface_id, "path": path, "value": value
        }})
        .to_string()
    };
    vec![
        update("/status", json!(format!("Signed in as {user:?}"))),
        update(
            "/history/-",
            json!({"label": format!("{} — {user} (remember: {remember})", action.timestamp)}),
        ),
        update("/password", json!("")),
    ]
}

impl Render for Demo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let data_model = self
            .surface
            .read(cx)
            .surfaces()
            .get("login")
            .map(|surface| serde_json::to_string_pretty(surface.data()).unwrap_or_default())
            .unwrap_or_default();
        let muted = cx.theme().muted_foreground;
        let panel = |title: &'static str, body: String| {
            v_flex()
                .gap(px(4.))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(title),
                )
                .child(div().text_xs().text_color(muted).child(body))
        };
        h_flex()
            .size_full()
            .items_start()
            .gap(px(24.))
            .p(px(16.))
            .bg(cx.theme().background)
            .child(div().w(px(360.)).child(self.surface.clone()))
            .child(
                v_flex()
                    .flex_1()
                    .gap(px(16.))
                    .child(panel("Data model", data_model))
                    .child(panel("Sent to agent", self.sent.join("\n")))
                    .when(!self.errors.is_empty(), |this| {
                        this.child(panel("Errors", self.errors.join("\n")))
                    }),
            )
    }
}

fn main() {
    let script = match std::env::args().nth(1) {
        Some(path) => std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {path}: {error}")),
        None => DEFAULT_SCRIPT.to_string(),
    };
    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        let bounds = Bounds::centered(None, size(px(960.), px(640.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let demo = cx.new(|cx| Demo::new(script, window, cx));
                cx.new(|cx| Root::new(demo, window, cx))
            },
        )
        .expect("open window");
        cx.activate(true);
    });
}
