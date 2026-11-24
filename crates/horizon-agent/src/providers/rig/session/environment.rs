//! Provider half of the serialized environment handoff. No provider request
//! or tool execution can start while the host publishes replacement resources.
//!
//! The session keeps sending the system prompt and tools it started with
//! (`docs/agent-core-extension-design.md`, "段": the fixed prefix a
//! provider's prompt cache reads): what the new environment changes in them
//! is told to the model in the conversation instead.
use super::state::SessionLoopState;
use crate::contract::{Command, Event};

impl SessionLoopState {
    pub(super) async fn activate_environment(&mut self) {
        horizon_sandbox::reprobe_sandbox();
        while !self.has_pending_stop() {
            let Some(base) = self.activation.pop_front() else {
                break;
            };
            let _ = self
                .events_tx
                .send(Event::EnvironmentReady { base: base.clone() }.into());
            while let Some(command) = self.commands.recv().await {
                match command {
                    Command::EnvironmentPrepared {
                        workspace_root,
                        branch,
                        trusted_project,
                    } => {
                        let environment = crate::prompt::SessionEnvironment::for_workspace_root(
                            Some(&workspace_root),
                        );
                        let mut config = self.config.clone();
                        config.trusted_project = trusted_project;
                        let resolved = super::super::session_prompt::resolve(
                            &self.modules,
                            &environment,
                            &config,
                            self.role,
                            trusted_project,
                        );
                        let changes = prefix_changes(&self.resolved, &resolved);
                        self.config = config;
                        self.inputs
                            .note_environment_ready(&workspace_root, &branch, &base);
                        if let Some(changes) = changes {
                            self.inputs.note_prefix_changes(changes);
                        }
                        break;
                    }
                    Command::EnvironmentActivationFailed { message } => {
                        let _ = self
                            .events_tx
                            .send(Event::EnvironmentActivationFailed(message.clone()).into());
                        self.inputs.note_environment_failure(message);
                        break;
                    }
                    command @ (Command::Cancel { .. } | Command::Shutdown) => {
                        self.pause_inputs(true);
                        self.inbox.push_front(command);
                    }
                    other => self.inbox.push_back(other),
                }
            }
        }
    }
}

/// What `now` changes in the prefix a session sent (`sent`), told in words:
/// the sections that now apply in full, the ones that no longer apply by
/// their first line, and the tools no longer to be used. `None` when
/// nothing changed.
fn prefix_changes(
    sent: &super::super::session_prompt::Resolved,
    now: &super::super::session_prompt::Resolved,
) -> Option<String> {
    let added: Vec<&String> = (now.sections.iter())
        .filter(|section| !sent.sections.contains(section))
        .collect();
    let gone: Vec<&str> = (sent.sections.iter())
        .filter(|section| !now.sections.contains(section))
        .map(|section| section.lines().next().unwrap_or_default())
        .collect();
    let withdrawn: Vec<String> = (sent.tools.iter())
        .filter(|tool| !now.tools.iter().any(|now| now.name == tool.name))
        .map(|tool| format!("`{}`", tool.name))
        .collect();
    if added.is_empty() && gone.is_empty() && withdrawn.is_empty() {
        return None;
    }
    let mut text = String::from(
        "What applies to this session changed with its environment; the system prompt still \
         shows what applied at its start.",
    );
    for section in added {
        text.push_str("\n\n");
        text.push_str(section);
    }
    if !gone.is_empty() {
        text.push_str("\n\nNo longer applies here: ");
        text.push_str(&gone.join("; "));
    }
    if !withdrawn.is_empty() {
        text.push_str("\n\nNo longer to be used here: ");
        text.push_str(&withdrawn.join(", "));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{InputResult, SessionInput};
    use rig_core::completion::Message;

    #[tokio::test]
    async fn prepared_environment_preserves_history_model_and_answer_route() {
        let (commands, receive) = tokio::sync::mpsc::unbounded_channel();
        let mut state = SessionLoopState {
            commands: receive,
            ..Default::default()
        };
        state.config.model = "spawn-model".into();
        state
            .rig_history
            .append_prompt(
                super::super::super::conversation::Prompt::input(
                    crate::contract::ConversationInputKind::User,
                    "retained consultation",
                ),
                &state.events_tx,
            )
            .unwrap();
        state.inputs.accept(
            SessionInput {
                resume_work: false,
                id: "input".into(),
                origin: "owner".into(),
                text: "implement".into(),
                reply_to: Some("opaque:task".into()),
            },
            false,
        );
        state.inputs.start_next();
        state.activation.push_back("selected-base".into());
        let root = std::env::current_dir().unwrap();
        commands
            .send(Command::EnvironmentPrepared {
                workspace_root: root.clone(),
                branch: "horizon/selected".into(),
                trusted_project: false,
            })
            .unwrap();
        let sent = state.environment.clone();
        state.activate_environment().await;
        assert_eq!(
            state.environment, sent,
            "the prefix sent at the start stays"
        );
        assert_eq!(state.config.model, "spawn-model");
        assert_eq!(
            state.rig_history.messages(),
            [Message::user("retained consultation")]
        );
        assert_eq!(
            state
                .inputs
                .finish(InputResult::Interrupted)
                .unwrap()
                .reply_to
                .as_deref(),
            Some("opaque:task")
        );
        let note = state.inputs.take_additions().unwrap();
        assert!(note.contains(&root.display().to_string()), "{note}");
        assert!(note.contains("horizon/selected"), "{note}");
        assert!(note.contains("selected-base"), "{note}");
    }

    /// A section the new environment no longer gives is told in the
    /// conversation, and the prefix keeps it.
    #[tokio::test]
    async fn a_changed_section_is_told_and_the_prefix_kept() {
        let (commands, receive) = tokio::sync::mpsc::unbounded_channel();
        let mut state = SessionLoopState {
            commands: receive,
            ..Default::default()
        };
        state.resolved.sections = vec!["Old instructions\nfrom the first root.".into()];
        state.activation.push_back("base".into());
        commands
            .send(Command::EnvironmentPrepared {
                workspace_root: std::env::current_dir().unwrap(),
                branch: "horizon/worktree".into(),
                trusted_project: false,
            })
            .unwrap();
        state.activate_environment().await;
        assert_eq!(
            state.resolved.sections,
            ["Old instructions\nfrom the first root."]
        );
        let note = state.inputs.take_additions().unwrap();
        assert!(
            note.contains("No longer applies here: Old instructions"),
            "{note}"
        );
    }

    #[tokio::test]
    async fn failed_environment_keeps_consultation_resources() {
        let (commands, receive) = tokio::sync::mpsc::unbounded_channel();
        let mut state = SessionLoopState {
            commands: receive,
            ..Default::default()
        };
        let old_environment = state.environment.clone();
        state.resolved.sections = vec!["original instructions".into()];
        state.activation.push_back("missing".into());
        commands
            .send(Command::EnvironmentActivationFailed {
                message: "base missing".into(),
            })
            .unwrap();
        state.activate_environment().await;
        assert_eq!(state.environment, old_environment);
        assert_eq!(state.resolved.sections, ["original instructions"]);
        assert!(
            state
                .inputs
                .take_additions()
                .unwrap()
                .contains("base missing")
        );
    }
}
