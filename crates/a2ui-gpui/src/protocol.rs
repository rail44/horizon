//! The A2UI v1.0 wire vocabulary this spike understands.
//!
//! Field names follow `specification/v1_0/json/agent_to_renderer.json` and
//! `renderer_to_agent.json` in the A2UI repository. Only the messages the
//! minimal round trip needs are modelled; any other message kind fails to
//! parse, which [`crate::Surfaces::apply_json`] reports as an error.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The only protocol version this renderer accepts.
pub const PROTOCOL_VERSION: &str = "1.0";

/// One agent-to-renderer message: a `version` plus exactly one body key.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ServerMessage {
    pub version: String,
    #[serde(flatten)]
    pub body: ServerBody,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ServerBody {
    CreateSurface(CreateSurface),
    UpdateComponents(UpdateComponents),
    UpdateDataModel(UpdateDataModel),
    DeleteSurface(DeleteSurface),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSurface {
    pub surface_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_id: Option<String>,
    #[serde(default)]
    pub send_data_model: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<Component>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_model: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateComponents {
    pub surface_id: String,
    pub components: Vec<Component>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDataModel {
    pub surface_id: String,
    /// A JSON Pointer; absent (or `/`) replaces the whole data model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Required by the spec. An explicit `null` removes the key at `path`.
    pub value: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSurface {
    pub surface_id: String,
}

/// One component in the flat adjacency list. Type-specific properties
/// (`text`, `child`, `children`, `action`, ...) stay as raw JSON and are read
/// by the renderer, so an unknown component type still parses.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Component {
    pub id: String,
    pub component: String,
    #[serde(flatten)]
    pub props: Map<String, Value>,
}

impl Component {
    pub fn prop(&self, name: &str) -> Option<&Value> {
        self.props.get(name)
    }

    pub fn prop_str(&self, name: &str) -> Option<&str> {
        self.props.get(name).and_then(Value::as_str)
    }
}

/// A property value: a literal, a data binding, or a function call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dynamic<'a> {
    Literal(&'a Value),
    /// `{"@path": "..."}` -- absolute when it starts with `/`, otherwise
    /// relative to the enclosing template item.
    Path(&'a str),
    /// `{"@call": "...", "args": {...}}` -- not evaluated by this spike.
    Call(&'a Value),
}

impl<'a> Dynamic<'a> {
    pub fn of(value: &'a Value) -> Self {
        if let Value::Object(map) = value {
            if let Some(Value::String(path)) = map.get("@path") {
                return Self::Path(path);
            }
            if map.contains_key("@call") {
                return Self::Call(value);
            }
        }
        Self::Literal(value)
    }
}

/// A container's children: fixed ids, or a template stamped once per item of
/// an array in the data model.
#[derive(Clone, Debug, PartialEq)]
pub enum ChildList<'a> {
    Ids(Vec<&'a str>),
    Template {
        path: &'a str,
        component_id: &'a str,
    },
}

impl<'a> ChildList<'a> {
    pub fn of(value: &'a Value) -> Option<Self> {
        match value {
            Value::Array(ids) => Some(Self::Ids(ids.iter().filter_map(Value::as_str).collect())),
            Value::Object(map) => Some(Self::Template {
                path: map.get("path")?.as_str()?,
                component_id: map.get("componentId")?.as_str()?,
            }),
            _ => None,
        }
    }
}

/// One renderer-to-agent message. Only `action` is produced by this spike.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClientMessage {
    pub version: String,
    #[serde(flatten)]
    pub body: ClientBody,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ClientBody {
    Action(Action),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_message: Option<String>,
    pub surface_id: String,
    pub source_component_id: String,
    /// ISO 8601 / RFC 3339, UTC.
    pub timestamp: String,
    /// The event's `context` with every binding resolved.
    pub context: Map<String, Value>,
}
