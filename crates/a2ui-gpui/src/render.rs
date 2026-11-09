//! The GPUI half: draws every surface of one agent connection with
//! gpui-component widgets.
//!
//! GPUI rebuilds its element tree each render, so the view keeps no
//! retained mirror of the component tree. The only retained state is one
//! `InputState` per text field instance (keyed by surface, component id and
//! template scope), because a text input owns its cursor and IME state.

use std::collections::HashMap;
use std::time::SystemTime;

use gpui::{
    div, prelude::*, px, AnyElement, App, Context, Entity, EventEmitter, FontWeight, SharedString,
    Subscription, Window,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex, ActiveTheme as _};
use serde_json::Value;

use crate::protocol::{ClientMessage, Component};
use crate::surface::{SurfaceError, Surfaces, ROOT_ID};
use crate::time::rfc3339_utc;

/// Guards against a component graph that refers back to itself.
const MAX_DEPTH: usize = 64;

/// Emitted when a component's `event` action fires. The host forwards
/// `message` to the agent; `data_model` is the whole surface data model when
/// the surface was created with `sendDataModel: true` (the spec carries it
/// as transport-envelope metadata, which is the host's to attach).
#[derive(Clone, Debug)]
pub struct ActionEvent {
    pub message: ClientMessage,
    pub data_model: Option<Value>,
}

/// What a `TextField` instance needs to create its input state.
struct TextFieldBinding {
    surface_id: String,
    /// The absolute pointer the field's `value` is bound to, if bound.
    path: Option<String>,
    initial: String,
    obscured: bool,
}

struct BoundInput {
    surface_id: String,
    path: Option<String>,
    state: Entity<InputState>,
    _subscription: Subscription,
}

/// Every surface of one agent connection, drawn top to bottom.
pub struct SurfaceView {
    surfaces: Surfaces,
    inputs: HashMap<String, BoundInput>,
}

impl EventEmitter<ActionEvent> for SurfaceView {}

impl Default for SurfaceView {
    fn default() -> Self {
        Self::new()
    }
}

impl SurfaceView {
    pub fn new() -> Self {
        Self {
            surfaces: Surfaces::new(),
            inputs: HashMap::new(),
        }
    }

    pub fn surfaces(&self) -> &Surfaces {
        &self.surfaces
    }

    /// Applies one agent-to-renderer message (one JSONL line).
    pub fn apply_json(
        &mut self,
        line: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<String, SurfaceError> {
        let touched = self.surfaces.apply_json(line)?;
        self.sync_inputs(window, cx);
        cx.notify();
        Ok(touched)
    }

    /// Pushes data-model values the agent changed into the text inputs bound
    /// to them, and drops inputs whose surface is gone. `set_value` emits no
    /// change event, so this does not echo back into the model.
    fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let surfaces = &self.surfaces;
        self.inputs
            .retain(|_, input| surfaces.get(&input.surface_id).is_some());
        for input in self.inputs.values() {
            let (Some(path), Some(surface)) =
                (input.path.as_deref(), self.surfaces.get(&input.surface_id))
            else {
                continue;
            };
            let wanted = match surface.get(path) {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            };
            if input.state.read(cx).value().as_ref() != wanted {
                input
                    .state
                    .update(cx, |state, cx| state.set_value(wanted, window, cx));
            }
        }
    }

    fn fire(&mut self, surface_id: &str, component_id: &str, scope: &str, cx: &mut Context<Self>) {
        let Some(surface) = self.surfaces.get(surface_id) else {
            return;
        };
        let Some(component) = surface.component(component_id) else {
            return;
        };
        if let Some(message) = surface.action(component, scope, rfc3339_utc(SystemTime::now())) {
            let data_model = surface.sends_data_model().then(|| surface.data().clone());
            cx.emit(ActionEvent {
                message,
                data_model,
            });
        }
    }

    fn input_state(
        &mut self,
        key: &str,
        field: TextFieldBinding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(input) = self.inputs.get(key) {
            return input.state.clone();
        }
        let TextFieldBinding {
            surface_id,
            path,
            initial,
            obscured,
        } = field;
        let state = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(obscured)
                .default_value(initial)
        });
        let write_back = {
            let surface_id = surface_id.clone();
            let path = path.clone();
            cx.subscribe_in(&state, window, move |view, state, event, _window, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let text = state.read(cx).value().to_string();
                if let (Some(path), Some(surface)) =
                    (path.as_deref(), view.surfaces.get_mut(&surface_id))
                {
                    surface.set(path, Value::String(text));
                }
                cx.notify();
            })
        };
        self.inputs.insert(
            key.to_string(),
            BoundInput {
                surface_id,
                path,
                state: state.clone(),
                _subscription: write_back,
            },
        );
        state
    }

    fn render_node(
        &mut self,
        surface_id: &str,
        component_id: &str,
        scope: &str,
        depth: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if depth > MAX_DEPTH {
            return placeholder("component nesting too deep".into(), cx);
        }
        let Some(surface) = self.surfaces.get(surface_id) else {
            return div().into_any_element();
        };
        let Some(component) = surface.component(component_id).cloned() else {
            return placeholder(format!("missing component {component_id:?}"), cx);
        };
        let key = format!("{surface_id}:{component_id}@{scope}");
        let null = Value::Null;
        let prop = |name: &str| component.prop(name).unwrap_or(&null);

        match component.component.as_str() {
            "Text" => {
                let text = surface.resolve_string(prop("text"), scope);
                text_element(text, component.prop_str("variant"), cx)
            }
            "Row" | "Column" | "List" => {
                let horizontal = component.component == "Row"
                    || component.prop_str("direction") == Some("horizontal");
                let children = surface.children(&component, scope);
                let mut container = if horizontal { h_flex() } else { v_flex() }.gap(px(8.));
                container = flex_layout(container, &component);
                for (child_id, child_scope) in children {
                    container = container.child(self.render_node(
                        surface_id,
                        &child_id,
                        &child_scope,
                        depth + 1,
                        window,
                        cx,
                    ));
                }
                container.into_any_element()
            }
            "Card" => {
                let child = component.prop_str("child").map(str::to_string);
                let border = cx.theme().border;
                let mut card = div()
                    .p(px(12.))
                    .border_1()
                    .border_color(border)
                    .rounded(px(8.));
                if let Some(child) = child {
                    card = card.child(self.render_node(
                        surface_id,
                        &child,
                        scope,
                        depth + 1,
                        window,
                        cx,
                    ));
                }
                card.into_any_element()
            }
            "Button" => {
                let child = component.prop_str("child").map(str::to_string);
                let variant = component.prop_str("variant").map(str::to_string);
                let mut button = Button::new(SharedString::from(key));
                button = match variant.as_deref() {
                    Some("primary") => button.primary(),
                    Some("borderless") => button.ghost(),
                    _ => button,
                };
                if let Some(child) = child {
                    button = button.child(self.render_node(
                        surface_id,
                        &child,
                        scope,
                        depth + 1,
                        window,
                        cx,
                    ));
                }
                let (surface_id, component_id, scope) = (
                    surface_id.to_string(),
                    component_id.to_string(),
                    scope.to_string(),
                );
                button
                    .on_click(cx.listener(move |view, _, _window, cx| {
                        view.fire(&surface_id, &component_id, &scope, cx);
                    }))
                    .into_any_element()
            }
            "TextField" => {
                let label = surface.resolve_string(prop("label"), scope);
                let field = TextFieldBinding {
                    surface_id: surface_id.to_string(),
                    path: surface.bound_path(prop("value"), scope),
                    initial: surface.resolve_string(prop("value"), scope),
                    obscured: component.prop_str("variant") == Some("obscured"),
                };
                let state = self.input_state(&key, field, window, cx);
                v_flex()
                    .gap(px(4.))
                    .child(div().text_sm().child(label))
                    .child(Input::new(&state))
                    .into_any_element()
            }
            "CheckBox" => {
                let label = surface.resolve_string(prop("label"), scope);
                let checked = surface.resolve_bool(prop("value"), scope);
                let path = surface.bound_path(prop("value"), scope);
                let surface_id = surface_id.to_string();
                Checkbox::new(SharedString::from(key))
                    .label(label)
                    .checked(checked)
                    .on_click(cx.listener(move |view, requested: &bool, _window, cx| {
                        if let (Some(path), Some(surface)) =
                            (path.as_deref(), view.surfaces.get_mut(&surface_id))
                        {
                            surface.set(path, Value::Bool(*requested));
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }
            other => placeholder(format!("unsupported component {other:?}"), cx),
        }
    }
}

impl Render for SurfaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ids: Vec<String> = self.surfaces.ids().map(str::to_string).collect();
        let mut column = v_flex().gap(px(16.));
        for id in ids {
            let has_root = self
                .surfaces
                .get(&id)
                .is_some_and(|surface| surface.root().is_some());
            column = column.child(if has_root {
                self.render_node(&id, ROOT_ID, "", 0, window, cx)
            } else {
                // The spec buffers components until `root` arrives.
                div().into_any_element()
            });
        }
        column
    }
}

/// `Text` renders "simple Markdown"; this spike honours only a leading
/// heading marker (`#` .. `###`) and the `caption` variant.
fn text_element(text: String, variant: Option<&str>, cx: &App) -> AnyElement {
    let level = text.chars().take_while(|c| *c == '#').count();
    let body = match text[level..].strip_prefix(' ') {
        Some(rest) if level > 0 => rest.to_string(),
        _ => text.clone(),
    };
    let mut element = div().child(body);
    element = match level {
        1 => element.text_2xl().font_weight(FontWeight::BOLD),
        2 => element.text_xl().font_weight(FontWeight::BOLD),
        3 => element.text_lg().font_weight(FontWeight::SEMIBOLD),
        _ => element,
    };
    if variant == Some("caption") {
        element = element.text_sm().text_color(cx.theme().muted_foreground);
    }
    element.into_any_element()
}

fn flex_layout<E: Styled>(element: E, component: &Component) -> E {
    let element = match component.prop_str("justify") {
        Some("center") => element.justify_center(),
        Some("end") => element.justify_end(),
        Some("spaceBetween") => element.justify_between(),
        Some("spaceAround") => element.justify_around(),
        Some("spaceEvenly") => element.justify_evenly(),
        _ => element,
    };
    match component.prop_str("align") {
        Some("start") => element.items_start(),
        Some("center") => element.items_center(),
        Some("end") => element.items_end(),
        _ => element,
    }
}

/// What an unknown or broken node draws instead of failing the surface.
fn placeholder(message: String, cx: &App) -> AnyElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(format!("[{message}]"))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{TestAppContext, WindowHandle};
    use serde_json::json;

    use super::*;
    use crate::protocol::ClientBody;

    const SCRIPT: &str = include_str!("../examples/login.jsonl");

    fn open(cx: &mut TestAppContext) -> WindowHandle<SurfaceView> {
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| {
            let mut view = SurfaceView::new();
            for line in SCRIPT.lines().filter(|line| !line.trim().is_empty()) {
                view.apply_json(line, window, cx).unwrap();
            }
            view
        });
        cx.run_until_parked();
        window
    }

    fn input(view: &SurfaceView, component_id: &str) -> Entity<InputState> {
        view.inputs[&format!("login:{component_id}@")].state.clone()
    }

    /// Stands in for typing: replace the text and emit the change event the
    /// input emits for an edit.
    fn type_into(window: WindowHandle<SurfaceView>, cx: &mut TestAppContext, id: &str, text: &str) {
        window
            .update(cx, |view, window, cx| {
                input(view, id).update(cx, |state, cx| {
                    state.set_value(text.to_string(), window, cx);
                    cx.emit(InputEvent::Change);
                });
            })
            .unwrap();
        cx.run_until_parked();
    }

    fn model(window: WindowHandle<SurfaceView>, cx: &mut TestAppContext, path: &str) -> Value {
        window
            .read_with(cx, |view, _| {
                view.surfaces()
                    .get("login")
                    .unwrap()
                    .get(path)
                    .cloned()
                    .unwrap()
            })
            .unwrap()
    }

    #[gpui::test]
    fn rendering_creates_one_bound_input_per_text_field(cx: &mut TestAppContext) {
        let window = open(cx);
        window
            .read_with(cx, |view, _| {
                let mut keys: Vec<_> = view.inputs.keys().cloned().collect();
                keys.sort();
                assert_eq!(keys, ["login:password_field@", "login:username_field@"]);
            })
            .unwrap();
    }

    #[gpui::test]
    fn typing_writes_back_into_the_data_model(cx: &mut TestAppContext) {
        let window = open(cx);
        type_into(window, cx, "username_field", "grace");
        assert_eq!(model(window, cx, "/username"), json!("grace"));
    }

    #[gpui::test]
    fn an_agent_update_reaches_the_bound_input(cx: &mut TestAppContext) {
        let window = open(cx);
        type_into(window, cx, "password_field", "hunter2");
        window
            .update(cx, |view, window, cx| {
                view.apply_json(
                    r#"{"version":"1.0","updateDataModel":{"surfaceId":"login","path":"/password","value":""}}"#,
                    window,
                    cx,
                )
                .unwrap();
            })
            .unwrap();
        cx.run_until_parked();
        window
            .read_with(cx, |view, cx| {
                assert_eq!(input(view, "password_field").read(cx).value().as_ref(), "");
            })
            .unwrap();
        // The cleared value did not echo back as a user edit either.
        assert_eq!(model(window, cx, "/password"), json!(""));
    }

    #[gpui::test]
    fn a_button_event_emits_the_resolved_action(cx: &mut TestAppContext) {
        let window = open(cx);
        type_into(window, cx, "username_field", "ada");
        let seen: Rc<RefCell<Vec<ActionEvent>>> = Rc::default();
        let entity = window.root(cx).unwrap();
        let _subscription = cx.update(|cx| {
            let seen = seen.clone();
            cx.subscribe(&entity, move |_, event: &ActionEvent, _| {
                seen.borrow_mut().push(event.clone())
            })
        });
        window
            .update(cx, |view, _, cx| {
                view.fire("login", "submit_button", "", cx)
            })
            .unwrap();
        cx.run_until_parked();

        let seen = seen.borrow();
        assert_eq!(seen.len(), 1);
        let ClientBody::Action(action) = &seen[0].message.body;
        assert_eq!(action.name, "login_submitted");
        assert_eq!(action.source_component_id, "submit_button");
        assert_eq!(
            Value::Object(action.context.clone()),
            json!({"user": "ada", "remember": false})
        );
        // `sendDataModel: true` hands the host the whole model to attach.
        assert_eq!(
            seen[0].data_model.as_ref().unwrap()["username"],
            json!("ada")
        );
    }

    #[gpui::test]
    fn template_rows_follow_appended_items(cx: &mut TestAppContext) {
        let window = open(cx);
        for label in ["first", "second"] {
            window
                .update(cx, |view, window, cx| {
                    view.apply_json(
                        &json!({"version": "1.0", "updateDataModel": {
                            "surfaceId": "login", "path": "/history/-", "value": {"label": label}
                        }})
                        .to_string(),
                        window,
                        cx,
                    )
                    .unwrap();
                })
                .unwrap();
        }
        cx.run_until_parked();
        window
            .read_with(cx, |view, _| {
                let surface = view.surfaces().get("login").unwrap();
                let list = surface.component("history_list").unwrap();
                let row = surface.component("history_row").unwrap();
                let labels: Vec<_> = surface
                    .children(list, "")
                    .iter()
                    .map(|(_, scope)| surface.resolve_string(row.prop("text").unwrap(), scope))
                    .collect();
                assert_eq!(labels, ["first", "second"]);
            })
            .unwrap();
    }
}
