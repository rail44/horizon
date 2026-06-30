//! Plugin manifests and WASM validation for future hot-reloadable pane
//! development. The runtime host path is intentionally not wired into the app
//! shell yet; built-in terminal and agent panes are currently native sessions.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use wasmtime::{Engine, Module};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub entrypoint: PluginEntrypoint,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginEntrypoint {
    Builtin { kind: BuiltinPlugin },
    Wasm { module: PathBuf },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinPlugin {
    Terminal,
    Agent,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginCommand {
    Render,
    Input { text: String },
    Resize { cols: u16, rows: u16 },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PluginFrame {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("wasm plugin `{path}` failed validation")]
    InvalidWasm {
        path: PathBuf,
        #[source]
        source: wasmtime::Error,
    },
}

pub struct WasmPluginHost {
    engine: Engine,
}

impl WasmPluginHost {
    pub fn new() -> Self {
        Self {
            engine: Engine::default(),
        }
    }

    pub fn validate_module(&self, path: impl AsRef<Path>) -> Result<(), PluginError> {
        let path = path.as_ref();
        Module::from_file(&self.engine, path)
            .map(|_| ())
            .map_err(|source| PluginError::InvalidWasm {
                path: path.to_path_buf(),
                source,
            })
    }
}

pub fn builtin_manifests() -> Vec<PluginManifest> {
    vec![
        PluginManifest {
            id: "builtin.terminal".to_string(),
            name: "Terminal".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            entrypoint: PluginEntrypoint::Builtin {
                kind: BuiltinPlugin::Terminal,
            },
        },
        PluginManifest {
            id: "builtin.agent".to_string(),
            name: "AI Agent".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            entrypoint: PluginEntrypoint::Builtin {
                kind: BuiltinPlugin::Agent,
            },
        },
    ]
}
