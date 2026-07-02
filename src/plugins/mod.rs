//! Plugin manifests and WASM validation for future hot-reloadable pane
//! development. The runtime host path is intentionally not wired into the app
//! shell yet; built-in terminal and agent panes are currently native sessions.

#![allow(dead_code)]

mod builtin;
mod host;
mod manifest;

#[cfg(test)]
mod tests {
    use super::{
        builtin::builtin_manifests,
        manifest::{BuiltinPlugin, PluginEntrypoint},
    };

    #[test]
    fn builtin_manifests_describe_native_terminal_and_agent_plugins() {
        let manifests = builtin_manifests();

        assert_eq!(manifests.len(), 2);
        assert_eq!(manifests[0].id, "builtin.terminal");
        assert_eq!(manifests[0].name, "Terminal");
        assert_eq!(manifests[1].id, "builtin.agent");
        assert_eq!(manifests[1].name, "AI Agent");
        assert!(matches!(
            manifests[0].entrypoint,
            PluginEntrypoint::Builtin {
                kind: BuiltinPlugin::Terminal
            }
        ));
        assert!(matches!(
            manifests[1].entrypoint,
            PluginEntrypoint::Builtin {
                kind: BuiltinPlugin::Agent
            }
        ));
    }
}
