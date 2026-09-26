//! Deserialize model or replayed input once at the execution/approval boundary.
//! Handlers receive typed arguments; the original JSON remains in the audit log.

mod bounded;
mod context;
mod filesystem;
mod memory;

pub(crate) use context::*;
pub(crate) use filesystem::*;
pub(crate) use memory::MemoryUpdate;

use crate::contract::ToolCallRequest;
use schemars::JsonSchema;
use serde_json::Value;

// One id-to-type mapping owns both decoding and advertised schemas.
macro_rules! inputs {
    ($( $variant:ident($ty:ty) => $id:literal ),+ $(,)?) => {
        #[derive(Clone, Debug)]
        pub(crate) enum ToolInput { $( $variant($ty), )+ External }

        impl ToolInput {
            #[cfg(test)]
            fn as_json(&self) -> Value {
                match self { $( Self::$variant(input) => serde_json::to_value(input).unwrap(), )+ Self::External => panic!("external input") }
            }

            pub(crate) fn parse(id: &str, value: &Value) -> Result<Self, String> {
                let input = match id {
                    $( $id => decode(value).map(Self::$variant), )+
                    _ => Ok(Self::External),
                }.map_err(|error| format!("invalid {id} input: {error}"))?;
                input.validate().map_err(|error| format!("invalid {id} input: {error}"))?;
                Ok(input)
            }
        }

        pub(crate) fn schema(id: &str) -> Option<Value> {
            match id { $( $id => Some(schema_for::<$ty>()), )+ _ => None }
        }
    };
}

inputs! {
    ReadFile(ReadFile) => "fs.read",
    Glob(Glob) => "fs.glob",
    Grep(Grep) => "fs.grep",
    WriteFile(WriteFile) => "fs.write",
    EditFiles(EditFiles) => "fs.edit",
    Bash(Bash) => "bash",
    ReadConfig(Empty) => "config.read",
    WriteConfig(ConfigWrite) => "config.write",
    ReadSkill(ReadEntry) => "skill.read",
    SearchRecall(RecallSearch) => "recall.search",
    ReadRecall(RecallRead) => "recall.read",
    ReadKnowledge(ReadEntry) => "knowledge.read",
    WriteKnowledge(KnowledgeWrite) => "knowledge.write",
    UpdateMemory(MemoryUpdate) => "memory.update",
    Task(Task) => "task",
    TaskOutput(TaskOutput) => "task_output",
    WebSearch(WebSearch) => "web_search",
    WebFetch(WebFetch) => "web_fetch",
}

fn schema_for<T: JsonSchema>() -> Value {
    let settings = schemars::generate::SchemaSettings::draft07().with(|settings| {
        settings.inline_subschemas = true;
    });
    let mut schema = settings.into_generator().into_root_schema_for::<T>();
    schema.remove("$schema");
    schema.remove("title");
    schema.to_value()
}

impl ToolInput {
    pub(crate) fn requires_metadata_write(&self) -> bool {
        matches!(self, Self::Bash(input) if crate::tools::command_requires_metadata_write(&input.command))
    }

    pub(crate) fn bash(&self) -> &Bash {
        match self {
            Self::Bash(input) => input,
            _ => unreachable!("bash dispatch requires Bash input"),
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Self::EditFiles(input) => {
                for (index, edit) in input.edits.iter().enumerate() {
                    if edit.old_string.is_empty() || edit.old_string == edit.new_string {
                        return Err(format!(
                            "edits[{index}] requires nonempty old_string different from new_string"
                        ));
                    }
                }
            }
            Self::SearchRecall(input) => {
                if input.query.is_none() && input.turn_outcome.is_none() {
                    return Err("supply a `query` or a `turn_outcome` filter".into());
                }
                if matches!(input.scope, RecallScope::All) && input.session_id.is_some() {
                    return Err("session_id cannot be combined with scope=all".into());
                }
            }
            Self::UpdateMemory(input) => {
                input.digest()?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn read_path(&self) -> Option<&str> {
        match self {
            Self::ReadFile(input) => Some(&input.path),
            Self::Glob(input) => Some(&input.base_path),
            Self::Grep(input) => Some(&input.base_path),
            _ => None,
        }
    }
}

pub(crate) struct PreparedCall<'a> {
    pub request: &'a ToolCallRequest,
    pub input: ToolInput,
}

impl<'a> PreparedCall<'a> {
    pub(crate) fn new(request: &'a ToolCallRequest) -> Result<Self, String> {
        Ok(Self {
            request,
            input: ToolInput::parse(&request.tool_id, &request.input)?,
        })
    }
}

impl std::ops::Deref for PreparedCall<'_> {
    type Target = ToolCallRequest;
    fn deref(&self) -> &Self::Target {
        self.request
    }
}

pub(crate) fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, String> {
    if !value.is_object() {
        return Err("input must be a JSON object".into());
    }
    serde_path_to_error::deserialize(value).map_err(|error| error.to_string())
}

#[cfg(test)]
pub(crate) mod tests;
