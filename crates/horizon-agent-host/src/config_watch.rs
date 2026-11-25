//! The configuration file, read again when it changes: the agent's
//! providers follow the file without anyone asking.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use notify::event::{EventKind, ModifyKind};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::session::AgentdState;

/// How long the file must sit still before it is read again: an editor's
/// save is several events, and a file half written is not worth reading.
const SETTLE: Duration = Duration::from_millis(300);

/// Reads the providers again from the state's configuration file whenever
/// it changes. `None` when there is no file to watch or the watch cannot
/// be made; the watch lasts as long as what is returned is kept.
pub fn watch_config(state: Arc<AgentdState>) -> Option<RecommendedWatcher> {
    let path = state.config_path()?.to_path_buf();
    watch_file(&path, move || {
        if let Err(error) = state.reload_provider_config() {
            eprintln!("horizon-agent: config reload failed, keeping the previous config: {error}");
        }
    })
}

/// Calls `changed` once `file` has changed and sat still. The directory
/// is watched, not the file: an editor that saves by renaming a new file
/// over the old one leaves a watch on the old file with nothing to see.
fn watch_file(file: &Path, changed: impl Fn() + Send + 'static) -> Option<RecommendedWatcher> {
    let directory = file.parent()?.to_path_buf();
    let name = file.file_name()?.to_owned();
    let (tell, told) = mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        let ours = event
            .paths
            .iter()
            .any(|path| path.file_name() == Some(name.as_os_str()));
        if ours && changes_content(&event.kind) {
            let _ = tell.send(());
        }
    })
    .map_err(|error| eprintln!("horizon-agent: cannot watch the config file: {error}"))
    .ok()?;
    watcher
        .watch(&directory, RecursiveMode::NonRecursive)
        .map_err(|error| {
            eprintln!(
                "horizon-agent: cannot watch {}: {error}",
                directory.display()
            )
        })
        .ok()?;
    std::thread::spawn(move || {
        while told.recv().is_ok() {
            // Every change until the file sits still is one reading.
            while told.recv_timeout(SETTLE).is_ok() {}
            changed();
        }
    });
    Some(watcher)
}

/// Whether an event's kind means the file's content may have changed.
fn changes_content(kind: &EventKind) -> bool {
    match kind {
        EventKind::Create(_) | EventKind::Remove(_) => true,
        EventKind::Modify(ModifyKind::Metadata(_)) => false,
        EventKind::Modify(_) => true,
        EventKind::Access(_) | EventKind::Any | EventKind::Other => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A change of the configuration file rebuilds the provider registry in
    /// place, so the next session sees the new model: the registry's
    /// `resolved_model` and `agent_config.rig.model` both move.
    /// `OPENAI_API_KEY` only flips `api_key_present` on so `resolved_model`
    /// reports the model; nextest's per-test processes keep the `set_var`
    /// from racing another test.
    #[test]
    fn a_change_of_the_config_file_rebuilds_the_registry() {
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var("OPENAI_API_KEY", "test-only") };
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let with_model = |model: &str| {
            format!(
                "default_model = {{ provider = \"default\", model = \"{model}\" }}\n[[providers]]\nname = \"default\"\n"
            )
        };
        std::fs::write(&config, with_model("before-model")).unwrap();
        let agent_config = crate::providers::agent_config(
            &horizon_config::reload_from_path(Some(&config)).unwrap(),
        );
        let providers =
            horizon_agent::registry::ProviderRegistry::builtin_with_config(agent_config.clone());
        let state = Arc::new(AgentdState::new(
            providers,
            agent_config,
            None,
            Some(config.clone()),
            Vec::new(),
            Vec::new(),
        ));
        let model = |state: &AgentdState| {
            let providers = state.providers.lock().unwrap();
            providers.resolved_model(&providers.default_provider_id(), None)
        };
        assert_eq!(model(&state), Some("before-model".to_string()));
        let _watch = watch_config(state.clone()).expect("the watch is made");

        std::fs::write(&config, with_model("after-model")).unwrap();
        wait_for("the registry is rebuilt", || {
            model(&state).as_deref() == Some("after-model")
        });
        assert_eq!(state.agent_config.lock().unwrap().rig.model, "after-model");
    }

    #[test]
    fn a_change_of_the_file_is_told_once_it_sits_still() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "a = 1\n").unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let counted = count.clone();
        let _watch = watch_file(&file, move || {
            counted.fetch_add(1, Ordering::SeqCst);
        })
        .expect("the watch is made");

        // A save that is several writes is one change.
        for value in 2..5 {
            std::fs::write(&file, format!("a = {value}\n")).unwrap();
        }
        wait_for("the change is told", || count.load(Ordering::SeqCst) == 1);
        std::thread::sleep(SETTLE * 2);
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // Another file in the directory is not this one.
        std::fs::write(dir.path().join("other.toml"), "b = 1\n").unwrap();
        std::thread::sleep(SETTLE * 2);
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // A file renamed over this one is a change of it.
        let new = dir.path().join("config.toml.new");
        std::fs::write(&new, "a = 9\n").unwrap();
        std::fs::rename(&new, &file).unwrap();
        wait_for("the rename is told", || count.load(Ordering::SeqCst) == 2);
    }
}
