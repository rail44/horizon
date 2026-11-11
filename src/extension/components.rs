//! The components extensions place by name: Horizon's own parts, drawn by
//! Horizon, so an extension's view reads like the views around it.

use gpui::{ElementId, IntoElement, SharedString};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::Sizable as _;
use gpui_wit_plugin::Runtime;
use serde_json::{json, Value};

use crate::theme;
use crate::ui::parts::{chip, linked_markdown_body};

fn string(props: &Value, key: &str) -> String {
    props[key].as_str().unwrap_or_default().to_owned()
}

pub(super) fn register(runtime: &mut Runtime) {
    // `{ id, source }`: Markdown as running text. A clicked link is the
    // `link` event, with its target.
    runtime.register("markdown", |props, _, _| {
        let emit = props.emit.clone();
        linked_markdown_body(
            ElementId::Name(string(props.props, "id").into()),
            string(props.props, "source"),
            theme::text_primary(),
            move |target, _, cx| emit.emit("link", json!(target.as_ref()), cx),
        )
        .into_any_element()
    });
    // `{ label }`: a quiet button. `click` when pressed.
    runtime.register("button", |props, _, _| {
        let emit = props.emit.clone();
        Button::new(props.id)
            .ghost()
            .xsmall()
            .label(SharedString::from(string(props.props, "label")))
            .on_click(move |_, _, cx| emit.emit("click", Value::Null, cx))
            .into_any_element()
    });
    // `{ label }`: a muted chip.
    runtime.register("chip", |props, _, _| {
        chip(string(props.props, "label"), theme::text_muted()).into_any_element()
    });
}
