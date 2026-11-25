use serde::{Deserialize, Serialize};

/// What a result says about how its call was approved and contained. None
/// means not observed, rather than a fabricated negative fact.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Evidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_approved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandboxed: Option<bool>,
    /// Hosts the sandbox runtime's proxy refused this call.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refused_hosts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_approved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_domains: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_execution_approved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_source: Option<String>,
}
