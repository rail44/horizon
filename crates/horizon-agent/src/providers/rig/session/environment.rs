//! Provider half of the serialized environment handoff. No provider request
//! or tool execution can start while the host publishes replacement resources.
use super::state::SessionLoopState;
use crate::contract::{Command, Event};

impl SessionLoopState {
    pub(super) async fn activate_environment(&mut self) {
        while !self.has_pending_stop() {
            let Some(base) = self.activation.pop_front() else {
                break;
            };
            let _ = self.events_tx.send(Event::EnvironmentReady { base }.into());
            while let Some(command) = self.commands.recv().await {
                match command {
                    Command::EnvironmentPrepared {
                        workspace_root,
                        trusted_project,
                    } => {
                        let environment = crate::prompt::SessionEnvironment::for_workspace_root(
                            Some(&workspace_root),
                        );
                        let mut config = self.config.clone();
                        config.trusted_project = trusted_project;
                        let sections = super::super::session_prompt::session_extra_sections(
                            &environment,
                            &config,
                            self.role,
                            trusted_project,
                        );
                        self.environment = environment;
                        self.extra_sections = sections;
                        self.config = config;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{InputResult, SessionInput};
    use rig_core::completion::Message;

    #[tokio::test]
    async fn prepared_environment_preserves_history_model_memory_and_answer_route() {
        let (commands, receive) = tokio::sync::mpsc::unbounded_channel();
        let mut state = SessionLoopState {
            commands: receive,
            ..Default::default()
        };
        state.config.model = "spawn-model".into();
        state.memory = Some(Default::default());
        state
            .rig_history
            .push(Message::user("retained consultation"));
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
                trusted_project: false,
            })
            .unwrap();
        state.activate_environment().await;
        assert_eq!(state.environment.cwd, root);
        assert_eq!(state.config.model, "spawn-model");
        assert!(state.memory.is_some());
        assert_eq!(state.rig_history, [Message::user("retained consultation")]);
        assert_eq!(
            state
                .inputs
                .finish(InputResult::Interrupted)
                .unwrap()
                .reply_to
                .as_deref(),
            Some("opaque:task")
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
        state.extra_sections = vec!["original instructions".into()];
        state.activation.push_back("missing".into());
        commands
            .send(Command::EnvironmentActivationFailed {
                message: "base missing".into(),
            })
            .unwrap();
        state.activate_environment().await;
        assert_eq!(state.environment, old_environment);
        assert_eq!(state.extra_sections, ["original instructions"]);
        assert!(state
            .inputs
            .take_additions()
            .unwrap()
            .contains("base missing"));
    }
}
