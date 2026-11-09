//! Surface state: the component map, the data model, and the reads the
//! renderer needs (bindings, children, actions). Nothing here touches GPUI.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use serde_json::{Map, Value};

use crate::pointer;
use crate::protocol::{
    Action, ChildList, ClientBody, ClientMessage, Component, CreateSurface, Dynamic, ServerBody,
    ServerMessage, PROTOCOL_VERSION,
};

/// The id every surface's root component must carry.
pub const ROOT_ID: &str = "root";

#[derive(Clone, Debug, PartialEq)]
pub enum SurfaceError {
    Parse(String),
    UnsupportedVersion(String),
    UnknownSurface(String),
    DuplicateSurface(String),
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "malformed message: {message}"),
            Self::UnsupportedVersion(version) => write!(f, "unsupported version {version:?}"),
            Self::UnknownSurface(id) => write!(f, "no surface with id {id:?}"),
            Self::DuplicateSurface(id) => write!(f, "surface {id:?} already exists"),
        }
    }
}

impl std::error::Error for SurfaceError {}

/// One surface: its components by id and its data model.
#[derive(Clone, Debug, PartialEq)]
pub struct Surface {
    id: String,
    catalog_id: Option<String>,
    send_data_model: bool,
    components: HashMap<String, Component>,
    data: Value,
}

impl Surface {
    pub fn new(create: CreateSurface) -> Self {
        // The spec applies `dataModel` before `components` when both are
        // present; with no observers yet the order only matters for intent.
        let mut surface = Self {
            id: create.surface_id,
            catalog_id: create.catalog_id,
            send_data_model: create.send_data_model,
            components: HashMap::new(),
            data: create
                .data_model
                .unwrap_or_else(|| Value::Object(Map::new())),
        };
        surface.update_components(create.components);
        surface
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn catalog_id(&self) -> Option<&str> {
        self.catalog_id.as_deref()
    }

    pub fn sends_data_model(&self) -> bool {
        self.send_data_model
    }

    pub fn data(&self) -> &Value {
        &self.data
    }

    /// Upserts components by id.
    pub fn update_components(&mut self, components: Vec<Component>) {
        for component in components {
            self.components.insert(component.id.clone(), component);
        }
    }

    pub fn component(&self, id: &str) -> Option<&Component> {
        self.components.get(id)
    }

    /// `None` until a component with id `root` has arrived; the spec buffers
    /// the other components until then.
    pub fn root(&self) -> Option<&Component> {
        self.component(ROOT_ID)
    }

    pub fn get(&self, pointer: &str) -> Option<&Value> {
        pointer::get(&self.data, pointer)
    }

    pub fn set(&mut self, pointer: &str, value: Value) {
        pointer::set(&mut self.data, pointer, value);
    }

    /// The absolute pointer a property is bound to, if it is a binding.
    pub fn bound_path(&self, value: &Value, scope: &str) -> Option<String> {
        match Dynamic::of(value) {
            Dynamic::Path(path) => Some(pointer::join(scope, path)),
            _ => None,
        }
    }

    /// Resolves a property to a JSON value. A function call resolves to
    /// `None`: this spike does not evaluate functions.
    pub fn resolve(&self, value: &Value, scope: &str) -> Option<Value> {
        match Dynamic::of(value) {
            Dynamic::Literal(literal) => Some(literal.clone()),
            Dynamic::Path(path) => self.get(&pointer::join(scope, path)).cloned(),
            Dynamic::Call(_) => None,
        }
    }

    /// Resolves a property for display: strings as-is, other scalars via
    /// their JSON text, missing values as the empty string.
    pub fn resolve_string(&self, value: &Value, scope: &str) -> String {
        match self.resolve(value, scope) {
            Some(Value::String(text)) => text,
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    }

    pub fn resolve_bool(&self, value: &Value, scope: &str) -> bool {
        matches!(self.resolve(value, scope), Some(Value::Bool(true)))
    }

    /// A container's children as `(component id, scope)` pairs. A template
    /// is stamped once per item of the array at its path; each stamp's scope
    /// is that item's absolute pointer, which relative bindings resolve
    /// against.
    pub fn children(&self, component: &Component, scope: &str) -> Vec<(String, String)> {
        let Some(list) = component.prop("children").and_then(ChildList::of) else {
            return Vec::new();
        };
        match list {
            ChildList::Ids(ids) => ids
                .into_iter()
                .map(|id| (id.to_string(), scope.to_string()))
                .collect(),
            ChildList::Template { path, component_id } => {
                let base = pointer::join(scope, path);
                let count = match self.get(&base) {
                    Some(Value::Array(items)) => items.len(),
                    _ => 0,
                };
                (0..count)
                    .map(|i| (component_id.to_string(), format!("{base}/{i}")))
                    .collect()
            }
        }
    }

    /// The `action` message a component's `event` action produces, with its
    /// `context` bindings resolved against the current data model. `None`
    /// when the component has no `event` action (a `functionCall` action is
    /// local to the renderer and out of scope here).
    pub fn action(
        &self,
        component: &Component,
        scope: &str,
        timestamp: String,
    ) -> Option<ClientMessage> {
        let event = component.prop("action")?.get("event")?;
        let name = event.get("name")?.as_str()?.to_string();
        let context = event
            .get("context")
            .and_then(Value::as_object)
            .map(|context| {
                context
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            self.resolve(value, scope).unwrap_or(Value::Null),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let user_message = event
            .get("userMessage")
            .map(|message| self.resolve_string(message, scope));
        Some(ClientMessage {
            version: PROTOCOL_VERSION.to_string(),
            body: ClientBody::Action(Action {
                name,
                user_message,
                surface_id: self.id.clone(),
                source_component_id: component.id.clone(),
                timestamp,
                context,
            }),
        })
    }
}

/// Every live surface of one agent connection, keyed by surface id.
#[derive(Clone, Debug, Default)]
pub struct Surfaces {
    surfaces: BTreeMap<String, Surface>,
}

impl Surfaces {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: &str) -> Option<&Surface> {
        self.surfaces.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Surface> {
        self.surfaces.get_mut(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.surfaces.keys().map(String::as_str)
    }

    /// Parses one JSON message (one JSONL line) and applies it. Returns the
    /// id of the surface it touched.
    pub fn apply_json(&mut self, line: &str) -> Result<String, SurfaceError> {
        let message: ServerMessage =
            serde_json::from_str(line).map_err(|error| SurfaceError::Parse(error.to_string()))?;
        self.apply(message)
    }

    pub fn apply(&mut self, message: ServerMessage) -> Result<String, SurfaceError> {
        if message.version != PROTOCOL_VERSION {
            return Err(SurfaceError::UnsupportedVersion(message.version));
        }
        match message.body {
            ServerBody::CreateSurface(create) => {
                let id = create.surface_id.clone();
                if self.surfaces.contains_key(&id) {
                    return Err(SurfaceError::DuplicateSurface(id));
                }
                self.surfaces.insert(id.clone(), Surface::new(create));
                Ok(id)
            }
            ServerBody::UpdateComponents(update) => {
                self.surface_mut(&update.surface_id)?
                    .update_components(update.components);
                Ok(update.surface_id)
            }
            ServerBody::UpdateDataModel(update) => {
                let path = update.path.as_deref().unwrap_or("/");
                self.surface_mut(&update.surface_id)?
                    .set(path, update.value);
                Ok(update.surface_id)
            }
            ServerBody::DeleteSurface(delete) => {
                self.surfaces
                    .remove(&delete.surface_id)
                    .ok_or_else(|| SurfaceError::UnknownSurface(delete.surface_id.clone()))?;
                Ok(delete.surface_id)
            }
        }
    }

    fn surface_mut(&mut self, id: &str) -> Result<&mut Surface, SurfaceError> {
        self.surfaces
            .get_mut(id)
            .ok_or_else(|| SurfaceError::UnknownSurface(id.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The A2UI basic catalog's "Simple Login Form" example
    /// (`catalogs/basic/v1/examples/00_simple-login-form.json`), one message
    /// per line, plus a data model message.
    const LOGIN: &str = r###"
{"version":"1.0","createSurface":{"surfaceId":"login","catalogId":"https://a2ui.org/specification/v1_0/catalogs/basic/catalog.json","sendDataModel":true}}
{"version":"1.0","updateComponents":{"surfaceId":"login","components":[{"id":"root","component":"Column","children":["form_title","username_field","password_field","submit_button"],"justify":"start","align":"stretch"},{"id":"form_title","component":"Text","text":"## Login"},{"id":"username_field","component":"TextField","label":"Username","value":{"@path":"/username"},"variant":"shortText"},{"id":"password_field","component":"TextField","label":"Password","value":{"@path":"/password"},"variant":"obscured"},{"id":"submit_button","component":"Button","child":"submit_label","variant":"primary","action":{"event":{"name":"login_submitted","context":{"user":{"@path":"/username"},"pass":{"@path":"/password"}}}}},{"id":"submit_label","component":"Text","text":"Sign In"}]}}
{"version":"1.0","updateDataModel":{"surfaceId":"login","path":"/username","value":"ada"}}
"###;

    fn login() -> Surfaces {
        let mut surfaces = Surfaces::new();
        for line in LOGIN.lines().filter(|line| !line.trim().is_empty()) {
            surfaces.apply_json(line).unwrap();
        }
        surfaces
    }

    #[test]
    fn applies_the_official_login_example() {
        let surfaces = login();
        let surface = surfaces.get("login").unwrap();
        assert!(surface.sends_data_model());
        assert_eq!(surface.root().unwrap().component, "Column");
        let root = surface.root().unwrap();
        let children: Vec<_> = surface
            .children(root, "")
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            children,
            [
                "form_title",
                "username_field",
                "password_field",
                "submit_button"
            ]
        );
        let field = surface.component("username_field").unwrap();
        assert_eq!(
            surface.resolve_string(field.prop("value").unwrap(), ""),
            "ada"
        );
        assert_eq!(
            surface.bound_path(field.prop("value").unwrap(), ""),
            Some("/username".to_string())
        );
    }

    #[test]
    fn action_resolves_context_bindings_into_the_spec_shape() {
        let mut surfaces = login();
        surfaces
            .get_mut("login")
            .unwrap()
            .set("/password", json!("hunter2"));
        let surface = surfaces.get("login").unwrap();
        let button = surface.component("submit_button").unwrap();
        let message = surface
            .action(button, "", "2026-11-10T00:00:00.000Z".to_string())
            .unwrap();
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({
                "version": "1.0",
                "action": {
                    "name": "login_submitted",
                    "surfaceId": "login",
                    "sourceComponentId": "submit_button",
                    "timestamp": "2026-11-10T00:00:00.000Z",
                    "context": {"user": "ada", "pass": "hunter2"}
                }
            })
        );
    }

    #[test]
    fn template_children_scope_relative_bindings_to_each_item() {
        let mut surfaces = Surfaces::new();
        surfaces
            .apply_json(
                r#"{"version":"1.0","createSurface":{"surfaceId":"s","dataModel":{"items":[{"name":"a"},{"name":"b"}]},"components":[
                    {"id":"root","component":"List","children":{"path":"/items","componentId":"row"}},
                    {"id":"row","component":"Text","text":{"@path":"name"}}]}}"#,
            )
            .unwrap();
        let surface = surfaces.get("s").unwrap();
        let stamps = surface.children(surface.root().unwrap(), "");
        assert_eq!(
            stamps,
            [
                ("row".to_string(), "/items/0".to_string()),
                ("row".to_string(), "/items/1".to_string())
            ]
        );
        let row = surface.component("row").unwrap();
        let texts: Vec<_> = stamps
            .iter()
            .map(|(_, scope)| surface.resolve_string(row.prop("text").unwrap(), scope))
            .collect();
        assert_eq!(texts, ["a", "b"]);
    }

    #[test]
    fn update_data_model_without_path_replaces_the_model() {
        let mut surfaces = login();
        surfaces
            .apply_json(
                r#"{"version":"1.0","updateDataModel":{"surfaceId":"login","value":{"fresh":1}}}"#,
            )
            .unwrap();
        assert_eq!(surfaces.get("login").unwrap().data(), &json!({"fresh": 1}));
    }

    #[test]
    fn errors_are_reported_not_applied() {
        let mut surfaces = login();
        assert_eq!(
            surfaces.apply_json(r#"{"version":"0.9","deleteSurface":{"surfaceId":"login"}}"#),
            Err(SurfaceError::UnsupportedVersion("0.9".to_string()))
        );
        assert_eq!(
            surfaces.apply_json(r#"{"version":"1.0","deleteSurface":{"surfaceId":"nope"}}"#),
            Err(SurfaceError::UnknownSurface("nope".to_string()))
        );
        assert!(matches!(
            surfaces
                .apply_json(r#"{"version":"1.0","callRendererFunction":{"functionCallId":"f"}}"#),
            Err(SurfaceError::Parse(_))
        ));
        assert_eq!(
            surfaces.apply_json(r#"{"version":"1.0","deleteSurface":{"surfaceId":"login"}}"#),
            Ok("login".to_string())
        );
        assert!(surfaces.get("login").is_none());
    }

    #[test]
    fn literals_and_missing_bindings_resolve_for_display() {
        let surfaces = login();
        let surface = surfaces.get("login").unwrap();
        assert_eq!(surface.resolve_string(&json!("hi"), ""), "hi");
        assert_eq!(surface.resolve_string(&json!(42), ""), "42");
        assert_eq!(surface.resolve_string(&json!({"@path": "/absent"}), ""), "");
        assert_eq!(
            surface.resolve(&json!({"@call": "formatDate", "args": {}}), ""),
            None
        );
        assert!(!surface.resolve_bool(&json!({"@path": "/absent"}), ""));
    }
}
