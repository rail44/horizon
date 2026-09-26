//! Typed handler results. JSON is produced only for the persisted/provider
//! boundary. External tools retain an explicit adapter for their own contract.
mod annotations;
mod evidence;
pub(crate) use crate::contract::tool_output::*;
use crate::contract::{ToolCallIdentity, ToolCallResult, ToolOutcome};
pub(crate) use annotations::*;
pub(crate) use evidence::*;
use serde::Serialize;
use serde_json::Value;

macro_rules! bodies {
    ($($variant:ident($ty:ty)),* $(,)?) => {
        #[derive(Clone, Debug, PartialEq, Serialize)]
        #[serde(untagged)]
        pub(crate) enum Body { $($variant($ty),)* External(Value) }
        $(impl From<$ty> for Body { fn from(value: $ty) -> Self { Self::$variant(value) } })*
    };
}
bodies! {
    Error(ToolError), Read(FileRead), Glob(Matches<String>), Grep(Matches<Location>),
    Written(FileWritten), Edits(FileEdits), Config(ConfigRead), Skill(SkillRead),
    Knowledge(KnowledgeRead), KnowledgeWritten(KnowledgeWritten),
    RecallSearch(RecallSearch), RecallRead(RecallRead), Memory(MemoryUpdated),
    Bash(BashOutput), Task(TaskOutput), Fetch(WebFetch), Search(WebSearch),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Response(Box<Contents>);

#[derive(Clone, Debug, PartialEq)]
struct Contents {
    body: Body,
    failed: bool,
    evidence: Evidence,
}
impl Response {
    pub(crate) fn succeeded(body: impl Into<Body>) -> Self {
        Self(Box::new(Contents {
            body: body.into(),
            failed: false,
            evidence: Evidence::default(),
        }))
    }
    pub(crate) fn failed(body: impl Into<Body>) -> Self {
        let mut response = Self::succeeded(body);
        response.mark_failed();
        response
    }
    fn evidence_mut(&mut self) -> &mut Evidence {
        &mut self.0.evidence
    }
    pub(crate) fn mark_failed(&mut self) {
        self.0.failed = true;
    }
    pub(crate) fn message(&self) -> &str {
        match &self.0.body {
            Body::Error(error) => &error.message,
            _ => "tool failed",
        }
    }
    pub(crate) fn bash_mut(&mut self) -> Option<&mut BashOutput> {
        match &mut self.0.body {
            Body::Bash(body) => Some(body),
            _ => None,
        }
    }
    /// Board and shell-hosted tools own their JSON contract outside this set.
    pub(crate) fn external(output: Value) -> Self {
        let failed = output.get("is_error").and_then(Value::as_bool) == Some(true);
        Self(Box::new(Contents {
            body: Body::External(output),
            failed,
            evidence: Evidence::default(),
        }))
    }
    pub(crate) fn into_result(self, identity: &ToolCallIdentity) -> ToolCallResult {
        let outcome = if self.0.failed {
            ToolOutcome::Failed
        } else {
            ToolOutcome::Succeeded
        };
        ToolCallResult {
            call_id: identity.call_id.clone(),
            occurrence_id: identity.occurrence_id.clone(),
            outcome,
            output: self.to_json().into(),
        }
    }
    pub(crate) fn to_json(&self) -> Value {
        let mut body = serde_json::to_value(&self.0.body).expect("built-in result serializes");
        if let Some(map) = body.as_object_mut() {
            // Retained as provider-visible data, never read back to decide a built-in outcome.
            if self.0.failed {
                map.insert("is_error".into(), Value::Bool(true));
            }
            let evidence =
                serde_json::to_value(&self.0.evidence).expect("result evidence serializes");
            map.extend(evidence.as_object().expect("evidence is an object").clone());
        }
        body
    }
}
pub(crate) fn error(message: impl Into<String>) -> Response {
    Response::failed(ToolError {
        message: message.into(),
    })
}

impl ToolCallIdentity {
    pub(crate) fn finish(&self, response: Response) -> ToolCallResult {
        response.into_result(self)
    }
}

#[cfg(test)]
mod tests;
