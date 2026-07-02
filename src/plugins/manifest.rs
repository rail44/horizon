use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PluginManifest {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) entrypoint: PluginEntrypoint,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PluginEntrypoint {
    Builtin { kind: BuiltinPlugin },
    Wasm { module: PathBuf },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BuiltinPlugin {
    Terminal,
    Agent,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PluginCommand {
    Render,
    Input { text: String },
    Resize { cols: u16, rows: u16 },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PluginFrame {
    pub(crate) title: String,
    pub(crate) body: String,
}
