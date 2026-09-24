//! Closed set of local synchronous tools. Selection may fail; execution always
//! produces a result. Approval policy remains at the execution/approval boundary.

use serde_json::Value;

use super::{
    catalog::Definition, config, error_output, fs, knowledge, memory, recall, ToolSessionState,
};
use crate::contract::ToolPermission;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SynchronousTool {
    ReadFile,
    Glob,
    Grep,
    WriteFile,
    EditFile,
    ReadConfig,
    WriteConfig,
    ReadSkill,
    SearchRecall,
    ReadRecall,
    ReadKnowledge,
    WriteKnowledge,
    UpdateMemory,
}

impl SynchronousTool {
    const ALL: [Self; 13] = [
        Self::ReadFile,
        Self::Glob,
        Self::Grep,
        Self::WriteFile,
        Self::EditFile,
        Self::ReadConfig,
        Self::WriteConfig,
        Self::ReadSkill,
        Self::SearchRecall,
        Self::ReadRecall,
        Self::ReadKnowledge,
        Self::WriteKnowledge,
        Self::UpdateMemory,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::ReadFile => "fs.read",
            Self::Glob => "fs.glob",
            Self::Grep => "fs.grep",
            Self::WriteFile => "fs.write",
            Self::EditFile => "fs.edit",
            Self::ReadConfig => "config.read",
            Self::WriteConfig => "config.write",
            Self::ReadSkill => "skill.read",
            Self::SearchRecall => "recall.search",
            Self::ReadRecall => "recall.read",
            Self::ReadKnowledge => "knowledge.read",
            Self::WriteKnowledge => "knowledge.write",
            Self::UpdateMemory => memory::TOOL_ID,
        }
    }

    fn permission(self) -> ToolPermission {
        match self {
            Self::WriteFile | Self::EditFile | Self::WriteConfig => ToolPermission::RequireApproval,
            Self::ReadFile
            | Self::Glob
            | Self::Grep
            | Self::ReadConfig
            | Self::ReadSkill
            | Self::SearchRecall
            | Self::ReadRecall
            | Self::ReadKnowledge
            | Self::WriteKnowledge
            | Self::UpdateMemory => ToolPermission::AutoAllowRead,
        }
    }

    pub(super) fn definition(
        self,
        title: String,
        description: String,
        input_schema: Value,
    ) -> Definition {
        Definition {
            id: self.id().into(),
            title,
            description,
            input_schema,
            permission: self.permission(),
        }
    }

    fn find(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.id() == id)
    }

    fn execute(self, state: &ToolSessionState, input: &Value, read_scope: ReadScope) -> Value {
        let allow_external = matches!(read_scope, ReadScope::ApprovedExternal);
        match self {
            Self::ReadFile => fs::read(state, input, allow_external),
            Self::Glob => fs::glob(state, input, allow_external),
            Self::Grep => fs::grep(state, input, allow_external),
            Self::WriteFile => fs::write(state, input),
            Self::EditFile => fs::edit(state, input),
            Self::ReadConfig => config::read(state, input),
            Self::WriteConfig => config::write(state, input),
            Self::ReadSkill => crate::skills::execute_read(state.skill_registry(), input),
            Self::SearchRecall => recall::search(state, input),
            Self::ReadRecall => recall::read(state, input),
            Self::ReadKnowledge => knowledge::read(state, input),
            Self::WriteKnowledge => knowledge::write(state, input),
            Self::UpdateMemory => memory::execute(input),
        }
    }
}

enum ReadScope {
    Workspace,
    ApprovedExternal,
}

pub(super) fn execute_auto(state: &ToolSessionState, id: &str, input: &Value) -> Option<Value> {
    let tool = SynchronousTool::find(id)?;
    (tool.permission() == ToolPermission::AutoAllowRead)
        .then(|| tool.execute(state, input, ReadScope::Workspace))
}

pub(super) fn execute_approved(state: &ToolSessionState, id: &str, input: &Value) -> Value {
    match SynchronousTool::find(id) {
        Some(
            tool @ (SynchronousTool::ReadFile
            | SynchronousTool::Glob
            | SynchronousTool::Grep
            | SynchronousTool::WriteFile
            | SynchronousTool::EditFile
            | SynchronousTool::WriteConfig),
        ) => tool.execute(state, input, ReadScope::ApprovedExternal),
        _ => error_output(format!("tool `{id}` has no Horizon-side execution")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_and_local_execution_agree_on_ids_and_permissions() {
        let definitions = super::super::definitions();
        for tool in SynchronousTool::ALL {
            let matches: Vec<_> = definitions
                .iter()
                .filter(|definition| definition.id == tool.id())
                .collect();
            assert_eq!(matches.len(), 1, "{}", tool.id());
            assert_eq!(matches[0].permission, tool.permission());
        }
        let state = ToolSessionState::without_root();
        for definition in definitions {
            if let Some(tool) = SynchronousTool::find(&definition.id) {
                if tool.permission() == ToolPermission::RequireApproval {
                    assert!(execute_auto(&state, &definition.id, &serde_json::json!({})).is_none());
                }
            }
        }
        assert!(execute_auto(&state, "unknown.tool", &Value::Null).is_none());
    }
}
