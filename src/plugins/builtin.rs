use super::manifest::{BuiltinPlugin, PluginEntrypoint, PluginManifest};

pub(crate) fn builtin_manifests() -> Vec<PluginManifest> {
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
