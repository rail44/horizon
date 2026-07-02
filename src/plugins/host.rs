use std::path::{Path, PathBuf};

use thiserror::Error;
use wasmtime::{Engine, Module};

#[derive(Debug, Error)]
pub(crate) enum PluginError {
    #[error("wasm plugin `{path}` failed validation")]
    InvalidWasm {
        path: PathBuf,
        #[source]
        source: wasmtime::Error,
    },
}

pub(crate) struct WasmPluginHost {
    engine: Engine,
}

impl WasmPluginHost {
    pub(crate) fn new() -> Self {
        Self {
            engine: Engine::default(),
        }
    }

    pub(crate) fn validate_module(&self, path: impl AsRef<Path>) -> Result<(), PluginError> {
        let path = path.as_ref();
        Module::from_file(&self.engine, path)
            .map(|_| ())
            .map_err(|source| PluginError::InvalidWasm {
                path: path.to_path_buf(),
                source,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_reports_invalid_wasm_path() {
        let host = WasmPluginHost::new();
        let path = PathBuf::from("missing-plugin.wasm");

        let error = host.validate_module(&path).expect_err("invalid wasm");

        assert!(
            matches!(error, PluginError::InvalidWasm { path: error_path, .. } if error_path == path)
        );
    }
}
