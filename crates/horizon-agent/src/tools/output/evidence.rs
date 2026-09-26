use serde::{Deserialize, Serialize};

/// Independent evidence can coexist (for example filesystem and network
/// denials). None means not observed, rather than a fabricated negative fact.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied_domains: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied_filesystem_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied_mach_services: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ungrantable_filesystem_paths: Vec<UngrantablePath>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub denied_network_routes: Vec<NetworkDenial>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_approved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_domains: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mach_service_grant_approved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_mach_services: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denial_collection_unavailable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denial_collection_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_execution_approved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_filesystem_grants: Option<Vec<FilesystemGrant>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_trigger_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_operation_approved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_git_metadata_roots: Option<Vec<String>>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct UngrantablePath {
    pub path: String,
    pub guidance: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct NetworkDenial {
    pub target: String,
    pub operation: String,
    pub reason: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct FilesystemGrant {
    pub path: String,
    pub access: String,
    pub scope: String,
}
