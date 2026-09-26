//! Closed set of local synchronous tools. Selection may fail; execution always
//! produces a result. Approval policy remains at the execution/approval boundary.

#[cfg(test)]
use serde_json::Value;

use super::{catalog::Definition, config, fs, knowledge, memory, recall, ToolSessionState};
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
    #[cfg(test)]
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

    pub(super) fn definition(self, title: String, description: String) -> Definition {
        Definition {
            id: self.id().into(),
            title,
            description,
            input_schema: super::input::schema(self.id()).expect("registered tool input"),
            permission: self.permission(),
        }
    }

    #[cfg(test)]
    fn find(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.id() == id)
    }
}

pub(super) fn execute(
    state: &ToolSessionState,
    input: &super::input::ToolInput,
    allow_external: bool,
) -> Option<super::output::Response> {
    use super::input::ToolInput;
    Some(match input {
        ToolInput::ReadFile(input) => fs::read(state, input, allow_external),
        ToolInput::Glob(input) => fs::glob(state, input, allow_external),
        ToolInput::Grep(input) => fs::grep(state, input, allow_external),
        ToolInput::WriteFile(input) => fs::write(state, input),
        ToolInput::EditFiles(input) => fs::edit(state, input),
        ToolInput::ReadConfig(_) => config::read(state),
        ToolInput::WriteConfig(input) => config::write(state, input),
        ToolInput::ReadSkill(input) => {
            crate::skills::execute_read(state.skill_registry(), &input.id)
        }
        ToolInput::SearchRecall(input) => recall::search(state, input),
        ToolInput::ReadRecall(input) => recall::read(state, input),
        ToolInput::ReadKnowledge(input) => knowledge::read(state, input),
        ToolInput::WriteKnowledge(input) => knowledge::write(state, input),
        ToolInput::UpdateMemory(input) => memory::execute(input),
        _ => return None,
    })
}

#[cfg(test)]
pub(super) fn execute_auto(state: &ToolSessionState, id: &str, value: &Value) -> Option<Value> {
    let tool = SynchronousTool::find(id)?;
    if tool.permission() != ToolPermission::AutoAllowRead {
        return None;
    }
    Some(match super::input::ToolInput::parse(id, value) {
        Ok(input) => execute(state, &input, false)
            .expect("synchronous tool")
            .to_json(),
        Err(message) => super::error_output(message),
    })
}

#[cfg(test)]
pub(super) fn execute_approved(state: &ToolSessionState, id: &str, value: &Value) -> Value {
    match super::input::ToolInput::parse(id, value) {
        Ok(input) => super::execute_approved(state, &input).to_json(),
        Err(message) => super::error_output(message),
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
