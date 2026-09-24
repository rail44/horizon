//! Workspace recovery owns both mutation gating and permission to overwrite its file.

use horizon_workspace::commands::CommandId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WorkspacePhase {
    Restoring,
    RestoreFailed,
    Ready,
    /// The workspace is usable, but an unreadable or newer file must survive.
    PreservingFile,
}

impl WorkspacePhase {
    pub(super) fn blocks_mutation(self) -> bool {
        matches!(self, Self::Restoring | Self::RestoreFailed)
    }

    pub(super) fn can_save(self) -> bool {
        self == Self::Ready
    }

    pub(super) fn failed(self) -> bool {
        self == Self::RestoreFailed
    }

    pub(super) fn blocks_workspace_mode(self) -> bool {
        self == Self::Restoring
    }

    pub(super) fn blocks_command(self, command: CommandId) -> bool {
        match self {
            Self::Restoring => true,
            Self::RestoreFailed => !matches!(
                command,
                CommandId::ReloadAgentRuntime | CommandId::ReloadTerminalRuntime
            ),
            Self::Ready | Self::PreservingFile => false,
        }
    }

    pub(super) fn fail_restore(&mut self) -> bool {
        if !self.blocks_mutation() {
            return false;
        }
        *self = Self::RestoreFailed;
        true
    }

    pub(super) fn finish_restore(&mut self) {
        assert!(
            self.blocks_mutation(),
            "only a pending restore may complete"
        );
        *self = Self::Ready;
    }

    pub(super) fn discard_failed_restore(&mut self) -> bool {
        if !self.failed() {
            return false;
        }
        *self = Self::Ready;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_keeps_the_file_and_layout_frozen_but_allows_recovery_commands() {
        let mut phase = WorkspacePhase::Restoring;
        assert!(phase.blocks_workspace_mode());
        assert!(phase.blocks_command(CommandId::ReloadAgentRuntime));
        assert!(!phase.can_save());
        assert!(phase.fail_restore());
        assert!(phase.blocks_mutation());
        assert!(phase.blocks_command(CommandId::NewTab));
        assert!(!phase.blocks_workspace_mode());
        assert!(!phase.blocks_command(CommandId::ReloadAgentRuntime));
        assert!(!phase.blocks_command(CommandId::ReloadTerminalRuntime));
        assert!(!phase.can_save());
        assert!(phase.discard_failed_restore());
        assert!(phase.can_save());
        assert!(!phase.blocks_mutation());
        assert!(!phase.discard_failed_restore());
    }

    #[test]
    fn successful_attachment_enables_mutations_and_persistence() {
        let mut phase = WorkspacePhase::Restoring;
        phase.finish_restore();
        assert!(phase.can_save());
        assert!(!phase.blocks_command(CommandId::NewTab));
        assert!(!phase.fail_restore());
    }

    #[test]
    fn an_unreadable_file_stays_protected_across_recovery_commands() {
        let mut phase = WorkspacePhase::PreservingFile;
        assert!(!phase.blocks_mutation());
        assert!(!phase.blocks_workspace_mode());
        assert!(!phase.blocks_command(CommandId::ReloadAgentRuntime));
        assert!(!phase.blocks_command(CommandId::ReloadTerminalRuntime));
        assert!(!phase.fail_restore());
        assert!(!phase.discard_failed_restore());
        assert!(!phase.can_save());
    }
}
